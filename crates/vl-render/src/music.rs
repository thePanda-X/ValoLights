//! Music: song analysis (beats, bars, loudness), the best part of a song, beat-synced cuts, and the one
//! filter chain that places the song on the export timeline (used by the render and the previews alike).
//! the analysis is cached in data/cache/music/*.json.
//!
//! Times: "song" = seconds in the music file; "edit" = seconds in the exported video (transitions included).
use crate::editlist::EditList;
use crate::numfmt::{self, f, json_f, json_list, json_str, percentile, repr, round_i, round_n};
use anyhow::{bail, Context, Result};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use vl_core::ffmpeg as ff;
use vl_core::models::{MusicSettings, Part};
use vl_core::paths;

pub const SR: usize = 22050;
pub const HOP: usize = 512;
/// onset frames per second (~43)
pub const FPS: f64 = SR as f64 / HOP as f64;
pub const ANALYSIS_VERSION: i64 = 1;
pub const FADE_OUT: f64 = 1.5;
pub const FADE_IN: f64 = 0.25;
const N_FFT: usize = 2048;

/// The analysis of a song (its cache JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SongInfo {
    #[serde(default)]
    pub version: i64,
    pub duration: f64,
    pub bpm: f64,
    pub beats: Vec<f64>,
    pub bars: Vec<f64>,
    pub energy: Vec<f64>,
    pub wave: Vec<f64>,
}

impl SongInfo {
    /// `json.dumps(res)` byte for byte.
    pub fn to_cache_json(&self) -> String {
        format!(
            "{{\"version\": {}, \"duration\": {}, \"bpm\": {}, \"beats\": {}, \"bars\": {}, \"energy\": {}, \"wave\": {}}}",
            self.version,
            json_f(self.duration),
            json_f(self.bpm),
            json_list(&self.beats),
            json_list(&self.bars),
            json_list(&self.energy),
            json_list(&self.wave)
        )
    }
}

// ---------------------------------------------------------------- placement
/// Edit-time range the music plays in.
pub fn span(mus: &MusicSettings, total: f64) -> (f64, f64) {
    let a = mus.start.max(0.0).min(total);
    let b = match mus.end {
        None => total,
        Some(e) => e.max(a).min(total),
    };
    (a, b)
}

/// Filter chain (after a 48 kHz stereo input, looped with -stream_loop -1) that turns the song into a track
/// exactly `total` seconds long on the edit timeline: the chosen part, faded, delayed to its start.
pub fn shape_filter(mus: &MusicSettings, total: f64, volume: bool) -> String {
    let (a, b) = span(mus, total);
    let length = (b - a).max(0.05);
    let fo = FADE_OUT.min(length / 3.0);
    let mut chain =
        vec![format!("atrim=start={}:duration={}", f(mus.song_in.max(0.0), 3), f(length, 3)), "asetpts=PTS-STARTPTS".to_string()];
    if mus.song_in > 0.05 {
        chain.push(format!("afade=t=in:d={}", f(FADE_IN.min(length / 4.0), 3)));
    }
    chain.push(format!("afade=t=out:st={}:d={}", f(length - fo, 3), f(fo, 3)));
    if volume {
        chain.push(format!("volume={}", f(mus.volume, 2)));
    }
    if a > 0.001 {
        chain.push(format!("adelay={}:all=1", round_i(a * 1000.0)));
    }
    chain.push("apad".into());
    chain.push(format!("atrim=0:{}", f(total, 3)));
    chain.join(",")
}

/// The file's modification time (float seconds).
pub fn mtime(p: &Path) -> Result<f64> {
    let m = std::fs::metadata(p)?.modified()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Ok(d.as_secs() as f64 + d.subsec_nanos() as f64 * 1e-9)
}

