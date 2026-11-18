//! Text recognition: PP-OCRv6 (small) on CPU ONNX Runtime, with RapidOCR's pre/post-processing.
//!
//! We localise text ourselves (kill feed parser, fixed HUD ROIs) and run only the recognition model for
//! `read_line`; `read_block` adds the DB text detector for larger regions (banners).
use crate::imgproc::{self, Interp};
use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::sync::{Mutex, OnceLock};
use vl_core::image::BgrImage;

const REC_H: usize = 48;
const REC_W: usize = 320;
const REC_BATCH: usize = 6;
const MIN_SIDE: usize = 30;
const MAX_SIDE: usize = 2000;
const TEXT_SCORE: f32 = 0.5;

/// Recognition sessions: a small pool of single-threaded sessions so kill feed / HUD workers can read text in
/// parallel (one multi-threaded session behind a mutex serialises every call).
struct Rec {
    pool: Vec<Mutex<Option<Session>>>,
    next: std::sync::atomic::AtomicUsize,
    chars: Vec<String>,
}

fn pool_size() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 6)
}

fn rec_session() -> Result<Session> {
    let path = vl_core::paths::ocr_dir().join("PP-OCRv6_rec_small.onnx");
    Session::builder(environment()?)
        .map_err(|e| anyhow!("{e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(&path)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading {}", path.display()))
}

impl Rec {
    /// Run `f` with a free session (created on first use).
    fn with_session<R>(&self, f: impl FnOnce(&mut Session) -> Result<R>) -> Result<R> {
        let n = self.pool.len();
        let start = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = None;
        for k in 0..n {
            if let Ok(g) = self.pool[(start + k) % n].try_lock() {
                guard = Some(g);
                break;
            }
        }
        let mut g = match guard {
            Some(g) => g,
            None => self.pool[start % n].lock().map_err(|_| anyhow!("OCR session poisoned"))?,
        };
        if g.is_none() {
            *g = Some(rec_session()?);
        }
        f(g.as_mut().unwrap())
    }
}

static ENV: OnceLock<std::result::Result<ort::environment::Environment, String>> = OnceLock::new();
static REC: OnceLock<std::result::Result<Rec, String>> = OnceLock::new();
static DET: OnceLock<std::result::Result<Mutex<Session>, String>> = OnceLock::new();

/// The process-wide ONNX Runtime environment (ort allows only one).
pub fn environment() -> Result<&'static ort::environment::Environment> {
    ENV.get_or_init(|| ort::init().with_name("valolights").build().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow!("onnxruntime init failed: {e}"))
}

fn session(file: &str) -> Result<Session> {
    let path = vl_core::paths::ocr_dir().join(file);
    let env = environment()?;
    let s = Session::builder(env)
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(&path)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading {}", path.display()))?;
    Ok(s)
}

/// Split lines at every Unicode line break (the dictionary is read that way; index alignment matters).
fn splitlines(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn rec() -> Result<&'static Rec> {
    REC.get_or_init(|| {
        (|| -> Result<Rec> {
            let s = rec_session()?;
            let dict = s
                .metadata()
                .map_err(|e| anyhow!("{e}"))?
                .custom("character")
                .ok_or_else(|| anyhow!("rec model has no character dictionary"))?;
            let mut chars = vec!["blank".to_string()];
            chars.extend(splitlines(&dict));
            chars.push(" ".to_string());
            let mut pool: Vec<Mutex<Option<Session>>> = (0..pool_size()).map(|_| Mutex::new(None)).collect();
            pool[0] = Mutex::new(Some(s));
            Ok(Rec { pool, next: std::sync::atomic::AtomicUsize::new(0), chars })
        })()
        .map_err(|e| format!("{e:#}"))
    })
    .as_ref()
    .map_err(|e| anyhow!("OCR model unavailable: {e}"))
}

fn det() -> Result<&'static Mutex<Session>> {
    DET.get_or_init(|| session("PP-OCRv6_det_small.onnx").map(Mutex::new).map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("OCR detector unavailable: {e}"))
}

/// Round to 5 decimals: correctly rounded from the exact binary value (Rust's `{:.5}` formatting is exact too).
fn round5(x: f64) -> f64 {
    format!("{x:.5}").parse().unwrap_or(x)
}

