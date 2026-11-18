//! The image operations the engine used from OpenCV / numpy, on `BgrImage` and single-channel planes.
//!
//! Formulas follow OpenCV's (8-bit HSV with H in 0..180, fixed-point grayscale, resize coefficient tables,
//! reflect-101 borders for blurs, TM_CCOEFF_NORMED with OpenCV's degenerate-window rules) because the detector
//! thresholds were tuned against OpenCV's numbers.
use std::sync::OnceLock;
use vl_core::image::BgrImage;

/// A single-channel image (row-major).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plane<T> {
    pub w: usize,
    pub h: usize,
    pub data: Vec<T>,
}

pub type Gray = Plane<u8>;
pub type PlaneF = Plane<f32>;

impl<T: Copy + Default> Plane<T> {
    pub fn new(w: usize, h: usize) -> Self {
        Plane { w, h, data: vec![T::default(); w * h] }
    }
    pub fn from_raw(w: usize, h: usize, data: Vec<T>) -> Self {
        assert_eq!(data.len(), w * h);
        Plane { w, h, data }
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> T {
        self.data[y * self.w + x]
    }
    #[inline]
    pub fn row(&self, y: usize) -> &[T] {
        &self.data[y * self.w..(y + 1) * self.w]
    }
    /// Copy of (x, y, w, h), clamped to the plane (numpy slicing semantics for non-negative bounds).
    pub fn crop(&self, x: i64, y: i64, w: i64, h: i64) -> Self {
        let x0 = x.clamp(0, self.w as i64) as usize;
        let y0 = y.clamp(0, self.h as i64) as usize;
        let x1 = (x + w).clamp(0, self.w as i64) as usize;
        let y1 = (y + h).clamp(0, self.h as i64) as usize;
        let (cw, ch) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        let mut out = Vec::with_capacity(cw * ch);
        for yy in y0..y0 + ch {
            out.extend_from_slice(&self.data[yy * self.w + x0..yy * self.w + x0 + cw]);
        }
        Plane { w: cw, h: ch, data: out }
    }
    pub fn map<U: Copy + Default>(&self, f: impl Fn(T) -> U) -> Plane<U> {
        Plane { w: self.w, h: self.h, data: self.data.iter().map(|&v| f(v)).collect() }
    }
}

impl Plane<u8> {
    pub fn to_f32(&self) -> PlaneF {
        self.map(|v| v as f32)
    }
    /// Fraction of non-zero values (numpy `mask.mean()` on a boolean mask).
    pub fn frac(&self) -> f64 {
        if self.data.is_empty() {
            return f64::NAN;
        }
        self.data.iter().filter(|&&v| v != 0).count() as f64 / self.data.len() as f64
    }
    pub fn count_nonzero(&self) -> usize {
        self.data.iter().filter(|&&v| v != 0).count()
    }
}

/// numpy-style slice of an image with clamped bounds: `img[y0:y1, x0:x1]` (negative starts clamp to 0).
pub fn slice(img: &BgrImage, y0: i64, y1: i64, x0: i64, x1: i64) -> BgrImage {
    let y1 = y1.min(img.h as i64);
    let x1 = x1.min(img.w as i64);
    let y0 = y0.max(0);
    let x0 = x0.max(0);
    img.crop(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

// ---------------------------------------------------------------- rounding helpers
/// OpenCV `saturate_cast<uchar>(float)`: round half to even, clamp.
#[inline]
pub fn sat_u8(v: f32) -> u8 {
    let r = v.round_ties_even();
    if r <= 0.0 {
        0
    } else if r >= 255.0 {
        255
    } else {
        r as u8
    }
}

#[inline]
fn sat_u8_f64(v: f64) -> u8 {
    let r = v.round_ties_even();
    r.clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------- colour conversion
/// HSV planes with OpenCV's 8-bit conventions (H 0..180, S/V 0..255).
#[derive(Clone, Debug, Default)]
pub struct Hsv {
    pub w: usize,
    pub h: usize,
    pub hue: Vec<u8>,
    pub sat: Vec<u8>,
    pub val: Vec<u8>,
}

impl Hsv {
    pub fn len(&self) -> usize {
        self.hue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.hue.is_empty()
    }
}

struct HsvTabs {
    sdiv: [i32; 256],
    hdiv: [i32; 256],
}

fn hsv_tabs() -> &'static HsvTabs {
    static T: OnceLock<HsvTabs> = OnceLock::new();
    T.get_or_init(|| {
        let mut sdiv = [0i32; 256];
        let mut hdiv = [0i32; 256];
        for i in 1..256 {
            sdiv[i] = ((255i64 << 12) as f64 / i as f64).round_ties_even() as i32;
            hdiv[i] = ((180i64 << 12) as f64 / (6.0 * i as f64)).round_ties_even() as i32;
        }
        HsvTabs { sdiv, hdiv }
    })
}

/// One pixel BGR -> HSV exactly like OpenCV's `RGB2HSV_b`.
#[inline]
pub fn hsv_px(b: u8, g: u8, r: u8) -> (u8, u8, u8) {
    let t = hsv_tabs();
    let (b, g, r) = (b as i32, g as i32, r as i32);
    let v = b.max(g).max(r);
    let vmin = b.min(g).min(r);
    let diff = v - vmin;
    let vr: i32 = if v == r { -1 } else { 0 };
    let vg: i32 = if v == g { -1 } else { 0 };
    let s = (diff * t.sdiv[v as usize] + (1 << 11)) >> 12;
    let mut h = (vr & (g - b)) + (!vr & ((vg & (b - r + 2 * diff)) + ((!vg) & (r - g + 4 * diff))));
    h = (h * t.hdiv[diff as usize] + (1 << 11)) >> 12;
    if h < 0 {
        h += 180;
    }
    (h as u8, s as u8, v as u8)
}

pub fn bgr2hsv(img: &BgrImage) -> Hsv {
    let n = img.w * img.h;
    let (mut hue, mut sat, mut val) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    for p in img.data.as_chunks::<3>().0 {
        let (h, s, v) = hsv_px(p[0], p[1], p[2]);
        hue.push(h);
        sat.push(s);
        val.push(v);
    }
    Hsv { w: img.w, h: img.h, hue, sat, val }
}

/// OpenCV 5 BGR2GRAY (fixed point, 15 bits; verified against cv2 5.0 for all 2^24 colours).
#[inline]
pub fn gray_px(b: u8, g: u8, r: u8) -> u8 {
    ((b as u32 * 3735 + g as u32 * 19235 + r as u32 * 9798 + (1 << 14)) >> 15) as u8
}

pub fn bgr2gray(img: &BgrImage) -> Gray {
    Plane { w: img.w, h: img.h, data: img.data.as_chunks::<3>().0.iter().map(|p| gray_px(p[0], p[1], p[2])).collect() }
}

// ---------------------------------------------------------------- resize
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Nearest,
    Linear,
    Cubic,
    Area,
}

/// Per output coordinate: (source indices, weights).
type Tab = Vec<(Vec<usize>, Vec<f32>)>;

fn interpolate_cubic64(x: f64) -> [f32; 4] {
    const A: f64 = -0.75;
    let c0 = ((A * (x + 1.0) - 5.0 * A) * (x + 1.0) + 8.0 * A) * (x + 1.0) - 4.0 * A;
    let c1 = ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
    let c2 = ((A + 2.0) * (1.0 - x) - (A + 3.0)) * (1.0 - x) * (1.0 - x) + 1.0;
    [c0 as f32, c1 as f32, c2 as f32, (1.0 - c0 - c1 - c2) as f32]
}

/// Coefficients of OpenCV's `resizeGeneric_` (linear / cubic / "area" upscaling).
/// `vertical`: OpenCV resets the fraction at the borders only for columns; rows are just clipped.
fn generic_tab(ssize: usize, dsize: usize, inv_scale: f64, interp: Interp, vertical: bool) -> Tab {
    let scale = 1.0 / inv_scale;
    let area_mode = interp == Interp::Area;
    let ksize: i64 = if interp == Interp::Cubic { 4 } else { 2 };
    let ksize2 = ksize / 2;
    let last = ssize as i64 - 1;
    (0..dsize)
        .map(|dx| {
            let (mut sx, mut fx): (i64, f32);
            if interp == Interp::Cubic {
                let p = (dx as f64 + 0.5) * scale - 0.5;
                sx = p.floor() as i64;
                fx = 0.0;
            } else if !area_mode {
                fx = ((dx as f64 + 0.5) * scale - 0.5) as f32;
                sx = fx.floor() as i64;
                fx -= sx as f32;
            } else {
                sx = (dx as f64 * scale).floor() as i64;
                fx = ((dx as f64 + 1.0) - (sx as f64 + 1.0) * inv_scale) as f32;
                fx = if fx <= 0.0 { 0.0 } else { fx - fx.floor() };
            }
            if !vertical && sx < ksize2 - 1 && sx < 0 && interp != Interp::Cubic {
                fx = 0.0;
                sx = 0;
            }
            if !vertical && sx + ksize2 >= ssize as i64 && sx >= last && interp != Interp::Cubic {
                fx = 0.0;
                sx = last;
            }
            let w: Vec<f32> = if interp == Interp::Cubic {
                // OpenCV 5 evaluates the cubic weights from a double-precision position (matches cv2 5.0 to the ulp
                // in ~95% of random cases, the rest differ in single pixels)
                let p = (dx as f64 + 0.5) * scale - 0.5;
                interpolate_cubic64(p - p.floor()).to_vec()
            } else {
                vec![1.0 - fx, fx]
            };
            let idx: Vec<usize> = (0..ksize).map(|k| (sx - ksize2 + 1 + k).clamp(0, last) as usize).collect();
            (idx, w)
        })
        .collect()
}

/// OpenCV `computeResizeAreaTab`.
fn area_tab(ssize: usize, dsize: usize, scale: f64) -> Tab {
    (0..dsize)
        .map(|dx| {
            let fsx1 = dx as f64 * scale;
            let fsx2 = fsx1 + scale;
            let cell = scale.min(ssize as f64 - fsx1);
            let mut sx1 = fsx1.ceil() as i64;
            let mut sx2 = fsx2.floor() as i64;
            sx2 = sx2.min(ssize as i64 - 1);
            sx1 = sx1.min(sx2);
            let (mut idx, mut w) = (vec![], vec![]);
            if sx1 as f64 - fsx1 > 1e-3 {
                idx.push((sx1 - 1) as usize);
                w.push(((sx1 as f64 - fsx1) / cell) as f32);
            }
            for sx in sx1..sx2 {
                idx.push(sx as usize);
                w.push((1.0 / cell) as f32);
            }
            if fsx2 - sx2 as f64 > 1e-3 {
                idx.push(sx2 as usize);
                w.push(((fsx2 - sx2 as f64).min(1.0).min(cell) / cell) as f32);
            }
            (idx, w)
        })
        .collect()
}

/// Separable resampling of an interleaved `cn`-channel buffer with float accumulation.
fn apply_tabs_f(src: &[f32], sw: usize, sh: usize, cn: usize, xt: &Tab, yt: &Tab) -> Vec<f32> {
    let dw = xt.len();
    let mut tmp = vec![0f32; sh * dw * cn];
    for y in 0..sh {
        let srow = &src[y * sw * cn..(y + 1) * sw * cn];
        let trow = &mut tmp[y * dw * cn..(y + 1) * dw * cn];
        for (dx, (idx, w)) in xt.iter().enumerate() {
            for c in 0..cn {
                let mut s = 0f32;
                for (i, k) in idx.iter().zip(w) {
                    s += srow[i * cn + c] * k;
                }
                trow[dx * cn + c] = s;
            }
        }
    }
    let mut out = vec![0f32; yt.len() * dw * cn];
    for (dy, (idx, w)) in yt.iter().enumerate() {
        let orow = &mut out[dy * dw * cn..(dy + 1) * dw * cn];
        for (i, k) in idx.iter().zip(w) {
            let trow = &tmp[i * dw * cn..(i + 1) * dw * cn];
            for (o, t) in orow.iter_mut().zip(trow) {
                *o += t * k;
            }
        }
    }
    out
}

/// Fixed-point (2048 = 1.0) 2-tap resampling of OpenCV 5 for 8-bit linear (and area upscaling): exact integer
/// horizontal sums, then the vectorised vertical step `((S0>>4)*b0>>16) + ((S1>>4)*b1>>16) + 2 >> 2`, which OpenCV
/// uses for every width (verified bit-exact against cv2 5.0).
fn apply_tabs_fixed(src: &[u8], sw: usize, sh: usize, cn: usize, xt: &Tab, yt: &Tab) -> Vec<u8> {
    let q = |w: &Vec<f32>| -> Vec<i32> { w.iter().map(|&v| (v * 2048.0).round_ties_even() as i32).collect() };
    let xq: Vec<(Vec<usize>, Vec<i32>)> = xt.iter().map(|(i, w)| (i.clone(), q(w))).collect();
    let dw = xt.len();
    let mut tmp = vec![0i32; sh * dw * cn];
    for y in 0..sh {
        let srow = &src[y * sw * cn..(y + 1) * sw * cn];
        let trow = &mut tmp[y * dw * cn..(y + 1) * dw * cn];
        for (dx, (idx, w)) in xq.iter().enumerate() {
            for c in 0..cn {
                let mut s = 0i32;
                for (i, k) in idx.iter().zip(w) {
                    s += srow[i * cn + c] as i32 * k;
                }
                trow[dx * cn + c] = s;
            }
        }
    }
    let mut out = vec![0u8; yt.len() * dw * cn];
    let pack = |v: i32| -> i32 { (v >> 4).clamp(i16::MIN as i32, i16::MAX as i32) };
    for (dy, (idx, w)) in yt.iter().enumerate() {
        let b = q(w);
        let (r0, r1) = (&tmp[idx[0] * dw * cn..(idx[0] + 1) * dw * cn], &tmp[idx[1] * dw * cn..(idx[1] + 1) * dw * cn]);
        let orow = &mut out[dy * dw * cn..(dy + 1) * dw * cn];
        for ((o, &s0), &s1) in orow.iter_mut().zip(r0).zip(r1) {
            let v = ((pack(s0) * b[0]) >> 16) + ((pack(s1) * b[1]) >> 16);
            *o = ((v + 2) >> 2).clamp(0, 255) as u8;
        }
    }
    out
}

fn nearest(src_len_per_px: usize, src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize, ifx: f64, ify: f64) -> Vec<u8> {
    let cn = src_len_per_px;
    let mut out = Vec::with_capacity(dw * dh * cn);
    let xs: Vec<usize> = (0..dw).map(|x| ((x as f64 * ifx).floor() as usize).min(sw - 1)).collect();
    for y in 0..dh {
        let sy = ((y as f64 * ify).floor() as usize).min(sh - 1);
        let row = &src[sy * sw * cn..(sy + 1) * sw * cn];
        for &sx in &xs {
            out.extend_from_slice(&row[sx * cn..sx * cn + cn]);
        }
    }
    out
}

/// What OpenCV's `resize` decides to do for these sizes / scales.
enum Plan {
    Copy,
    Nearest(f64, f64),
    Area(Tab, Tab),
    Generic(Tab, Tab),
}

fn plan(sw: usize, sh: usize, dw: usize, dh: usize, inv_sx: f64, inv_sy: f64, mut interp: Interp) -> Plan {
    if sw == dw && sh == dh {
        return Plan::Copy;
    }
    let (scale_x, scale_y) = (1.0 / inv_sx, 1.0 / inv_sy);
    if interp == Interp::Nearest {
        return Plan::Nearest(scale_x, scale_y);
    }
    let (isx, isy) = (scale_x.round_ties_even(), scale_y.round_ties_even());
    let area_fast = (scale_x - isx).abs() < f64::EPSILON && (scale_y - isy).abs() < f64::EPSILON;
    if interp == Interp::Linear && area_fast && isx == 2.0 && isy == 2.0 {
        interp = Interp::Area;
    }
    if interp == Interp::Area && scale_x >= 1.0 && scale_y >= 1.0 {
        return Plan::Area(area_tab(sw, dw, scale_x), area_tab(sh, dh, scale_y));
    }
    Plan::Generic(generic_tab(sw, dw, inv_sx, interp, false), generic_tab(sh, dh, inv_sy, interp, true))
}

fn resize_u8(src: &[u8], sw: usize, sh: usize, cn: usize, dw: usize, dh: usize, inv_sx: f64, inv_sy: f64, interp: Interp) -> Vec<u8> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return vec![0; dw * dh * cn];
    }
    match plan(sw, sh, dw, dh, inv_sx, inv_sy, interp) {
        Plan::Copy => src.to_vec(),
        Plan::Nearest(fx, fy) => nearest(cn, src, sw, sh, dw, dh, fx, fy),
        Plan::Area(xt, yt) => {
            let f: Vec<f32> = src.iter().map(|&v| v as f32).collect();
            apply_tabs_f(&f, sw, sh, cn, &xt, &yt).into_iter().map(sat_u8).collect()
        }
        // OpenCV 5 resamples 8-bit cubic in float (unquantised coefficients); linear / area-upscale keep the
        // fixed-point path (within +-1 of OpenCV; only used for cosmetic crops and odd-height refine frames)
        Plan::Generic(xt, yt) if interp == Interp::Cubic => {
            let f: Vec<f32> = src.iter().map(|&v| v as f32).collect();
            apply_tabs_f(&f, sw, sh, cn, &xt, &yt).into_iter().map(sat_u8).collect()
        }
        Plan::Generic(xt, yt) => apply_tabs_fixed(src, sw, sh, cn, &xt, &yt),
    }
}