fn sha1_hex(s: &str) -> String {
    let d = Sha1::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// `json.dumps(mus.model_dump(), sort_keys=True)`
fn music_json(m: &MusicSettings) -> String {
    format!(
        "{{\"duck\": {}, \"enabled\": {}, \"end\": {}, \"game_volume\": {}, \"path\": {}, \"song_in\": {}, \"start\": {}, \"synced\": {}, \"volume\": {}}}",
        m.duck,
        m.enabled,
        m.end.map(json_f).unwrap_or_else(|| "null".into()),
        json_f(m.game_volume),
        json_str(&m.path),
        json_f(m.song_in),
        json_f(m.start),
        m.synced,
        json_f(m.volume)
    )
}

/// The music as heard under a back-to-back preview of `parts` (no transitions in the preview). Each part
/// gets exactly the music it gets in the export, so cuts land on the same beats. Cached by its inputs
/// (the cache file name).
pub fn preview_wav(mus: &MusicSettings, parts: &[Part], transition: &str, out_dir: &Path) -> Result<Option<PathBuf>> {
    if !(mus.enabled && !mus.path.is_empty() && Path::new(&mus.path).exists() && !parts.is_empty()) {
        return Ok(None);
    }
    let el = EditList::new(parts, transition);
    if el.placed.is_empty() {
        return Ok(None);
    }
    let mp = Path::new(&mus.path);
    let size = std::fs::metadata(mp)?.len();
    let plist: Vec<String> = el.parts.iter().map(|p| format!("[{}, {}]", json_f(p.src_in), json_f(p.src_out))).collect();
    let key = format!("[{}, {}, {}, [{}], {}]", music_json(mus), json_f(mtime(mp)?), size, plist.join(", "), json_str(transition));
    let out = out_dir.join(format!("music_{}.wav", &sha1_hex(&key)[..16]));
    if out.exists() {
        return Ok(Some(out));
    }
    std::fs::create_dir_all(out_dir)?;
    if let Ok(rd) = std::fs::read_dir(out_dir) {
        // one preview at a time per project
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("music_") && n.ends_with(".wav") {
                let _ = std::fs::remove_file(e.path()); // still open in a player: removed next time
            }
        }
    }
    let n = el.placed.len();
    // asetpts: a late music start (long adelay) left timestamps that made the split/concat below fail
    // ("Invalid data found when processing input"); renumbering from the sample count is exact
    let mut g =
        vec![format!("[0:a]aresample=48000,aformat=channel_layouts=stereo,{},asetpts=N/SR/TB[m]", shape_filter(mus, el.duration, true))];
    if n == 1 {
        g.push("[m]anull[out]".into());
    } else {
        g.push(format!("[m]asplit={n}") + &(0..n).map(|i| format!("[s{i}]")).collect::<String>());
        for (i, pl) in el.placed.iter().enumerate() {
            g.push(format!("[s{i}]atrim={}:{},asetpts=PTS-STARTPTS[p{i}]", f(pl.out_start, 4), f(pl.out_start + pl.dur(), 4)));
        }
        g.push((0..n).map(|i| format!("[p{i}]")).collect::<String>() + &format!("concat=n={n}:v=0:a=1[out]"));
    }
    let tmp = out.with_extension("part.wav");
    let ffm = ff::ffmpeg_exe()?;
    ff::run(&[
        ffm.as_str(),
        "-y",
        "-v",
        "error",
        "-stream_loop",
        "-1",
        "-i",
        &mus.path,
        "-filter_complex",
        &g.join(";"),
        "-map",
        "[out]",
        "-c:a",
        "pcm_s16le",
        "-f",
        "wav",
        &tmp.to_string_lossy(),
    ])?;
    std::fs::rename(&tmp, &out)?;
    Ok(Some(out))
}

// ---------------------------------------------------------------- analysis
/// The resolved absolute path as a string.
fn resolved(p: &Path) -> String {
    match p.canonicalize() {
        Ok(c) => paths::clean(c).to_string_lossy().into_owned(),
        Err(_) => std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned(),
    }
}

pub fn music_cache_dir() -> PathBuf {
    paths::data_dir().join("cache").join("music")
}

/// Cache file of a song's analysis inside `dir` (keyed by path, size and modification time).
pub fn cache_path_in(path: &str, dir: &Path) -> Result<PathBuf> {
    let p = Path::new(path);
    let size = std::fs::metadata(p)?.len();
    let key = format!("{}|{}|{}|{}", resolved(p), repr(mtime(p)?), size, ANALYSIS_VERSION);
    Ok(dir.join(format!("{}.json", &sha1_hex(&key)[..20])))
}

pub fn cache_path(path: &str) -> Result<PathBuf> {
    cache_path_in(path, &music_cache_dir())
}

