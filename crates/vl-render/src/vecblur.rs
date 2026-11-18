//! "Faster" super smooth: optical-flow motion blur instead of AI in-between frames.
//!
//! For 60 fps gameplay exported at 60 fps the AI frames are never shown, they only exist to be averaged into
//! motion blur. Here each pixel is blurred along its own motion instead:
//!
//! 1. Optical flow between neighbouring frames, forwards and backwards ([`crate::flow`], quarter or half
//!    resolution).
//! 2. A per-pixel confidence: does following the flow land on the same colour in the next frame? Things that
//!    don't move with what's around them (the crosshair, HUD bits, edges being uncovered) get low confidence:
//!    their own motion is damped, and other pixels' blur doesn't pick them up.
//! 3. For every output frame, the shutter interval (e.g. half a frame, centred on the frame) is sampled at
//!    a few points with tent weights. A sample at time `n + d` takes the colour from frame `n`, moved back
//!    along the motion. The number of samples follows the motion length, so still pixels cost one read.
//! 4. Everything is averaged in linear light, like the AI path, then converted back to sRGB.
//!
//! "High detail" measures motion at half resolution and samples both neighbouring frames (true
//! interpolation, also used when the output frame rate is higher than the source's).
//!
//! Hard cuts are never blurred across. Frames stream: only a few decoded frames are held at once and the
//! encoder (ffmpeg, raw frames on stdin) runs alongside.
use crate::flow::{self, Flow, Gray};
use crate::smooth::{self, SmoothParams, MATRIX};
use anyhow::{anyhow, bail, Result};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use vl_core::ffmpeg as ff;
use vl_core::jobs::Cancel;

/// confidence 1 below this grey difference (0-255), 0 above `CONF_HI`
const CONF_LO: f32 = 12.0;
const CONF_HI: f32 = 40.0;
/// confidence smoothing radius (half-res pixels)
const CONF_SOFT: usize = 2;
/// target spacing between blur samples along the motion, pixels
const TAP_SPACING: f32 = 2.0;

static S2L: once_cell::sync::Lazy<[f32; 256]> = once_cell::sync::Lazy::new(|| {
    let mut t = [0f32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    }
    t
});
const L2S_N: usize = 4096;
static L2S: once_cell::sync::Lazy<Vec<u8>> = once_cell::sync::Lazy::new(|| {
    (0..L2S_N)
        .map(|i| {
            let l = i as f32 / (L2S_N - 1) as f32;
            let c = if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
            (c * 255.0).round().clamp(0.0, 255.0) as u8
        })
        .collect()
});

#[cfg(test)]
fn to_srgb(l: f32) -> u8 {
    srgb_with(&L2S, l)
}

/// `to_srgb` with the table already looked up (hot loops: no lazy-init check per pixel).
#[inline]
fn srgb_with(t: &[u8], l: f32) -> u8 {
    t[((l * (L2S_N - 1) as f32 + 0.5) as usize).min(L2S_N - 1)]
}

/// One decoded frame, ready for flow and blur.
pub struct Frame {
    w: usize,
    h: usize,
    /// the decoded pixels (B G R); samples go to linear light through a 256-entry table on the fly (a
    /// float copy of the frame would be 4x the memory traffic)
    bgr: Vec<u8>,
    s2l: &'static [f32; 256],
    /// grey at half resolution (for the confidence)
    half: Gray,
    /// flow pyramid (finest level = flow resolution)
    pyr: Vec<Gray>,
    /// cut-detection thumbnail
    small: Vec<i16>,
}

impl Frame {
    pub fn new(img: vl_core::image::BgrImage, high_detail: bool) -> Frame {
        let (w, h, bgr) = (img.w, img.h, img.data);
        let half = flow::grey_down(&bgr, w, h, 2);
        let base = if high_detail { half.clone() } else { half.half() };
        let small = thumb(&half);
        Frame { w, h, bgr, s2l: &S2L, half, pyr: flow::pyramid(base), small }
    }

    /// Linear-light colour of byte offset `i`.
    #[inline]
    fn lin(&self, i: usize) -> [f32; 3] {
        let t = self.s2l;
        [t[self.bgr[i] as usize], t[self.bgr[i + 1] as usize], t[self.bgr[i + 2] as usize]]
    }