fn resize_f32(src: &[f32], sw: usize, sh: usize, cn: usize, dw: usize, dh: usize, inv_sx: f64, inv_sy: f64, interp: Interp) -> Vec<f32> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return vec![0.0; dw * dh * cn];
    }
    match plan(sw, sh, dw, dh, inv_sx, inv_sy, interp) {
        Plan::Copy => src.to_vec(),
        Plan::Nearest(fx, fy) => {
            let xs: Vec<usize> = (0..dw).map(|x| ((x as f64 * fx).floor() as usize).min(sw - 1)).collect();
            let mut out = Vec::with_capacity(dw * dh * cn);
            for y in 0..dh {
                let sy = ((y as f64 * fy).floor() as usize).min(sh - 1);
                for &sx in &xs {
                    out.extend_from_slice(&src[(sy * sw + sx) * cn..(sy * sw + sx) * cn + cn]);
                }
            }
            out
        }
        Plan::Area(xt, yt) | Plan::Generic(xt, yt) => apply_tabs_f(src, sw, sh, cn, &xt, &yt),
    }
}

/// `cv2.resize(img, (dw, dh), interpolation=...)`.
pub fn resize(img: &BgrImage, dw: usize, dh: usize, interp: Interp) -> BgrImage {
    let (isx, isy) = (dw as f64 / img.w.max(1) as f64, dh as f64 / img.h.max(1) as f64);
    BgrImage { w: dw, h: dh, data: resize_u8(&img.data, img.w, img.h, 3, dw, dh, isx, isy, interp) }
}