/// Round half to even for the /32 snapping.
fn round_even(v: f64) -> f64 {
    v.round_ties_even()
}

/// RapidOCR `resize_image_within_bounds` (min side 30, max side 2000).
fn within_bounds(img: &BgrImage) -> BgrImage {
    let mut img = img.clone();
    let snap = |img: &BgrImage, ratio: f64| -> BgrImage {
        let rh = (img.h as f64 * ratio) as i64;
        let rw = (img.w as f64 * ratio) as i64;
        let rh = (round_even(rh as f64 / 32.0) * 32.0) as i64;
        let rw = (round_even(rw as f64 / 32.0) * 32.0) as i64;
        if rh <= 0 || rw <= 0 {
            return img.clone();
        }
        imgproc::resize(img, rw as usize, rh as usize, Interp::Linear)
    };
    if img.w.max(img.h) > MAX_SIDE {
        let ratio = if img.h > img.w { MAX_SIDE as f64 / img.h as f64 } else { MAX_SIDE as f64 / img.w as f64 };
        img = snap(&img, ratio);
    }
    if img.w.min(img.h) < MIN_SIDE {
        let ratio = if img.h < img.w { MIN_SIDE as f64 / img.h as f64 } else { MIN_SIDE as f64 / img.w as f64 };
        img = snap(&img, ratio);
    }
    img
}

/// RapidOCR `TextRecognizer.__call__`: width-sorted batches of 6, each padded to its widest aspect ratio.
fn recognize(imgs: &[BgrImage]) -> Result<Vec<(String, f32)>> {
    let r = rec()?;
    let mut res = vec![(String::new(), 0f32); imgs.len()];
    let ratios: Vec<f64> = imgs.iter().map(|i| i.w as f64 / i.h as f64).collect();
    let mut order: Vec<usize> = (0..imgs.len()).collect();
    order.sort_by(|&a, &b| ratios[a].partial_cmp(&ratios[b]).unwrap_or(std::cmp::Ordering::Equal));
    for chunk in order.chunks(REC_BATCH) {
        let mut max_wh = REC_W as f64 / REC_H as f64;
        for &i in chunk {
            max_wh = max_wh.max(imgs[i].w as f64 / imgs[i].h as f64);
        }
        let width = (REC_H as f64 * max_wh) as usize;
        let n = chunk.len();
        let plane = REC_H * width;
        let mut input = vec![0f32; n * 3 * plane];
        for (b, &i) in chunk.iter().enumerate() {
            let img = &imgs[i];
            let ratio = img.w as f64 / img.h as f64;
            let c = (REC_H as f64 * ratio).ceil();
            let rw = if c > width as f64 { width } else { c as usize }.max(1);
            let rs = imgproc::resize(img, rw, REC_H, Interp::Linear);
            let base = b * 3 * plane;
            for y in 0..REC_H {
                for x in 0..rw {
                    let p = (y * rw + x) * 3;
                    for ch in 0..3 {
                        let v = rs.data[p + ch] as f32 / 255.0;
                        input[base + ch * plane + y * width + x] = (v - 0.5) / 0.5;
                    }
                }
            }
        }
        let tensor = Tensor::from_array((vec![n as i64, 3, REC_H as i64, width as i64], input)).map_err(|e| anyhow!("{e}"))?;
        let (t, c, data) = r.with_session(|sess| {
            let outputs = sess.run(ort::inputs![tensor]).map_err(|e| anyhow!("{e}"))?;
            let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
            Ok((shape[1] as usize, shape[2] as usize, data.to_vec()))
        })?;
        for (b, &i) in chunk.iter().enumerate() {
            let mut text = String::new();
            let mut confs: Vec<f64> = vec![];
            let mut prev = usize::MAX;
            for step in 0..t {
                let row = &data[(b * t + step) * c..(b * t + step + 1) * c];
                let (mut bi, mut bv) = (0usize, f32::NEG_INFINITY);
                for (k, &v) in row.iter().enumerate() {
                    if v > bv {
                        bv = v;
                        bi = k;
                    }
                }
                if bi != 0 && bi != prev {
                    if let Some(ch) = r.chars.get(bi) {
                        text.push_str(ch);
                    }
                    confs.push(round5(bv as f64));
                }
                prev = bi;
            }
            if confs.is_empty() {
                confs.push(0.0);
            }
            let mean = confs.iter().sum::<f64>() / confs.len() as f64;
            res[i] = (text, ((mean * 1e5).round_ties_even() / 1e5) as f32);
        }
    }
    Ok(res)
}

