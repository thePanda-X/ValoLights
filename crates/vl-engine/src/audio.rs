//! Sound events for Smart cut: laughter, shouting, cheering and loudness along the clip, with Google's YAMNet
//! (AudioSet, 521 classes; ONNX export with the log-mel frontend inside the graph, assets/audio/yamnet.onnx).
//!
//! The audio is decoded by ffmpeg to 16 kHz mono floats and streamed through the model in chunks of
//! [`CHUNK_FRAMES`] frames. YAMNet frames are 0.96 s long and start every 0.48 s; chunks start on a frame
//! boundary and overlap by one frame length, so the result is the same as one pass over the whole file.
use crate::ocr;
use anyhow::{anyhow, bail, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use vl_core::ffmpeg as ff;
use vl_core::jobs::{Cancel, Cancelled};
use vl_core::models::AudioSignals;

pub const SR: usize = 16_000;
/// samples per frame (0.96 s) and between frame starts (0.48 s)
pub const WIN: usize = 15_360;
pub const HOP: usize = 7_680;
pub const STEP: f64 = HOP as f64 / SR as f64;
/// frames per model call (about a minute of audio)
pub const CHUNK_FRAMES: usize = 128;
/// bump when the signals change meaning (older projects then run the pass again)
pub const VERSION: u32 = 1;

/// AudioSet class indices (yamnet_class_map.csv)
const LAUGH: &[usize] = &[13, 14, 15, 16, 17, 18]; // Laughter, Baby laughter, Giggle, Snicker, Belly laugh, Chuckle
const SHOUT: &[usize] = &[6, 8, 9, 10, 11]; // Shout, Whoop, Yell, Children shouting, Screaming
const CHEER: &[usize] = &[61, 62]; // Cheering, Applause
const SPEECH: &[usize] = &[0, 1, 2, 3]; // Speech, Child speech, Conversation, Narration
const N_CLASSES: usize = 521;

pub fn model_path() -> PathBuf {
    vl_core::paths::assets_dir().join("audio").join("yamnet.onnx")
}

fn session() -> Result<Session> {
    let path = model_path();
    if !path.exists() {
        bail!("the sound model is missing ({})", path.display());
    }
    Session::builder(ocr::environment()?)
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(&path)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading {}", path.display()))
}

/// 0..1 -> 0..100
fn pct(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Loudness of a frame window, -60..0 dBFS -> 0..100.
fn loudness(samples: &[f32]) -> u8 {
    if samples.is_empty() {
        return 0;
    }
    let ms = samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32;
    let db = 10.0 * ms.max(1e-12).log10();
    (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Run the model on `wave` (a whole number of frames, frame 0 starting at sample 0) and append the frames.
fn run_chunk(sess: &mut Session, wave: &[f32], n_frames: usize, out: &mut AudioSignals) -> Result<()> {
    let tensor = Tensor::from_array((vec![wave.len() as i64], wave.to_vec())).map_err(|e| anyhow!("{e}"))?;
    let outputs = sess.run(ort::inputs![tensor]).map_err(|e| anyhow!("{e}"))?;
    let (shape, scores) = outputs[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
    let frames = (shape[0] as usize).min(n_frames);
    if shape.len() != 2 || shape[1] as usize != N_CLASSES {
        bail!("unexpected sound model output {shape:?}");
    }
    let max_of = |row: &[f32], ids: &[usize]| ids.iter().map(|&i| row[i]).fold(0.0f32, f32::max);
    for f in 0..frames {
        let row = &scores[f * N_CLASSES..(f + 1) * N_CLASSES];
        out.laugh.push(pct(max_of(row, LAUGH)));
        out.shout.push(pct(max_of(row, SHOUT)));
        out.cheer.push(pct(max_of(row, CHEER)));
        out.speech.push(pct(max_of(row, SPEECH)));
        let a = f * HOP;
        out.loud.push(loudness(&wave[a.min(wave.len())..(a + WIN).min(wave.len())]));
    }
    Ok(())
}

/// Sound events of the whole clip. `progress` gets 0..1.
pub fn analyze_audio(src: &Path, duration: f64, progress: &dyn Fn(f64), cancel: &Cancel) -> Result<AudioSignals> {
    let mut sess = session()?;
    let mut child = ff::command(&ff::ffmpeg_exe()?)
        .args(["-hide_banner", "-v", "error", "-nostdin", "-i"])
        .arg(src)
        .args(["-vn", "-sn", "-ac", "1", "-ar", &SR.to_string(), "-f", "f32le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start ffmpeg")?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_th = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut out = AudioSignals { version: VERSION, step: STEP, ..Default::default() };
    let chunk_len = WIN + HOP * (CHUNK_FRAMES - 1);
    let advance = HOP * CHUNK_FRAMES;
    let mut buf: Vec<f32> = Vec::with_capacity(chunk_len * 2);
    let mut bytes = vec![0u8; 1 << 16];
    let mut pending: Vec<u8> = vec![];
    let total = (duration * SR as f64).max(1.0);
    let mut done_samples = 0usize;
    let mut ended = false;
    while !ended {
        if cancel.is_set() {
            let _ = child.kill();
            return Err(Cancelled.into());
        }
        match stdout.read(&mut bytes) {
            Ok(0) | Err(_) => ended = true,
            Ok(n) => pending.extend_from_slice(&bytes[..n]),
        }
        let whole = pending.len() / 4 * 4;
        buf.extend(pending[..whole].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
        pending.drain(..whole);
        while buf.len() >= chunk_len {
            run_chunk(&mut sess, &buf[..chunk_len], CHUNK_FRAMES, &mut out)?;
            buf.drain(..advance);
            done_samples += advance;
            progress((done_samples as f64 / total).min(0.99));
        }
    }
    // the tail: frames that start before the end (the last one zero-padded, like YAMNet itself)
    if !buf.is_empty() {
        let n_frames = 1 + buf.len().saturating_sub(WIN).div_ceil(HOP);
        let need = WIN + HOP * (n_frames - 1);
        buf.resize(need, 0.0);
        run_chunk(&mut sess, &buf, n_frames, &mut out)?;
    }
    let status = child.wait()?;
    let err = err_th.join().unwrap_or_default();
    if !status.success() {
        bail!("could not read the clip's sound: {}", err.trim());
    }
    if out.loud.is_empty() {
        bail!("this clip has no sound");
    }
    progress(1.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_scale() {
        assert_eq!(loudness(&[0.0; 100]), 0);
        assert_eq!(loudness(&[1.0; 100]), 100);
        // a sine at half scale: -9 dBFS -> 85
        let s: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
        assert!((84..=86).contains(&loudness(&s)), "{}", loudness(&s));
    }

    #[test]
    fn chunks_line_up_with_frames() {
        // chunk starts are a whole number of hops apart, so frame k of chunk c is frame c*CHUNK_FRAMES + k
        assert_eq!((HOP * CHUNK_FRAMES) % HOP, 0);
        assert_eq!(WIN, 2 * HOP);
        assert!((STEP - 0.48).abs() < 1e-12);
    }
}