/// `cv2.resize(img, None, fx=f, fy=f, interpolation=...)`: size = round(w*f), and the mapping uses `f` itself.
pub fn resize_by(img: &BgrImage, fx: f64, fy: f64, interp: Interp) -> BgrImage {
    let dw = (img.w as f64 * fx).round_ties_even().max(0.0) as usize;
    let dh = (img.h as f64 * fy).round_ties_even().max(0.0) as usize;
    BgrImage { w: dw, h: dh, data: resize_u8(&img.data, img.w, img.h, 3, dw, dh, fx, fy, interp) }
}

pub fn resize_gray(img: &Gray, dw: usize, dh: usize, interp: Interp) -> Gray {
    let (isx, isy) = (dw as f64 / img.w.max(1) as f64, dh as f64 / img.h.max(1) as f64);
    Plane { w: dw, h: dh, data: resize_u8(&img.data, img.w, img.h, 1, dw, dh, isx, isy, interp) }
}

pub fn resize_f(img: &PlaneF, dw: usize, dh: usize, interp: Interp) -> PlaneF {
    let (isx, isy) = (dw as f64 / img.w.max(1) as f64, dh as f64 / img.h.max(1) as f64);
    Plane { w: dw, h: dh, data: resize_f32(&img.data, img.w, img.h, 1, dw, dh, isx, isy, interp) }
}

