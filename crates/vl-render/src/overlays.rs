//! Graphic overlays rendered to PNG (kill cards, vertical-format header, drawn kill feed rows),
//! drawn on a small RGBA canvas with `ab_glyph` text in the bundled fonts.
//!
//! Drawing follows Pillow's semantics: shapes overwrite pixels (no antialiasing), text is antialiased and
//! blended into all four channels, pasted images are alpha-composited.
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, VariableFont};
use anyhow::{Context, Result};
use image::RgbaImage;
use std::path::{Path, PathBuf};
use vl_core::paths;

pub type Rgba = [u8; 4];

pub const TEAL: [u8; 3] = [79, 186, 160];
pub const RED: [u8; 3] = [226, 86, 92];

pub fn font_path(name: &str) -> PathBuf {
    let file = match name {
        "anton" => "Anton-Regular.ttf",
        _ => "Inter-Variable.ttf",
    };
    paths::assets_dir().join("fonts").join(file)
}

/// A bundled font sized like Pillow's `ImageFont.truetype(path, size)` (size = pixels per em).
/// Inter is set to bold (opsz 14, wght 700).
pub struct SizedFont {
    pub font: FontVec,
    pub size: f32,
}

impl SizedFont {
    pub fn scale(&self) -> PxScale {
        let upem = self.font.units_per_em().unwrap_or(1000.0);
        PxScale::from(self.size * self.font.height_unscaled() / upem)
    }

    /// Advance width of a line of text (Pillow basic layout `textlength`: whole-pixel advances).
    pub fn text_length(&self, text: &str) -> f32 {
        let sf = self.font.as_scaled(self.scale());
        let mut w = 0.0;
        let mut prev = None;
        for c in text.chars() {
            let g = sf.glyph_id(c);
            if let Some(p) = prev {
                w += sf.kern(p, g).round();
            }
            w += sf.h_advance(g).round();
            prev = Some(g);
        }
        w
    }

    pub fn ascent(&self) -> f32 {
        self.font.as_scaled(self.scale()).ascent()
    }
    pub fn descent(&self) -> f32 {
        self.font.as_scaled(self.scale()).descent()
    }
}

pub fn font(name: &str, size: i64) -> Result<SizedFont> {
    let p = font_path(name);
    let data = std::fs::read(&p).with_context(|| format!("font missing: {}", p.display()))?;
    let mut f = FontVec::try_from_vec(data).map_err(|e| anyhow::anyhow!("bad font {}: {e}", p.display()))?;
    if name == "inter" {
        f.set_variation(b"opsz", 14.0);
        f.set_variation(b"wght", 700.0);
    }
    Ok(SizedFont { font: f, size: size.max(1) as f32 })
}

fn rgb(hex: &str) -> [u8; 3] {
    let h = hex.trim_start_matches('#');
    let c = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(255);
    [c(0), c(2), c(4)]
}

