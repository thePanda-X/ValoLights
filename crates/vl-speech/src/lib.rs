//! Voice-chat transcription with whisper.cpp (whisper-rs): CUDA when an NVIDIA GPU is usable, CPU otherwise.
//!
//! Transcribes the kept parts of a project (or the whole video, or one caption's range again) and writes
//! `transcript.json` in the same format the app edits. Runs in its own process (`vl-speech.exe`), so the
//! CUDA libraries never load into the app itself.
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use vl_core::config;
use vl_core::ffmpeg as ff;
use vl_core::paths;
use vl_core::projects::ProjectHandle;
use vl_core::speech_words;
use vl_core::transcript::{retime_words, Segment, Transcript, Word};
use whisper_rs::{DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// seconds of audio kept around each part, so words at a cut are heard whole
const MARGIN: f64 = 2.0;
const SR: usize = 16_000;

/// Progress callback (shared, so whisper.cpp's own progress callback can report through it).
pub type Progress = std::sync::Arc<dyn Fn(f64, &str) + Send + Sync>;

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------- models
/// The ggerganov/whisper.cpp model repository revision the downloads are pinned to.
const MODEL_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

/// SHA-256 of the model files at [`MODEL_REVISION`]; a download that doesn't match is discarded.
fn model_sha256(file: &str) -> Option<&'static str> {
    match file {
        "ggml-large-v3-turbo-q5_0.bin" => Some("394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"),
        "ggml-small-q5_1.bin" => Some("ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb"),
        "ggml-base-q5_1.bin" => Some("422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898"),
        _ => None,
    }
}

/// The whisper.cpp model file for a model name of the settings ("large-v3-turbo", "small", "base").
pub fn model_file(name: &str) -> String {
    match name {
        "large-v3-turbo" => "ggml-large-v3-turbo-q5_0.bin".into(),
        "small" => "ggml-small-q5_1.bin".into(),
        "base" => "ggml-base-q5_1.bin".into(),
        other => format!("ggml-{other}.bin"),
    }
}

fn dtw_preset(name: &str) -> Option<DtwModelPreset> {
    match name {
        "large-v3-turbo" => Some(DtwModelPreset::LargeV3Turbo),
        "small" => Some(DtwModelPreset::Small),
        "base" => Some(DtwModelPreset::Base),
        _ => None,
    }
}

pub fn models_dir() -> PathBuf {
    paths::models_dir().join("whisper-ggml")
}

/// The model file, downloaded on first use into data/models/whisper-ggml (with progress).
pub fn ensure_model(name: &str, prog: &Progress) -> Result<PathBuf> {
    let file = model_file(name);
    let path = models_dir().join(&file);
    if path.exists() {
        return Ok(path);
    }
    let Some(want) = model_sha256(&file) else { bail!("unknown speech model '{name}'") };
    std::fs::create_dir_all(models_dir())?;
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/{MODEL_REVISION}/{file}");
    prog(0.02, &format!("Downloading speech model '{name}' (first use only)…"));
    let resp = ureq::get(&url).call().with_context(|| format!("could not download the speech model from {url}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let mut reader = resp.into_body().into_reader();
    let part = path.with_extension("bin.part");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    let mut last = 0u64;
    let mut hash = Sha256::new();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])?;
        hash.update(&buf[..n]);
        got += n as u64;
        if total > 0 && got - last > 8 << 20 {
            last = got;
            prog(0.02 + 0.05 * got as f64 / total as f64, &format!("Downloading speech model · {} of {} MB", got >> 20, total >> 20));
        }
    }
    drop(out);
    if total > 0 && got != total {
        let _ = std::fs::remove_file(&part);
        bail!("the speech model download was incomplete ({got} of {total} bytes)");
    }
    let sha: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if sha != want {
        let _ = std::fs::remove_file(&part);
        bail!("the downloaded speech model is damaged or not the expected file (SHA-256 {sha}); try again");
    }
    std::fs::rename(&part, &path)?;
    Ok(path)
}

