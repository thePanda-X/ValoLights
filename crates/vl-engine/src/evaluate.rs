//! Evaluation harness: compare detected "my kills" with hand-checked labels.
//!
//! Label file (tests/eval/labels/<name>.json):
//!
//! ```text
//! {"video": "example-long-16-9.mp4", "aliases": ["thePanda X"],
//!  "my_kills": [{"t": 74.5, "victim": "saauul X", "headshot": false}, ...], "rounds": 4}
//! ```
//!
//! Usage: `evaluate [--labels tests/eval/labels] [--only NAME]` or `valolights cli eval`
use serde_json::Value;
use std::path::{Path, PathBuf};
use vl_core::jobs::Cancel;
use vl_core::models::{Events, Kill};

/// seconds; labels are checked at ~0.25 s precision
const TOL: f64 = 1.0;

fn my_kills<'a>(ev: &'a Events, me: &[String]) -> Vec<&'a Kill> {
    let mut out: Vec<&Kill> = ev.kills.iter().filter(|k| k.killer != k.victim && me.contains(&k.killer)).collect();
    out.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round_ties_even() / 1000.0
}
fn r1(x: f64) -> f64 {
    (x * 10.0).round_ties_even() / 10.0
}

/// Shortest round-trip float text for the report.
fn num(x: f64) -> String {
    let s = format!("{x}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

struct Row {
    gt: usize,
    det: usize,
    tp: usize,
}

fn evaluate_one(label_path: &Path) -> anyhow::Result<Row> {
    let lab: Value = serde_json::from_str(&std::fs::read_to_string(label_path)?)?;
    let mut video = PathBuf::from(lab["video"].as_str().unwrap_or_default());
    if !video.is_absolute() {
        let base = lab.get("video_base").and_then(|v| v.as_str()).unwrap_or("../../..");
        video = vl_core::paths::clean(label_path.parent().unwrap_or(Path::new(".")).join(base).join(&video).canonicalize()?);
    }
    let mut cfg = vl_core::config::load();
    cfg.identity.aliases =
        lab["aliases"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let stem = label_path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let td = std::env::temp_dir().join(format!("vl-eval-{}-{stem}", std::process::id()));
    let _ = std::fs::remove_dir_all(&td);
    std::fs::create_dir_all(&td)?;
    let t0 = std::time::Instant::now();
    let res = crate::analyze::analyze_with(&video, &td, &|_, _| {}, &Cancel::default(), &cfg);
    let elapsed = t0.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&td);
    let (ev, _ga, info) = res?;
    let me: Vec<String> = ev.players.iter().filter(|p| !p.me_hint.is_empty()).map(|p| p.id.clone()).collect();
    let det = my_kills(&ev, &me);
    let mut gt: Vec<&Value> = lab["my_kills"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    gt.sort_by(|a, b| a["t"].as_f64().partial_cmp(&b["t"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    let mut used = vec![false; det.len()];
    let mut matches: Vec<(usize, usize, f64)> = vec![];
    for (gi, g) in gt.iter().enumerate() {
        let gtt = g["t"].as_f64().unwrap_or(0.0);
        let mut best: Option<(f64, usize)> = None;
        for (i, k) in det.iter().enumerate() {
            if used[i] {
                continue;
            }
            let d = (k.t - gtt).abs();
            if d <= TOL && best.map(|b| d < b.0).unwrap_or(true) {
                best = Some((d, i));
            }
        }
        if let Some((d, i)) = best {
            used[i] = true;
            matches.push((gi, i, d));
        }
    }
    let tp = matches.len();
    let missed: Vec<String> = gt
        .iter()
        .enumerate()
        .filter(|(gi, _)| !matches.iter().any(|m| m.0 == *gi))
        .map(|(_, g)| num(g["t"].as_f64().unwrap_or(0.0)))
        .collect();
    let false_: Vec<String> =
        det.iter().enumerate().filter(|(i, _)| !used[*i]).map(|(_, k)| num((k.t * 100.0).round_ties_even() / 100.0)).collect();
    let hs: Vec<(bool, bool)> =
        matches.iter().filter_map(|m| gt[m.0].get("headshot").and_then(|h| h.as_bool()).map(|h| (h, det[m.1].headshot))).collect();
    let timing = if matches.is_empty() { "null".into() } else { num(r3(median(matches.iter().map(|m| m.2).collect()))) };
    let hs_acc = if hs.is_empty() { "null".into() } else { num(r3(hs.iter().filter(|(a, b)| a == b).count() as f64 / hs.len() as f64)) };
    let rounds_gt = lab.get("rounds").map(|v| v.to_string()).unwrap_or_else(|| "null".into());
    println!(
        "{{\"clip\": \"{stem}\", \"duration\": {}, \"seconds\": {}, \"realtime_factor\": {}, \"gt\": {}, \"detected\": {}, \
         \"recall\": {}, \"precision\": {}, \"timing_median\": {timing}, \"headshot_acc\": {hs_acc}, \"rounds_gt\": {rounds_gt}, \
         \"rounds_detected\": {}, \"missed\": [{}], \"false\": [{}]}}",
        num(r1(info.duration)),
        num(r1(elapsed)),
        num(r3(elapsed / info.duration.max(1e-6))),
        gt.len(),
        det.len(),
        num(r3(tp as f64 / 1.max(gt.len()) as f64)),
        num(r3(tp as f64 / 1.max(det.len()) as f64)),
        ev.rounds.len(),
        missed.join(", "),
        false_.join(", "),
    );
    Ok(Row { gt: gt.len(), det: det.len(), tp })
}

/// Evaluate every label file in `labels` (optionally only names containing `only`); prints the report.
/// Returns the process exit code (1 when there are no label files).
pub fn run_eval(labels: &Path, only: Option<&str>) -> anyhow::Result<i32> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(labels)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|e| e == "json").unwrap_or(false)).collect())
        .unwrap_or_default();
    files.sort();
    if let Some(o) = only {
        files.retain(|f| f.file_stem().map(|s| s.to_string_lossy().contains(o)).unwrap_or(false));
    }
    if files.is_empty() {
        println!("no label files in {}", labels.display());
        return Ok(1);
    }
    let mut rows = vec![];
    for f in &files {
        rows.push(evaluate_one(f)?);
    }
    let tot_gt: usize = rows.iter().map(|r| r.gt).sum();
    let tot_tp: usize = rows.iter().map(|r| r.tp).sum();
    let tot_det: usize = rows.iter().map(|r| r.det).sum();
    println!(
        "
OVERALL my-kill recall {tot_tp}/{tot_gt} = {:.3}, precision {tot_tp}/{tot_det} = {:.3}",
        tot_tp as f64 / 1.max(tot_gt) as f64,
        tot_tp as f64 / 1.max(tot_det) as f64
    );
    Ok(0)
}
