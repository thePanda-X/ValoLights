//! Super smooth: subtle, physically sensible motion blur for the gameplay layer.
//!
//! Presets (`params`): "faster" blurs every pixel along its optical flow (`vecblur`, CPU only, no AI
//! frames); "fast" and "subtle" (HD subtle) use AI frame interpolation as below with 4 or 8 sub-frames;
//! "custom" takes the method, samples and shutter from `SmoothCustom`. The AI path:
//!
//! 1. The gameplay is sampled on a grid of ~8 sub-frames per output frame; each output frame blends the
//!    sub-frames of a ~180-degree shutter with tent weights (`Sampler`).
//! 2. RIFE v4.6 (rife-ncnn-vulkan, GPU via Vulkan) makes only those sub-frames: our build's `-T` job list
//!    (tools/rife/jobs.patch) skips the rest of the grid, roughly half of the work.
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

/// Where one blended sample comes from, in a chunk: a source frame, or the i-th RIFE job's output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sample {
    Frame(usize),
    Rife(usize),
}

/// Which sub-frames each output frame blends.
///
/// The gameplay is sampled on a grid of `m` sub-frames per source frame (`hi_fps` of them per second). Output frame
/// `o` blends the `k` sub-frames ending at the last one ffmpeg's `fps` filter (round to nearest) would have kept
/// for it, `n * out / hi < o + 0.5`, so it looks exactly like blending the whole grid and then dropping frames.
#[derive(Debug, Clone)]
pub struct Sampler {
    pub m: usize,
    pub hi_fps: i64,
    pub out_fps: f64,
    pub k: usize,
    /// output frames to make
    pub n_out: usize,
}

impl Sampler {
    pub fn new(in_fps: f64, out_fps: f64, dur: f64, p: &SmoothParams) -> Sampler {
        let (m, hi_fps, k) = blend_window(in_fps, out_fps, p);
        let n_out = 1.max((dur * out_fps - 1e-6).ceil() as usize);
        Sampler { m, hi_fps, out_fps, k, n_out }
    }

    /// Grid index of sample `s` (output `s / k`, oldest sample first).
    pub fn grid(&self, s: usize) -> usize {
        let (o, j) = (s / self.k, s % self.k);
        let last = ((o as f64 + 0.5) * self.hi_fps as f64 / self.out_fps - 1e-9).floor() as i64;
        (last - (self.k - 1 - j) as i64).max(0) as usize
    }

    /// Source frames two chunks share, so every output's samples fit inside one chunk.
    pub fn overlap(&self) -> usize {
        1 + (self.k - 1).div_ceil(self.m)
    }

    /// Plan the outputs that can be made from source frames `base..base+n` (`last`: no frames follow), starting at
    /// output `*next`, which is advanced. Returns the samples in feed order and the RIFE jobs (pair, sub-frame),
    /// each interpolated once. Pairs in `cuts` are never interpolated across: the nearer source frame stands in.
    pub fn plan(
        &self,
        next: &mut usize,
        base: usize,
        n: usize,
        cuts: &std::collections::HashSet<usize>,
        last: bool,
    ) -> (Vec<Sample>, Vec<(usize, usize)>) {
        let (m, k) = (self.m, self.k);
        let mut samples = vec![];
        let mut jobs: Vec<(usize, usize)> = vec![];
        let mut job_of = std::collections::HashMap::new();
        while *next < self.n_out {
            let grid: Vec<usize> = (0..k).map(|j| self.grid(*next * k + j)).collect();
            // the newest sample needs frames up to here (one frame for an exact source frame, else the pair)
            let need = grid.iter().map(|g| g / m + usize::from(g % m != 0)).max().unwrap_or(0);
            if !last && need >= base + n {
                break;
            }
            for g in grid {
                let (i, r) = ((g / m).saturating_sub(base), g % m);
                let smp = if n < 2 || i >= n - 1 {
                    Sample::Frame(i.min(n - 1)) // the end of the source: hold the last frame
                } else if r == 0 {
                    Sample::Frame(i)
                } else if cuts.contains(&i) {
                    Sample::Frame(if (r as f64) < m as f64 / 2.0 { i } else { i + 1 })
                } else {
                    Sample::Rife(*job_of.entry((i, r)).or_insert_with(|| {
                        jobs.push((i, r));
                        jobs.len() - 1
                    }))
                };
                samples.push(smp);
            }
            *next += 1;
        }
        (samples, jobs)
    }
}

/// One chunk of source frames on disk (`<dir>/in/*.png`) and the samples planned from it.
struct Chunk {
    dir: PathBuf,
    samples: Vec<Sample>,
    jobs: Vec<(usize, usize)>,
    /// output frames finished once this chunk is fed
    outputs_done: usize,
}

fn write_chunk(work: &Path, idx: usize, frames: &[BgrImage]) -> Result<(PathBuf, std::collections::HashSet<usize>)> {
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
    Ok((dir, cuts))
}

