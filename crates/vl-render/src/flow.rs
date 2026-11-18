//! Dense optical flow for the "Faster" super smooth: a DIS-style patch search (Kroeger et al. 2016, "Fast
//! Optical Flow using Dense Inverse Search") on an image pyramid, all on the CPU.
//!
//! Per pyramid level, coarse to fine:
//! 1. 8x8 patches on a 4-pixel grid start from the flow of the coarser level and refine it with an
//!    inverse-compositional Gauss-Newton search (mean-normalised, so brightness changes don't matter).
//!    A patch whose refined match is worse than its start keeps the start.
//! 2. Densification: every pixel averages the patches covering it, each weighted by how well that patch's
//!    motion explains the pixel (1 / max(1, |residual|)), so motion edges stay reasonably sharp.
//! 3. A 3x3 median removes the odd wrong patch.
//!
//! Motion blur only needs coarse motion, so the flow runs at a quarter (or half) of the gameplay resolution.

/// patch size and patch grid stride (pixels of the level)
const PATCH: usize = 8;
const STRIDE: usize = 4;
/// Gauss-Newton iterations per patch
const ITERS: usize = 10;
const ITERS_FINE: usize = 5;
/// coarsest pyramid level is at least this many pixels on its short side
const MIN_DIM: usize = 12;
const MAX_LEVELS: usize = 6;
/// exhaustive search radius on the coarsest level (pixels of that level)
const SEARCH: i32 = 4;

/// A single-channel float image (grey, 0..255).
#[derive(Clone, Debug, Default)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub d: Vec<f32>,
}

impl Gray {
    pub fn new(w: usize, h: usize) -> Gray {
        Gray { w, h, d: vec![0.0; w * h] }
    }
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.d[y * self.w + x]
    }
    /// Bilinear sample, clamped to the edges.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let r0 = self.d[y0 * self.w + x0] * (1.0 - fx) + self.d[y0 * self.w + x1] * fx;
        let r1 = self.d[y1 * self.w + x0] * (1.0 - fx) + self.d[y1 * self.w + x1] * fx;
        r0 * (1.0 - fy) + r1 * fy
    }
    /// 2x2 box downscale.
    pub fn half(&self) -> Gray {
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let mut o = Gray::new(w, h);
        par_rows(&mut o.d, w, |y, row| {
            let (ya, yb) = ((2 * y).min(self.h - 1), (2 * y + 1).min(self.h - 1));
            for (x, out) in row.iter_mut().enumerate() {
                let (xa, xb) = ((2 * x).min(self.w - 1), (2 * x + 1).min(self.w - 1));
                *out = 0.25 * (self.at(xa, ya) + self.at(xb, ya) + self.at(xa, yb) + self.at(xb, yb));
            }
        });
        o
    }
}

/// Grey (BT.601 luma weights) of a BGR8 buffer, area-averaged down by an integer factor.
pub fn grey_down(bgr: &[u8], w: usize, h: usize, div: usize) -> Gray {
    let div = div.max(1);
    let (ow, oh) = ((w / div).max(1), (h / div).max(1));
    let mut o = Gray::new(ow, oh);
    let norm = 1.0 / (div * div) as f32;
    par_rows(&mut o.d, ow, |y, row| {
        for (x, out) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for yy in y * div..((y + 1) * div).min(h) {
                let base = (yy * w + x * div) * 3;
                for xx in 0..div.min(w - x * div) {
                    let p = &bgr[base + xx * 3..base + xx * 3 + 3];
                    acc += 0.114 * p[0] as f32 + 0.587 * p[1] as f32 + 0.299 * p[2] as f32;
                }
            }
            *out = acc * norm;
        }
    });
    o
}

/// Image pyramid, finest level first.
pub fn pyramid(g: Gray) -> Vec<Gray> {
    let mut levels = vec![g];
    while levels.len() < MAX_LEVELS {
        let last = levels.last().unwrap();
        if last.w.min(last.h) / 2 < MIN_DIM {
            break;
        }
        let next = last.half();
        levels.push(next);
    }
    levels
}