fn with_a(c: [u8; 3], a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

/// RGBA drawing surface with the Pillow operations the overlays use.
pub struct Canvas {
    pub img: RgbaImage,
}

impl Canvas {
    pub fn new(w: u32, h: u32, fill: Rgba) -> Self {
        Canvas { img: RgbaImage::from_pixel(w.max(1), h.max(1), image::Rgba(fill)) }
    }

    fn put(&mut self, x: i64, y: i64, c: Rgba) {
        if x >= 0 && y >= 0 && (x as u32) < self.img.width() && (y as u32) < self.img.height() {
            self.img.put_pixel(x as u32, y as u32, image::Rgba(c));
        }
    }

    /// Pillow `rectangle([x0, y0, x1, y1], fill)`: inclusive corners.
    pub fn rectangle(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: Rgba) {
        let (x0, y0, x1, y1) = (x0 as i64, y0 as i64, x1 as i64, y1 as i64);
        for y in y0.max(0)..=y1.min(self.img.height() as i64 - 1) {
            for x in x0.max(0)..=x1.min(self.img.width() as i64 - 1) {
                self.put(x, y, c);
            }
        }
    }

    /// Pillow `rounded_rectangle(..., radius, fill)`.
    pub fn rounded_rectangle(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, r: f64, c: Rgba) {
        let (x0, y0, x1, y1) = (x0 as i64, y0 as i64, x1 as i64, y1 as i64);
        let r = (r as i64).min((x1 - x0) / 2).min((y1 - y0) / 2).max(0) as f64;
        for y in y0.max(0)..=y1.min(self.img.height() as i64 - 1) {
            for x in x0.max(0)..=x1.min(self.img.width() as i64 - 1) {
                let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                let cx = if fx < x0 as f64 + r {
                    Some(x0 as f64 + r)
                } else if fx > x1 as f64 + 1.0 - r {
                    Some(x1 as f64 + 1.0 - r)
                } else {
                    None
                };
                let cy = if fy < y0 as f64 + r {
                    Some(y0 as f64 + r)
                } else if fy > y1 as f64 + 1.0 - r {
                    Some(y1 as f64 + 1.0 - r)
                } else {
                    None
                };
                if let (Some(cx), Some(cy)) = (cx, cy) {
                    if (fx - cx).powi(2) + (fy - cy).powi(2) > r * r {
                        continue;
                    }
                }
                self.put(x, y, c);
            }
        }
    }

    /// Pillow `polygon(points, fill)` (even-odd fill of pixel centres, edges included).
    pub fn polygon(&mut self, pts: &[(f64, f64)], c: Rgba) {
        if pts.len() < 3 {
            return;
        }
        let ymin = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).floor().max(0.0) as i64;
        let ymax = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max).ceil().min(self.img.height() as f64 - 1.0) as i64;
        for y in ymin..=ymax {
            let fy = y as f64;
            let mut xs = vec![];
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if (a.1 <= fy && fy < b.1) || (b.1 <= fy && fy < a.1) || (fy == a.1 && fy == b.1) {
                    if a.1 == b.1 {
                        xs.push(a.0);
                        xs.push(b.0);
                    } else {
                        xs.push(a.0 + (fy - a.1) * (b.0 - a.0) / (b.1 - a.1));
                    }
                }
            }
            if fy == ymax as f64 {
                // include the bottom vertex row like Pillow
                for p in pts {
                    if p.1 == fy {
                        xs.push(p.0);
                        xs.push(p.0);
                    }
                }
            }
            xs.sort_by(|a, b| a.total_cmp(b));
            for pair in xs.chunks(2) {
                if pair.len() == 2 {
                    for x in pair[0].round() as i64..=pair[1].round() as i64 {
                        self.put(x, y, c);
                    }
                }
            }
        }
    }

    /// Thick line (round-ish ends), Pillow `line(..., width)`.
    pub fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: Rgba, width: i64) {
        let hw = width as f64 / 2.0;
        let (minx, maxx) = (x0.min(x1) - hw, x0.max(x1) + hw);
        let (miny, maxy) = (y0.min(y1) - hw, y0.max(y1) + hw);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        for y in miny.floor() as i64..=maxy.ceil() as i64 {
            for x in minx.floor() as i64..=maxx.ceil() as i64 {
                let (px, py) = (x as f64, y as f64);
                let t = (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (x0 + t * dx, y0 + t * dy);
                if (px - qx).powi(2) + (py - qy).powi(2) <= hw * hw {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// Ellipse outline, Pillow `ellipse(bbox, outline, width)`.
    pub fn ellipse_outline(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: Rgba, width: i64) {
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let w = width as f64;
        for y in y0.floor() as i64..=y1.ceil() as i64 {
            for x in x0.floor() as i64..=x1.ceil() as i64 {
                let (dx, dy) = (x as f64 + 0.5 - cx - 0.5, y as f64 + 0.5 - cy - 0.5);
                let outer = (dx / (rx + 0.5)).powi(2) + (dy / (ry + 0.5)).powi(2) <= 1.0;
                let (irx, iry) = ((rx + 0.5 - w).max(0.0), (ry + 0.5 - w).max(0.0));
                let inner = irx > 0.0 && iry > 0.0 && (dx / irx).powi(2) + (dy / iry).powi(2) < 1.0;
                if outer && !inner {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// Pillow `text((x, y), text, font, fill, anchor="lm")`: left edge at x, vertical middle at y.
    pub fn text_lm(&mut self, x: f64, y: f64, text: &str, f: &SizedFont, c: Rgba) {
        let scale = f.scale();
        let sf = f.font.as_scaled(scale);
        // Pillow's "m" anchor: halfway between ascender and descender
        // (FreeType rounds the size metrics outwards to whole pixels)
        let baseline = y as f32 + ((sf.ascent().ceil() + sf.descent().floor()) / 2.0 + 0.5).floor();
        let mut pen = x as f32;
        let mut prev = None;
        for ch in text.chars() {
            let gid = sf.glyph_id(ch);
            if let Some(p) = prev {
                pen += sf.kern(p, gid).round();
            }
            let g = gid.with_scale_and_position(scale, ab_glyph::point(pen, baseline));
            pen += sf.h_advance(gid).round();
            prev = Some(gid);
            if let Some(og) = f.font.outline_glyph(g) {
                let b = og.px_bounds();
                let (bx, by) = (b.min.x as i64, b.min.y as i64);
                let (w, h) = (self.img.width() as i64, self.img.height() as i64);
                og.draw(|gx, gy, cov| {
                    let (px, py) = (bx + gx as i64, by + gy as i64);
                    if px < 0 || py < 0 || px >= w || py >= h {
                        return;
                    }
                    let cov = cov.clamp(0.0, 1.0);
                    let d = self.img.get_pixel_mut(px as u32, py as u32);
                    for k in 0..4 {
                        let v = d.0[k] as f32 * (1.0 - cov) + c[k] as f32 * cov;
                        d.0[k] = v.round().clamp(0.0, 255.0) as u8;
                    }
                });
            }
        }
    }

    /// Pillow `alpha_composite(src, (x, y))`.
    pub fn alpha_composite(&mut self, src: &RgbaImage, x: i64, y: i64) {
        for sy in 0..src.height() as i64 {
            for sx in 0..src.width() as i64 {
                let (dx, dy) = (x + sx, y + sy);
                if dx < 0 || dy < 0 || dx >= self.img.width() as i64 || dy >= self.img.height() as i64 {
                    continue;
                }
                let s = src.get_pixel(sx as u32, sy as u32).0;
                let d = self.img.get_pixel_mut(dx as u32, dy as u32);
                let sa = s[3] as f32 / 255.0;
                let da = d.0[3] as f32 / 255.0;
                let oa = sa + da * (1.0 - sa);
                if oa <= 0.0 {
                    d.0 = [0, 0, 0, 0];
                    continue;
                }
                for k in 0..3 {
                    let v = (s[k] as f32 * sa + d.0[k] as f32 * da * (1.0 - sa)) / oa;
                    d.0[k] = v.round().clamp(0.0, 255.0) as u8;
                }
                d.0[3] = (oa * 255.0).round() as u8;
            }
        }
    }

    pub fn save(&self, out: &Path) -> Result<()> {
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d)?;
        }
        self.img.save(out).with_context(|| format!("could not write {}", out.display()))?;
        Ok(())
    }
}

/// Pillow's default `Image.resize` filter (bicubic).
fn resize_bicubic(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    pil_resize(img, w, h, PilFilter::Bicubic)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilFilter {
    Bicubic,
    Lanczos,
}

impl PilFilter {
    fn support(self) -> f64 {
        match self {
            PilFilter::Bicubic => 2.0,
            PilFilter::Lanczos => 3.0,
        }
    }
    fn eval(self, x: f64) -> f64 {
        match self {
            PilFilter::Bicubic => {
                let a = -0.5;
                let x = x.abs();
                if x < 1.0 {
                    ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
                } else if x < 2.0 {
                    (((x - 5.0) * x + 8.0) * x - 4.0) * a
                } else {
                    0.0
                }
            }
            PilFilter::Lanczos => {
                let sinc = |x: f64| if x == 0.0 { 1.0 } else { (x * std::f64::consts::PI).sin() / (x * std::f64::consts::PI) };
                if (-3.0..3.0).contains(&x) {
                    sinc(x) * sinc(x / 3.0)
                } else {
                    0.0
                }
            }
        }
    }
}

const PRECISION_BITS: u32 = 32 - 8 - 2;

/// Pillow's `precompute_coeffs` + `normalize_coeffs_8bpc`: (ksize, bounds, fixed-point coefficients).
fn pil_coeffs(in_size: usize, out_size: usize, filter: PilFilter) -> (usize, Vec<(usize, usize)>, Vec<i32>) {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = filter.support() * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut kk = vec![0i32; out_size * ksize];
    for xx in 0..out_size {
        let center = (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = (((center + support + 0.5) as i64).min(in_size as i64) as usize).saturating_sub(xmin);
        let mut k: Vec<f64> = (0..xmax).map(|x| filter.eval((x as f64 + xmin as f64 - center + 0.5) * ss)).collect();
        let ww: f64 = k.iter().sum();
        if ww != 0.0 {
            for v in k.iter_mut() {
                *v /= ww;
            }
        }
        for (x, v) in k.iter().enumerate() {
            let f = v * (1u32 << PRECISION_BITS) as f64;
            kk[xx * ksize + x] = if *v < 0.0 { (-0.5 + f) as i32 } else { (0.5 + f) as i32 };
        }
        bounds.push((xmin, xmax));
    }
    (ksize, bounds, kk)
}

fn clip8(v: i64) -> u8 {
    if v >= (1i64 << PRECISION_BITS << 8) {
        255
    } else if v <= 0 {
        0
    } else {
        (v >> PRECISION_BITS) as u8
    }
}

/// Pillow's `Image.resize` for RGBA (premultiplied alpha, two fixed-point passes), so resized rows and
/// portraits match the reference images pixel for pixel.
pub fn pil_resize(img: &RgbaImage, w: u32, h: u32, filter: PilFilter) -> RgbaImage {
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    let (iw, ih) = (img.width() as usize, img.height() as usize);
    if iw == 0 || ih == 0 {
        return RgbaImage::new(w as u32, h as u32);
    }
    // RGBA -> RGBa (MULDIV255)
    let mut src: Vec<u8> = img.as_raw().clone();
    for p in src.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        for c in p.iter_mut().take(3) {
            let t = *c as u32 * a + 128;
            *c = ((t + (t >> 8)) >> 8) as u8;
        }
    }
    let (mut cur, mut cw, mut ch) = (src, iw, ih);
    let need_h = w != iw;
    let need_v = h != ih;
    let (ksv, mut bv, kv) = pil_coeffs(ih, h, filter);
    if need_h {
        let (ksh, bh, kh) = pil_coeffs(iw, w, filter);
        let first = bv[0].0;
        let last = bv[h - 1].0 + bv[h - 1].1;
        for b in bv.iter_mut() {
            b.0 -= first;
        }
        let rows = last - first;
        let mut tmp = vec![0u8; w * rows * 4];
        for y in 0..rows {
            let sy = y + first;
            for (xx, &(xmin, xmax)) in bh.iter().enumerate() {
                let k = &kh[xx * ksh..];
                for c in 0..4 {
                    let mut ss: i64 = 1 << (PRECISION_BITS - 1);
                    for x in 0..xmax {
                        ss += cur[(sy * cw + x + xmin) * 4 + c] as i64 * k[x] as i64;
                    }
                    tmp[(y * w + xx) * 4 + c] = clip8(ss);
                }
            }
        }
        cur = tmp;
        cw = w;
        ch = rows;
    }
    if need_v {
        let mut out = vec![0u8; w * h * 4];
        for (yy, &(ymin, ymax)) in bv.iter().enumerate() {
            let k = &kv[yy * ksv..];
            for x in 0..cw {
                for c in 0..4 {
                    let mut ss: i64 = 1 << (PRECISION_BITS - 1);
                    for y in 0..ymax {
                        ss += cur[((y + ymin) * cw + x) * 4 + c] as i64 * k[y] as i64;
                    }
                    out[(yy * cw + x) * 4 + c] = clip8(ss);
                }
            }
        }
        cur = out;
        ch = h;
    }
    let _ = ch;
    // RGBa -> RGBA
    for p in cur.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a != 0 && a != 255 {
            for c in p.iter_mut().take(3) {
                *c = ((255 * *c as u32) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(w as u32, h as u32, cur).unwrap_or_else(|| RgbaImage::new(w as u32, h as u32))
}

fn load_rgba(p: &Path) -> Option<RgbaImage> {
    image::open(p).ok().map(|i| i.to_rgba8())
}

/// Compact "killer > [portrait] victim [HS]" card. Returns (w, h) in pixels.
#[allow(clippy::too_many_arguments)]
pub fn kill_card(
    out: &Path,
    killer: &str,
    victim: &str,
    portrait: Option<&Path>,
    headshot: bool,
    highlight: &str,
    height: i64,
    count_label: &str,
) -> Result<(i64, i64)> {
    let h = height.max(24);
    let hf = h as f64;
    let pad = (hf * 0.28) as i64;
    let padf = pad as f64;
    let f = font("inter", (hf * 0.42) as i64)?;
    let fb = font("anton", (hf * 0.36) as i64)?;
    let hl = rgb(highlight);
    let kw = f.text_length(killer) as f64;
    let vw = f.text_length(victim) as f64;
    let arrow = "  \u{203a}  ";
    let aw = f.text_length(arrow) as f64;
    let pw = if portrait.is_some() { (hf * 0.82) as i64 } else { 0 };
    let hs = if headshot { "HS" } else { "" };
    let hsw = if !hs.is_empty() { fb.text_length(hs) as f64 + padf } else { 0.0 };
    let cl = count_label;
    let clw = if !cl.is_empty() { fb.text_length(cl) as f64 + padf * 1.4 } else { 0.0 };
    let w = (padf
        + clw
        + kw
        + aw
        + pw as f64
        + if pw > 0 { padf * 0.5 } else { 0.0 }
        + vw
        + if !hs.is_empty() { padf } else { 0.0 }
        + hsw
        + padf) as i64;
    let mut c = Canvas::new(w as u32, h as u32, [0, 0, 0, 0]);
    c.rounded_rectangle(0.0, 0.0, (w - 1) as f64, (h - 1) as f64, (hf * 0.18) as i64 as f64, [16, 16, 22, 215]);
    c.rectangle(0.0, 0.0, (hf * 0.08) as i64 as f64, (h - 1) as f64, with_a(hl, 255));
    let mut x = padf;
    let cy = (h / 2) as f64;
    if !cl.is_empty() {
        let bw = fb.text_length(cl) as f64 + padf * 0.8;
        c.rounded_rectangle(x, (hf * 0.2) as i64 as f64, x + bw, (hf * 0.8) as i64 as f64, (hf * 0.1) as i64 as f64, with_a(hl, 255));
        c.text_lm(x + padf * 0.4, cy, cl, &fb, [16, 16, 22, 255]);
        x += (bw + padf * 0.6) as i64 as f64;
    }
    c.text_lm(x, cy, killer, &f, with_a(hl, 255));
    x += kw as i64 as f64;
    c.text_lm(x, cy, arrow, &f, [200, 200, 210, 255]);
    x += aw as i64 as f64;
    if let Some(pp) = portrait {
        if pp.exists() {
            if let Some(p) = load_rgba(pp) {
                let ph = if p.width() > 0 { (pw as f64 * p.height() as f64 / p.width().max(1) as f64) as u32 } else { pw as u32 };
                let p = resize_bicubic(&p, pw as u32, ph);
                let target = (hf * 0.82) as i64;
                let top = 0.max((p.height() as i64 - target) / 2);
                let p = crop_padded(&p, 0, top, pw, target);
                c.alpha_composite(&p, x as i64, (hf * 0.09) as i64);
            }
            x += (pw + (padf * 0.5) as i64) as f64;
        }
    }
    c.text_lm(x, cy, victim, &f, [255, 255, 255, 255]);
    x += vw as i64 as f64;
    if !hs.is_empty() {
        x += padf;
        c.text_lm(x, cy, hs, &fb, with_a(hl, 255));
    }
    c.save(out)?;
    Ok((w, h))
}

/// Pillow `crop((x, y, x + w, y + h))`: areas outside the image are transparent.
fn crop_padded(img: &RgbaImage, x: i64, y: i64, w: i64, h: i64) -> RgbaImage {
    let mut out = RgbaImage::new(w.max(1) as u32, h.max(1) as u32);
    for yy in 0..h.max(0) {
        for xx in 0..w.max(0) {
            let (sx, sy) = (x + xx, y + yy);
            if sx >= 0 && sy >= 0 && (sx as u32) < img.width() && (sy as u32) < img.height() {
                out.put_pixel(xx as u32, yy as u32, *img.get_pixel(sx as u32, sy as u32));
            }
        }
    }
    out
}

/// Dark header panel for the 9:16 layout (the live score bar is overlaid on top of it).
pub fn vertical_header(out: &Path, width: i64, height: i64, title: &str, highlight: &str) -> Result<()> {
    let mut c = Canvas::new(width as u32, height as u32, [14, 14, 20, 255]);
    let hl = rgb(highlight);
    c.rectangle(0.0, (height - (height as f64 * 0.025) as i64) as f64, width as f64, height as f64, with_a(hl, 255));
    if !title.is_empty() {
        let f = font("inter", (height as f64 * 0.13) as i64)?;
        c.text_lm((width as f64 * 0.04) as i64 as f64, (height as f64 * 0.12) as i64 as f64, title, &f, [235, 235, 240, 255]);
    }
    c.save(out)
}

/// Killfeed-style row drawn from our own data (fallback when no in-game row crop exists).
///
/// Killer half in the killer's team colour, victim half in the other team's colour, portraits at both ends,
/// a crosshair mark for headshots.
#[allow(clippy::too_many_arguments)]
pub fn feed_row(
    out: &Path,
    killer: &str,
    victim: &str,
    killer_portrait: Option<&Path>,
    victim_portrait: Option<&Path>,
    headshot: bool,
    killer_side: &str,
    height: i64,
) -> Result<(i64, i64)> {
    let h = height.max(20);
    let hf = h as f64;
    let f = font("inter", (hf * 0.42) as i64)?;
    let kcol = if killer_side == "teal" { TEAL } else { RED };
    let vcol = if killer_side == "teal" { RED } else { TEAL };
    let (kw, vw) = (f.text_length(killer) as f64, f.text_length(victim) as f64);
    let pw = (hf * 1.25) as i64;
    let pad = (hf * 0.3) as i64;
    let mark = if headshot { (hf * 0.7) as i64 } else { 0 };
    let chev = (hf * 0.35) as i64;
    let split = pw + pad + kw as i64 + pad + if mark > 0 { mark + pad } else { 0 };
    let w = split + chev + pad + vw as i64 + pad + pw;
    let mut c = Canvas::new(w as u32, h as u32, [0, 0, 0, 0]);
    // halves with a little transparency like an overlay, chevron at the split
    let tri = [(split as f64, 0.0), ((split + chev) as f64, (h / 2) as f64), (split as f64, hf)];
    c.rectangle(0.0, 0.0, split as f64, hf, with_a(kcol, 225));
    c.polygon(&tri, with_a(kcol, 225));
    c.rectangle((split + 1) as f64, 0.0, w as f64, hf, with_a(vcol, 225));
    c.polygon(&tri, with_a(kcol, 225));

    let paste_portrait = |c: &mut Canvas, path: Option<&Path>, x: i64| {
        if let Some(path) = path {
            if path.exists() {
                if let Some(p) = load_rgba(path) {
                    let ph = (pw as f64 * p.height() as f64 / p.width().max(1) as f64) as u32;
                    let p = resize_bicubic(&p, pw as u32, ph);
                    let top = 0.max((p.height() as i64 - h) / 2);
                    let p = crop_padded(&p, 0, top, pw, h);
                    c.alpha_composite(&p, x, 0);
                }
            }
        }
    };
    paste_portrait(&mut c, killer_portrait, 0);
    paste_portrait(&mut c, victim_portrait, w - pw);
    let mut x = pw + pad;
    c.text_lm(x as f64, (h / 2) as f64, killer, &f, [255, 255, 255, 255]);
    x += kw as i64 + pad;
    if mark > 0 {
        let (cx, cy, r) = ((x + mark / 2) as f64, (h / 2) as f64, (mark / 2) as f64);
        let lw = 2.max(h / 14);
        c.ellipse_outline(cx - r * 0.6, cy - r * 0.6, cx + r * 0.6, cy + r * 0.6, [255, 255, 255, 255], lw);
        for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            c.line(cx + dx * r * 0.3, cy + dy * r * 0.3, cx + dx * r, cy + dy * r, [255, 255, 255, 255], lw);
        }
    }
    c.text_lm((split + chev + pad) as f64, (h / 2) as f64, victim, &f, [255, 255, 255, 255]);
    c.save(out)?;
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_rows_and_cards() {
        let d = tempfile::tempdir().unwrap();
        let (w, h) = feed_row(&d.path().join("r.png"), "thePanda X", "Enemy", None, None, true, "teal", 48).unwrap();
        assert_eq!(h, 48);
        assert!(w > 200);
        let img = image::open(d.path().join("r.png")).unwrap().to_rgba8();
        assert_eq!(img.width() as i64, w);
        // killer half is teal, victim half red
        assert_eq!(img.get_pixel(2, 2).0, [79, 186, 160, 225]);
        assert_eq!(img.get_pixel(w as u32 - 3, 2).0, [226, 86, 92, 225]);
        let (cw, ch) = kill_card(&d.path().join("c.png"), "me", "you", None, true, "#F4A6C4", 60, "2K").unwrap();
        assert!(cw > 100 && ch == 60);
        vertical_header(&d.path().join("h.png"), 1080, 200, "Title", "#F4A6C4").unwrap();
        let f = font("inter", 34).unwrap();
        assert!(f.text_length("thePanda X") > 100.0);
    }
}
