//! Kill feed row segmentation and parsing.
//!
//! A kill feed row is a right-aligned horizontal bar made of two coloured halves: killer half (left) in the
//! killer's team colour and victim half (right) in the victim's team colour. Colours are relative to the
//! *watched* player: teal = watched player's team, red = enemy team.
//!
//! ```text
//! [assist icons][killer portrait] killer name  WEAPON [mods] > victim name [victim portrait]
//! ```
use crate::imgproc::{self, Hsv, Interp, Plane, PlaneF};
use crate::layout::{round_half_even, Layout};
use std::sync::OnceLock;
use vl_core::image::BgrImage;

pub const TEAL: &str = "teal";
pub const RED: &str = "red";

#[derive(Debug, Clone, PartialEq)]
pub struct RowBox {
    pub y0: i64,
    pub y1: i64,
    pub x0: i64,
    pub x1: i64,
    /// x where killer half ends / victim half begins (ROI coords)
    pub split: i64,
    pub killer_color: &'static str,
    pub victim_color: &'static str,
    /// local-player outline (live footage)
    pub gold: bool,
}

impl RowBox {
    pub fn h(&self) -> i64 {
        self.y1 - self.y0
    }
    pub fn w(&self) -> i64 {
        self.x1 - self.x0
    }
}

/// Truncation toward zero of a non-negative-ish float (truncation toward zero).
#[inline]
fn ti(v: f64) -> i64 {
    v.trunc() as i64
}

// ---------------------------------------------------------------- colour masks (0/1 planes)
#[inline]
fn teal_px(h: u8, s: u8, v: u8, strict: bool) -> bool {
    if strict {
        (74..=94).contains(&h) && s >= 85 && v >= 160
    } else {
        // lenient teal also spans the gold->teal gradient of the local player's own rows
        (((72..=96).contains(&h) && s >= 55) || ((18..72).contains(&h) && s >= 80)) && v >= 110
    }
}

#[inline]
fn red_px(h: u8, s: u8, v: u8, strict: bool) -> bool {
    if strict {
        (h <= 5 || h >= 174) && s >= 120 && v >= 160
    } else {
        (h <= 7 || h >= 172) && s >= 85 && v >= 110
    }
}

/// Teal / red kill feed colours. `strict` rejects dull scenery (orange walls etc.).
pub fn color_masks_hsv(hsv: &Hsv, strict: bool) -> (Plane<u8>, Plane<u8>) {
    let n = hsv.len();
    let (mut t, mut r) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        let (h, s, v) = (hsv.hue[i], hsv.sat[i], hsv.val[i]);
        t.push(teal_px(h, s, v, strict) as u8);
        r.push(red_px(h, s, v, strict) as u8);
    }
    (Plane::from_raw(hsv.w, hsv.h, t), Plane::from_raw(hsv.w, hsv.h, r))
}

pub fn color_masks(bgr: &BgrImage, strict: bool) -> (Plane<u8>, Plane<u8>) {
    color_masks_hsv(&imgproc::bgr2hsv(bgr), strict)
}

pub fn white_mask_hsv(hsv: &Hsv, soft: bool) -> Plane<u8> {
    if !soft {
        let d = hsv.sat.iter().zip(&hsv.val).map(|(&s, &v)| (s <= 60 && v >= 200) as u8).collect();
        return Plane::from_raw(hsv.w, hsv.h, d);
    }
    // Low-res video: thin white text comes out grey on teal and pink on red (4:2:0 colour bleeds into it).
    // "White" = clearly brighter than the row right around it, and not strongly coloured.
    let k = 9usize.max((hsv.h * 2) | 1);
    let vp = Plane::from_raw(hsv.w, hsv.h, hsv.val.clone());
    let bg = imgproc::box_blur_h(&vp, k);
    let d = (0..hsv.len())
        .map(|i| {
            let v = hsv.val[i] as i16;
            (v >= 180 && v >= bg.data[i] as i16 + 20 && hsv.sat[i] <= 130) as u8
        })
        .collect();
    Plane::from_raw(hsv.w, hsv.h, d)
}

pub fn white_mask(bgr: &BgrImage, soft: bool) -> Plane<u8> {
    white_mask_hsv(&imgproc::bgr2hsv(bgr), soft)
}