/// A dense flow field: pixel (x, y) of the first image moves to (x + u, y + v) in the second.
#[derive(Clone, Debug, Default)]
pub struct Flow {
    pub w: usize,
    pub h: usize,
    pub u: Vec<f32>,
    pub v: Vec<f32>,
}

impl Flow {
    pub fn zero(w: usize, h: usize) -> Flow {
        Flow { w, h, u: vec![0.0; w * h], v: vec![0.0; w * h] }
    }
    /// Bilinear sample, clamped to the edges.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> (f32, f32) {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b, c, d) = (y0 * self.w + x0, y0 * self.w + x1, y1 * self.w + x0, y1 * self.w + x1);
        let wa = (1.0 - fx) * (1.0 - fy);
        let wb = fx * (1.0 - fy);
        let wc = (1.0 - fx) * fy;
        let wd = fx * fy;
        (
            self.u[a] * wa + self.u[b] * wb + self.u[c] * wc + self.u[d] * wd,
            self.v[a] * wa + self.v[b] * wb + self.v[c] * wc + self.v[d] * wd,
        )
    }
    /// The same motion on a grid of `w` x `h` (vectors scaled with the grid).
    pub fn resize(&self, w: usize, h: usize) -> Flow {
        let (sx, sy) = (w as f32 / self.w as f32, h as f32 / self.h as f32);
        let mut uv = vec![(0.0f32, 0.0f32); w * h];
        par_rows(&mut uv, w, |y, row| {
            for (x, o) in row.iter_mut().enumerate() {
                let (u, v) = self.sample((x as f32 + 0.5) / sx - 0.5, (y as f32 + 0.5) / sy - 0.5);
                *o = (u * sx, v * sy);
            }
        });
        Flow::from_pairs(w, h, uv)
    }
    fn from_pairs(w: usize, h: usize, uv: Vec<(f32, f32)>) -> Flow {
        let (u, v) = uv.into_iter().unzip();
        Flow { w, h, u, v }
    }
    fn median3(&self) -> Flow {
        let (w, h) = (self.w, self.h);
        // 3x3 neighbourhood with edge pixels repeated, median by a sorting network
        let med = |src: &[f32], x: usize, y: usize| -> f32 {
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
            let mut k = [
                src[yu * w + xl],
                src[yu * w + x],
                src[yu * w + xr],
                src[y * w + xl],
                src[y * w + x],
                src[y * w + xr],
                src[yd * w + xl],
                src[yd * w + x],
                src[yd * w + xr],
            ];
            median9(&mut k)
        };
        let mut uv = vec![(0.0f32, 0.0f32); w * h];
        par_rows(&mut uv, w, |y, row| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = (med(&self.u, x, y), med(&self.v, x, y));
            }
        });
        Flow::from_pairs(w, h, uv)
    }
}

/// Median of 9 values (Paeth's 19-exchange network).
#[inline]
fn median9(p: &mut [f32; 9]) -> f32 {
    #[inline]
    fn s(p: &mut [f32; 9], a: usize, b: usize) {
        let (x, y) = (p[a], p[b]);
        p[a] = x.min(y);
        p[b] = x.max(y);
    }
    s(p, 1, 2);
    s(p, 4, 5);
    s(p, 7, 8);
    s(p, 0, 1);
    s(p, 3, 4);
    s(p, 6, 7);
    s(p, 1, 2);
    s(p, 4, 5);
    s(p, 7, 8);
    s(p, 0, 3);
    s(p, 5, 8);
    s(p, 4, 7);
    s(p, 3, 6);
    s(p, 1, 4);
    s(p, 2, 5);
    s(p, 4, 7);
    s(p, 4, 2);
    s(p, 6, 4);
    s(p, 4, 2);
    p[4]
}