    #[inline]
    fn rgb(&self, x: f32, y: f32) -> [f32; 3] {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b, c, d) = ((y0 * self.w + x0) * 3, (y0 * self.w + x1) * 3, (y1 * self.w + x0) * 3, (y1 * self.w + x1) * 3);
        let (wa, wb, wc, wd) = ((1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy);
        debug_assert!(d + 2 < self.bgr.len());
        // SAFETY: x0, x1 < w and y0, y1 < h after the clamps above (a NaN coordinate casts to 0), so every
        // offset is inside the w*h*3 buffer; bytes index the 256-entry table. This is the innermost loop of
        // the blur (several reads per pixel per frame), where the bounds checks cost about a third.
        let lin = |i: usize| unsafe {
            let t = self.s2l;
            let p = self.bgr.as_ptr().add(i);
            [*t.get_unchecked(*p as usize), *t.get_unchecked(*p.add(1) as usize), *t.get_unchecked(*p.add(2) as usize)]
        };
        let (la, lb, lc, ld) = (lin(a), lin(b), lin(c), lin(d));
        [
            la[0] * wa + lb[0] * wb + lc[0] * wc + ld[0] * wd,
            la[1] * wa + lb[1] * wb + lc[1] * wc + ld[1] * wd,
            la[2] * wa + lb[2] * wb + lc[2] * wc + ld[2] * wd,
        ]
    }

    #[inline]
    fn px(&self, x: usize, y: usize) -> [f32; 3] {
        self.lin((y * self.w + x) * 3)
    }
}

/// Separable box blur of radius `r` (edges clamped).
fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let n = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; w * h];
    flow::par_rows(&mut tmp, w, |y, row| {
        let s = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let mut a = 0.0;
            for k in 0..=2 * r {
                a += s[(x + k).saturating_sub(r).min(w - 1)];
            }
            *o = a / n;
        }
    });
    let mut out = vec![0f32; w * h];
    flow::par_rows(&mut out, w, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut a = 0.0;
            for k in 0..=2 * r {
                a += tmp[(y + k).saturating_sub(r).min(h - 1) * w + x];
            }
            *o = a / n;
        }
    });
    out
}

/// The 96x54 grey thumbnail cut detection compares (like `smooth::small`, from the half-res grey).
fn thumb(g: &Gray) -> Vec<i16> {
    let (dw, dh) = (96usize, 54usize);
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let y0 = y * g.h / dh;
        let y1 = ((y + 1) * g.h / dh).max(y0 + 1).min(g.h);
        for x in 0..dw {
            let x0 = x * g.w / dw;
            let x1 = ((x + 1) * g.w / dw).max(x0 + 1).min(g.w);
            let mut a = 0.0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    a += g.at(xx, yy);
                }
            }
            out.push((a / ((y1 - y0) * (x1 - x0)).max(1) as f32).round() as i16);
        }
    }
    out
}

/// One way of a frame pair: the flow and how much to trust it per pixel.
struct Motion {
    flow: Flow,
    /// half resolution, 0..1
    conf: Vec<f32>,
    cw: usize,
    ch: usize,
    /// full resolution / flow resolution
    sx: f32,
    sy: f32,
    /// full resolution -> confidence grid
    kx: f32,
    ky: f32,
    /// per full-res column: flow columns and weight, confidence column (row-wise lookups, see `row`)
    xmap: Vec<(u32, u32, f32)>,
    cmap: Vec<u32>,
}

impl Motion {
    fn new(a: &Frame, b: &Frame) -> Motion {
        let flow = flow::dis(&a.pyr, &b.pyr);
        Motion::with_flow(a, b, flow)
    }