pub fn gold_mask_hsv(hsv: &Hsv) -> Plane<u8> {
    let d = (0..hsv.len()).map(|i| ((20..=38).contains(&hsv.hue[i]) && hsv.sat[i] >= 75 && hsv.val[i] >= 170) as u8).collect();
    Plane::from_raw(hsv.w, hsv.h, d)
}

/// Sub-rectangle of an HSV image with numpy slice semantics `[y0:y1, x0:x1]` (clamped).
pub fn hsv_slice(hsv: &Hsv, y0: i64, y1: i64, x0: i64, x1: i64) -> Hsv {
    let (y0, x0) = (y0.clamp(0, hsv.h as i64) as usize, x0.clamp(0, hsv.w as i64) as usize);
    let (y1, x1) = (y1.clamp(0, hsv.h as i64) as usize, x1.clamp(0, hsv.w as i64) as usize);
    let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let mut out = Hsv { w, h, hue: Vec::with_capacity(w * h), sat: Vec::with_capacity(w * h), val: Vec::with_capacity(w * h) };
    for y in y0..y0 + h {
        let a = y * hsv.w + x0;
        out.hue.extend_from_slice(&hsv.hue[a..a + w]);
        out.sat.extend_from_slice(&hsv.sat[a..a + w]);
        out.val.extend_from_slice(&hsv.val[a..a + w]);
    }
    out
}

/// Contiguous true runs -> (start, end_exclusive).
pub fn runs(b: &[bool]) -> Vec<(i64, i64)> {
    let mut out = vec![];
    let mut start: Option<usize> = None;
    for (i, &v) in b.iter().enumerate() {
        match (v, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s as i64, i as i64));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s as i64, b.len() as i64));
    }
    out
}

/// Mean over rows [y0, y1) of each column in [x0, x1) of a 0/1 plane (clamped like numpy).
fn col_means(m: &Plane<u8>, y0: i64, y1: i64, x0: i64, x1: i64) -> Vec<f64> {
    let (y0, y1) = (y0.clamp(0, m.h as i64) as usize, y1.clamp(0, m.h as i64) as usize);
    let (x0, x1) = (x0.clamp(0, m.w as i64) as usize, x1.clamp(0, m.w as i64) as usize);
    let n = y1.saturating_sub(y0);
    let mut acc = vec![0u32; x1.saturating_sub(x0)];
    for y in y0..y1 {
        let row = &m.data[y * m.w + x0..y * m.w + x1.max(x0)];
        for (a, &v) in acc.iter_mut().zip(row) {
            *a += v as u32;
        }
    }
    acc.into_iter().map(|a| if n == 0 { f64::NAN } else { a as f64 / n as f64 }).collect()
}

/// Mean over columns [x0, x1) of each row of a 0/1 plane.
fn row_means(m: &Plane<u8>, x0: i64, x1: i64) -> Vec<f64> {
    let (x0, x1) = (x0.clamp(0, m.w as i64) as usize, x1.clamp(0, m.w as i64) as usize);
    let n = x1.saturating_sub(x0);
    (0..m.h)
        .map(|y| {
            let s: u32 = m.data[y * m.w + x0..y * m.w + x1.max(x0)].iter().map(|&v| v as u32).sum();
            if n == 0 {
                f64::NAN
            } else {
                s as f64 / n as f64
            }
        })
        .collect()
}

fn or_planes(a: &Plane<u8>, b: &Plane<u8>) -> Plane<u8> {
    Plane::from_raw(a.w, a.h, a.data.iter().zip(&b.data).map(|(&x, &y)| x | y).collect())
}

/// Per-frame kill feed context: the ROI and its HSV planes (computed once).
pub struct KfFrame<'a> {
    pub roi: &'a BgrImage,
    pub hsv: Hsv,
}

impl<'a> KfFrame<'a> {
    pub fn new(roi: &'a BgrImage) -> Self {
        KfFrame { roi, hsv: imgproc::bgr2hsv(roi) }
    }
}

pub fn segment_rows(roi: &BgrImage, lay: &Layout) -> Vec<RowBox> {
    segment_rows_ctx(&KfFrame::new(roi), lay)
}