/// Flow from the first image to the second (pyramids from [`pyramid`], same sizes).
pub fn dis(p0: &[Gray], p1: &[Gray]) -> Flow {
    let top = p0.len().min(p1.len()) - 1;
    let mut f = Flow::zero(p0[top].w, p0[top].h);
    for l in (0..=top).rev() {
        if f.w != p0[l].w || f.h != p0[l].h {
            f = f.resize(p0[l].w, p0[l].h);
        }
        f = level(&p0[l], &p1[l], &f, (l == top).then_some(SEARCH), l == 0);
    }
    f
}

/// Patch positions along one axis: every STRIDE pixels, plus one flush with the far edge.
fn grid(n: usize, p: usize) -> Vec<usize> {
    let last = n - p;
    let mut g: Vec<usize> = (0..=last).step_by(STRIDE).collect();
    if *g.last().unwrap() != last {
        g.push(last);
    }
    g
}

/// One pyramid level; `search` = radius of an exhaustive integer search (coarsest level only).
fn level(i0: &Gray, i1: &Gray, init: &Flow, search: Option<i32>, fine: bool) -> Flow {
    let (w, h) = (i0.w, i0.h);
    let p = PATCH.min(w).min(h);
    if p < 2 {
        return init.clone();
    }
    // gradients of the template image (central differences)
    let mut g = vec![(0.0f32, 0.0f32); w * h];
    par_rows(&mut g, w, |y, row| {
        let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
            *o = ((i0.at(xr, y) - i0.at(xl, y)) / (xr - xl).max(1) as f32, (i0.at(x, yd) - i0.at(x, yu)) / (yd - yu).max(1) as f32);
        }
    });
    let gf = Flow::from_pairs(w, h, g);
    let gx = Gray { w, h, d: gf.u };
    let gy = Gray { w, h, d: gf.v };
    let xs = grid(w, p);
    let ys = grid(h, p);
    // 1. patch search from the coarser level's flow; on the coarsest level an exhaustive integer search
    //    first (large flicks), one patch row per task
    // fewer iterations on the finest level: the coarser levels already got close
    let iters = if fine { ITERS_FINE } else { ITERS };
    let pt = |xi: usize, yi: usize| Patch::new(i0, i1, &gx, &gy, xs[xi], ys[yi], p, iters);
    let mut rows: Vec<Vec<(f32, f32)>> = par_collect(ys.len(), |yi| {
        (0..xs.len())
            .map(|xi| {
                let pa = pt(xi, yi);
                let mut start = init.sample(pa.cx(), pa.cy());
                if let Some(r) = search {
                    let mut cands = vec![start];
                    for dy in -r..=r {
                        for dx in -r..=r {
                            cands.push((dx as f32, dy as f32));
                        }
                    }
                    start = pa.best(&cands);
                }
                pa.refine(start)
            })
            .collect()
    });
    // 2. propagation: a patch takes a neighbour's motion when that fits it better, then refines it
    for _ in 0..if fine { 0 } else { 2 } {
        let prev = rows.clone();
        rows = par_collect(ys.len(), |yi| {
            (0..xs.len())
                .map(|xi| {
                    let own = prev[yi][xi];
                    let mut cands = vec![own];
                    if xi > 0 {
                        cands.push(prev[yi][xi - 1]);
                    }
                    if xi + 1 < xs.len() {
                        cands.push(prev[yi][xi + 1]);
                    }
                    if yi > 0 {
                        cands.push(prev[yi - 1][xi]);
                    }
                    if yi + 1 < ys.len() {
                        cands.push(prev[yi + 1][xi]);
                    }
                    let pa = pt(xi, yi);
                    let best = pa.best(&cands);
                    if best == own {
                        own
                    } else {
                        pa.refine(best)
                    }
                })
                .collect()
        });
    }

    // 3. densification: which patches cover each column / row
    let cover = |g: &[usize], n: usize| -> Vec<(usize, usize)> {
        (0..n)
            .map(|c| {
                let lo = g.iter().position(|&s| s + p > c).unwrap_or(g.len());
                let hi = g.iter().rposition(|&s| s <= c).map(|i| i + 1).unwrap_or(0);
                (lo, hi.max(lo))
            })
            .collect()
    };
    let (cx, cy) = (cover(&xs, w), cover(&ys, h));
    let mut uv = vec![(0.0f32, 0.0f32); w * h];
    par_rows(&mut uv, w, |y, row| {
        let (ylo, yhi) = cy[y];
        for (x, o) in row.iter_mut().enumerate() {
            let (xlo, xhi) = cx[x];
            let (mut su, mut sv, mut sw) = (0.0f32, 0.0f32, 0.0f32);
            let t = i0.at(x, y);
            for r in &rows[ylo..yhi] {
                for &(u, v) in &r[xlo..xhi] {
                    let res = (i1.sample(x as f32 + u, y as f32 + v) - t).abs();
                    let wt = 1.0 / res.max(1.0);
                    su += u * wt;
                    sv += v * wt;
                    sw += wt;
                }
            }
            *o = if sw > 0.0 { (su / sw, sv / sw) } else { init.sample(x as f32, y as f32) };
        }
    });
    Flow::from_pairs(w, h, uv).median3()
}