/// RIFE on one chunk's jobs; their output frames in job order.
fn run_rife(c: &Chunk, m: usize) -> Result<Vec<PathBuf>> {
    let (cin, cout) = (c.dir.join("in"), c.dir.join("out"));
    std::fs::create_dir_all(&cout)?;
    if c.jobs.is_empty() {
        return Ok(vec![]);
    }
    let list = c.dir.join("jobs.txt");
    std::fs::write(
        &list,
        c.jobs
            .iter()
            .map(|(i, r)| {
                format!(
                    "{i} {r} {m}
"
                )
            })
            .collect::<String>(),
    )?;
    // transient Vulkan/driver hiccups happen (e.g. another app grabbing the GPU): retry with
    // fewer threads before giving up on the render
    let mut last_err = String::new();
    for threads in ["2:6:6", "1:2:2", "1:1:1"] {
        let _ = std::fs::remove_dir_all(&cout);
        std::fs::create_dir_all(&cout)?;
        let o = ff::command(&rife_exe().to_string_lossy())
            .args(["-i", &cin.to_string_lossy(), "-o", &cout.to_string_lossy(), "-T", &list.to_string_lossy(), "-m"])
            .arg(rife_dir().join("rife-v4.6"))
            .args(["-g", "0", "-j", threads, "-f", "png"])
            .stdin(Stdio::null())
            .output()?;
        if o.status.success() && count_png(&cout) == c.jobs.len() {
            // named 1, 2, ... in job order
            return Ok((1..=c.jobs.len()).map(|j| cout.join(format!("{j:08}.png"))).collect());
        }
        let e = String::from_utf8_lossy(&o.stderr);
        last_err = format!("code {}: {}", o.status.code().unwrap_or(-1), &e[e.len().saturating_sub(300)..]);
        log::warn!("RIFE chunk failed ({last_err}, threads {threads}), retrying");
    }
    bail!("frame interpolation failed ({last_err})")
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
            // the way renders call it: a job list (-T, our patched build), here two in-between frames
            let list = work.join("jobs.txt");
            std::fs::write(
                &list,
                "0 1 4
0 3 4
",
            )?;
            let out = ff::command(&exe.to_string_lossy())
                .args(["-i", &work.join("in").to_string_lossy(), "-o", &work.join("out").to_string_lossy(), "-T", &list.to_string_lossy()])
                .arg("-m")
                .arg(rife_dir().join("rife-v4.6"))
                .args(["-f", "png"])
                .stdin(Stdio::null())
                .output()?;
            if String::from_utf8_lossy(&out.stderr).contains("Usage:") {
                bail!("the frame interpolator in the bin folder is an old build (no -T option)");
            }
            Ok(out.status.success() && count_png(&work.join("out")) == 2)
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

/// The blend for input that carries `k` samples per output frame (at `out_fps * k`): each group of `k` becomes one
/// frame, weighted with a tent.
pub fn blend_filter(k: usize, out_fps: f64, params: &str) -> String {
    let w = tent_weights(k).iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
    // sRGB -> linear light, blend, back to sRGB (all in RGB, primaries pinned), then BT.709 YUV
    format!(
        "zscale=pin=709:p=709:tin=iec61966-2-1:t=linear:npl=100,format=gbrpf32le,tmix=frames={k}:weights='{w}',         select='eq(mod(n,{k}),{})',setpts=N/({}*TB),         zscale=pin=709:p=709:tin=linear:t=iec61966-2-1,format=gbrp,         scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,fps={}{}",
        k - 1,
        num(out_fps),
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
    let sampler = Sampler::new(in_fps, out_fps, dur, params);
    let (m, k) = (sampler.m, sampler.k);
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let work = out.parent().unwrap_or(Path::new(".")).join(format!("{stem}_frames"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    // k samples per output frame, blended group by group
    let mut args: Vec<String> =
        ["-hide_banner", "-v", "error", "-y", "-f", "image2pipe", "-framerate"].iter().map(|s| s.to_string()).collect();
    args.push(num(out_fps * k as f64));
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
    let total = sampler.n_out;

    // Three stages overlap: this thread decodes a chunk, writes its PNGs and plans which sub-frames its output
    // frames blend -> RIFE thread (GPU) makes only the in-between frames in that plan -> feeder thread sends the
    // samples to ffmpeg (blend + encode). RIFE is the slow step; the rest happens while it runs. At most three
    // chunks exist on disk at once.
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
                for smp in &c.samples {
                    match *smp {
                        Sample::Frame(i) => feed(&cin.join(format!("{i:08}.png")))?,
                        Sample::Rife(j) => feed(&outs[j])?,
                    }
                }
                let _ = std::fs::remove_dir_all(&c.dir);
                done_ref.store(c.outputs_done, std::sync::atomic::Ordering::Relaxed);
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
            let overlap = sampler.overlap();
            let mut buf: Vec<BgrImage> = vec![];
            let mut base = 0usize; // source index of buf[0]
            let mut next = 0usize; // next output frame to plan
            let mut idx = 0usize;
            let mut send = |frames: &[BgrImage], base: usize, last: bool, idx: usize| -> Result<()> {
                let (dir, cuts) = write_chunk(&work, idx, frames)?;
                let (samples, jobs) = sampler.plan(&mut next, base, frames.len(), &cuts, last);
                rife_tx.send(Chunk { dir, samples, jobs, outputs_done: next }).map_err(|_| anyhow!("frame interpolation stopped"))
            };
            for fr in ff::read_frames(Path::new(src), opts, Some(cancel.0.clone()))? {
                if cancel.is_set() {
                    bail!(vl_core::jobs::Cancelled);
                }
                buf.push(fr?.img);
                if buf.len() == CHUNK_FRAMES + overlap {
                    send(&buf, base, false, idx)?;
                    idx += 1;
                    progress((done.load(std::sync::atomic::Ordering::Relaxed) as f64 / total as f64).min(0.99));
                    // the next chunk starts on the last `overlap` frames of this one
                    base += buf.len() - overlap;
                    buf.drain(..buf.len() - overlap);
                }
            }
            if cancel.is_set() {
                bail!(vl_core::jobs::Cancelled);
            }
            if !buf.is_empty() {
                send(&buf, base, true, idx)?;
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
        // one blended frame per group of 4 samples
        assert!(f.contains("select='eq(mod(n,4),3)'"));
    }

    fn sampler(in_fps: f64, out_fps: f64, dur: f64, amount: &str) -> Sampler {
        Sampler::new(in_fps, out_fps, dur, &pr(amount))
    }

    #[test]
    fn sampler_matches_what_ffmpeg_fps_kept() {
        // ffmpeg's fps filter (round to nearest) keeps grid frames 3, 11, 19 at 480 -> 60, 7, 23 at 480 -> 30,
        // 1, 5, 9 at 240 -> 60 and 4, 12, 20 at 480 -> 59.94 (checked against ffmpeg 9): each output blends the
        // k frames ending there
        let last = |s: &Sampler, o: usize| s.grid(o * s.k + s.k - 1);
        let s = sampler(60.0, 60.0, 1.0, "subtle");
        assert_eq!((0..3).map(|o| last(&s, o)).collect::<Vec<_>>(), vec![3, 11, 19]);
        assert_eq!((0..4).map(|j| s.grid(8 + j)).collect::<Vec<_>>(), vec![16, 17, 18, 19]);
        let s = sampler(60.0, 30.0, 1.0, "subtle"); // m 4, hi 240, k 4
        assert_eq!((0..3).map(|o| last(&s, o)).collect::<Vec<_>>(), vec![3, 11, 19]);
        let s = sampler(60.0, 60.0, 1.0, "fast");
        assert_eq!((0..3).map(|o| last(&s, o)).collect::<Vec<_>>(), vec![1, 5, 9]);
        let s = Sampler { m: 8, hi_fps: 480, out_fps: 59.94, k: 4, n_out: 10 };
        assert_eq!((0..3).map(|o| last(&s, o)).collect::<Vec<_>>(), vec![4, 12, 20]);
        // the start holds frame 0 when the window reaches before it
        let s = Sampler { m: 8, hi_fps: 480, out_fps: 60.0, k: 6, n_out: 10 };
        assert_eq!((0..6).map(|j| s.grid(j)).collect::<Vec<_>>(), vec![0, 0, 0, 1, 2, 3]);
        assert_eq!(sampler(60.0, 60.0, 5.0, "subtle").n_out, 300);
    }

    #[test]
    fn plan_interpolates_only_used_sub_frames() {
        let s = sampler(60.0, 60.0, 1.0, "subtle"); // 8 sub-frames, 4 blended: the source frame and 1/8..3/8
        let none = Default::default();
        let mut next = 0;
        let (samples, jobs) = s.plan(&mut next, 0, 4, &none, false);
        // frames 0..3 are on disk: pairs 0-1, 1-2 and 2-3 can be made, output 3 needs frame 4
        assert_eq!(next, 3);
        assert_eq!(jobs, vec![(0, 1), (0, 2), (0, 3), (1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (2, 3)]);
        assert_eq!(&samples[..4], &[Sample::Frame(0), Sample::Rife(0), Sample::Rife(1), Sample::Rife(2)]);
        // the next chunk starts `overlap` frames back and goes on from output 3, numbering frames locally
        assert_eq!(s.overlap(), 2);
        let (samples, jobs) = s.plan(&mut next, 2, 4, &none, false);
        assert_eq!(next, 5);
        assert_eq!(jobs[0], (1, 1)); // global pair 3-4
        assert_eq!(samples[0], Sample::Frame(1));
    }

    #[test]
    fn plan_never_interpolates_across_a_cut_and_holds_the_end() {
        let s = sampler(60.0, 60.0, 1.0, "subtle");
        let cuts = [0usize].into_iter().collect();
        let mut next = 0;
        let (samples, jobs) = s.plan(&mut next, 0, 2, &cuts, true);
        assert!(jobs.is_empty());
        // across the cut the nearer frame stands in (1/8..3/8 -> frame 0); past the last frame it is held
        assert_eq!(&samples[..4], &[Sample::Frame(0); 4]);
        assert!(samples[4..].iter().all(|x| *x == Sample::Frame(1)));
        assert_eq!(next, s.n_out);
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