// ---------------------------------------------------------------- filters
/// Reflect-101 index (OpenCV's default border): -1 -> 1, n -> n-2.
#[inline]
fn reflect101(i: i64, n: usize) -> usize {
    let n = n as i64;
    if n == 1 {
        return 0;
    }
    let mut i = i;
    loop {
        if i < 0 {
            i = -i;
        } else if i >= n {
            i = 2 * n - 2 - i;
        } else {
            return i as usize;
        }
    }
}

/// `cv::getGaussianKernel(n, sigma)` (f64).
pub fn gaussian_kernel(n: usize, sigma: f64) -> Vec<f64> {
    if sigma <= 0.0 {
        match n {
            1 => return vec![1.0],
            3 => return vec![0.25, 0.5, 0.25],
            5 => return vec![0.0625, 0.25, 0.375, 0.25, 0.0625],
            7 => return vec![0.03125, 0.109375, 0.21875, 0.28125, 0.21875, 0.109375, 0.03125],
            _ => {}
        }
    }
    let s = if sigma > 0.0 { sigma } else { ((n as f64 - 1.0) * 0.5 - 1.0) * 0.3 + 0.8 };
    let scale2x = -0.5 / (s * s);
    let mut k: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - (n as f64 - 1.0) * 0.5;
            (scale2x * x * x).exp()
        })
        .collect();
    let sum: f64 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    k
}