pub fn segment_rows_ctx(f: &KfFrame, lay: &Layout) -> Vec<RowBox> {
    let (w, _h) = (f.hsv.w as i64, f.hsv.h as i64);
    let u = |v: f64| lay.u(v);
    let (teal, red) = color_masks_hsv(&f.hsv, false);
    let (st, sr) = color_masks_hsv(&f.hsv, true);
    let colored = or_planes(&teal, &red);
    let k = 3.max(ti(u(0.035)) | 1) as usize;
    let strict_closed = imgproc::close(&or_planes(&st, &sr), k, 1);
    let closed = imgproc::close(&colored, k, 1);

    let right_anchor = ti(w as f64 - u(0.018));
    // Victim name area: always just left of the victim portrait at the right end.
    let band_l = 0.max(ti(right_anchor as f64 - u(0.20)));
    let band_r = (band_l + 1).max(ti(right_anchor as f64 - u(0.05)));
    let density = row_means(&closed, band_l, band_r);
    let strict_density = row_means(&strict_closed, band_l, band_r);
    let good: Vec<bool> = density.iter().map(|&d| d >= 0.45).collect();

    let row_h = u(0.031);
    let mut rows = vec![];
    for (y0, y1) in runs(&good) {
        let mut bands = vec![(y0, y1)];
        if (y1 - y0) as f64 > row_h * 1.45 {
            // merged rows: split on the pitch grid
            let n = 1.max(round_half_even((y1 - y0) as f64 / u(0.036)));
            let step = (y1 - y0) as f64 / n as f64;
            bands = (0..n).map(|i| (ti(y0 as f64 + i as f64 * step), ti(y0 as f64 + (i + 1) as f64 * step))).collect();
        }
        for (a, b) in bands {
            if ((b - a) as f64) < row_h * 0.55 || (b - a) as f64 > row_h * 1.45 {
                continue;
            }
            let smax = strict_density[a as usize..b as usize].iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            if smax < 0.40 {
                continue; // dull scenery, not a kill feed bar
            }
            let Some((x0, x1)) = row_extent(&closed, a, b, right_anchor, lay) else { continue };
            let x1 = x1.min(right_anchor); // rows are right-aligned; ignore background past the bar
            if let Some(bx) = classify_row(f, &teal, &red, a, b, x0, x1, lay) {
                rows.push(bx);
            }
        }
    }
    rows
}

/// Horizontal extent of the coloured bar in a row band (bridging portraits/icons).
fn row_extent(closed: &Plane<u8>, a: i64, b: i64, right_anchor: i64, lay: &Layout) -> Option<(i64, i64)> {
    let cov: Vec<u8> = col_means(closed, a, b, 0, closed.w as i64).iter().map(|&m| (m >= 0.25) as u8).collect();
    let k = 3.max(ti(lay.u(0.07)) | 1) as usize;
    let cov = imgproc::close(&Plane::from_raw(cov.len(), 1, cov), k, 1);
    let cb: Vec<bool> = cov.data.iter().map(|&v| v != 0).collect();
    let rs = runs(&cb);
    // the run that reaches closest to the right anchor
    let &(x0, x1) = rs.iter().min_by_key(|r| (right_anchor - r.1).abs())?;
    if (right_anchor - x1).abs() as f64 > lay.u(0.09) || ((x1 - x0) as f64) < lay.u(0.17) {
        return None;
    }
    Some((x0, x1))
}