/// Recognise a single text line already cropped tightly.
pub fn read_line(img: &BgrImage) -> (String, f32) {
    if img.is_empty() || img.h < 4 || img.w < 4 {
        return (String::new(), 0.0);
    }
    let img = if img.h != REC_H {
        let sc = REC_H as f64 / img.h as f64;
        imgproc::resize_by(img, sc, sc, Interp::Cubic)
    } else {
        img.clone()
    };
    let img = within_bounds(&img);
    match recognize(std::slice::from_ref(&img)) {
        Ok(mut v) => {
            let (t, s) = v.remove(0);
            (t.trim().to_string(), s)
        }
        Err(e) => {
            log::warn!("OCR failed: {e:#}");
            (String::new(), 0.0)
        }
    }
}

/// Recognise many tight line crops (same results as `read_line` on each).
pub fn read_lines(imgs: &[&BgrImage]) -> Vec<(String, f32)> {
    imgs.iter().map(|i| read_line(i)).collect()
}

// ---------------------------------------------------------------- detection (banners)
/// Connected text regions of the DB probability map -> axis-aligned boxes in source pixels + score.
fn db_boxes(pred: &[f32], w: usize, h: usize, dest_w: usize, dest_h: usize) -> Vec<([f64; 4], f32)> {
    // binarise (> 0.3) and dilate with a 2x2 kernel (anchor at its centre = covers x-1..x, y-1..y)
    let bin: Vec<u8> = pred.iter().map(|&v| (v > 0.3) as u8).collect();
    let mask = imgproc::dilate(&imgproc::Plane::from_raw(w, h, bin), 2, 2);
    let mut label = vec![0u32; w * h];
    let mut out = vec![];
    let mut next = 0u32;
    for start in 0..w * h {
        if mask.data[start] == 0 || label[start] != 0 {
            continue;
        }
        next += 1;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let mut stack = vec![start];
        label[start] = next;
        while let Some(p) = stack.pop() {
            let (px, py) = (p % w, p / w);
            x0 = x0.min(px);
            x1 = x1.max(px);
            y0 = y0.min(py);
            y1 = y1.max(py);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (px as i64 + dx, py as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let q = ny as usize * w + nx as usize;
                    if mask.data[q] != 0 && label[q] == 0 {
                        label[q] = next;
                        stack.push(q);
                    }
                }
            }
        }
        if out.len() >= 1000 {
            break;
        }
        // minAreaRect of the contour points (pixel centres) of an axis-aligned blob
        let (bw, bh) = ((x1 - x0) as f64, (y1 - y0) as f64);
        if bw.min(bh) < 3.0 {
            continue;
        }
        let mut s = 0f64;
        let mut n = 0usize;
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                s += pred[yy * w + xx] as f64;
                n += 1;
            }
        }
        let score = (s / n.max(1) as f64) as f32;
        if score < 0.5 {
            continue;
        }
        // unclip: offset the rectangle by area * 1.6 / perimeter
        let d = bw * bh * 1.6 / (2.0 * (bw + bh));
        let (ux0, uy0, ux1, uy1) = (x0 as f64 - d, y0 as f64 - d, x1 as f64 + d, y1 as f64 + d);
        if (ux1 - ux0).min(uy1 - uy0) < 5.0 {
            continue;
        }
        let sx = |v: f64| (v / w as f64 * dest_w as f64).round_ties_even().clamp(0.0, dest_w as f64);
        let sy = |v: f64| (v / h as f64 * dest_h as f64).round_ties_even().clamp(0.0, dest_h as f64);
        let b = [
            sx(ux0).min(dest_w as f64 - 1.0).trunc(),
            sy(uy0).min(dest_h as f64 - 1.0).trunc(),
            sx(ux1).min(dest_w as f64 - 1.0).trunc(),
            sy(uy1).min(dest_h as f64 - 1.0).trunc(),
        ];
        if (b[2] - b[0]) as i64 <= 3 || (b[3] - b[1]) as i64 <= 3 {
            continue;
        }
        out.push((b, score));
    }
    // RapidOCR sorted_boxes: by top, then lines (y gaps >= 10) left to right
    out.sort_by(|a, b| a.0[1].partial_cmp(&b.0[1]).unwrap_or(std::cmp::Ordering::Equal));
    let mut line = 0;
    let mut keyed: Vec<(i32, f64, ([f64; 4], f32))> = vec![];
    for (i, b) in out.iter().enumerate() {
        if i > 0 && b.0[1] - out[i - 1].0[1] >= 10.0 {
            line += 1;
        }
        keyed.push((line, b.0[0], *b));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
    keyed.into_iter().map(|k| k.2).collect()
}