/// Separable filter of an interleaved buffer (f32 accumulation, reflect-101 borders).
fn sep_filter(src: &[f32], w: usize, h: usize, cn: usize, kx: &[f64], ky: &[f64]) -> Vec<f32> {
    let (rx, ry) = ((kx.len() / 2) as i64, (ky.len() / 2) as i64);
    let mut tmp = vec![0f32; w * h * cn];
    for y in 0..h {
        for x in 0..w {
            for c in 0..cn {
                let mut s = 0f64;
                for (k, &kv) in kx.iter().enumerate() {
                    let sx = reflect101(x as i64 + k as i64 - rx, w);
                    s += src[(y * w + sx) * cn + c] as f64 * kv;
                }
                tmp[(y * w + x) * cn + c] = s as f32;
            }
        }
    }
    let mut out = vec![0f32; w * h * cn];
    for y in 0..h {
        for (k, &kv) in ky.iter().enumerate() {
            let sy = reflect101(y as i64 + k as i64 - ry, h);
            let trow = &tmp[sy * w * cn..(sy + 1) * w * cn];
            let orow = &mut out[y * w * cn..(y + 1) * w * cn];
            for (o, &t) in orow.iter_mut().zip(trow) {
                *o += (t as f64 * kv) as f32;
            }
        }
    }
    out
}

fn gauss_ksize(sigma: f64, is_u8: bool) -> usize {
    let n = (sigma * if is_u8 { 3.0 } else { 4.0 } * 2.0 + 1.0).round_ties_even() as i64 | 1;
    n.max(1) as usize
}

/// `cv2.GaussianBlur(img, (k, k), 0)` on a float plane.
pub fn gaussian_blur_f(img: &PlaneF, ksize: usize, sigma: f64) -> PlaneF {
    if img.is_empty() {
        return img.clone();
    }
    let n = if ksize == 0 { gauss_ksize(sigma, false) } else { ksize };
    let k = gaussian_kernel(n, sigma);
    Plane { w: img.w, h: img.h, data: sep_filter(&img.data, img.w, img.h, 1, &k, &k) }
}

/// `cv2.GaussianBlur(img, (0, 0), sigma)` on a BGR image.
pub fn gaussian_blur_bgr(img: &BgrImage, ksize: usize, sigma: f64) -> BgrImage {
    if img.is_empty() {
        return img.clone();
    }
    let n = if ksize == 0 { gauss_ksize(sigma, true) } else { ksize };
    let k = gaussian_kernel(n, sigma);
    let f: Vec<f32> = img.data.iter().map(|&v| v as f32).collect();
    let out = sep_filter(&f, img.w, img.h, 3, &k, &k);
    BgrImage { w: img.w, h: img.h, data: out.into_iter().map(sat_u8).collect() }
}

/// `cv2.blur(plane, (kw, 1))`: horizontal normalised box filter, reflect-101 borders.
pub fn box_blur_h(img: &Gray, kw: usize) -> Gray {
    let r = (kw / 2) as i64;
    let scale = 1.0 / kw as f64;
    let mut out = vec![0u8; img.data.len()];
    for y in 0..img.h {
        let row = img.row(y);
        for x in 0..img.w {
            let mut s = 0u32;
            for k in 0..kw as i64 {
                s += row[reflect101(x as i64 + k - r, img.w)] as u32;
            }
            out[y * img.w + x] = sat_u8_f64(s as f64 * scale);
        }
    }
    Plane { w: img.w, h: img.h, data: out }
}