/// Colours and split are read right-to-left from the fixed right anchor.
#[allow(clippy::too_many_arguments)]
fn classify_row(f: &KfFrame, teal: &Plane<u8>, red: &Plane<u8>, y0: i64, y1: i64, x0: i64, x1: i64, lay: &Layout) -> Option<RowBox> {
    let u = |v: f64| lay.u(v);
    let ym0 = y0 + (y1 - y0) / 4;
    let ym1 = y1 - (y1 - y0) / 4;
    let t_col = col_means(teal, ym0, ym1, x0, x1 + 1);
    let r_col = col_means(red, ym0, ym1, x0, x1 + 1);
    let lab: Vec<i32> = t_col
        .iter()
        .zip(&r_col)
        .map(|(&t, &r)| {
            if t + r > 0.3 {
                if t > r {
                    1
                } else {
                    -1
                }
            } else {
                0
            }
        })
        .collect();
    let n = lab.len() as i64;
    let portrait = ti(u(0.05));
    let (va, vb) = (0.max(n - portrait - ti(u(0.06))), 1.max(n - portrait));
    let vz: &[i32] = if va < vb.min(n) { &lab[va as usize..vb.min(n) as usize] } else { &[] };
    let nz = vz.iter().filter(|&&v| v != 0).count();
    if (nz as f64) < 3f64.max(vz.len() as f64 * 0.3) {
        return None;
    }
    let vc = if vz.iter().sum::<i32>() >= 0 { 1 } else { -1 };
    // text + weapon must be present (rejects plain coloured scenery)
    let wh = hsv_slice(&f.hsv, y0, y1, x0.max(x1 - ti(u(0.35))), x1 + 1);
    let wf = white_mask_hsv(&wh, lay.soft).frac();
    if wf < 0.03 || wf > 0.6 {
        return None;
    }
    // scan leftwards for the first solid block of the opposite colour (the killer half)
    let win = 3.max(ti(u(0.012)));
    let start = n - portrait - ti(u(0.02));
    let limit = 0.max(n - ti(u(0.55)));
    let mut split_rel = None;
    let mut x = start;
    while x > limit + win {
        let (a, b) = ((x - win).max(0), x.min(n));
        if a < b {
            let seg = &lab[a as usize..b as usize];
            if seg.iter().filter(|&&v| v == -vc).count() as f64 / seg.len() as f64 >= 0.7 {
                split_rel = Some(x);
                break;
            }
        }
        x -= 1;
    }
    let victim = if vc == 1 { TEAL } else { RED };
    let (killer, split) = match split_rel {
        // same colour on both sides: team kill / unusual row; split heuristically
        None => (victim, x0 + ti((x1 - x0) as f64 * 0.55)),
        Some(s) => (if vc == 1 { RED } else { TEAL }, x0 + s),
    };
    // Local player's own rows: gold -> teal gradient on the killer half (+ gold outline).
    let kh = hsv_slice(&f.hsv, ym0, ym1, 0.max(split - ti(u(0.30))), split);
    let gold = !kh.is_empty() && gold_mask_hsv(&kh).frac() > 0.02;
    Some(RowBox { y0, y1, x0, x1, split, killer_color: killer, victim_color: victim, gold })
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowParts {
    /// x-range in ROI coords
    pub killer_name: Option<(i64, i64)>,
    pub weapon: Option<(i64, i64)>,
    /// icons between weapon and split (headshot, wallbang...)
    pub mods: Option<(i64, i64)>,
    pub victim_name: Option<(i64, i64)>,
    pub killer_portrait: Option<(i64, i64)>,
    pub victim_portrait: (i64, i64),
}

fn merge_runs(rs: &[(i64, i64)], gap: f64) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = vec![];
    for &(a, b) in rs {
        match out.last_mut() {
            Some(l) if (a - l.1) as f64 <= gap => l.1 = b,
            _ => out.push((a, b)),
        }
    }
    out
}

pub fn parse_row(roi: &BgrImage, r: &RowBox, lay: &Layout) -> RowParts {
    parse_row_ctx(&KfFrame::new(roi), r, lay)
}