    /// `flow` (a -> b) and its confidence.
    fn with_flow(a: &Frame, b: &Frame, flow: Flow) -> Motion {
        let (sx, sy) = (a.w as f32 / flow.w as f32, a.h as f32 / flow.h as f32);
        let (cw, ch) = (a.half.w, a.half.h);
        let mut conf = vec![0f32; cw * ch];
        // the half-res grid against the flow grid
        let (hx, hy) = (a.w as f32 / cw as f32, a.h as f32 / ch as f32);
        flow::par_rows(&mut conf, cw, |y, row| {
            for (x, c) in row.iter_mut().enumerate() {
                // centre of this half-res pixel in full-res coordinates
                let (fx, fy) = ((x as f32 + 0.5) * hx - 0.5, (y as f32 + 0.5) * hy - 0.5);
                let (u, v) = flow.sample((fx + 0.5) / sx - 0.5, (fy + 0.5) / sy - 0.5);
                let (u, v) = (u * sx / hx, v * sy / hy);
                let err = (a.half.at(x, y) - b.half.sample(x as f32 + u, y as f32 + v)).abs();
                *c = ((CONF_HI - err) / (CONF_HI - CONF_LO)).clamp(0.0, 1.0);
            }
        });
        // soften it: hard on/off steps would show as blocky edges between blurred and sharp areas
        let conf = box_blur(&conf, cw, ch, CONF_SOFT);
        let (kx, ky) = (cw as f32 / a.w as f32, ch as f32 / a.h as f32);
        let xmap = (0..a.w)
            .map(|x| {
                let f = ((x as f32 + 0.5) / sx - 0.5).clamp(0.0, (flow.w - 1) as f32);
                let k0 = f as usize;
                (k0 as u32, (k0 + 1).min(flow.w - 1) as u32, f - k0 as f32)
            })
            .collect();
        let cmap = (0..a.w).map(|x| ((x as f32 * kx) as usize).min(cw - 1) as u32).collect();
        Motion { conf, cw, ch, sx, sy, kx, ky, flow, xmap, cmap }
    }

    /// Full-res row `y`: the flow interpolated vertically (scaled to full-res pixels) into `u` / `v`, and the
    /// confidence row. Then `at_row` per pixel.
    fn row(&self, y: usize, u: &mut Vec<f32>, v: &mut Vec<f32>) -> &[f32] {
        let f = ((y as f32 + 0.5) / self.sy - 0.5).clamp(0.0, (self.flow.h - 1) as f32);
        let (r0, wy) = (f as usize, f - (f as usize) as f32);
        let r1 = (r0 + 1).min(self.flow.h - 1);
        let fw = self.flow.w;
        u.clear();
        v.clear();
        for k in 0..fw {
            let (a, b) = (r0 * fw + k, r1 * fw + k);
            u.push((self.flow.u[a] * (1.0 - wy) + self.flow.u[b] * wy) * self.sx);
            v.push((self.flow.v[a] * (1.0 - wy) + self.flow.v[b] * wy) * self.sy);
        }
        let cy = ((y as f32 * self.ky) as usize).min(self.ch - 1);
        &self.conf[cy * self.cw..(cy + 1) * self.cw]
    }

    /// `at` for pixel x of the row prepared by `row`.
    #[inline]
    fn at_row(&self, x: usize, u: &[f32], v: &[f32], crow: &[f32]) -> (f32, f32) {
        let (k0, k1, wx) = self.xmap[x];
        let (k0, k1) = (k0 as usize, k1 as usize);
        let c = crow[self.cmap[x] as usize];
        ((u[k0] * (1.0 - wx) + u[k1] * wx) * c, (v[k0] * (1.0 - wx) + v[k1] * wx) * c)
    }

    /// Confidence near full-res position (x, y).
    #[inline]
    fn conf_at(&self, x: f32, y: f32) -> f32 {
        let xi = ((x * self.kx) as isize).clamp(0, self.cw as isize - 1) as usize;
        let yi = ((y * self.ky) as isize).clamp(0, self.ch as isize - 1) as usize;
        self.conf[yi * self.cw + xi]
    }

    /// The motion of full-res pixel (x, y), damped by its confidence.
    #[inline]
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        let (u, v) = self.flow.sample((x + 0.5) / self.sx - 0.5, (y + 0.5) / self.sy - 0.5);
        let c = self.conf_at(x, y);
        (u * self.sx * c, v * self.sy * c)
    }
}

/// Frames `i` and `i + 1` (no motion across a hard cut).
struct Pair {
    /// i -> i+1 (for frame i)
    fwd: Option<Motion>,
    /// i+1 -> i (for frame i+1)
    bwd: Option<Motion>,
}