/// Is an NVIDIA driver present (nvcuda.dll loadable)? Without it whisper.cpp runs on the CPU.
pub fn nvidia_driver() -> bool {
    #[cfg(windows)]
    unsafe {
        extern "system" {
            fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
        }
        let name: Vec<u16> = "nvcuda.dll\0".encode_utf16().collect();
        !LoadLibraryW(name.as_ptr()).is_null()
    }
    #[cfg(not(windows))]
    false
}

/// whisper.cpp + model. `gpu` = try CUDA.
pub struct Model {
    ctx: WhisperContext,
    pub device: &'static str,
}

impl Model {
    pub fn load(name: &str, path: &Path, gpu: bool) -> Result<Model> {
        let gpu = gpu && cfg!(feature = "cuda") && nvidia_driver();
        let mut cp = WhisperContextParameters::default();
        cp.use_gpu(gpu);
        // word timing by DTW over the alignment heads: far closer to the speech than token timestamps
        if let Some(p) = dtw_preset(name) {
            cp.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: p }, ..Default::default() });
        }
        let ctx = WhisperContext::new_with_params(path, cp).map_err(|e| anyhow!("could not load the speech model: {e:?}"))?;
        Ok(Model { ctx, device: if gpu { "cuda" } else { "cpu" } })
    }

    /// Recognised segments of 16 kHz mono audio, times relative to the audio start.
    /// `prog(fraction)` is called from whisper.cpp while it works.
    pub fn run(&self, audio: &[f32], prompt: &str, prog: std::sync::Arc<dyn Fn(f64) + Send + Sync>) -> Result<Vec<RawSeg>> {
        let mut st = self.ctx.create_state().map_err(|e| anyhow!("speech model state: {e:?}"))?;
        let mut p = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
        p.set_language(Some("en"));
        if !prompt.is_empty() {
            p.set_initial_prompt(prompt);
        }
        p.set_no_context(true); // like condition_on_previous_text=False: no repeated hallucinations
        p.set_token_timestamps(true);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        p.set_n_threads(std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4));
        p.set_progress_callback_safe(move |pct: i32| prog(pct as f64 / 100.0));
        st.full(p, audio).map_err(|e| anyhow!("transcription failed: {e:?}"))?;
        let mut out = vec![];
        for s in st.as_iter() {
            let (s0, s1) = (s.start_timestamp() as f64 / 100.0, s.end_timestamp() as f64 / 100.0);
            let mut words: Vec<(f64, String)> = vec![];
            let mut plog = 0.0;
            let mut n = 0usize;
            for k in 0..s.n_tokens() {
                let Some(tk) = s.get_token(k) else { continue };
                let text = tk.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default();
                if text.starts_with("[_") || text.starts_with("<|") || text.is_empty() {
                    continue; // timestamps / special tokens
                }
                let d = tk.token_data();
                plog += d.plog as f64;
                n += 1;
                let t = if d.t_dtw >= 0 { d.t_dtw as f64 / 100.0 } else { d.t0 as f64 / 100.0 };
                if text.starts_with(' ') || words.is_empty() {
                    words.push((t, text.trim().to_string()));
                } else if let Some(w) = words.last_mut() {
                    w.1.push_str(&text); // continues the word ("Rey" + "na")
                }
            }
            words.retain(|w| !w.1.is_empty());
            let text = s.to_str_lossy().map(|c| c.trim().to_string()).unwrap_or_default();
            out.push(RawSeg {
                start: s0,
                end: s1,
                text,
                words,
                no_speech_prob: s.no_speech_probability() as f64,
                avg_logprob: if n > 0 { plog / n as f64 } else { 0.0 },
            });
        }
        Ok(out)
    }
}

/// One segment as whisper.cpp heard it (times relative to the audio start).
#[derive(Debug, Clone)]
pub struct RawSeg {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// (start, text) per word; a word ends where the next one starts
    pub words: Vec<(f64, String)>,
    pub no_speech_prob: f64,
    pub avg_logprob: f64,
}

