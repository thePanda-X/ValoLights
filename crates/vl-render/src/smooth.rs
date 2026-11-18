//! Super smooth: subtle, physically sensible motion blur for the gameplay layer.
//!
//! Presets (`params`): "faster" blurs every pixel along its optical flow (`vecblur`, CPU only, no AI
//! frames); "fast" and "subtle" (HD subtle) use AI frame interpolation as below with 4 or 8 sub-frames;
//! "custom" takes the method, samples and shutter from `SmoothCustom`. The AI path:
//!
//! 1. RIFE v4.6 (rife-ncnn-vulkan, GPU via Vulkan) interpolates the gameplay to ~8 sub-frames per output frame.
//! 2. Sub-frames are blended back to the output rate over a ~180-degree shutter with tent weights.
//! 3. Blending happens in linear light (sRGB -> linear -> blend -> sRGB).
//! 4. Hard cuts are detected and never interpolated across.
//! 5. Only the gameplay layer is processed; overlays are composited afterwards and stay crisp.
//!
//! Frames stream through RIFE in one-second chunks; decoding, RIFE (GPU) and the blend/encode run as an
//! overlapping pipeline, so temporary disk use stays around one to two gigabytes.
use crate::numfmt::{num, round_i};
use anyhow::{anyhow, bail, Result};
use once_cell::sync::OnceCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use vl_core::ffmpeg as ff;
use vl_core::image::BgrImage;
use vl_core::jobs::Cancel;
use vl_core::paths;

/// Super smooth settings resolved from a preset (`smooth_amount`) or the custom values.
#[derive(Debug, Clone, PartialEq)]
pub struct SmoothParams {
    /// optical-flow motion blur (CPU, no AI frames, see `vecblur`) instead of RIFE
    pub flow: bool,
    /// RIFE: interpolated samples per output frame; flow: most blur samples per pixel
    pub samples: f64,
    /// fraction of the frame interval blended (0.5 = 180-degree shutter)
    pub shutter: f64,
    /// flow only: half-resolution motion and two-frame sampling
    pub high_detail: bool,
}

/// The presets offered in Layout, in order: (key, label).
pub const PRESETS: [(&str, &str); 4] = [("faster", "Faster"), ("fast", "Fast"), ("subtle", "HD subtle"), ("custom", "Custom")];

pub fn params(amount: &str, custom: &vl_core::models::SmoothCustom) -> SmoothParams {
    let p = |flow, samples: f64, shutter, high_detail| SmoothParams { flow, samples, shutter, high_detail };
    match amount {
        // "Fast": half the AI frames of HD subtle
        "fast" => p(false, 4.0, 0.5, false),
        "subtle" => p(false, 8.0, 0.5, false),
        "strong" => p(false, 8.0, 0.75, false),
        "custom" => {
            let flow = custom.method != "rife";
            let samples = if flow { custom.samples.clamp(2, 32) } else { custom.samples.clamp(2, 16) };
            p(flow, samples as f64, custom.shutter.clamp(0.05, 1.0), flow && custom.high_detail)
        }
        // "faster" and anything unknown
        _ => p(true, 8.0, 0.5, false),
    }
}

/// The optical-flow preset (also the stand-in when the AI interpolator can't run).
pub fn faster() -> SmoothParams {
    params("faster", &Default::default())
}
/// source frames per RIFE batch (plus one frame of overlap)
pub const CHUNK_FRAMES: usize = 60;
/// mean abs difference (0-255, on a small grey image) that marks a hard cut
pub const CUT_THRESHOLD: f64 = 28.0;
/// same matrix on the way to RGB and back, so colours are untouched
pub const MATRIX: &str = "bt709";

pub fn rife_dir() -> PathBuf {
    paths::bin_dir().join("rife")
}

pub fn rife_exe() -> PathBuf {
    rife_dir().join("rife-ncnn-vulkan.exe")
}