// ---------------------------------------------------------------- morphology (rectangular kernels)
/// Max (dilate) / min (erode) over a kw x kh window anchored at the kernel centre; out-of-image pixels are
/// ignored (OpenCV's default morphology border).
fn morph(img: &Gray, kw: usize, kh: usize, dilate: bool) -> Gray {
    let (ax, ay) = ((kw / 2) as i64, (kh / 2) as i64);
    let (w, h) = (img.w as i64, img.h as i64);
    // horizontal pass
    let mut tmp = vec![0u8; img.data.len()];
    for y in 0..img.h {
        let row = img.row(y);
        for x in 0..w {
            let a = (x - ax).max(0) as usize;
            let b = ((x - ax + kw as i64).min(w)) as usize;
            let it = row[a..b.max(a)].iter().copied();
            tmp[y * img.w + x as usize] = if dilate { it.max().unwrap_or(0) } else { it.min().unwrap_or(255) };
        }
    }
    if kh == 1 {
        return Plane { w: img.w, h: img.h, data: tmp };
    }
    let mut out = vec![0u8; img.data.len()];
    for y in 0..h {
        let a = (y - ay).max(0);
        let b = (y - ay + kh as i64).min(h);
        for x in 0..img.w {
            let it = (a..b).map(|yy| tmp[yy as usize * img.w + x]);
            out[y as usize * img.w + x] = if dilate { it.max().unwrap_or(0) } else { it.min().unwrap_or(255) };
        }
    }
    Plane { w: img.w, h: img.h, data: out }
}

pub fn dilate(img: &Gray, kw: usize, kh: usize) -> Gray {
    morph(img, kw, kh, true)
}
pub fn erode(img: &Gray, kw: usize, kh: usize) -> Gray {
    morph(img, kw, kh, false)
}
/// MORPH_CLOSE = dilate then erode.
pub fn close(img: &Gray, kw: usize, kh: usize) -> Gray {
    erode(&dilate(img, kw, kh), kw, kh)
}
/// MORPH_OPEN = erode then dilate.
pub fn open(img: &Gray, kw: usize, kh: usize) -> Gray {
    dilate(&erode(img, kw, kh), kw, kh)
}

// ---------------------------------------------------------------- correlation
/// `cv2.matchTemplate(img, tpl, TM_CCOEFF_NORMED)` -> result plane ((W-w+1) x (H-h+1)).
pub fn match_template_ccoeff_normed(img: &PlaneF, tpl: &PlaneF) -> PlaneF {
    if tpl.w > img.w || tpl.h > img.h || tpl.is_empty() {
        return Plane::new(0, 0);
    }
    let (rw, rh) = (img.w - tpl.w + 1, img.h - tpl.h + 1);
    let n = (tpl.w * tpl.h) as f64;
    let tmean = tpl.data.iter().map(|&v| v as f64).sum::<f64>() / n;
    let tz: Vec<f64> = tpl.data.iter().map(|&v| v as f64 - tmean).collect();
    let tnorm2: f64 = tz.iter().map(|v| v * v).sum();
    if tnorm2 < f64::EPSILON {
        return Plane { w: rw, h: rh, data: vec![1.0; rw * rh] };
    }
    let tnorm = tnorm2.sqrt();
    let mut out = vec![0f32; rw * rh];
    for y in 0..rh {
        for x in 0..rw {
            let (mut num, mut s1, mut s2) = (0f64, 0f64, 0f64);
            for ty in 0..tpl.h {
                let irow = &img.data[(y + ty) * img.w + x..(y + ty) * img.w + x + tpl.w];
                let trow = &tz[ty * tpl.w..(ty + 1) * tpl.w];
                for (&i, &t) in irow.iter().zip(trow) {
                    let i = i as f64;
                    num += i * t;
                    s1 += i;
                    s2 += i * i;
                }
            }
            let diff2 = (s2 - s1 * s1 / n).max(0.0);
            let t = if diff2 <= 0.5f64.min(10.0 * f32::EPSILON as f64 * s2) { 0.0 } else { diff2.sqrt() * tnorm };
            let r = if num.abs() < t {
                num / t
            } else if num.abs() < t * 1.125 {
                num.signum()
            } else {
                0.0
            };
            out[y * rw + x] = r as f32;
        }
    }
    Plane { w: rw, h: rh, data: out }
}

/// Normalised cross-correlation of two equally sized planes (mean-removed).
pub fn ncc(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let ma = a.iter().map(|&v| v as f64).sum::<f64>() / a.len() as f64;
    let mb = b.iter().map(|&v| v as f64).sum::<f64>() / b.len() as f64;
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = (x as f64 - ma, y as f64 - mb);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    let d = aa.sqrt() * bb.sqrt();
    if d > 1e-6 {
        ab / d
    } else {
        0.0
    }
}

// ---------------------------------------------------------------- geometry / stats
pub fn flip_h(img: &BgrImage) -> BgrImage {
    let mut out = Vec::with_capacity(img.data.len());
    for y in 0..img.h {
        for x in (0..img.w).rev() {
            let i = (y * img.w + x) * 3;
            out.extend_from_slice(&img.data[i..i + 3]);
        }
    }
    BgrImage { w: img.w, h: img.h, data: out }
}