impl Pair {
    /// `both`: measure the backward motion too; otherwise it is the forward motion reversed (half the flow
    /// work; only slightly off where things accelerate or at moving edges, which motion blur hides).
    fn new(a: &Frame, b: &Frame, both: bool) -> Pair {
        if smooth::is_cut(&a.small, &b.small) {
            return Pair { fwd: None, bwd: None };
        }
        let (fwd, bwd) = if both {
            std::thread::scope(|s| {
                let f = s.spawn(|| Motion::new(a, b));
                let bw = Motion::new(b, a);
                (f.join().expect("flow thread"), bw)
            })
        } else {
            let fwd = Motion::new(a, b);
            let rev = Flow {
                w: fwd.flow.w,
                h: fwd.flow.h,
                u: fwd.flow.u.iter().map(|v| -v).collect(),
                v: fwd.flow.v.iter().map(|v| -v).collect(),
            };
            let bwd = Motion::with_flow(b, a, rev);
            (fwd, bwd)
        };
        Pair { fwd: Some(fwd), bwd: Some(bwd) }
    }
}

/// The decoded frames and pairs around the current output frame.
struct Window {
    base: usize,
    frames: VecDeque<Arc<Frame>>,
    /// pairs[k] = (base + k, base + k + 1)
    pairs: VecDeque<Pair>,
}

impl Window {
    fn last(&self) -> usize {
        self.base + self.frames.len() - 1
    }
    fn frame(&self, n: usize) -> &Frame {
        &self.frames[n.clamp(self.base, self.last()) - self.base]
    }
    fn pair(&self, n: usize) -> Option<&Pair> {
        n.checked_sub(self.base).and_then(|k| self.pairs.get(k))
    }
    /// motion of frame n towards n+1
    fn fwd(&self, n: usize) -> Option<&Motion> {
        self.pair(n).and_then(|p| p.fwd.as_ref())
    }
    /// motion of frame n towards n-1
    fn bwd(&self, n: usize) -> Option<&Motion> {
        n.checked_sub(1).and_then(|m| self.pair(m)).and_then(|p| p.bwd.as_ref())
    }
}

/// Blur settings for one render.
#[derive(Clone, Copy, Debug)]
pub struct BlurCfg {
    /// half the shutter interval, in source frames
    pub half: f64,
    pub max_taps: usize,
    /// sample both neighbouring frames (interpolate) instead of only the nearest
    pub bidir: bool,
}

impl BlurCfg {
    pub fn new(p: &SmoothParams, in_fps: f64, out_fps: f64) -> BlurCfg {
        BlurCfg {
            half: p.shutter * in_fps / out_fps.max(1.0) / 2.0,
            max_taps: p.samples.round().clamp(1.0, 64.0) as usize,
            bidir: p.high_detail || in_fps < out_fps * 0.99,
        }
    }
}

/// Frames an output frame may sample, with each one's motions at the current pixel (looked up once per pixel).
const MAX_SPAN: usize = 6;

struct PixelMotion {
    lo: usize,
    n: usize,
    /// towards the next / previous frame (confidence-damped), None at cuts and ends
    fwd: [Option<(f32, f32)>; MAX_SPAN],
    bwd: [Option<(f32, f32)>; MAX_SPAN],
}

impl PixelMotion {
    fn new(win: &Window, lo: usize, n: usize, x: f32, y: f32) -> PixelMotion {
        let mut m = PixelMotion { lo, n, fwd: [None; MAX_SPAN], bwd: [None; MAX_SPAN] };
        for i in 0..n {
            m.fwd[i] = win.fwd(lo + i).map(|mo| mo.at(x, y));
            m.bwd[i] = win.bwd(lo + i).map(|mo| mo.at(x, y));
        }
        m
    }
    #[inline]
    fn fwd(&self, f: usize) -> Option<(f32, f32)> {
        f.checked_sub(self.lo).filter(|i| *i < self.n).and_then(|i| self.fwd[i])
    }
    #[inline]
    fn bwd(&self, f: usize) -> Option<(f32, f32)> {
        f.checked_sub(self.lo).filter(|i| *i < self.n).and_then(|i| self.bwd[i])
    }
}