fn count_png(d: &Path) -> usize {
    std::fs::read_dir(d).map(|rd| rd.flatten().filter(|e| e.path().extension().map(|x| x == "png").unwrap_or(false)).count()).unwrap_or(0)
}

/// One chunk of source frames on disk (`<dir>/in/*.png`), with the cuts that must not be interpolated.
struct Chunk {
    dir: PathBuf,
    n: usize,
    first: bool,
    cuts: std::collections::HashSet<usize>,
}

fn write_chunk(work: &Path, idx: usize, frames: &[BgrImage], first: bool) -> Result<Chunk> {
    let dir = work.join(format!("c{idx:05}"));
    let _ = std::fs::remove_dir_all(&dir);
    let cin = dir.join("in");
    std::fs::create_dir_all(&cin)?;
    for (i, f) in frames.iter().enumerate() {
        write_png(f, &cin.join(format!("{i:08}.png")))?;
    }
    let n = frames.len();
    let smalls: Vec<Vec<i16>> = frames.iter().map(small).collect();
    let cuts = (0..n.saturating_sub(1)).filter(|&i| is_cut(&smalls[i], &smalls[i + 1])).collect();
    Ok(Chunk { dir, n, first, cuts })
}

/// RIFE on one chunk; the sorted output frames (empty for a single-frame chunk).
fn run_rife(c: &Chunk, m: usize) -> Result<Vec<PathBuf>> {
    let (cin, cout) = (c.dir.join("in"), c.dir.join("out"));
    let n = c.n;
    if n >= 2 {
        // transient Vulkan/driver hiccups happen (e.g. another app grabbing the GPU): retry with
        // fewer threads before giving up on the render
        let mut ok = false;
        let mut last_err = String::new();
        for jobs in ["2:6:6", "1:2:2", "1:1:1"] {
            let _ = std::fs::remove_dir_all(&cout);
            std::fs::create_dir_all(&cout)?;
            let o = ff::command(&rife_exe().to_string_lossy())
                .args(["-i", &cin.to_string_lossy(), "-o", &cout.to_string_lossy(), "-n", &(n * m).to_string(), "-m"])
                .arg(rife_dir().join("rife-v4.6"))
                .args(["-g", "0", "-j", jobs, "-f", "png"])
                .stdin(Stdio::null())
                .output()?;
            if o.status.success() && count_png(&cout) > (n - 1) * m {
                ok = true;
                break;
            }
            let e = String::from_utf8_lossy(&o.stderr);
            last_err = format!("code {}: {}", o.status.code().unwrap_or(-1), &e[e.len().saturating_sub(300)..]);
            log::warn!("RIFE chunk failed ({last_err}, threads {jobs}), retrying");
        }
        if !ok {
            bail!("frame interpolation failed ({last_err})");
        }
    } else {
        std::fs::create_dir_all(&cout)?;
    }
    let mut outs: Vec<PathBuf> =
        std::fs::read_dir(&cout)?.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "png").unwrap_or(false)).collect();
    outs.sort();
    Ok(outs)
}

fn write_png(img: &BgrImage, p: &Path) -> Result<()> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let f = std::fs::File::create(p)?;
    let enc = PngEncoder::new_with_quality(std::io::BufWriter::new(f), CompressionType::Fast, FilterType::Sub);
    enc.write_image(&img.to_rgb(), img.w as u32, img.h as u32, image::ExtendedColorType::Rgb8)?;
    Ok(())
}