/// Images side by side (equal heights).
pub fn hstack(imgs: &[&BgrImage]) -> BgrImage {
    let h = imgs.first().map(|i| i.h).unwrap_or(0);
    let w: usize = imgs.iter().map(|i| i.w).sum();
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for im in imgs {
            out.extend_from_slice(&im.data[y * im.w * 3..(y + 1) * im.w * 3]);
        }
    }
    BgrImage { w, h, data: out }
}

/// Images on top of each other (equal widths).
pub fn vstack(imgs: &[&BgrImage]) -> BgrImage {
    let w = imgs.first().map(|i| i.w).unwrap_or(0);
    let mut out = vec![];
    let mut h = 0;
    for im in imgs {
        out.extend_from_slice(&im.data);
        h += im.h;
    }
    BgrImage { w, h, data: out }
}

/// numpy's float32 pairwise summation (`pairwise_sum_FLOAT`, block 128, 8 accumulators).
fn np_pairwise_f32(a: &[f32]) -> f32 {
    let n = a.len();
    if n < 8 {
        let mut res = 0f32;
        for &v in a {
            res += v;
        }
        res
    } else if n <= 128 {
        let mut r = [0f32; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        np_pairwise_f32(&a[..n2]) + np_pairwise_f32(&a[n2..])
    }
}

/// `arr.mean()` of a contiguous float32 numpy array (first element + pairwise sum of the rest, / n in float32).
pub fn np_mean_f32(a: &[f32]) -> f32 {
    if a.is_empty() {
        return f32::NAN;
    }
    (a[0] + np_pairwise_f32(&a[1..])) / a.len() as f32
}

pub fn mean_u8(v: &[u8]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().map(|&x| x as u64).sum::<u64>() as f64 / v.len() as f64
}

/// Population standard deviation (numpy `.std()`).
pub fn std_u8(v: &[u8]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let m = mean_u8(v);
    (v.iter().map(|&x| (x as f64 - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

pub fn mean_f(v: &[f32]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_matches_opencv() {
        // values from cv2.cvtColor(np.uint8([[[b, g, r]]]), cv2.COLOR_BGR2HSV)
        assert_eq!(hsv_px(255, 0, 0), (120, 255, 255));
        assert_eq!(hsv_px(0, 0, 255), (0, 255, 255));
        assert_eq!(hsv_px(200, 180, 30), (94, 217, 200));
        assert_eq!(hsv_px(37, 201, 90), (50, 208, 201));
        assert_eq!(gray_px(37, 201, 90), 149);
        assert_eq!(hsv_px(10, 20, 30), (15, 170, 30));
        assert_eq!(gray_px(10, 20, 30), 22);
    }

    const A: [u8; 63] = [
        255, 228, 34, 121, 243, 189, 6, 131, 102, 168, 82, 193, 187, 150, 81, 243, 205, 24, 236, 8, 246, 164, 231, 36, 210, 111, 172, 210,
        174, 176, 218, 242, 169, 114, 205, 63, 160, 47, 212, 79, 72, 112, 120, 222, 69, 31, 95, 108, 36, 109, 238, 69, 51, 81, 228, 211,
        202, 58, 202, 65, 76, 70, 193,
    ];

    fn diff(a: &[u8], b: &[u8]) -> i32 {
        a.iter().zip(b).map(|(&x, &y)| (x as i32 - y as i32).abs()).max().unwrap()
    }

    #[test]
    fn resize_matches_opencv() {
        let a = Plane::from_raw(9, 7, A.to_vec());
        let cubic: Vec<u8> = vec![
            255, 255, 190, 28, 49, 167, 251, 244, 104, 0, 90, 131, 108, 214, 194, 149, 102, 124, 173, 194, 164, 124, 118, 172, 124, 45,
            177, 100, 86, 198, 209, 180, 151, 72, 134, 255, 236, 119, 17, 236, 77, 34, 235, 209, 178, 206, 58, 95, 242, 155, 124, 145, 252,
            128, 82, 216, 206, 212, 255, 124, 99, 175, 142, 136, 152, 213, 190, 169, 174, 200, 233, 242, 197, 134, 118, 199, 149, 51, 198,
            128, 112, 203, 170, 125, 128, 127, 114, 125, 234, 170, 44, 120, 60, 67, 183, 111, 35, 59, 124, 135, 111, 176, 136, 59, 25, 56,
            98, 113, 59, 40, 100, 220, 192, 64, 58, 67, 75, 126, 149, 167, 157, 70, 67, 163, 171, 122, 63, 39, 89, 152, 248, 234, 221, 219,
            99, 95, 210, 100, 42, 77, 60, 131, 214,
        ];
        assert_eq!(diff(&resize_gray(&a, 13, 11, Interp::Cubic).data, &cubic), 0);
        let area: Vec<u8> = vec![194, 125, 178, 125, 110, 151, 190, 182, 148, 134, 127, 151, 127, 132, 124, 151, 124, 132, 94, 108];
        assert_eq!(diff(&resize_gray(&a, 5, 4, Interp::Area).data, &area), 0);
        let area_up: Vec<u8> = vec![
            243, 56, 216, 67, 115, 227, 73, 208, 84, 113, 130, 181, 158, 189, 104, 131, 192, 175, 178, 116, 135, 220, 216, 152, 145, 160,
            201, 225, 143, 137, 194, 176, 237, 132, 126, 146, 186, 143, 124, 132, 110, 194, 74, 117, 137, 74, 131, 87, 123, 87, 59, 107,
            93, 125, 68, 197, 189, 159, 80, 128, 220, 203, 170, 72, 138,
        ];
        assert!(diff(&resize_gray(&a, 5, 13, Interp::Area).data, &area_up) <= 1);
        let lin: Vec<u8> = vec![
            237, 213, 149, 74, 126, 191, 203, 153, 68, 111, 123, 86, 209, 102, 108, 218, 180, 189, 144, 75, 202, 176, 135, 113, 210, 188,
            175, 181, 213, 233, 215, 162, 121, 171, 152, 63, 108, 82, 105, 157, 75, 77, 115, 154, 107, 133, 124, 74, 188, 188, 186, 167,
            70, 135, 152, 96, 78, 69, 105, 170,
        ];
        assert!(diff(&resize_gray(&a, 12, 5, Interp::Linear).data, &lin) <= 1);
        let bgr = BgrImage::from_raw(9, 7, A.iter().flat_map(|&v| [v, v, v]).collect());
        let r = resize_by(&bgr, 1.7, 1.7, Interp::Cubic);
        assert_eq!((r.w, r.h), (15, 12));
    }

    #[test]
    fn blurs_match_opencv() {
        let a = Plane::from_raw(9, 7, A.to_vec());
        let bx: Vec<u8> = vec![
            156, 173, 176, 163, 119, 138, 134, 112, 75, 144, 142, 156, 139, 171, 173, 141, 152, 184, 149, 132, 177, 137, 177, 150, 152,
            128, 163, 182, 190, 204, 196, 184, 190, 159, 151, 140, 136, 109, 114, 104, 119, 121, 119, 149, 151, 87, 73, 76, 117, 112, 101,
            110, 98, 64, 211, 182, 180, 148, 121, 94, 121, 95, 97,
        ];
        assert_eq!(box_blur_h(&a, 5).data, bx);
        let f = a.map(|v| v as f32 / 255.0);
        let g = gaussian_blur_f(&f, 3, 0.0);
        let want =
            [0.718_627_45_f32, 0.622_549_06, 0.525_490_16, 0.605_882_35, 0.668_627_5, 0.579_411_8, 0.541_176_5, 0.513_235_33, 0.452_941_18];
        for (x, w) in g.data[..9].iter().zip(want) {
            assert!((x - w).abs() < 1e-5);
        }
        let gb: Vec<u8> = vec![
            154, 153, 155, 160, 158, 151, 145, 138, 135, 152, 153, 157, 163, 161, 153, 146, 141, 138, 148, 152, 161, 167, 162, 153, 148,
            144, 142, 142, 146, 153, 158, 154, 147, 143, 140, 138, 132, 135, 137, 138, 137, 135, 131, 126, 125, 128, 128, 125, 122, 124,
            124, 117, 111, 109, 128, 127, 122, 118, 119, 119, 111, 103, 101,
        ];
        let bgr = BgrImage::from_raw(9, 7, A.iter().flat_map(|&v| [v, v, v]).collect());
        let gg: Vec<u8> = gaussian_blur_bgr(&bgr, 0, 1.3).data.iter().step_by(3).copied().collect();
        assert!(diff(&gg, &gb) <= 1);
    }

    #[test]
    fn area_resize_integer_average() {
        let g = Plane::from_raw(4, 2, vec![0u8, 10, 20, 30, 40, 50, 60, 70]);
        let r = resize_gray(&g, 2, 1, Interp::Area);
        assert_eq!(r.data, vec![25, 45]);
    }

    #[test]
    fn morph_close_bridges_gaps() {
        let g = Plane::from_raw(7, 1, vec![1u8, 0, 0, 1, 0, 0, 0]);
        assert_eq!(close(&g, 3, 1).data, vec![1, 1, 1, 1, 0, 0, 0]);
        let g = Plane::from_raw(7, 1, vec![0u8, 1, 0, 0, 0, 1, 1]);
        assert_eq!(close(&g, 3, 1).data, vec![1, 1, 0, 0, 0, 1, 1]);
        assert_eq!(close(&g, 5, 1).data, vec![1; 7]);
        assert_eq!(open(&g, 3, 1).data, vec![0, 0, 0, 0, 0, 1, 1]);
    }

    #[test]
    fn template_finds_itself() {
        let mut img = PlaneF::new(20, 20);
        for y in 5..10 {
            for x in 7..9 {
                img.data[y * 20 + x] = 1.0;
            }
        }
        let tpl = img.crop(4, 4, 7, 7);
        let r = match_template_ccoeff_normed(&img, &tpl);
        let (mut best, mut at) = (-2.0, 0);
        for (i, &v) in r.data.iter().enumerate() {
            if v > best {
                best = v;
                at = i;
            }
        }
        assert!((best - 1.0).abs() < 1e-5);
        assert_eq!(at, 4 * r.w + 4);
    }
}
