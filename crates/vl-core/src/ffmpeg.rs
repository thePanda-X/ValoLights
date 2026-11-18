//! Thin wrappers around the bundled ffmpeg / ffprobe executables.
use crate::image::BgrImage;
use crate::paths;
use anyhow::{anyhow, bail, Context, Result};
use once_cell::sync::{Lazy, OnceCell};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A Command that never flashes a console window on Windows.
pub fn command(exe: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

fn exe(name: &str) -> Result<String> {
    let local = paths::bin_dir().join(format!("{name}.exe"));
    if local.exists() {
        return Ok(local.to_string_lossy().into_owned());
    }
    which(name).ok_or_else(|| anyhow!("{name} not found (expected in {})", paths::bin_dir().display()))
}

fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for d in std::env::split_paths(&path) {
        for cand in [d.join(format!("{name}.exe")), d.join(name)] {
            if cand.is_file() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

pub fn ffmpeg_exe() -> Result<String> {
    exe("ffmpeg")
}
pub fn ffprobe_exe() -> Result<String> {
    exe("ffprobe")
}

/// Run a command to completion; error (with the stderr tail) on a non-zero exit.
pub fn run<S: AsRef<str>>(args: &[S]) -> Result<Output> {
    let out = run_unchecked(args)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        let head: Vec<&str> = args.iter().take(6).map(|a| a.as_ref()).collect();
        bail!("command failed ({}): {} ...\n{}", out.status.code().unwrap_or(-1), head.join(" "), tail);
    }
    Ok(out)
}

pub fn run_unchecked<S: AsRef<str>>(args: &[S]) -> Result<Output> {
    let mut c = command(args[0].as_ref());
    for a in &args[1..] {
        c.arg(a.as_ref());
    }
    c.stdin(Stdio::null());
    c.output().with_context(|| format!("could not start {}", args[0].as_ref()))
}

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub path: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub avg_fps: f64,
    pub vfr: bool,
    pub has_audio: bool,
    pub size: u64,
    pub codec: String,
}

fn ratio(s: &str) -> f64 {
    match s.split_once('/') {
        Some((a, b)) => {
            let (a, b) = (a.parse::<f64>().unwrap_or(0.0), b.parse::<f64>().unwrap_or(0.0));
            if b != 0.0 {
                a / b
            } else {
                0.0
            }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

pub fn probe(path: &Path) -> Result<ProbeInfo> {
    let p = path.to_string_lossy().into_owned();
    let out = run(&[ffprobe_exe()?.as_str(), "-v", "error", "-print_format", "json", "-show_format", "-show_streams", &p])?;
    let d: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = d["streams"].as_array().cloned().unwrap_or_default();
    let v = streams.iter().find(|s| s["codec_type"] == "video").ok_or_else(|| anyhow!("no video stream in {p}"))?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let fps = ratio(v["r_frame_rate"].as_str().unwrap_or("0/1"));
    let avg = match ratio(v["avg_frame_rate"].as_str().unwrap_or("0/1")) {
        x if x > 0.0 => x,
        _ => fps,
    };
    let num = |x: &serde_json::Value| x.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| x.as_f64());
    let dur = num(&d["format"]["duration"]).or_else(|| num(&v["duration"])).unwrap_or(0.0);
    Ok(ProbeInfo {
        path: p,
        duration: dur,
        width: v["width"].as_u64().unwrap_or(0) as u32,
        height: v["height"].as_u64().unwrap_or(0) as u32,
        fps,
        avg_fps: avg,
        vfr: (fps - avg).abs() > 0.5,
        has_audio,
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        codec: v["codec_name"].as_str().unwrap_or("").to_string(),
    })
}

/// Use NVDEC when available; ffmpeg silently falls back to software otherwise.
pub fn hw_decode_args() -> &'static [&'static str] {
    static HW: OnceCell<bool> = OnceCell::new();
    let ok = *HW.get_or_init(|| {
        ffmpeg_exe()
            .and_then(|f| run(&[f.as_str(), "-hide_banner", "-hwaccels"]))
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("cuda"))
            .unwrap_or(false)
    });
    if ok {
        &["-hwaccel", "cuda"]
    } else {
        &[]
    }
}

pub fn available_encoders() -> &'static [String] {
    static ENC: Lazy<Vec<String>> = Lazy::new(|| {
        let Ok(f) = ffmpeg_exe() else { return vec![] };
        let Ok(o) = run(&[f.as_str(), "-hide_banner", "-encoders"]) else { return vec![] };
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| {
                let p: Vec<&str> = l.split_whitespace().collect();
                (p.len() >= 2 && p[0].len() == 6 && "VAS".contains(&p[0][..1])).then(|| p[1].to_string())
            })
            .collect()
    });
    &ENC
}