/// One patch of the template image and its (mean-normalised) Gauss-Newton system.
struct Patch<'a> {
    i1: &'a Gray,
    px: usize,
    py: usize,
    p: usize,
    iters: usize,
    /// template pixels and zero-mean gradients, row-major p x p
    t: [f32; PATCH * PATCH],
    gx: [f32; PATCH * PATCH],
    gy: [f32; PATCH * PATCH],
    h11: f32,
    h12: f32,
    h22: f32,
    det: f32,
}

impl<'a> Patch<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(i0: &Gray, i1: &'a Gray, gx: &Gray, gy: &Gray, px: usize, py: usize, p: usize, iters: usize) -> Patch<'a> {
        let n = p * p;
        let npx = n as f32;
        let mut pa = Patch {
            i1,
            px,
            py,
            p,
            iters,
            t: [0.0; PATCH * PATCH],
            gx: [0.0; PATCH * PATCH],
            gy: [0.0; PATCH * PATCH],
            h11: 0.0,
            h12: 0.0,
            h22: 0.0,
            det: 0.0,
        };
        let (mut mx, mut my) = (0.0f32, 0.0f32);
        for dy in 0..p {
            for dx in 0..p {
                let k = dy * p + dx;
                pa.t[k] = i0.at(px + dx, py + dy);
                pa.gx[k] = gx.at(px + dx, py + dy);
                pa.gy[k] = gy.at(px + dx, py + dy);
                mx += pa.gx[k];
                my += pa.gy[k];
            }
        }
        // zero-mean gradients (the residual is mean-normalised) and their Hessian
        mx /= npx;
        my /= npx;
        let (mut h11, mut h12, mut h22) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..n {
            pa.gx[k] -= mx;
            pa.gy[k] -= my;
            h11 += pa.gx[k] * pa.gx[k];
            h12 += pa.gx[k] * pa.gy[k];
            h22 += pa.gy[k] * pa.gy[k];
        }
        // a little damping: flat patches (no texture) barely move away from their start
        let lam = npx * 0.5;
        h11 += lam;
        h22 += lam;
        pa.h11 = h11;
        pa.h12 = h12;
        pa.h22 = h22;
        pa.det = h11 * h22 - h12 * h12;
        pa
    }
    fn cx(&self) -> f32 {
        self.px as f32 + (self.p as f32 - 1.0) / 2.0
    }
    fn cy(&self) -> f32 {
        self.py as f32 + (self.p as f32 - 1.0) / 2.0
    }
    /// residuals I1(x + (u, v)) - I0(x) over the patch (one set of bilinear weights for the whole patch)
    #[inline]
    fn residual(&self, u: f32, v: f32, out: &mut [f32; PATCH * PATCH]) {
        let p = self.p;
        let (x0, y0) = (self.px as f32 + u, self.py as f32 + v);
        let i1 = self.i1;
        if x0 >= 0.0 && y0 >= 0.0 && x0 + (p as f32) < (i1.w - 1) as f32 && y0 + (p as f32) < (i1.h - 1) as f32 {
            let (xi, yi) = (x0 as usize, y0 as usize);
            let (fx, fy) = (x0 - xi as f32, y0 - yi as f32);
            let (w00, w01, w10, w11) = ((1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy);
            let d = &i1.d;
            for dy in 0..p {
                let r0 = (yi + dy) * i1.w + xi;
                let r1 = r0 + i1.w;
                for dx in 0..p {
                    let v = d[r0 + dx] * w00 + d[r0 + dx + 1] * w01 + d[r1 + dx] * w10 + d[r1 + dx + 1] * w11;
                    out[dy * p + dx] = v - self.t[dy * p + dx];
                }
            }
        } else {
            for dy in 0..p {
                for dx in 0..p {
                    out[dy * p + dx] = i1.sample(x0 + dx as f32, y0 + dy as f32) - self.t[dy * p + dx];
                }
            }
        }
    }
    /// sum of squared mean-normalised differences
    fn cost(&self, u: f32, v: f32) -> f32 {
        let mut r = [0f32; PATCH * PATCH];
        self.residual(u, v, &mut r);
        let n = self.p * self.p;
        let (mut s, mut s2) = (0.0f32, 0.0f32);
        for d in &r[..n] {
            s += d;
            s2 += d * d;
        }
        s2 - s * s / n as f32
    }
    fn best(&self, cands: &[(f32, f32)]) -> (f32, f32) {
        let mut best = cands[0];
        let mut bc = self.cost(best.0, best.1);
        for &c in &cands[1..] {
            let k = self.cost(c.0, c.1);
            if k < bc {
                bc = k;
                best = c;
            }
        }
        best
    }
    /// inverse-compositional Gauss-Newton from `start`; keeps `start` when that matched better
    fn refine(&self, start: (f32, f32)) -> (f32, f32) {
        if self.det <= 1e-6 {
            return start;
        }
        let n = self.p * self.p;
        let mut r = [0f32; PATCH * PATCH];
        let (mut u, mut v) = start;
        for _ in 0..self.iters {
            self.residual(u, v, &mut r);
            let (mut b1, mut b2) = (0.0f32, 0.0f32);
            for k in 0..n {
                b1 += self.gx[k] * r[k];
                b2 += self.gy[k] * r[k];
            }
            let du = (self.h22 * b1 - self.h12 * b2) / self.det;
            let dv = (self.h11 * b2 - self.h12 * b1) / self.det;
            u -= du;
            v -= dv;
            if du * du + dv * dv < 1e-4 {
                break;
            }
        }
        if !(u.is_finite() && v.is_finite()) || self.cost(u, v) > self.cost(start.0, start.1) {
            start
        } else {
            (u, v)
        }
    }
}