/// Colour of pixel (x, y) at source time `q` (frames), and its weight.
#[inline]
fn sample_at(win: &Window, cfg: &BlurCfg, pm: &PixelMotion, x: usize, y: usize, q: f64) -> ([f32; 3], f32) {
    let (xf, yf) = (x as f32, y as f32);
    let q = q.clamp(win.base as f64, win.last() as f64);
    if cfg.bidir {
        let i = (q.floor() as usize).min(win.last());
        let f = (q - i as f64) as f32;
        if f < 1e-3 || i == win.last() {
            return (win.frame(i).px(x, y), 1.0);
        }
        if f > 0.999 {
            return (win.frame(i + 1).px(x, y), 1.0);
        }
        let (Some((fu, fv)), Some((bu, bv)), Some(fm), Some(bm)) = (pm.fwd(i), pm.bwd(i + 1), win.fwd(i), win.bwd(i + 1)) else {
            // a cut (or a frame outside the cached span): the nearest frame as it is
            return (win.frame(if f < 0.5 { i } else { i + 1 }).px(x, y), 1.0);
        };
        let (pa, qa) = (xf - f * fu, yf - f * fv);
        let (pb, qb) = (xf - (1.0 - f) * bu, yf - (1.0 - f) * bv);
        let wa = (1.0 - f) * fm.conf_at(pa, qa);
        let wb = f * bm.conf_at(pb, qb);
        let ws = wa + wb;
        if ws < 1e-4 {
            return (win.frame(if f < 0.5 { i } else { i + 1 }).px(x, y), 1e-3);
        }
        let (ca, cb) = (win.frame(i).rgb(pa, qa), win.frame(i + 1).rgb(pb, qb));
        ([(ca[0] * wa + cb[0] * wb) / ws, (ca[1] * wa + cb[1] * wb) / ws, (ca[2] * wa + cb[2] * wb) / ws], ws)
    } else {
        let n = (q.round() as usize).clamp(win.base, win.last());
        let d = (q - n as f64) as f32;
        if d.abs() < 1e-3 {
            return (win.frame(n).px(x, y), 1.0);
        }
        let (v, m) = if d > 0.0 { (pm.fwd(n), win.fwd(n)) } else { (pm.bwd(n), win.bwd(n)) };
        let (Some((u, v)), Some(m)) = (v, m) else { return (win.frame(n).px(x, y), 1.0) };
        let (px, py) = (xf - d.abs() * u, yf - d.abs() * v);
        (win.frame(n).rgb(px, py), m.conf_at(px, py))
    }
}