fn cached_in(path: &str, dir: &Path) -> Option<SongInfo> {
    let p = cache_path_in(path, dir).ok()?;
    let s = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn cached_analysis(path: &str) -> Option<SongInfo> {
    cached_in(path, &music_cache_dir())
}

fn decode(path: &str, limit: f64) -> Result<Vec<f32>> {
    let ffm = ff::ffmpeg_exe()?;
    let out = ff::run(&[
        ffm.as_str(),
        "-v",
        "error",
        "-i",
        path,
        "-t",
        &repr(limit),
        "-vn",
        "-ac",
        "1",
        "-ar",
        &SR.to_string(),
        "-f",
        "f32le",
        "-",
    ])?;
    Ok(out.stdout.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// Hann-windowed frames (centred, zero padded).
fn frames(x: &[f32]) -> (usize, Vec<f32>) {
    let half = N_FFT / 2;
    let padded_len = x.len() + N_FFT;
    let n = 1 + (padded_len - N_FFT) / HOP;
    let win: Vec<f32> =
        (0..N_FFT).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (N_FFT - 1) as f64).cos()) as f32).collect();
    let mut out = vec![0f32; n * N_FFT];
    for fi in 0..n {
        let base = fi * HOP;
        for j in 0..N_FFT {
            let pi = base + j;
            let v = if pi >= half && pi - half < x.len() { x[pi - half] } else { 0.0 };
            out[fi * N_FFT + j] = v * win[j];
        }
    }
    (n, out)
}

/// numpy `convolve(a, ones(k)/k, mode="same")`.
fn box_same(a: &[f64], k: usize) -> Vec<f64> {
    let n = a.len();
    if n == 0 {
        return vec![];
    }
    let (long, short) = if n >= k { (n, k) } else { (k, n) };
    let off = (short - 1) / 2;
    // full convolution index i + off; kernel is a constant so only the window matters
    let mut out = vec![0.0; long];
    let kv = 1.0 / k as f64;
    for (i, o) in out.iter_mut().enumerate() {
        let c = i + off; // index into the full convolution
        let lo = c.saturating_sub(k - 1);
        let hi = c.min(n - 1);
        let mut s = 0.0;
        if lo <= hi {
            for v in &a[lo..=hi] {
                s += v * kv;
            }
        }
        *o = s;
    }
    if n >= k {
        out
    } else {
        out[..k].to_vec()
    }
}

/// Beat period in onset frames: autocorrelation weighted towards ~120 BPM.
fn tempo(onset: &[f64]) -> f64 {
    let n = onset.len();
    let mean = onset.iter().sum::<f64>() / n.max(1) as f64;
    let o: Vec<f64> = onset.iter().map(|v| v - mean).collect();
    let maxlag = n.min(128);
    let ac: Vec<f64> = (0..maxlag).map(|lag| (0..n - lag).map(|i| o[i] * o[i + lag]).sum()).collect();
    let at = |lag: f64| -> f64 {
        let i = lag as usize;
        if i + 1 < n && i + 1 < ac.len() {
            ac[i] + (ac[i + 1] - ac[i]) * (lag - i as f64)
        } else {
            -1e9
        }
    };
    let (mut best, mut best_lag) = (-1e9, FPS * 0.5);
    for s in 0..560 {
        let bpm = 60.0 + 0.25 * s as f64;
        let lag = 60.0 * FPS / bpm;
        let w = (-0.5 * ((bpm / 120.0).log2() / 0.9).powi(2)).exp();
        if at(lag) * w > best {
            best = at(lag) * w;
            best_lag = lag;
        }
    }
    // octave check: off-beat hats make every-other-beat look strongest; a slow tempo whose half lag is also
    // strongly periodic is really the double tempo
    if 60.0 * FPS / best_lag < 95.0 && at(best_lag / 2.0) > 0.3 * at(best_lag) {
        best_lag /= 2.0;
    }
    best_lag
}