/// Run RIFE once on two tiny frames to make sure a Vulkan GPU is usable. (ok, reason)
pub fn rife_works() -> (bool, String) {
    static R: OnceCell<(bool, String)> = OnceCell::new();
    R.get_or_init(|| {
        let exe = rife_exe();
        if !exe.exists() {
            return (false, "the frame interpolator is missing from the bin folder".into());
        }
        let work = paths::tmp_dir().join("rife_probe");
        let _ = std::fs::remove_dir_all(&work);
        let res = (|| -> Result<bool> {
            std::fs::create_dir_all(work.join("in"))?;
            std::fs::create_dir_all(work.join("out"))?;
            for (i, v) in [40u8, 200].iter().enumerate() {
                let img = BgrImage::from_raw(64, 64, vec![*v; 64 * 64 * 3]);
                write_png(&img, &work.join("in").join(format!("{i:08}.png")))?;
            }
            let out = ff::command(&exe.to_string_lossy())
                .args(["-i", &work.join("in").to_string_lossy(), "-o", &work.join("out").to_string_lossy(), "-n", "4", "-m"])
                .arg(rife_dir().join("rife-v4.6"))
                .args(["-f", "png"])
                .stdin(Stdio::null())
                .output()?;
            Ok(out.status.success() && count_png(&work.join("out")) == 4)
        })();
        let _ = std::fs::remove_dir_all(&work);
        match res {
            Ok(true) => (true, String::new()),
            Ok(false) => (false, "no Vulkan-capable GPU found for frame interpolation".into()),
            Err(e) => (false, e.to_string()),
        }
    })
    .clone()
}

/// The source's colour tags as ffmpeg output options (`-colorspace bt709 ...`), or none.
///
/// Newer ffmpeg negotiates YUV colour spaces between filter inputs: a BT.709-tagged gameplay layer over an
/// untagged background would be converted and shift the chroma.
pub fn source_tags(src: &str) -> Vec<String> {
    let Ok(probe) = ff::ffprobe_exe() else { return vec![] };
    let Ok(r) = ff::run(&[
        probe.as_str(),
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=color_space,color_primaries,color_transfer,color_range",
        "-of",
        "json",
        src,
    ]) else {
        return vec![];
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&r.stdout) else { return vec![] };
    let st = v["streams"].get(0).cloned().unwrap_or(serde_json::Value::Null);
    let mut out = vec![];
    for (key, opt) in [
        ("color_space", "-colorspace"),
        ("color_primaries", "-color_primaries"),
        ("color_transfer", "-color_trc"),
        ("color_range", "-color_range"),
    ] {
        if let Some(s) = st[key].as_str() {
            if !s.is_empty() && !["unknown", "unspecified", "reserved"].contains(&s) {
                out.push(opt.to_string());
                out.push(s.to_string());
            }
        }
    }
    out
}

/// setparams filter giving the smoothed frames exactly the source's colour tags (or none).
pub fn source_params(src: &str) -> String {
    let tags = source_tags(src);
    let get = |k: &str| tags.chunks(2).find(|c| c[0] == k).map(|c| c[1].clone()).unwrap_or_else(|| "unknown".into());
    format!(
        "setparams=colorspace={}:color_primaries={}:color_trc={}:range={}",
        get("-colorspace"),
        get("-color_primaries"),
        get("-color_trc"),
        get("-color_range")
    )
}

pub fn tent_weights(k: usize) -> Vec<usize> {
    (0..k).map(|i| (i + 1).min(k - i)).collect()
}

/// (interpolation multiple M, high frame rate, frames blended K).
pub fn blend_window(in_fps: f64, out_fps: f64, p: &SmoothParams) -> (usize, i64, usize) {
    let m = 2.max(round_i(out_fps * p.samples / in_fps.max(1.0))) as usize;
    let hi = in_fps * m as f64;
    let k = 2.max(round_i(hi / out_fps * p.shutter)) as usize;
    (m, round_i(hi), k)
}

pub fn blend_filter(k: usize, out_fps: f64, params: &str) -> String {
    let w = tent_weights(k).iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
    // sRGB -> linear light, blend, back to sRGB (all in RGB, primaries pinned), then BT.709 YUV
    format!(
        "zscale=pin=709:p=709:tin=iec61966-2-1:t=linear:npl=100,format=gbrpf32le,tmix=frames={k}:weights='{w}',\
         zscale=pin=709:p=709:tin=linear:t=iec61966-2-1,format=gbrp,\
         scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,fps={}{}",
        num(out_fps),
        if params.is_empty() { String::new() } else { format!(",{params}") }
    )
}