// ------------------------------------------------------------------------------------------- threads

/// Run `f(row_index, row)` over the rows of `buf` (rows of `row_len` items) on all cores (rayon's pool:
/// these run many times per frame, so threads are reused rather than started per call).
pub fn par_rows<T: Send>(buf: &mut [T], row_len: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    use rayon::prelude::*;
    if row_len == 0 || buf.is_empty() {
        return;
    }
    buf.par_chunks_mut(row_len).enumerate().for_each(|(y, row)| f(y, row));
}

/// `(0..n).map(f)` on all cores, in order.
pub fn par_collect<R: Send>(n: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let mut slots: Vec<Option<R>> = (0..n).map(|_| None).collect();
    par_rows(&mut slots, 1, |i, s| s[0] = Some(f(i)));
    slots.into_iter().map(|s| s.unwrap()).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// a view of a smooth random texture, shifted by (ox, oy)
    fn texture(w: usize, h: usize, ox: f32, oy: f32) -> Gray {
        let big = noise(w + 128, h + 128, 7);
        let mut g = Gray::new(w, h);
        for y in 0..h {
            for x in 0..w {
                g.d[y * w + x] = big.sample(x as f32 + 64.0 - ox, y as f32 + 64.0 - oy);
            }
        }
        g
    }

    /// Non-periodic texture with detail at several scales (like game scenes): blurred noise octaves.
    pub(crate) fn noise(w: usize, h: usize, seed: u64) -> Gray {
        let mut g = Gray::new(w, h);
        for (i, (scale, amp)) in [(1usize, 0.35f32), (4, 0.35), (16, 0.3)].iter().enumerate() {
            let o = octave(w / scale + 2, h / scale + 2, seed * 31 + i as u64);
            for y in 0..h {
                for x in 0..w {
                    g.d[y * w + x] += amp * o.sample(x as f32 / *scale as f32, y as f32 / *scale as f32);
                }
            }
        }
        g
    }

    fn octave(w: usize, h: usize, seed: u64) -> Gray {
        let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut g = Gray::new(w, h);
        for v in g.d.iter_mut() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *v = (s >> 56) as f32;
        }
        for _ in 0..3 {
            let src = g.clone();
            for y in 0..h {
                for x in 0..w {
                    let mut a = 0.0;
                    let mut n = 0.0;
                    for yy in y.saturating_sub(2)..=(y + 2).min(h - 1) {
                        for xx in x.saturating_sub(2)..=(x + 2).min(w - 1) {
                            a += src.at(xx, yy);
                            n += 1.0;
                        }
                    }
                    g.d[y * w + x] = a / n;
                }
            }
        }
        // stretch the contrast back up
        let (lo, hi) = g.d.iter().fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
        for v in g.d.iter_mut() {
            *v = (*v - lo) / (hi - lo).max(1.0) * 200.0 + 28.0;
        }
        g
    }

    fn mean_flow(f: &Flow) -> (f32, f32) {
        // ignore a border where the shifted content leaves the frame
        let (mut u, mut v, mut n) = (0.0, 0.0, 0.0);
        for y in f.h / 6..f.h * 5 / 6 {
            for x in f.w / 6..f.w * 5 / 6 {
                u += f.u[y * f.w + x];
                v += f.v[y * f.w + x];
                n += 1.0;
            }
        }
        (u / n, v / n)
    }

    #[test]
    fn finds_small_and_large_shifts() {
        for (dx, dy) in [(1.5f32, -0.75f32), (-14.0, 6.0), (31.0, -9.0)] {
            let a = texture(240, 136, 0.0, 0.0);
            let b = texture(240, 136, dx, dy);
            let f = dis(&pyramid(a), &pyramid(b));
            let (u, v) = mean_flow(&f);
            assert!((u - dx).abs() < 0.5 && (v - dy).abs() < 0.5, "shift ({dx}, {dy}) measured ({u}, {v})");
        }
    }

    #[test]
    fn static_image_has_no_motion() {
        let a = texture(160, 90, 0.0, 0.0);
        let f = dis(&pyramid(a.clone()), &pyramid(a));
        assert!(f.u.iter().chain(&f.v).all(|x| x.abs() < 0.05));
    }

    #[test]
    fn grey_down_averages_blocks() {
        let bgr = vec![100u8; 8 * 4 * 3];
        let g = grey_down(&bgr, 8, 4, 4);
        assert_eq!((g.w, g.h), (2, 1));
        assert!(g.d.iter().all(|v| (v - 100.0).abs() < 0.01));
    }

    #[test]
    fn median9_is_the_median() {
        let mut s = 12345u64;
        for _ in 0..1000 {
            let mut v = [0f32; 9];
            for x in v.iter_mut() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                *x = (s >> 40) as f32;
            }
            let mut sorted = v;
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(median9(&mut v), sorted[4]);
        }
    }

    #[test]
    fn par_collect_keeps_order() {
        assert_eq!(par_collect(100, |i| i * 2), (0..100).map(|i| i * 2).collect::<Vec<_>>());
    }
}