pub fn parse_row_ctx(f: &KfFrame, r: &RowBox, lay: &Layout) -> RowParts {
    let u = |v: f64| lay.u(v);
    let h = r.y1 - r.y0;
    let band = hsv_slice(&f.hsv, r.y0 + ti(h as f64 * 0.18), r.y1 - ti(h as f64 * 0.18), 0, f.hsv.w as i64);
    let wm = white_mask_hsv(&band, lay.soft);
    let wcol: Vec<bool> = (0..band.w).map(|x| (0..band.h).any(|y| wm.data[y * band.w + x] != 0)).collect();
    let dark: Vec<bool> = (0..band.w)
        .map(|x| {
            let c = (0..band.h).filter(|&y| band.val[y * band.w + x] < 85).count();
            band.h > 0 && c as f64 / band.h as f64 >= 0.12
        })
        .collect();
    let wl = wcol.len() as i64;
    let sl = |a: i64, b: i64| -> &[bool] {
        let (a, b) = (a.clamp(0, wl), b.clamp(0, wl));
        if a < b {
            &wcol[a as usize..b as usize]
        } else {
            &[]
        }
    };
    let portrait_w = ti(u(0.052));
    let vp = (r.split.max(r.x1 - portrait_w), r.x1);

    // ---- victim name: white text between chevron and victim portrait
    let off = r.split + ti(u(0.008));
    let vr: Vec<(i64, i64)> = runs(sl(off, vp.0)).into_iter().map(|(a, b)| (a + off, b + off)).collect();
    let vr = merge_runs(&vr, u(0.03));
    let mut victim_name = None;
    for q in &vr {
        if victim_name.map(|v: (i64, i64)| q.1 - q.0 > v.1 - v.0).unwrap_or(true) {
            victim_name = Some(*q);
        }
    }

    // ---- killer half: weapon = rightmost solid white run close to the split
    let lo = 0.max(r.split - ti(u(0.55)));
    let kr: Vec<(i64, i64)> = runs(sl(lo, r.split)).into_iter().map(|(a, b)| (a + lo, b + lo)).collect();
    let (mut weapon, mut mods, mut killer_name, mut killer_portrait) = (None, None, None, None);
    let solid = merge_runs(&kr, 1f64.max(u(0.003)));
    let cand: Vec<(i64, i64)> =
        solid.iter().copied().filter(|q| (q.1 - q.0) as f64 >= u(0.034) && (r.split - q.1) as f64 <= u(0.12)).collect();
    let dark_at = |x: i64| x >= 0 && (x as usize) < dark.len() && dark[x as usize];
    let portrait_end = |from: i64, stop: i64| -> Option<i64> {
        let mut x = from;
        let mut run = 0;
        while x > stop {
            run = if dark_at(x) { run + 1 } else { 0 };
            if run >= 2.max(ti(u(0.004))) {
                return Some(x + run);
            }
            x -= 1;
        }
        None
    };
    if let Some(&(mut wa, mut wb)) = cand.last() {
        for q in &solid {
            // weapons can be split by a dark detail
            if q.0 >= wb && (q.0 - wb) as f64 <= u(0.0035) && (q.1 - wa) as f64 <= u(0.16) {
                wb = q.1;
            }
        }
        for q in solid.iter().rev() {
            if q.1 <= wa && (wa - q.1) as f64 <= u(0.006) && (wb - q.0) as f64 <= u(0.16) {
                wa = q.0;
            }
        }
        weapon = Some((wa, wb));
        let right: Vec<&(i64, i64)> = kr.iter().filter(|q| q.0 > wb).collect();
        if let (Some(f0), Some(fl)) = (right.first(), right.last()) {
            // the crosshair icon's coloured edges are not "white": extend to the chevron
            mods = Some((f0.0, fl.1.max(r.split)));
        }
        // killer portrait = first dark column block left of the weapon
        let stop = lo.max(wa - ti(u(0.32)));
        let left_bound = portrait_end(wa - ti(u(0.01)), stop).unwrap_or(stop);
        let words: Vec<(i64, i64)> = kr.iter().copied().filter(|q| q.0 >= left_bound && q.1 < wa).collect();
        let words = merge_runs(&words, u(0.02));
        if let (Some(f0), Some(fl)) = (words.first(), words.last()) {
            killer_name = Some((f0.0, fl.1));
            killer_portrait = Some((0.max(left_bound - portrait_w), left_bound));
        }
    }
    if killer_name.is_none() && r.killer_color != r.victim_color {
        // Fallback (weapon not separable, e.g. short row box at low resolution): read the whole killer
        // half after the portrait; name matching later tolerates the weapon glyph junk at the end.
        let stop = lo.max(r.split - ti(u(0.45)));
        let a = portrait_end(r.split - ti(u(0.02)), stop).unwrap_or(stop);
        if (r.split - a) as f64 > u(0.06) {
            killer_name = Some((a, r.split - ti(u(0.02))));
            killer_portrait = Some((0.max(a - portrait_w), a));
        }
    }
    RowParts { killer_name, weapon, mods, victim_name, killer_portrait, victim_portrait: vp }
}

// ---------------------------------------------------------------- headshot icon
/// Light icon pixels: far less saturated than the row's own background (teal or red).
fn icon_mask(crop: &BgrImage) -> Plane<u8> {
    let hsv = imgproc::bgr2hsv(crop);
    let mut bright: Vec<u8> = hsv.sat.iter().zip(&hsv.val).filter(|(_, &v)| v >= 120).map(|(&s, _)| s).collect();
    let bg_s: f64 = if bright.is_empty() {
        120.0
    } else {
        bright.sort_unstable();
        let n = bright.len();
        if n % 2 == 1 {
            bright[n / 2] as f64
        } else {
            (bright[n / 2 - 1] as f32 + bright[n / 2] as f32) as f64 / 2.0
        }
    };
    // compared in float32, as the thresholds were tuned
    let thr = (0.62 * bg_s) as f32;
    let d = hsv.sat.iter().zip(&hsv.val).map(|(&s, &v)| ((s as f32) <= thr && v >= 180) as u8).collect();
    Plane::from_raw(crop.w, crop.h, d)
}