/// 96x54 grey thumbnail (OpenCV INTER_AREA resize + BGR2GRAY) as i16.
pub fn small(img: &BgrImage) -> Vec<i16> {
    let (dw, dh) = (96usize, 54usize);
    let (sw, sh) = (img.w.max(1), img.h.max(1));
    let (fx, fy) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let (y0, y1) = (y as f64 * fy, (y + 1) as f64 * fy);
        for x in 0..dw {
            let (x0, x1) = (x as f64 * fx, (x + 1) as f64 * fx);
            let mut acc = [0f64; 3];
            let mut wsum = 0.0;
            let mut yy = y0.floor() as usize;
            while (yy as f64) < y1 && yy < sh {
                let wy = (y1.min(yy as f64 + 1.0) - y0.max(yy as f64)).max(0.0);
                let mut xx = x0.floor() as usize;
                while (xx as f64) < x1 && xx < sw {
                    let wx = (x1.min(xx as f64 + 1.0) - x0.max(xx as f64)).max(0.0);
                    let wgt = wx * wy;
                    if wgt > 0.0 && img.w > 0 && img.h > 0 {
                        let p = img.px(xx, yy);
                        for c in 0..3 {
                            acc[c] += p[c] as f64 * wgt;
                        }
                        wsum += wgt;
                    }
                    xx += 1;
                }
                yy += 1;
            }
            let px: Vec<i32> = acc.iter().map(|v| if wsum > 0.0 { (v / wsum).round() as i32 } else { 0 }).collect();
            // OpenCV fixed-point BGR2GRAY
            let g = (px[2] * 4899 + px[1] * 9617 + px[0] * 1868 + 8192) >> 14;
            out.push(g as i16);
        }
    }
    out
}

pub fn is_cut(a: &[i16], b: &[i16]) -> bool {
    let n = a.len().min(b.len()).max(1);
    let s: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum();
    s / n as f64 > CUT_THRESHOLD
}

pub(crate) fn encoder_args() -> Vec<&'static str> {
    if ff::nvenc_works() {
        vec!["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "14", "-b:v", "0", "-pix_fmt", "yuv420p"]
    } else {
        vec!["-c:v", "libx264", "-preset", "fast", "-crf", "12", "-pix_fmt", "yuv420p"]
    }
}