// ---------------------------------------------------------------- project transcription
/// Kept parts (+ margins, merged) or the whole video.
fn ranges(h: &ProjectHandle, whole: bool) -> Vec<(f64, f64)> {
    let dur = h.project.source.duration;
    if whole {
        return vec![(0.0, dur)];
    }
    let mut parts = h.edits().parts;
    if parts.is_empty() {
        if let Some(ev) = h.events() {
            parts = vl_render::highlights::quick_cut("segments", &ev, &h.project.identity.me, &config::load().highlights, dur, &[])
                .unwrap_or_default();
        }
    }
    let mut rs: Vec<(f64, f64)> = parts.iter().map(|p| ((p.src_in - MARGIN).max(0.0), (p.src_out + MARGIN).min(dur))).collect();
    rs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = vec![];
    for (a, b) in rs {
        match merged.last_mut() {
            Some(m) if a <= m.1 => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    if merged.is_empty() {
        vec![(0.0, dur)]
    } else {
        merged
    }
}

/// Whisper reads the prompt as preceding text; a short natural sentence keeps it from echoing the list.
fn prompt(h: &ProjectHandle) -> String {
    let names: Vec<String> = h
        .events()
        .map(|ev| {
            ev.players.iter().filter(|p| !p.display.is_empty() && p.display.to_lowercase() != "me").map(|p| p.display.clone()).collect()
        })
        .unwrap_or_default();
    let vocab = speech_words::vocabulary(&names);
    format!("Valorant voice comms. {}.", vocab.iter().take(120).cloned().collect::<Vec<_>>().join(", "))
}

/// 16 kHz mono float audio of [a, b] of the source.
fn extract(src: &str, a: f64, b: f64) -> Result<Vec<f32>> {
    let out = ff::run(&[
        ff::ffmpeg_exe()?.as_str(),
        "-v",
        "error",
        "-ss",
        &format!("{a:.3}"),
        "-t",
        &format!("{:.3}", b - a),
        "-i",
        src,
        "-vn",
        "-ac",
        "1",
        "-ar",
        &SR.to_string(),
        "-f",
        "f32le",
        "-",
    ])?;
    Ok(out.stdout.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// Turn whisper.cpp's segments of one range into transcript segments (absolute source times).
fn to_segments(raw: &[RawSeg], a: f64, only: bool, voice: &[regex::Regex]) -> Vec<Segment> {
    let mut out = vec![];
    for s in raw {
        let text = s.text.trim();
        if text.is_empty() || (!only && (s.no_speech_prob > 0.75 || s.avg_logprob < -1.2)) {
            log::info!(
                "dropped {:.2}-{:.2} {text:?} (no speech {:.2}, avg logprob {:.2})",
                a + s.start,
                a + s.end,
                s.no_speech_prob,
                s.avg_logprob
            );
            continue;
        }
        let mut words: Vec<Word> = vec![];
        for (i, (t, w)) in s.words.iter().enumerate() {
            let start = t.max(s.start);
            let next = s.words.get(i + 1).map(|n| n.0).unwrap_or(s.end);
            let end = next.max(start + 0.08).min(s.end.max(start + 0.08));
            words.push(Word { start: r3(a + start), end: r3(a + end), text: w.clone(), censored: false });
        }
        if words.is_empty() {
            out.push(caption(text.to_string(), vec![], r3(a + s.start), r3(a + s.end), voice));
            continue;
        }
        // whisper.cpp keeps several short sentences in one segment ("Teleport ready. Teleport ready."):
        // one caption per sentence, timed by its words, is what the editor works with
        let mut cur: Vec<Word> = vec![];
        for w in words {
            let end_of_sentence = w.text.ends_with(['.', '!', '?']);
            cur.push(w);
            if end_of_sentence {
                let ws = std::mem::take(&mut cur);
                out.push(sentence(ws, voice));
            }
        }
        if !cur.is_empty() {
            out.push(sentence(cur, voice));
        }
    }
    out
}

/// A caption from consecutive words (times follow the words: whisper.cpp's segment times are coarse).
fn sentence(words: Vec<Word>, voice: &[regex::Regex]) -> Segment {
    let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let (start, end) = (words[0].start, words[words.len() - 1].end);
    caption(text, words, start, end, voice)
}

fn caption(text: String, words: Vec<Word>, start: f64, end: f64, voice: &[regex::Regex]) -> Segment {
    let voice_line = voice.iter().any(|p| p.is_match(&text));
    Segment { start, end, text, speaker: "s1".into(), words, voice_line, hidden: voice_line, edited: false, added: false }
}

/// Transcribe the kept parts (or the whole video). `only` re-transcribes one caption's range and merges the
/// result into the existing transcript, leaving every other caption untouched. Writes transcript.json.
pub fn transcribe_project(
    h: &ProjectHandle,
    prog: &Progress,
    whole: bool,
    only: Option<(f64, f64)>,
    force_cpu: bool,
) -> Result<Transcript> {
    if !h.project.source.has_audio {
        bail!("This video has no audio track");
    }
    let cfg = config::load();
    let name = cfg.subtitles.model.clone();
    let path = ensure_model(&name, prog)?;
    prog(0.08, "Loading speech model");
    let gpu = !force_cpu && cfg.subtitles.device != "cpu";
    let model = Model::load(&name, &path, gpu)?;
    let ranges = match only {
        Some((a0, b0)) => vec![((a0 - 0.35).max(0.0), (b0 + 0.35).min(h.project.source.duration))],
        None => ranges(h, whole),
    };
    let prompt = prompt(h);
    let total: f64 = ranges.iter().map(|(a, b)| b - a).sum::<f64>().max(1e-6);
    let voice = speech_words::voice_line_patterns();
    let mut tr = Transcript { model: format!("{name} ({})", model.device), ranges: ranges.clone(), ..Default::default() };
    let mut done = 0.0;
    let label = format!("Transcribing ({})", model.device);
    for &(a, b) in &ranges {
        let audio = extract(h.source(), a, b)?;
        let (p2, l2) = (prog.clone(), label.clone());
        let raw =
            model.run(&audio, &prompt, std::sync::Arc::new(move |f| p2(0.1 + 0.88 * ((done + f * (b - a)) / total).min(1.0), &l2)))?;
        tr.segments.extend(to_segments(&raw, a, only.is_some(), &voice));
        done += b - a;
    }
    tr.sort();
    if let Some(r) = only {
        tr = merge_line(h, tr, r);
    }
    h.save_transcript(&tr)?;
    prog(1.0, "Done");
    Ok(tr)
}

/// Replace the caption(s) inside `only` with what the model heard there this time.
fn merge_line(h: &ProjectHandle, fresh: Transcript, only: (f64, f64)) -> Transcript {
    let mut old = h.transcript().unwrap_or_default();
    let (a, b) = only;
    let words: Vec<Word> = fresh
        .segments
        .iter()
        .flat_map(|s| s.words.iter())
        .filter(|w| a - 0.3 <= (w.start + w.end) / 2.0 && (w.start + w.end) / 2.0 <= b + 0.3)
        .cloned()
        .collect();
    let text = if words.is_empty() {
        fresh.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ")
    } else {
        words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    };
    if text.trim().is_empty() {
        return old; // nothing heard: keep the user's caption
    }
    let idx = match old.segments.iter().position(|s| (s.start - a).abs() < 1e-3 && (s.end - b).abs() < 1e-3) {
        Some(i) => i,
        None => {
            old.segments.push(Segment {
                start: a,
                end: b,
                text: String::new(),
                speaker: "s1".into(),
                words: vec![],
                voice_line: false,
                hidden: false,
                edited: false,
                added: false,
            });
            old.segments.len() - 1
        }
    };
    let target = &mut old.segments[idx];
    target.text = text.trim().to_string();
    if words.is_empty() {
        let t = target.text.clone();
        retime_words(target, &t);
    } else {
        target.words = words
            .into_iter()
            .map(|mut w| {
                w.start = w.start.max(a);
                w.end = w.end.min(b);
                w
            })
            .collect();
    }
    target.hidden = false;
    target.edited = true;
    old.model = fresh.model;
    old.sort();
    old
}