fn hs_templates() -> &'static Vec<PlaneF> {
    static T: OnceLock<Vec<PlaneF>> = OnceLock::new();
    T.get_or_init(|| {
        let dir = vl_core::paths::assets_dir().join("templates");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        n.starts_with("headshot") && n.ends_with(".png")
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files.iter().filter_map(|p| BgrImage::load(p).ok()).map(|img| imgproc::bgr2gray(&img).map(|v| v as f32 / 255.0)).collect()
    })
}

/// Best normalised correlation with the crosshair (headshot) icon template.
pub fn headshot_match(crop: Option<&BgrImage>) -> f64 {
    let tpls = hs_templates();
    let Some(crop) = crop.filter(|c| !c.is_empty()) else { return 0.0 };
    if tpls.is_empty() {
        return 0.0;
    }
    let h = crop.h as f64;
    let m = imgproc::gaussian_blur_f(&icon_mask(crop).to_f32(), 3, 0.0);
    let mut best = 0f64;
    for f in [0.50, 0.58, 0.66, 0.74, 0.82] {
        let side = round_half_even(h * f);
        if side < 6 || side > m.w as i64 || side > m.h as i64 {
            continue;
        }
        for tpl in tpls {
            let t = imgproc::gaussian_blur_f(&imgproc::resize_f(tpl, side as usize, side as usize, Interp::Area), 3, 0.0);
            let r = imgproc::match_template_ccoeff_normed(&m, &t);
            let mx = r.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as f64;
            best = best.max(mx);
        }
    }
    best
}

pub const HEADSHOT_THRESHOLD: f64 = 0.55;

pub fn classify_mods(crop: Option<&BgrImage>) -> Vec<String> {
    if headshot_match(crop) >= HEADSHOT_THRESHOLD {
        vec!["headshot".into()]
    } else {
        vec![]
    }
}

pub fn is_headshot(crop: Option<&BgrImage>) -> bool {
    headshot_match(crop) >= HEADSHOT_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> BgrImage {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name);
        BgrImage::load(&p).unwrap()
    }

    fn rows(name: &str, w: i64, h: i64) -> (BgrImage, Layout, Vec<RowBox>) {
        let img = fixture(name);
        let lay = Layout::new(w, h);
        let rows = segment_rows(&img, &lay);
        (img, lay, rows)
    }

    #[test]
    fn segment_rows_169() {
        let (_, _, rows) = rows("kf_169_152.png", 1280, 720);
        assert_eq!(rows.len(), 3);
        let c: Vec<_> = rows.iter().map(|r| (r.killer_color, r.victim_color)).collect();
        assert_eq!(c, vec![("red", "teal"), ("teal", "red"), ("red", "teal")]);
    }

    #[test]
    fn segment_rows_own_rows_are_gold() {
        let (_, _, rows) = rows("kf_43_39.png", 1440, 1080);
        assert!(rows.len() >= 3);
        assert!(rows[..3].iter().all(|r| r.killer_color == "teal" && r.victim_color == "red"));
        assert!(rows[..3].iter().filter(|r| r.gold).count() >= 2);
    }

    #[test]
    fn parse_row_finds_names() {
        let (img, lay, rows) = rows("kf_43_39.png", 1440, 1080);
        let p = parse_row(&img, &rows[0], &lay);
        let (k, w, v) = (p.killer_name.unwrap(), p.weapon.unwrap(), p.victim_name.unwrap());
        assert!(k.1 < w.0 && w.0 < w.1 && w.1 <= rows[0].split && rows[0].split < v.0);
    }

    #[test]
    fn headshot_icon() {
        let (img, lay, rows) = rows("kf_43_39.png", 1440, 1080);
        let p = parse_row(&img, &rows[0], &lay);
        let m = p.mods.unwrap();
        let crop = imgproc::slice(&img, rows[0].y0, rows[0].y1, m.0 - 6, m.1 + 6);
        assert!(is_headshot(Some(&crop)));
    }
}