/// NVENC may be compiled in but unusable (no NVIDIA GPU / old driver): test-encode a few frames.
pub fn nvenc_works() -> bool {
    static OK: Lazy<bool> = Lazy::new(|| {
        if !available_encoders().iter().any(|e| e == "h264_nvenc") {
            return false;
        }
        let Ok(f) = ffmpeg_exe() else { return false };
        run(&[
            f.as_str(),
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=black:s=256x256:d=0.2",
            "-c:v",
            "h264_nvenc",
            "-f",
            "null",
            "-",
        ])
        .is_ok()
    });
    *OK
}

// ---------------------------------------------------------------- frame reader
#[derive(Debug, Clone)]
pub struct Frame {
    pub t: f64,
    pub img: BgrImage,
}

#[derive(Debug, Clone, Default)]
pub struct ReadOpts {
    pub fps: f64,
    /// (x, y, w, h) in source pixels, applied before scaling (snapped to even values)
    pub crop: Option<(i32, i32, i32, i32)>,
    /// output height (width keeps the aspect, even)
    pub scale_h: Option<u32>,
    /// explicit output width (with scale_h) for non-uniform scaling
    pub scale_w: Option<u32>,
    pub start: f64,
    pub duration: Option<f64>,
    pub hwaccel: bool,
    /// explicit YUV->RGB matrix (e.g. "bt709") so a later RGB->YUV conversion is colour-neutral
    pub matrix: Option<String>,
}

/// Frames sampled at `fps` with their true source timestamps (`-copyts` + `showinfo` pts).
pub struct FrameReader {
    child: Child,
    stdout: std::process::ChildStdout,
    pts: mpsc::Receiver<Option<f64>>,
    err_tail: Arc<std::sync::Mutex<Vec<String>>>,
    w: usize,
    h: usize,
    i: usize,
    opts: ReadOpts,
    path: String,
    cancel: Option<Arc<AtomicBool>>,
    done: bool,
    retried: bool,
    /// decoded frames stay on the GPU; only the sampled ones are downloaded (see `start`)
    gpu_frames: bool,
}

/// Set once GPU-resident decoding failed in this process (e.g. a codec the GPU can't decode): later reads
/// go straight to the plain GPU decode instead of failing first every time.
static GPU_FRAMES_FAILED: AtomicBool = AtomicBool::new(false);

pub fn read_frames(path: &Path, opts: ReadOpts, cancel: Option<Arc<AtomicBool>>) -> Result<FrameReader> {
    FrameReader::start(path.to_string_lossy().into_owned(), opts, cancel)
}

impl FrameReader {
    fn start(path: String, opts: ReadOpts, cancel: Option<Arc<AtomicBool>>) -> Result<FrameReader> {
        let gpu_frames = opts.hwaccel && !hw_decode_args().is_empty() && !GPU_FRAMES_FAILED.load(Ordering::Relaxed);
        FrameReader::start_mode(path, opts, cancel, gpu_frames)
    }