/// Dynamic-programming beat tracker (Ellis 2007): beats on strong onsets, spaced close to `period`.
fn track_beats(onset: &[f64], period: f64, tightness: f64) -> Vec<usize> {
    let n = onset.len();
    if n == 0 {
        return vec![];
    }
    let mut score = onset.to_vec();
    let mut back = vec![-1i64; n];
    let lo_off = round_i(period / 2.0).max(0) as usize;
    let hi_off = round_i(2.0 * period).max(0) as usize;
    let offs: Vec<usize> = (lo_off..=hi_off).collect();
    let cost: Vec<f64> = offs.iter().map(|&o| -tightness * (o as f64 / period).ln().powi(2)).collect();
    for t in lo_off..n {
        let mut best: Option<(f64, usize)> = None;
        for (k, &o) in offs.iter().enumerate() {
            if o > t {
                continue;
            }
            let prev = t - o;
            let c = score[prev] + cost[k];
            if best.is_none_or(|(b, _)| c > b) {
                best = Some((c, prev));
            }
        }
        if let Some((c, prev)) = best {
            if c > 0.0 {
                score[t] += c;
                back[t] = prev as i64;
            }
        }
    }
    let argmax = |s: &[f64]| s.iter().enumerate().fold((0usize, f64::NEG_INFINITY), |m, (i, &v)| if v > m.1 { (i, v) } else { m }).0;
    let tail = round_i(period).max(0) as usize;
    let mut t: i64 = if n > tail && tail > 0 { (n - tail + argmax(&score[n - tail..])) as i64 } else { argmax(&score) as i64 };
    let mut beats = vec![];
    while t >= 0 {
        beats.push(t as usize);
        t = back[t as usize];
    }
    beats.reverse();
    beats
}

/// Beats, bars, a loudness curve and a waveform overview for a song. Cached in data/cache/music.
pub fn analyze(path: &str) -> Result<SongInfo> {
    analyze_cached_in(path, &music_cache_dir())
}

/// `analyze` with an explicit cache folder.
pub fn analyze_cached_in(path: &str, cache_dir: &Path) -> Result<SongInfo> {
    if let Some(c) = cached_in(path, cache_dir) {
        return Ok(c);
    }
    let x = decode(path, 900.0).with_context(|| format!("could not decode {path}"))?;
    let res = analyze_samples(&x)?;
    if let Ok(p) = cache_path_in(path, cache_dir) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, res.to_cache_json());
    }
    log::info!(
        "music {}: {:.1}s, {:.1} bpm, {} beats",
        Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        res.duration,
        res.bpm,
        res.beats.len()
    );
    Ok(res)
}

