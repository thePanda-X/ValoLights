//! `valolights --worker job.json`: runs one background job and reports JSON lines on stdout.
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use vl_core::jobs::{emit, emit_done, emit_error, emit_progress};
use vl_core::projects;

use crate::backend;

pub fn run_job(spec_path: &str) -> i32 {
    let res = (|| -> Result<Value> {
        let spec: Value = serde_json::from_str(&std::fs::read_to_string(spec_path)?)?;
        let kind = spec.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        match kind.as_str() {
            "analyze" => analyze(&spec),
            "transcribe" => transcribe(spec_path),
            "render" => render(&spec, false),
            "proxy" => render(&spec, true),
            "smart" => smart(&spec),
            k => Err(anyhow!("unknown job kind {k}")),
        }
    })();
    match res {
        Ok(Value::Null) => 0, // transcribe relayed its own final line
        Ok(r) => {
            emit_done(r);
            0
        }
        Err(e) => {
            log::error!("job failed: {e:?}");
            emit_error(&e.to_string(), &format!("{e:?}"));
            1
        }
    }
}

fn progress(p: f64, msg: &str) {
    emit_progress(p, msg);
}

fn project_dir(spec: &Value) -> Result<PathBuf> {
    Ok(PathBuf::from(spec.get("project_dir").and_then(Value::as_str).context("project_dir missing")?))
}

fn analyze(spec: &Value) -> Result<Value> {
    let mut h = projects::open_dir(&project_dir(spec)?)?;
    // names the user confirmed as "me" survive a re-analysis (player ids are renumbered)
    let mut kept: HashSet<String> = HashSet::new();
    if h.project.identity.confirmed && !h.project.identity.me.is_empty() {
        if let Some(old) = h.events() {
            for p in old.players.iter().filter(|p| h.project.identity.me.contains(&p.id)) {
                for n in p.names.iter().take(3).chain(std::iter::once(&p.display)) {
                    if !n.is_empty() {
                        kept.insert(backend::normalize_name(n));
                    }
                }
            }
            // only names that identify someone: never OCR scraps ("1", "M") or the generic "Me"
            kept.retain(|n| n.chars().count() >= 3 && !backend::is_me_text(n));
        }
    }
    h.project.status = "analyzing".into();
    h.save()?;
    let src = PathBuf::from(h.source());
    let (mut ev, ga) = match backend::analyze(&src, &h.dir, &progress) {
        Ok(r) => r,
        Err(e) => {
            h.project.status = "error".into();
            h.project.error = e.to_string();
            h.save()?;
            return Err(e);
        }
    };
    // low-res sources are identified by portraits + gold rows, not names
    if !kept.is_empty() && ga.h >= 720 {
        let picked: Vec<String> = ev
            .players
            .iter()
            .filter(|p| {
                p.names
                    .iter()
                    .take(3)
                    .chain(std::iter::once(&p.display))
                    .any(|n| !n.is_empty() && kept.contains(&backend::normalize_name(n)))
            })
            .map(|p| p.id.clone())
            .collect();
        if !picked.is_empty() {
            for p in ev.players.iter_mut() {
                p.me_hint = if picked.contains(&p.id) { "user".into() } else { String::new() };
            }
            backend::finalize_identity(&mut ev, &[]);
        }
    }
    // the Smart cut's sound signals don't depend on the video analysis: keep them
    ev.audio = h.events().and_then(|old| old.audio);
    h.save_events(&ev)?;
    h.project.game_area = Some(ga);
    let me: Vec<String> = ev.players.iter().filter(|p| !p.me_hint.is_empty()).map(|p| p.id.clone()).collect();
    h.project.identity.me = me.clone();
    h.project.identity.confirmed = !me.is_empty();
    h.project.status = "ready".into();
    h.project.error.clear();
    h.project.analysis_version = ev.engine_version.clone();
    h.save()?;
    Ok(json!({"kills": ev.kills.len(), "me": me}))
}

/// Smart cut's sound pass: what the clip sounds like (laughter, shouting, loud voices) and the funny / hype
/// words of its transcript, stored with the events.
fn smart(spec: &Value) -> Result<Value> {
    let h = projects::open_dir(&project_dir(spec)?)?;
    let mut ev = h.events().context("analyse the clip first")?;
    progress(0.01, "Listening to the clip…");
    let src = PathBuf::from(h.source());
    let mut a =
        backend::analyze_audio(&src, h.project.source.duration, &|p| progress(0.02 + 0.96 * p, "Listening for laughter and reactions…"))?;
    a.words = backend::smart_words(&transcript_words(&h));
    let n = a.loud.len();
    ev.audio = Some(a);
    h.save_events(&ev)?;
    Ok(json!({"frames": n}))
}

/// Every word of the clip's transcript: (start, text).
pub fn transcript_words(h: &projects::ProjectHandle) -> Vec<(f64, String)> {
    h.transcript()
        .map(|tr| tr.segments.iter().flat_map(|s| s.words.iter().map(|w| (w.start, w.text.clone()))).collect())
        .unwrap_or_default()
}

fn render(spec: &Value, proxy: bool) -> Result<Value> {
    let h = projects::open_dir(&project_dir(spec)?)?;
    let out = PathBuf::from(spec.get("out").and_then(Value::as_str).context("out missing")?);
    let opts = spec.get("options").cloned().unwrap_or(json!({}));
    let out = backend::render_project(&h, &out, &progress, proxy, &opts)?;
    Ok(json!({"out": out.to_string_lossy()}))
}

/// Run the speech helper (vl-speech.exe) with the same spec; relay its JSON lines. Returns Null when the helper
/// printed its own final line (done / error), else an error.
fn transcribe(spec_path: &str) -> Result<Value> {
    match run_speech(spec_path, false) {
        // the GPU run died without an answer (e.g. a CUDA kernel this GPU can't run): once more on the CPU
        Err(e) if e.to_string().starts_with("the speech helper stopped") => {
            log::warn!("speech helper failed on the GPU, retrying on the CPU: {e}");
            emit_progress(0.1, "GPU failed, transcribing on CPU (slower)");
            run_speech(spec_path, true)
        }
        r => r,
    }
}

/// Run `vl-speech.exe` and relay its JSON lines. Ok(Null) once it reported its own final line.
fn run_speech(spec_path: &str, cpu: bool) -> Result<Value> {
    let mut child = crate::speech::transcribe_command(Path::new(spec_path), cpu)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start the speech helper (vl-speech.exe)")?;
    let mut err_pipe = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err_pipe.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let mut final_seen = false;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
            let s = String::from_utf8_lossy(&line);
            if let Ok(v) = serde_json::from_str::<Value>(s.trim()) {
                if matches!(v.get("type").and_then(Value::as_str), Some("done") | Some("error")) {
                    final_seen = true;
                }
                emit(&v);
            }
        }
    }
    let status = child.wait()?;
    let stderr = err_thread.join().unwrap_or_default();
    if final_seen {
        return Ok(Value::Null);
    }
    let tail: String = stderr.trim().lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    Err(anyhow!(
        "the speech helper stopped without an answer (exit code {}){}{}",
        status.code().map(|c| c.to_string()).unwrap_or("?".into()),
        if tail.is_empty() { "" } else { "\n" },
        tail
    ))
}