    /// `gpu_frames`: decode on the GPU and keep the frames there; `fps` drops the unsampled ones on the GPU
    /// and only the kept ones are downloaded (NV12) for crop / scale / BGR on the CPU. Same pixels as
    /// downloading every frame, but sampling 6 of 60 frames a second no longer copies all 60 to memory:
    /// about 2.4x faster on 1080p60 AV1.
    fn start_mode(path: String, opts: ReadOpts, cancel: Option<Arc<AtomicBool>>, gpu_frames: bool) -> Result<FrameReader> {
        let mut vf = vec![];
        if gpu_frames {
            vf.push(format!("fps={}", opts.fps));
            vf.push("hwdownload".to_string());
            vf.push("format=nv12|p010le".to_string());
        }
        let (mut out_w, mut out_h);
        if let Some((x, y, w, h)) = opts.crop {
            let (x, y, w, h) = (x / 2 * 2, y / 2 * 2, w / 2 * 2, h / 2 * 2);
            vf.push(format!("crop={w}:{h}:{x}:{y}"));
            out_w = w as u32;
            out_h = h as u32;
        } else {
            let info = probe(Path::new(&path))?;
            out_w = info.width;
            out_h = info.height;
        }
        if let Some(sh) = opts.scale_h {
            let nw = opts.scale_w.unwrap_or_else(|| ((out_w as f64 * sh as f64 / out_h as f64 / 2.0).round() * 2.0) as u32);
            if sh != out_h || nw != out_w {
                // area averaging when shrinking; bicubic when enlarging low-res video so thin HUD edges stay crisp
                let flags = if sh < out_h { "area" } else { "bicubic" };
                vf.push(format!("scale={nw}:{sh}:flags={flags}"));
                out_w = nw;
                out_h = sh;
            }
        }
        if !gpu_frames {
            vf.push(format!("fps={}", opts.fps));
        }
        if let Some(m) = &opts.matrix {
            vf.push(format!("scale=in_color_matrix={m}:in_range=tv"));
        }
        vf.push("showinfo".into());
        let mut c = command(&ffmpeg_exe()?);
        c.args(["-hide_banner", "-v", "info", "-nostdin", "-copyts"]);
        if opts.hwaccel {
            c.args(hw_decode_args());
            if gpu_frames {
                c.args(["-hwaccel_output_format", "cuda"]);
            }
        }
        if opts.start > 0.0 {
            c.args(["-ss", &format!("{:.3}", opts.start)]);
        }
        if let Some(d) = opts.duration {
            c.args(["-t", &format!("{d:.3}")]);
        }
        c.args(["-i", &path, "-an", "-sn", "-vf", &vf.join(","), "-f", "rawvideo", "-pix_fmt", "bgr24", "pipe:1"]);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().context("could not start ffmpeg")?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let tail = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tail2 = tail.clone();
        std::thread::spawn(move || {
            let re = regex::Regex::new(r"pts_time:\s*(-?[0-9.]+)").unwrap();
            let mut r = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf);
                if line.contains("showinfo") && line.contains("pts_time:") {
                    if let Some(m) = re.captures(&line) {
                        let _ = tx.send(m[1].parse::<f64>().ok());
                        continue;
                    }
                }
                let mut t = tail2.lock().unwrap();
                if t.len() < 200 {
                    t.push(line.into_owned());
                }
            }
            let _ = tx.send(None);
        });
        Ok(FrameReader {
            child,
            stdout,
            pts: rx,
            err_tail: tail,
            w: out_w as usize,
            h: out_h as usize,
            i: 0,
            opts,
            path,
            cancel,
            done: false,
            retried: false,
            gpu_frames,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn finish_error(&mut self) -> Option<Result<Frame>> {
        let _ = self.child.wait();
        let failed = self.i == 0 && !self.child.try_wait().ok().flatten().map(|s| s.success()).unwrap_or(true);
        let msg = self.err_tail.lock().unwrap().join("");
        if failed && !msg.is_empty() {
            if self.gpu_frames {
                // GPU-resident decode not possible for this video: the plain GPU decode next
                log::warn!("GPU frame sampling failed, decoding with frame downloads: {}", &msg[msg.len().saturating_sub(300)..]);
                GPU_FRAMES_FAILED.store(true, Ordering::Relaxed);
                return match FrameReader::start_mode(self.path.clone(), self.opts.clone(), self.cancel.clone(), false) {
                    Ok(r) => {
                        *self = r;
                        self.next()
                    }
                    Err(e) => Some(Err(e)),
                };
            }
            if self.opts.hwaccel && !hw_decode_args().is_empty() && !self.retried {
                log::warn!("hw decode failed, retrying in software: {}", &msg[msg.len().saturating_sub(300)..]);
                let mut o = self.opts.clone();
                o.hwaccel = false;
                match FrameReader::start(self.path.clone(), o, self.cancel.clone()) {
                    Ok(mut r) => {
                        r.retried = true;
                        *self = r;
                        return self.next();
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
            return Some(Err(anyhow!("ffmpeg decode failed: {}", &msg[msg.len().saturating_sub(1000)..])));
        }
        None
    }
}

impl Iterator for FrameReader {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.cancel.as_ref().map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) {
            self.done = true;
            let _ = self.child.kill();
            return None;
        }
        let n = self.w * self.h * 3;
        let mut buf = vec![0u8; n];
        if self.stdout.read_exact(&mut buf).is_err() {
            self.done = true;
            return self.finish_error();
        }
        let t = match self.pts.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Some(t)) => t,
            _ => self.opts.start + self.i as f64 / self.opts.fps,
        };
        self.i += 1;
        Some(Ok(Frame { t, img: BgrImage::from_raw(self.w, self.h, buf) }))
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn grab_frame(path: &Path, t: f64, crop: Option<(i32, i32, i32, i32)>) -> Result<BgrImage> {
    let opts = ReadOpts { fps: 60.0, crop, start: t, duration: Some(0.05), hwaccel: false, ..Default::default() };
    match read_frames(path, opts, None)?.next() {
        Some(f) => Ok(f?.img),
        None => bail!("no frame at {t}"),
    }
}

/// Median cropdetect over several timestamps -> (x, y, w, h) of the active picture.
pub fn detect_crop(path: &Path, info: &ProbeInfo, samples: usize) -> Result<(i32, i32, i32, i32)> {
    let re = regex::Regex::new(r"crop=(\d+):(\d+):(\d+):(\d+)").unwrap();
    let ff = ffmpeg_exe()?;
    let p = path.to_string_lossy().into_owned();
    let mut rects = vec![];
    for k in 0..samples {
        let t = info.duration * (k + 1) as f64 / (samples + 1) as f64;
        let out = run_unchecked(&[
            ff.as_str(),
            "-hide_banner",
            "-nostdin",
            "-ss",
            &format!("{t:.2}"),
            "-i",
            &p,
            "-t",
            "1.0",
            "-vf",
            "cropdetect=limit=24:round=2:reset=0",
            "-an",
            "-f",
            "null",
            "-",
        ])?;
        let err = String::from_utf8_lossy(&out.stderr);
        if let Some(c) = re.captures_iter(&err).last() {
            let v: Vec<i32> = (1..=4).map(|i| c[i].parse().unwrap_or(0)).collect();
            rects.push((v[2], v[3], v[0], v[1]));
        }
    }
    if rects.is_empty() {
        return Ok((0, 0, info.width as i32, info.height as i32));
    }
    let med = |f: fn(&(i32, i32, i32, i32)) -> i32| {
        let mut v: Vec<i32> = rects.iter().map(f).collect();
        v.sort();
        // statistics.median: mean of the two middle values for even counts
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2
        }
    };
    let (mut x, mut y, mut w, mut h) = (med(|r| r.0), med(|r| r.1), med(|r| r.2), med(|r| r.3));
    if w >= info.width as i32 - 16 {
        x = 0;
        w = info.width as i32;
    }
    if h >= info.height as i32 - 16 {
        y = 0;
        h = info.height as i32;
    }
    Ok((x, y, w, h))
}