/// The analysis of mono samples at `SR`.
pub fn analyze_samples(x: &[f32]) -> Result<SongInfo> {
    let dur = x.len() as f64 / SR as f64;
    if dur < 1.0 {
        bail!("The music file has no usable audio");
    }
    let (n, fr) = frames(x);
    let nb = N_FFT / 2 + 1;
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let mut lm = vec![0f32; n * nb];
    let mut buf = vec![Complex::new(0f32, 0f32); N_FFT];
    let mut scratch = vec![Complex::new(0f32, 0f32); fft.get_inplace_scratch_len()];
    for fi in 0..n {
        for j in 0..N_FFT {
            buf[j] = Complex::new(fr[fi * N_FFT + j], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for j in 0..nb {
            lm[fi * nb + j] = (10.0 * buf[j].norm()).ln_1p();
        }
    }
    let mut onset = vec![0f64; n];
    let mut bass = vec![0f64; n];
    for fi in 1..n {
        let (mut s, mut b) = (0f64, 0f64);
        for j in 0..nb {
            let d = (lm[fi * nb + j] - lm[(fi - 1) * nb + j]).max(0.0) as f64;
            s += d;
            if (1..19).contains(&j) {
                b += d; // ~10-200 Hz: kicks, for the bar phase
            }
        }
        onset[fi] = s as f32 as f64;
        bass[fi] = b as f32 as f64;
    }
    let k = 1.max((FPS * 0.4) as usize);
    let local = box_same(&onset, k);
    let mut onset: Vec<f64> = onset.iter().zip(&local).map(|(o, l)| (o - l).max(0.0)).collect();
    let mean = onset.iter().sum::<f64>() / n as f64;
    let std = (onset.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
    for v in onset.iter_mut() {
        *v /= std + 1e-9;
    }
    let period = tempo(&onset);
    let bidx = track_beats(&onset, period, 100.0);
    let beats: Vec<f64> = bidx.iter().map(|&b| b as f64 / FPS).collect();
    // bars: the beat phase (of 4) with the strongest kicks
    let mut phase = 0;
    let mut best = f64::NEG_INFINITY;
    for p in 0..4 {
        let s: f64 = bidx.iter().skip(p).step_by(4).map(|&i| bass[i]).sum();
        if s > best {
            best = s;
            phase = p;
        }
    }
    let bars: Vec<f64> = beats.iter().skip(phase).step_by(4).copied().collect();
    // loudness at 10 Hz, smoothed over 1 s, mapped p5..p95 -> 0..1
    let db: Vec<f64> = (0..n)
        .map(|fi| {
            let row = &fr[fi * N_FFT..(fi + 1) * N_FFT];
            let ms = row.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / N_FFT as f64;
            let rms = ((ms as f32) + 1e-12f32).sqrt();
            (20.0 * rms.log10()) as f64
        })
        .collect();
    let step = FPS / 10.0;
    let m = (n as f64 / step) as usize;
    let mut db10: Vec<f64> = (0..m)
        .map(|i| {
            let (a, b) = ((i as f64 * step) as usize, (((i + 1) as f64 * step) as usize).min(n));
            let s = &db[a..b];
            (s.iter().sum::<f64>() / s.len().max(1) as f64) as f32 as f64
        })
        .collect();
    if db10.len() >= 10 {
        db10 = box_same(&db10, 10);
    }
    let (lo, hi) = (percentile(&db10, 5.0), percentile(&db10, 95.0));
    let energy: Vec<f64> = db10.iter().map(|v| ((v - lo) / (1e-6f64).max(hi - lo)).clamp(0.0, 1.0)).collect();
    // waveform overview: peak per 1/20 s
    let w = SR / 20;
    let pk: Vec<f32> = x[..x.len() / w * w].chunks_exact(w).map(|c| c.iter().fold(0f32, |m, v| m.max(v.abs()))).collect();
    let pmax = pk.iter().fold(0f32, |m, v| m.max(*v));
    let wave: Vec<f64> = pk.iter().map(|v| (v / (pmax + 1e-9)).sqrt() as f64).collect();
    let r3 = |v: &[f64]| v.iter().map(|x| round_n(*x, 3)).collect::<Vec<_>>();
    Ok(SongInfo {
        version: ANALYSIS_VERSION,
        duration: round_n(dur, 3),
        bpm: round_n(60.0 * FPS / period, 1),
        beats: r3(&beats),
        bars: r3(&bars),
        energy: r3(&energy),
        wave: r3(&wave),
    })
}

// ---------------------------------------------------------------- best part + beat sync
fn mean_range(e: &[f64], a: f64, b: f64) -> f64 {
    let i = 0i64.max((a * 10.0) as i64);
    let j = (e.len() as i64).min((b * 10.0) as i64);
    if j > i {
        let s = &e[i as usize..j as usize];
        s.iter().sum::<f64>() / s.len() as f64
    } else {
        0.0
    }
}

/// Where the song should start (song time) so its biggest moment (a loud bar right after a quieter one: the
/// drop) lands on the first kill, and the rest of the edit stays on the strong part of the song.
pub fn best_part(info: &SongInfo, length: f64, first_kill: f64) -> f64 {
    let e: Vec<f64> = info.energy.iter().map(|v| *v as f32 as f64).collect();
    let dur = info.duration;
    if dur <= length + 0.5 || info.bars.is_empty() {
        return 0.0;
    }
    let (mut best, mut best_in) = (-1e9, 0.0);
    for &s in &info.bars {
        let song_in = s - first_kill;
        if song_in < 0.0 || song_in + length > dur {
            continue;
        }
        let a = ((song_in * 10.0) as i64).max(0) as usize;
        let b = (((song_in + length) * 10.0) as i64).max(0) as usize;
        let win: &[f64] = if a < b.min(e.len()) { &e[a..b.min(e.len())] } else { &[] };
        let after: &[f64] = if win.len() as f64 > first_kill * 10.0 + 5.0 {
            let k = ((first_kill * 10.0) as i64).max(0) as usize;
            &win[k.min(win.len())..]
        } else {
            win
        };
        if after.is_empty() {
            continue; // numpy: mean of nothing is nan, never the best
        }
        let rise = mean_range(&e, s, s + 3.0) - mean_range(&e, s - 3.0, s);
        let am = after.iter().sum::<f64>() / after.len() as f64;
        let score = 0.45 * am + 0.25 * percentile(after, 20.0) + 0.6 * rise.max(0.0);
        if score > best {
            best = score;
            best_in = song_in;
        }
    }
    best_in
}

/// Song beats in edit time, where the music plays (`bounded=false`: also past the end, for moving it).
pub fn edit_beats(info: &SongInfo, mus: &MusicSettings, total: f64, bounded: bool) -> Vec<f64> {
    let (a, b) = span(mus, total);
    info.beats.iter().map(|t| t - mus.song_in + a).filter(|&t| a <= t && (!bounded || t <= b)).collect()
}

/// Move each cut (and the end) onto the nearest beat by lengthening or shortening the part before it,
/// never by more than one beat and never cutting into a kill. Returns (parts, cuts moved, cuts total).
pub fn sync_cuts(parts: &[Part], transition: &str, beats: &[f64], kill_times: &[f64], src_duration: f64) -> (Vec<Part>, usize, usize) {
    let mut parts = parts.to_vec();
    parts.sort_by(|a, b| a.src_in.total_cmp(&b.src_in));
    if beats.is_empty() || parts.is_empty() {
        return (parts, 0, 0);
    }
    let period = if beats.len() > 1 { numfmt::median(&beats.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>()) } else { 0.5 };
    let mut moved = 0;
    for i in 0..parts.len() {
        let el = EditList::new(&parts, transition);
        if i >= el.placed.len() {
            break;
        }
        let last = i == parts.len() - 1;
        let cut = if last { el.duration } else { el.placed[i + 1].out_start + el.td / 2.0 };
        let p = parts[i];
        let kills: Vec<f64> = kill_times.iter().copied().filter(|&k| p.src_in <= k && k <= p.src_out).collect();
        let kmax = if kills.is_empty() { -1e9 } else { kills.iter().fold(f64::NEG_INFINITY, |m, v| m.max(*v)) + 0.4 };
        let lo = (p.src_in + 1.0f64.max(2.5 * el.td)).max(kmax) - p.src_out;
        let mut hi = (if last { src_duration } else { parts[i + 1].src_in }) - p.src_out;
        hi = hi.min(src_duration - p.src_out);
        let cand: Vec<f64> =
            beats.iter().filter(|&&b| lo <= b - cut && b - cut <= hi && (b - cut).abs() <= period * 1.01).map(|&b| b - cut).collect();
        if cand.is_empty() {
            continue;
        }
        let d = cand.iter().copied().fold(None, |m: Option<f64>, v| match m {
            Some(x) if x.abs() <= v.abs() => Some(x),
            _ => Some(v),
        });
        let d = d.unwrap();
        if d.abs() > 1e-3 {
            parts[i].src_out += d;
        }
        moved += 1;
    }
    let total = parts.len();
    (parts, moved, total)
}

/// A synthetic 128 BPM test song with a drop (quiet intro, louder from bar 9): used by the unit tests and
/// the Trim UI check, so neither needs a real music file.
pub mod test_song {
    pub const BEAT: f64 = 60.0 / 128.0;
    pub const OFFSET: f64 = 0.37;
    pub fn drop_t() -> f64 {
        OFFSET + BEAT * 4.0 * 9.0
    }

    /// 60 s at 22.05 kHz, mono, already through 16-bit PCM like a wav file.
    pub fn song() -> Vec<f32> {
        let sr = 22050usize;
        let dur = 60.0;
        let n = (sr as f64 * dur) as usize;
        let drop = drop_t();
        let mut x: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                0.08 * (2.0 * std::f64::consts::PI * 220.0 * t).sin() * if t >= drop { 2.5 } else { 1.0 }
            })
            .collect();
        let mut seed: u64 = 1;
        let mut rnd = || {
            // Box-Muller over a small LCG
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u1 = ((seed >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u2 = (seed >> 11) as f64 / (1u64 << 53) as f64;
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        };
        for i in 0..(dur / BEAT) as usize {
            let b = OFFSET + i as f64 * BEAT;
            let s = (b * sr as f64) as usize;
            let amp = if b >= drop { 1.0 } else { 0.35 };
            let m = ((sr as f64 * 0.18) as usize).min(n.saturating_sub(s));
            for j in 0..m {
                let tt = j as f64 / sr as f64;
                x[s + j] += (2.0 * std::f64::consts::PI * (55.0 + 90.0 * (-tt * 30.0).exp()) * tt).sin() * (-tt * 14.0).exp() * amp;
            }
            if i % 2 == 1 {
                let m = ((sr as f64 * 0.06) as usize).min(n.saturating_sub(s));
                for j in 0..m {
                    x[s + j] += rnd() * 0.15 * amp * (-(j as f64) / sr as f64 * 60.0).exp();
                }
            }
        }
        let mx = x.iter().fold(0f64, |m, v| m.max(v.abs()));
        // through 16-bit PCM like the wav file
        x.iter().map(|v| ((v / mx * 0.9 * 32767.0) as i16) as f32 / 32768.0).collect()
    }

    /// Write the song as a 16-bit mono wav (for the UI checks).
    pub fn write_wav(path: &std::path::Path) -> std::io::Result<()> {
        let x = song();
        let sr: u32 = 22050;
        let mut b: Vec<u8> = Vec::with_capacity(44 + x.len() * 2);
        let data_len = (x.len() * 2) as u32;
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&sr.to_le_bytes());
        b.extend_from_slice(&(sr * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for v in x {
            b.extend_from_slice(&((v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::test_song::{drop_t, song, BEAT, OFFSET};

    fn info() -> &'static SongInfo {
        static I: once_cell::sync::OnceCell<SongInfo> = once_cell::sync::OnceCell::new();
        I.get_or_init(|| analyze_samples(&song()).unwrap())
    }

    #[test]
    fn tempo_and_beats() {
        let info = info();
        assert!((info.bpm - 128.0).abs() < 2.5, "bpm {}", info.bpm);
        let mut err: Vec<f64> =
            info.beats.iter().map(|b| (0..200).map(|i| (OFFSET + i as f64 * BEAT - b).abs()).fold(f64::INFINITY, f64::min)).collect();
        err.sort_by(|a, b| a.total_cmp(b));
        assert!(numfmt::median(&err) < 0.03);
    }

    #[test]
    fn best_part_puts_the_drop_on_the_first_kill() {
        let song_in = best_part(info(), 20.0, 3.5);
        assert!((song_in + 3.5 - drop_t()).abs() < 0.06, "song_in {song_in}");
    }

    #[test]
    fn cuts_land_on_beats_and_keep_kills() {
        let info = info();
        let parts = [Part::new(10.0, 16.3), Part::new(30.0, 34.1), Part::new(50.0, 57.77)];
        let kills = [13.5, 32.0, 52.0, 55.0];
        for tr in ["none", "fade"] {
            let el = EditList::new(&parts, tr);
            let mus = MusicSettings { path: "x".into(), enabled: true, song_in: best_part(info, el.duration, 3.5), ..Default::default() };
            let (new, moved, total) = sync_cuts(&parts, tr, &edit_beats(info, &mus, el.duration, false), &kills, 120.0);
            assert!(moved == 3 && total == 3);
            let el2 = EditList::new(&new, tr);
            let beats: Vec<f64> = info.beats.iter().map(|b| b - mus.song_in).collect();
            let mut cuts: Vec<f64> = el2.placed[1..].iter().map(|pl| pl.out_start + el2.td / 2.0).collect();
            cuts.push(el2.duration);
            for c in cuts {
                assert!(beats.iter().map(|b| (b - c).abs()).fold(f64::INFINITY, f64::min) < 1e-6);
            }
            assert!(kills.iter().all(|k| new.iter().any(|p| p.src_in <= *k && *k <= p.src_out)));
        }
    }

    #[test]
    fn shape_filter_places_the_part() {
        let mus = MusicSettings { song_in: 12.0, start: 2.0, end: Some(10.0), volume: 0.5, ..Default::default() };
        let f = shape_filter(&mus, 15.0, true);
        assert!(f.contains("atrim=start=12.000:duration=8.000") && f.contains("adelay=2000") && f.ends_with("atrim=0:15.000"));
        assert_eq!(span(&MusicSettings::default(), 15.0), (0.0, 15.0)); // default: background under the whole video
    }

    #[test]
    fn cache_json_format() {
        let i =
            SongInfo { version: 1, duration: 60.0, bpm: 128.0, beats: vec![0.371, 1.0], bars: vec![], energy: vec![0.5], wave: vec![1.0] };
        let s = i.to_cache_json();
        assert_eq!(s, "{\"version\": 1, \"duration\": 60.0, \"bpm\": 128.0, \"beats\": [0.371, 1.0], \"bars\": [], \"energy\": [0.5], \"wave\": [1.0]}");
        assert_eq!(serde_json::from_str::<SongInfo>(&s).unwrap(), i);
    }
}