/// Write a smoothed, cropped copy of [start, start+dur] of the source at out_fps.
#[allow(clippy::too_many_arguments)]
pub fn smooth_part(
    src: &str,
    start: f64,
    dur: f64,
    crop: (i32, i32, i32, i32),
    in_fps: f64,
    out_fps: f64,
    params: &SmoothParams,
    out: &Path,
    progress: &dyn Fn(f64),
    cancel: &Cancel,
) -> Result<PathBuf> {
    if params.flow {
        return crate::vecblur::blur_part(src, start, dur, crop, in_fps, out_fps, params, out, progress, cancel);
    }
    let (m, hi_fps, k) = blend_window(in_fps, out_fps, params);
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let work = out.parent().unwrap_or(Path::new(".")).join(format!("{stem}_frames"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let mut args: Vec<String> =
        ["-hide_banner", "-v", "error", "-y", "-f", "image2pipe", "-framerate"].iter().map(|s| s.to_string()).collect();
    args.push(hi_fps.to_string());
    args.extend(["-c:v", "png", "-i", "-", "-vf"].iter().map(|s| s.to_string()));
    args.push(blend_filter(k, out_fps, &source_params(src)));
    args.extend(encoder_args().iter().map(|s| s.to_string()));
    args.extend(source_tags(src));
    args.extend(["-t".to_string(), format!("{dur:.3}"), out.to_string_lossy().into_owned()]);
    let mut sink =
        ff::command(&ff::ffmpeg_exe()?).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    let mut stderr = sink.stderr.take().unwrap();
    let err_th = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stderr.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    });
    let stdin = sink.stdin.take();
    let total = 1.max((dur * in_fps) as usize);

    // Three stages overlap (same frames, same order, same output as running them one after another):
    //   this thread: decode a chunk and write its PNGs  ->  RIFE thread (GPU)  ->  feeder thread (ffmpeg blend
    //   + encode). RIFE is the slow step; decoding, writing and feeding now happen while it runs instead of
    //   between its runs. At most three chunks exist on disk at once.
    let done = std::sync::atomic::AtomicUsize::new(0);
    let result = std::thread::scope(|scope| -> Result<()> {
        let (rife_tx, rife_rx) = std::sync::mpsc::sync_channel::<Chunk>(1);
        let (feed_tx, feed_rx) = std::sync::mpsc::sync_channel::<(Chunk, Result<Vec<PathBuf>>)>(1);
        let rife = scope.spawn(move || {
            for c in rife_rx {
                let r = run_rife(&c, m);
                let failed = r.is_err();
                if feed_tx.send((c, r)).is_err() || failed {
                    break;
                }
            }
        });
        let done_ref = &done;
        let feeder = scope.spawn(move || -> Result<()> {
            let mut stdin = stdin;
            let mut feed = |p: &Path| -> Result<()> {
                let bytes = std::fs::read(p)?;
                stdin.as_mut().ok_or_else(|| anyhow!("encoder closed"))?.write_all(&bytes).map_err(|_| anyhow!("smoothing encode failed"))
            };
            for (c, outs) in feed_rx {
                let outs = outs?;
                let cin = c.dir.join("in");
                let n = c.n;
                // output j sits at timestep j/m; keep positions 0..n-1 (later outputs duplicate the last frame)
                let end = (n.saturating_sub(1)) * m + 1;
                for j in (if c.first { 0 } else { 1 })..end {
                    let (i, r) = (j / m, j % m);
                    if r == 0 || c.cuts.contains(&i) || n < 2 || j >= outs.len() {
                        let src_i = if r != 0 { (n - 1).min(if (r as f64) < m as f64 / 2.0 { i } else { i + 1 }) } else { i };
                        feed(&cin.join(format!("{src_i:08}.png")))?;
                    } else {
                        feed(&outs[j])?;
                    }
                }
                let _ = std::fs::remove_dir_all(&c.dir);
                done_ref.fetch_add(c.n.saturating_sub(if c.first { 0 } else { 1 }), std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        });

        let produce = (|| -> Result<()> {
            let (x, y, w, h) = crop;
            let opts = ff::ReadOpts {
                fps: in_fps,
                crop: Some((x, y, w, h)),
                start,
                duration: Some(dur),
                hwaccel: true,
                matrix: Some(MATRIX.into()),
                ..Default::default()
            };
            let mut buf: Vec<BgrImage> = vec![];
            let mut first = true;
            let mut idx = 0usize;
            let send = |frames: &[BgrImage], first: bool, idx: usize| -> Result<()> {
                let c = write_chunk(&work, idx, frames, first)?;
                rife_tx.send(c).map_err(|_| anyhow!("frame interpolation stopped"))
            };
            for fr in ff::read_frames(Path::new(src), opts, Some(cancel.0.clone()))? {
                if cancel.is_set() {
                    bail!(vl_core::jobs::Cancelled);
                }
                buf.push(fr?.img);
                if buf.len() == CHUNK_FRAMES + 1 {
                    send(&buf, first, idx)?;
                    first = false;
                    idx += 1;
                    progress((done.load(std::sync::atomic::Ordering::Relaxed) as f64 / total as f64).min(0.99));
                    let last = buf.pop().unwrap();
                    buf = vec![last]; // overlap: the next chunk starts on this frame
                }
            }
            if cancel.is_set() {
                bail!(vl_core::jobs::Cancelled);
            }
            if buf.len() > if first { 0 } else { 1 } {
                send(&buf, first, idx)?;
            }
            Ok(())
        })();
        drop(rife_tx); // no more chunks: the RIFE thread ends, then the feeder
        let fed = feeder.join().unwrap_or_else(|_| Err(anyhow!("smoothing feeder crashed")));
        let _ = rife.join();
        // the first error wins (a RIFE failure surfaces in the feeder, a decode failure in the producer)
        produce.and(fed)
    });
    if result.is_err() {
        let _ = sink.kill();
    }
    let status = sink.wait();
    let err = err_th.join().unwrap_or_default();
    let _ = std::fs::remove_dir_all(&work);
    if let Err(e) = result {
        if e.is::<vl_core::jobs::Cancelled>() {
            return Err(e);
        }
        bail!("{e}: {}", &err[err.len().saturating_sub(800)..]);
    }
    if !status.map(|s| s.success()).unwrap_or(false) {
        bail!("smoothing encode failed: {}", &err[err.len().saturating_sub(800)..]);
    }
    progress(1.0);
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(amount: &str) -> SmoothParams {
        params(amount, &Default::default())
    }

    #[test]
    fn blend_window_60_to_60_is_8x_with_half_shutter() {
        assert_eq!(blend_window(60.0, 60.0, &pr("subtle")), (8, 480, 4));
        assert_eq!(blend_window(60.0, 60.0, &pr("strong")).2, 6); // 270 degrees, never a full 360 smear
                                                                  // fast: half the interpolated frames, same half shutter
        assert_eq!(blend_window(60.0, 60.0, &pr("fast")), (4, 240, 2));
    }

    #[test]
    fn blend_window_other_rates() {
        assert_eq!(blend_window(30.0, 60.0, &pr("subtle")), (16, 480, 4));
        assert_eq!(blend_window(60.0, 30.0, &pr("subtle")), (4, 240, 4));
    }

    #[test]
    fn presets_and_custom() {
        assert!(pr("faster").flow && !pr("fast").flow && !pr("subtle").flow);
        let mut c = vl_core::models::SmoothCustom { method: "rife".into(), samples: 40, shutter: 3.0, high_detail: true };
        let p = params("custom", &c);
        assert_eq!((p.flow, p.samples, p.shutter, p.high_detail), (false, 16.0, 1.0, false));
        c.method = "flow".into();
        let p = params("custom", &c);
        assert_eq!((p.flow, p.samples, p.high_detail), (true, 32.0, true));
    }

    #[test]
    fn tent_weights_are_symmetric() {
        assert_eq!(tent_weights(4), vec![1, 2, 2, 1]);
        assert_eq!(tent_weights(6), vec![1, 2, 3, 3, 2, 1]);
    }

    #[test]
    fn blend_filter_blends_in_linear_light() {
        let f = blend_filter(4, 60.0, "");
        let (a, b, c) = (f.find("t=linear").unwrap(), f.find("tmix").unwrap(), f.find("t=iec61966-2-1").unwrap());
        assert!(a < b && b < c);
        assert!(f.contains("weights='1 2 2 1'") && f.ends_with("fps=60"));
    }

    #[test]
    fn cut_detection() {
        let a = vec![0i16; 54 * 96];
        let b: Vec<i16> = a.iter().map(|v| v + 5).collect();
        let c: Vec<i16> = a.iter().map(|v| v + 120).collect();
        assert!(!is_cut(&a, &b));
        assert!(is_cut(&a, &c));
    }

    #[test]
    fn small_is_grey_area_average() {
        let img = BgrImage::from_raw(192, 108, vec![100; 192 * 108 * 3]);
        let s = small(&img);
        assert_eq!(s.len(), 96 * 54);
        assert!(s.iter().all(|v| *v == 100));
    }
}