/// Render the output frame centred on source time `p` (frames) into `out` (BGR8).
fn blur_frame(win: &Window, cfg: &BlurCfg, p: f64, out: &mut [u8]) {
    let (w, _h) = (win.frames[0].w, win.frames[0].h);
    let centre_n = (p.round() as usize).clamp(win.base, win.last());
    let on_frame = (p - centre_n as f64).abs() < 1e-6;
    if !cfg.bidir && cfg.half <= 0.5 && on_frame {
        return blur_on_frame(win, cfg, centre_n, out);
    }
    let lo = ((p - cfg.half).floor().max(0.0) as usize).clamp(win.base, win.last());
    let hi = (((p + cfg.half).ceil() as usize) + 1).clamp(lo, win.last());
    let span = (hi - lo + 1).min(MAX_SPAN);
    let centre_frame = win.frame(centre_n);
    let l2s: &[u8] = &L2S;
    flow::par_rows(out, w * 3, |y, row| {
        for x in 0..w {
            let pm = PixelMotion::new(win, lo, span, x as f32, y as f32);
            let len = |m: Option<(f32, f32)>| m.map(|(u, v)| (u * u + v * v).sqrt());
            let l = match (len(pm.fwd(centre_n)), len(pm.bwd(centre_n))) {
                (Some(a), Some(b)) => a.max(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => 0.0,
            } * 2.0
                * cfg.half as f32;
            if on_frame && l < 0.5 {
                let i = (y * w + x) * 3;
                row[x * 3..x * 3 + 3].copy_from_slice(&centre_frame.bgr[i..i + 3]);
                continue;
            }
            let taps = if cfg.half <= 0.0 { 1 } else { ((l / TAP_SPACING).ceil() as usize + 1).clamp(1, cfg.max_taps.max(1)) };
            // the pixel itself, faintly: the fallback when every sample is distrusted
            let c0 = centre_frame.px(x, y);
            let mut acc = [c0[0] * 1e-4, c0[1] * 1e-4, c0[2] * 1e-4];
            let mut wsum = 1e-4f32;
            for k in 0..taps {
                let (q, tw) = if taps == 1 {
                    (p, 1.0f32)
                } else {
                    let u = -1.0 + (2 * k + 1) as f64 / taps as f64;
                    (p + u * cfg.half, 1.0 - u.abs() as f32)
                };
                let (col, cw) = sample_at(win, cfg, &pm, x, y, q);
                let wt = tw * cw;
                for c in 0..3 {
                    acc[c] += col[c] * wt;
                }
                wsum += wt;
            }
            let o = &mut row[x * 3..x * 3 + 3];
            for c in 0..3 {
                o[c] = srgb_with(l2s, acc[c] / wsum);
            }
        }
    });
}

/// The common case (output frame = source frame, shutter within the neighbouring frames, nearest-frame
/// sampling): every sample comes from frame `n`, so each pixel's two motions are looked up once, and pixels
/// that don't move are copied as decoded.
fn blur_on_frame(win: &Window, cfg: &BlurCfg, n: usize, out: &mut [u8]) {
    let fr = win.frame(n);
    let w = fr.w;
    let (fm, bm) = (win.fwd(n), win.bwd(n));
    let half = cfg.half as f32;
    let max_taps = cfg.max_taps.max(1);
    let l2s: &[u8] = &L2S;
    flow::par_rows(out, w * 3, |y, row| {
        let yf = y as f32;
        let src = &fr.bgr[y * w * 3..(y + 1) * w * 3];
        let (mut fu, mut fv, mut bu, mut bv) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let fc = fm.map(|m| m.row(y, &mut fu, &mut fv));
        let bc = bm.map(|m| m.row(y, &mut bu, &mut bv));
        for x in 0..w {
            let xf = x as f32;
            let mf = match (fm, fc) {
                (Some(m), Some(c)) => m.at_row(x, &fu, &fv, c),
                _ => (0.0, 0.0),
            };
            let mb = match (bm, bc) {
                (Some(m), Some(c)) => m.at_row(x, &bu, &bv, c),
                _ => (0.0, 0.0),
            };
            let len = (mf.0 * mf.0 + mf.1 * mf.1).max(mb.0 * mb.0 + mb.1 * mb.1).sqrt() * 2.0 * half;
            if len < 0.5 {
                row[x * 3..x * 3 + 3].copy_from_slice(&src[x * 3..x * 3 + 3]);
                continue;
            }
            let taps = ((len / TAP_SPACING).ceil() as usize + 1).clamp(1, max_taps);
            let c0 = fr.px(x, y);
            let mut acc = [c0[0] * 1e-4, c0[1] * 1e-4, c0[2] * 1e-4];
            let mut wsum = 1e-4f32;
            for k in 0..taps {
                let u = -1.0 + (2 * k + 1) as f32 / taps as f32;
                let d = u * half;
                let tw = 1.0 - u.abs();
                let (m, v) = if d > 0.0 { (fm, mf) } else { (bm, mb) };
                let (col, cw) = match m {
                    Some(m) if d.abs() >= 1e-3 => {
                        let (px, py) = (xf - d.abs() * v.0, yf - d.abs() * v.1);
                        (fr.rgb(px, py), m.conf_at(px, py))
                    }
                    _ => (c0, 1.0),
                };
                let wt = tw * cw;
                acc[0] += col[0] * wt;
                acc[1] += col[1] * wt;
                acc[2] += col[2] * wt;
                wsum += wt;
            }
            let o = &mut row[x * 3..x * 3 + 3];
            for c in 0..3 {
                o[c] = srgb_with(l2s, acc[c] / wsum);
            }
        }
    });
}

/// Write a motion-blurred, cropped copy of [start, start+dur] of the source at out_fps (H.264).
#[allow(clippy::too_many_arguments)]
pub fn blur_part(
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
    let cfg = BlurCfg::new(params, in_fps, out_fps);
    let high = params.high_detail;
    let (x, y, w, h) = crop;
    let (w, h) = ((w / 2 * 2) as usize, (h / 2 * 2) as usize);
    if w < 16 || h < 16 {
        bail!("the gameplay area is too small to smooth");
    }
    let n_out = 1.max((dur * out_fps).round() as usize);
    let step = in_fps / out_fps;

    // encoder: raw BGR frames on stdin
    let mut args: Vec<String> =
        ["-hide_banner", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr24", "-s"].iter().map(|s| s.to_string()).collect();
    args.push(format!("{w}x{h}"));
    args.push("-framerate".into());
    args.push(crate::numfmt::num(out_fps));
    args.extend(["-i", "-", "-vf"].iter().map(|s| s.to_string()));
    let params_f = smooth::source_params(src);
    args.push(format!("format=gbrp,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,{params_f}"));
    args.extend(smooth::encoder_args().iter().map(|s| s.to_string()));
    args.extend(smooth::source_tags(src));
    args.extend(["-t".to_string(), format!("{dur:.3}"), out.to_string_lossy().into_owned()]);
    let mut sink =
        ff::command(&ff::ffmpeg_exe()?).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    let mut stderr = sink.stderr.take().unwrap();
    let err_th = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stderr.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    });
    let mut stdin = sink.stdin.take();

    let result = std::thread::scope(|scope| -> Result<()> {
        // decoder thread: frames -> linear light, pyramids (runs while the previous frame blurs)
        // (pairs are measured here too, so the flow overlaps the blur of earlier frames)
        let (ptx, prx) = std::sync::mpsc::sync_channel::<Result<Arc<Frame>>>(2);
        let (ftx, frx) = std::sync::mpsc::sync_channel::<Result<(Arc<Frame>, Option<Pair>)>>(2);
        let decoder = scope.spawn(move || {
            let opts = ff::ReadOpts {
                fps: in_fps,
                crop: Some((x, y, w as i32, h as i32)),
                start,
                duration: Some(dur + 2.0 / in_fps.max(1.0)),
                hwaccel: true,
                matrix: Some(MATRIX.into()),
                ..Default::default()
            };
            let reader = match ff::read_frames(Path::new(src), opts, Some(cancel.0.clone())) {
                Ok(r) => r,
                Err(e) => {
                    let _ = ptx.send(Err(e));
                    return;
                }
            };
            for fr in reader {
                let r = fr.map(|f| Arc::new(Frame::new(f.img, high)));
                let stop = r.is_err();
                if ptx.send(r).is_err() || stop {
                    return;
                }
            }
        });
        // flow thread: motion between neighbouring frames
        let pairs = scope.spawn(move || {
            let mut prev: Option<Arc<Frame>> = None;
            for r in prx {
                let r = r.map(|f| {
                    let pair = prev.as_ref().map(|p| Pair::new(p, &f, high));
                    prev = Some(f.clone());
                    (f, pair)
                });
                let stop = r.is_err();
                if ftx.send(r).is_err() || stop {
                    return;
                }
            }
        });
        // encoder feeder thread
        let (otx, orx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);
        let feeder = scope.spawn(move || -> Result<()> {
            for buf in orx {
                stdin.as_mut().ok_or_else(|| anyhow!("encoder closed"))?.write_all(&buf).map_err(|_| anyhow!("smoothing encode failed"))?;
            }
            drop(stdin);
            Ok(())
        });

        let run = (|| -> Result<()> {
            let mut frames = frx.into_iter();
            let mut win = Window { base: 0, frames: VecDeque::new(), pairs: VecDeque::new() };
            let mut ended = false;
            let mut pull = |win: &mut Window, ended: &mut bool| -> Result<()> {
                match frames.next() {
                    Some(f) => {
                        let (f, pair) = f?;
                        if f.w != w || f.h != h {
                            bail!("unexpected frame size {}x{}", f.w, f.h);
                        }
                        if let Some(pair) = pair {
                            win.pairs.push_back(pair);
                        }
                        win.frames.push_back(f);
                    }
                    None => *ended = true,
                }
                Ok(())
            };
            for j in 0..n_out {
                if cancel.is_set() {
                    bail!(vl_core::jobs::Cancelled);
                }
                let p = j as f64 * step;
                // frames up to the end of this shutter (and one more, for pairs)
                let need = (p + cfg.half).ceil() as usize + 1;
                while !ended && (win.frames.is_empty() || win.last() < need) {
                    pull(&mut win, &mut ended)?;
                }
                if win.frames.is_empty() {
                    bail!("no frames decoded");
                }
                // drop what the rest no longer needs
                let keep_from = ((p - cfg.half).floor() - 1.0).max(0.0) as usize;
                while win.base < keep_from && win.frames.len() > 1 {
                    win.frames.pop_front();
                    win.pairs.pop_front();
                    win.base += 1;
                }
                let mut buf = vec![0u8; w * h * 3];
                blur_frame(&win, &cfg, p.min(win.last() as f64), &mut buf);
                otx.send(buf).map_err(|_| anyhow!("smoothing encode failed"))?;
                progress(((j + 1) as f64 / n_out as f64).min(0.99));
            }
            Ok(())
        })();
        drop(otx);
        let fed = feeder.join().unwrap_or_else(|_| Err(anyhow!("smoothing feeder crashed")));
        let _ = pairs.join();
        let _ = decoder.join();
        run.and(fed)
    });
    if result.is_err() {
        let _ = sink.kill();
    }
    let status = sink.wait();
    let err = err_th.join().unwrap_or_default();
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

    fn frame_with_bar(x0: usize, w: usize, h: usize) -> Frame {
        // a white vertical bar 8 px wide on a textured dark background that moves with it
        let tex = crate::flow::tests::noise(w + 200, h, 3);
        let mut bgr = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let t = (tex.at(x + 150 - x0, y) * 0.5) as u8;
                let v = if x >= x0 && x < x0 + 8 { 250 } else { t };
                bgr[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v, v, v]);
            }
        }
        Frame::new(vl_core::image::BgrImage::from_raw(w, h, bgr), true)
    }

    fn window(frames: Vec<Frame>) -> Window {
        let frames: VecDeque<Arc<Frame>> = frames.into_iter().map(Arc::new).collect();
        let pairs = (0..frames.len() - 1).map(|i| Pair::new(&frames[i], &frames[i + 1], true)).collect();
        Window { base: 0, frames, pairs }
    }

    #[test]
    fn moving_bar_gets_blurred_along_its_motion() {
        let (w, h) = (256, 144);
        let win = window(vec![frame_with_bar(100, w, h), frame_with_bar(120, w, h), frame_with_bar(140, w, h)]);
        let cfg = BlurCfg { half: 0.25, max_taps: 16, bidir: false };
        let mut out = vec![0u8; w * h * 3];
        blur_frame(&win, &cfg, 1.0, &mut out);
        let row = h / 2;
        let at = |x: usize| out[(row * w + x) * 3] as i32;
        let src = |x: usize| win.frames[1].bgr[(row * w + x) * 3] as i32;
        // the bar (120..128) is smeared over about +-5 px (20 px/frame, a quarter frame each way)
        assert!(at(124) > 200, "bar centre {}", at(124));
        assert!(at(118) > src(118) + 10, "smear before the bar {} vs {}", at(118), src(118));
        assert!(at(129) > src(129) + 10, "smear after the bar {} vs {}", at(129), src(129));
        assert!(at(121) < 250, "bar edge softened {}", at(121));
    }

    #[test]
    fn still_frames_stay_identical() {
        let (w, h) = (128, 72);
        let f = || frame_with_bar(50, w, h);
        let win = window(vec![f(), f(), f()]);
        let cfg = BlurCfg { half: 0.25, max_taps: 16, bidir: true };
        let mut out = vec![0u8; w * h * 3];
        blur_frame(&win, &cfg, 1.0, &mut out);
        let src = &win.frames[1];
        for i in 0..w * h * 3 {
            assert!((out[i] as i32 - src.bgr[i] as i32).abs() <= 1);
        }
    }

    #[test]
    fn srgb_round_trip() {
        for v in 0..=255u8 {
            assert_eq!(to_srgb(S2L[v as usize]), v);
        }
    }
}