fn detect_and_read(img: &BgrImage) -> Result<Vec<(String, f32)>> {
    let img = within_bounds(img);
    // vertical padding for very flat / short images
    let img = if img.h <= MIN_SIDE || img.w as f64 / img.h as f64 > 8.0 {
        let new_h = ((img.w as f64 / 8.0) as usize).max(MIN_SIDE) * 2;
        let pad = (new_h as i64 - img.h as i64).unsigned_abs() as usize / 2;
        let blank = BgrImage::new(img.w, pad);
        imgproc::vstack(&[&blank, &img, &blank])
    } else {
        img
    };
    // DetPreProcess: limit_type "min", 736, snapped to multiples of 32
    let (h, w) = (img.h, img.w);
    let ratio = if h.min(w) < 736 {
        if h < w {
            736.0 / h as f64
        } else {
            736.0 / w as f64
        }
    } else {
        1.0
    };
    let rh = (round_even((h as f64 * ratio) as i64 as f64 / 32.0) * 32.0) as usize;
    let rw = (round_even((w as f64 * ratio) as i64 as f64 / 32.0) * 32.0) as usize;
    if rh == 0 || rw == 0 {
        return Ok(vec![]);
    }
    let rs = imgproc::resize(&img, rw, rh, Interp::Linear);
    let plane = rw * rh;
    let mut input = vec![0f32; 3 * plane];
    for (i, p) in rs.data.as_chunks::<3>().0.iter().enumerate() {
        for ch in 0..3 {
            input[ch * plane + i] = (p[ch] as f32 / 255.0 - 0.5) / 0.5;
        }
    }
    let boxes = {
        let tensor = Tensor::from_array((vec![1i64, 3, rh as i64, rw as i64], input)).map_err(|e| anyhow!("{e}"))?;
        let mut sess = det()?.lock().map_err(|_| anyhow!("OCR session poisoned"))?;
        let outputs = sess.run(ort::inputs![tensor]).map_err(|e| anyhow!("{e}"))?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
        let (ph, pw) = (shape[2] as usize, shape[3] as usize);
        db_boxes(&data[..ph * pw], pw, ph, w, h)
    };
    let crops: Vec<BgrImage> = boxes
        .iter()
        .map(|(b, _)| {
            let c = img.crop(b[0] as i64, b[1] as i64, (b[2] - b[0]) as i64, (b[3] - b[1]) as i64);
            if c.h as f64 / c.w.max(1) as f64 >= 1.5 {
                rot90(&c)
            } else {
                c
            }
        })
        .filter(|c| !c.is_empty())
        .collect();
    if crops.is_empty() {
        return Ok(vec![]);
    }
    let res = recognize(&crops)?;
    Ok(res.into_iter().filter(|(t, s)| !t.trim().is_empty() && *s >= TEXT_SCORE).collect())
}

/// numpy rot90 (counter-clockwise).
fn rot90(img: &BgrImage) -> BgrImage {
    let mut out = BgrImage::new(img.h, img.w);
    for y in 0..img.h {
        for x in 0..img.w {
            out.set(y, img.w - 1 - x, img.px(x, y));
        }
    }
    out
}

/// Detect + recognise text in a larger region (banners).
pub fn read_block(img: &BgrImage, min_score: f32) -> Vec<(String, f32)> {
    if img.is_empty() {
        return vec![];
    }
    match detect_and_read(img) {
        Ok(v) => v.into_iter().filter(|(_, s)| *s >= min_score).map(|(t, s)| (t.trim().to_string(), s)).collect(),
        Err(e) => {
            log::warn!("OCR block failed: {e:#}");
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splitlines_rules() {
        assert_eq!(splitlines("a\nb\r\nc\rd\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\n\nb"), vec!["a", "", "b"]);
    }
}
