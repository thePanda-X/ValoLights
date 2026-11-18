//! Track kill feed rows over time and turn new rows into kill candidates.
use crate::detectors::killfeed::{self as kf, KfFrame, RowBox};
use crate::detectors::sc64;
use crate::imgproc::{self, Gray, Interp};
use crate::layout::Layout;
use crate::{names, ocr};
use std::collections::HashSet;
use vl_core::image::BgrImage;

fn ti(v: f64) -> i64 {
    v.trunc() as i64
}

fn norm_vec(mut v: Vec<f32>) -> Option<Vec<f32>> {
    let m = (v.iter().map(|&x| x as f64).sum::<f64>() / v.len().max(1) as f64) as f32;
    v.iter_mut().for_each(|x| *x -= m);
    let n = v.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt() as f32;
    if n > 1e-6 {
        v.iter_mut().for_each(|x| *x /= n);
        Some(v)
    } else {
        None
    }
}

pub fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| x * y).sum::<f32>() as f64
}

/// Appearance signature of the right-aligned part of a row (victim side + weapon area).
fn sig(roi: &BgrImage, r: &RowBox, lay: &Layout) -> Vec<f32> {
    let w = ti(lay.u(0.42));
    let x0 = r.x0.max(r.x1 - w);
    let g = imgproc::bgr2gray(&imgproc::slice(roi, r.y0, r.y1, x0, r.x1));
    let g = imgproc::resize_gray(&g, 96, 12, Interp::Area);
    let v: Vec<f32> = g.data.iter().map(|&x| x as f32).collect();
    match norm_vec(v.clone()) {
        Some(n) => n,
        None => {
            let m = (v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64) as f32;
            v.iter().map(|x| x - m).collect()
        }
    }
}

fn opacity(f: &KfFrame, r: &RowBox) -> f64 {
    let h = kf::hsv_slice(&f.hsv, r.y0, r.y1, r.x0, r.x1);
    if h.is_empty() {
        return f64::NAN;
    }
    // numpy: (s.astype(float32) * v).mean() in float32
    let p: Vec<f32> = h.sat.iter().zip(&h.val).map(|(&s, &v)| s as f32 * v as f32).collect();
    imgproc::np_mean_f32(&p) as f64 / (255.0 * 255.0)
}

#[derive(Debug, Clone)]
pub struct Obs {
    pub t: f64,
    pub bx: RowBox,
    pub sig: Vec<f32>,
    pub opacity: f64,
    pub killer_crop: Option<BgrImage>,
    pub victim_crop: Option<BgrImage>,
    pub mods_crop: Option<BgrImage>,
    pub victim_portrait: BgrImage,
    pub killer_portrait: Option<BgrImage>,
    pub row_img: BgrImage,
    pub vhash: Option<Vec<u8>>,
    pub patch: Gray,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: usize,
    pub obs: Vec<Obs>,
    pub width: i64,
}

impl Track {
    pub fn first_t(&self) -> f64 {
        self.obs[0].t
    }
    pub fn last_t(&self) -> f64 {
        self.obs[self.obs.len() - 1].t
    }
    pub fn sig(&self) -> &[f32] {
        &self.obs[self.obs.len() - 1].sig
    }
}

#[derive(Debug, Clone)]
pub struct KillCandidate {
    pub t: f64,
    pub t_last: f64,
    pub n_obs: usize,
    pub killer_text: String,
    pub killer_score: f64,
    pub victim_text: String,
    pub victim_score: f64,
    pub killer_hash: Vec<u8>,
    pub victim_hash: Vec<u8>,
    pub killer_color: &'static str,
    pub victim_color: &'static str,
    pub gold: bool,
    pub headshot: bool,
    pub mods_crop: Option<BgrImage>,
    pub victim_portrait: Option<BgrImage>,
    pub killer_portrait: Option<BgrImage>,
    pub killer_crop: Option<BgrImage>,
    pub victim_crop: Option<BgrImage>,
    pub sig: Vec<f32>,
    pub width: i64,
    pub row_img: Option<BgrImage>,
    /// observation times in ms (t rounded to 3 decimals)
    pub obs_times: HashSet<i64>,
    pub vport: Option<Vec<f32>>,
    pub first_box: Option<RowBox>,
    /// grayscale row patch at first sighting
    pub first_img: Option<Gray>,
}

/// 12x12 colour thumbnail, normalised (agent portrait similarity).
pub fn portrait_sig(img: Option<&BgrImage>) -> Option<Vec<f32>> {
    let img = img.filter(|i| !i.is_empty())?;
    let r = imgproc::resize(img, 12, 12, Interp::Area);
    norm_vec(r.data.iter().map(|&v| v as f32).collect())
}

fn pad_crop(roi: &BgrImage, r: &RowBox, span: Option<(i64, i64)>, lay: &Layout) -> Option<BgrImage> {
    let (s0, s1) = span?;
    let pad = ti(lay.u(0.006));
    let a = 0.max(s0 - pad);
    let b = (roi.w as i64).min(s1 + pad);
    let c = imgproc::slice(roi, r.y0, r.y1, a, b);
    (!c.is_empty()).then_some(c)
}

pub type HeadshotFn = fn(Option<&BgrImage>) -> bool;

pub struct KillfeedTracker {
    pub lay: Layout,
    pub active: Vec<Track>,
    pub done: Vec<Track>,
    next: usize,
    pub headshot_fn: Option<HeadshotFn>,
}

impl KillfeedTracker {
    /// seconds a track may be unseen (occlusion, flashes)
    pub const MAX_GAP: f64 = 1.2;
    /// signature correlation threshold
    pub const MATCH: f64 = 0.6;

    pub fn new(lay: Layout, headshot_fn: Option<HeadshotFn>) -> Self {
        KillfeedTracker { lay, active: vec![], done: vec![], next: 0, headshot_fn }
    }

    /// Row observations of one kill feed ROI (pure: safe to run on several frames in parallel).
    pub fn observe(t: f64, roi: &BgrImage, lay: &Layout) -> Vec<Obs> {
        let f = KfFrame::new(roi);
        let rows = kf::segment_rows_ctx(&f, lay);
        rows.into_iter()
            .map(|r| {
                let p = kf::parse_row_ctx(&f, &r, lay);
                let victim_crop = pad_crop(roi, &r, p.victim_name, lay);
                let vhash = victim_crop.as_ref().map(|c| names::name_hash(Some(c)));
                let rx0 = p.killer_portrait.map(|k| k.0).unwrap_or(r.x0);
                Obs {
                    t,
                    sig: sig(roi, &r, lay),
                    opacity: opacity(&f, &r),
                    killer_crop: pad_crop(roi, &r, p.killer_name, lay),
                    victim_crop,
                    mods_crop: pad_crop(roi, &r, p.mods, lay),
                    victim_portrait: imgproc::slice(roi, r.y0, r.y1, p.victim_portrait.0, p.victim_portrait.1),
                    killer_portrait: pad_crop(roi, &r, p.killer_portrait, lay),
                    row_img: imgproc::slice(roi, 0.max(r.y0 - 1), r.y1 + 1, 0.max(rx0 - 1), r.x1 + 1),
                    vhash,
                    patch: imgproc::bgr2gray(&imgproc::slice(roi, r.y0, r.y1, 0.max(r.x1 - ti(lay.u(0.42))), r.x1)),
                    bx: r,
                }
            })
            .collect()
    }

    pub fn feed(&mut self, t: f64, roi: &BgrImage) -> Vec<RowBox> {
        let obs = Self::observe(t, roi, &self.lay);
        let rows = obs.iter().map(|o| o.bx.clone()).collect();
        self.feed_obs(t, obs);
        rows
    }

    pub fn feed_obs(&mut self, t: f64, obs: Vec<Obs>) {
        // greedy matching
        let mut pairs: Vec<(f64, usize, usize)> = vec![];
        for (i, o) in obs.iter().enumerate() {
            for (j, tr) in self.active.iter().enumerate() {
                let last = &tr.obs[tr.obs.len() - 1];
                if (last.bx.killer_color, last.bx.victim_color) != (o.bx.killer_color, o.bx.victim_color) {
                    continue;
                }
                if (tr.width - o.bx.w()).abs() as f64 > 8f64.max(0.25 * tr.width as f64) {
                    continue;
                }
                let c = dot(tr.sig(), &o.sig);
                let gap = t - tr.last_t();
                let need = if gap < 0.4 { Self::MATCH } else { 0.8 };
                if c < need {
                    continue;
                }
                if let (Some(lv), Some(ov)) = (&last.vhash, &o.vhash) {
                    if names::hash_distance(lv, ov) > 0.25 {
                        continue;
                    }
                }
                pairs.push((c, i, j));
            }
        }
        // best pairs first (descending)
        pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
        let mut used_o = vec![false; obs.len()];
        let mut used_t = vec![false; self.active.len()];
        let mut assign: Vec<Option<usize>> = vec![None; obs.len()];
        for &(_, i, j) in &pairs {
            if used_o[i] || used_t[j] {
                continue;
            }
            used_o[i] = true;
            used_t[j] = true;
            assign[i] = Some(j);
        }
        let mut new_tracks = vec![];
        for (i, o) in obs.into_iter().enumerate() {
            match assign[i] {
                Some(j) => {
                    let tr = &mut self.active[j];
                    tr.width = ti(0.7 * tr.width as f64 + 0.3 * o.bx.w() as f64);
                    tr.obs.push(o);
                }
                None => {
                    let w = o.bx.w();
                    new_tracks.push(Track { id: self.next, obs: vec![o], width: w });
                    self.next += 1;
                }
            }
        }
        self.active.extend(new_tracks);
        // retire stale tracks
        let mut keep = vec![];
        for tr in std::mem::take(&mut self.active) {
            if t - tr.last_t() > Self::MAX_GAP {
                self.done.push(tr);
            } else {
                keep.push(tr);
            }
        }
        self.active = keep;
    }

    pub fn finish(&mut self) -> Vec<KillCandidate> {
        let act = std::mem::take(&mut self.active);
        self.done.extend(act);
        let tracks: Vec<&Track> = self.done.iter().filter(|tr| tr.obs.len() >= 2).collect();
        let hs = self.headshot_fn;
        // OCR per track is independent: run it on a few threads (results are identical to a sequential run)
        let mut out: Vec<KillCandidate> = par_map(&tracks, 6, |tr| candidate(tr, hs));
        out.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        merge_candidates(out, 7.0)
    }
}

/// Order-preserving parallel map over a slice with a small number of worker threads.
pub fn par_map<T: Sync, R: Send>(items: &[T], max_threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, max_threads.max(1)).min(n.max(1));
    if threads <= 1 {
        return items.iter().map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<R>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= n {
                    break;
                }
                let r = f(&items[i]);
                *results[i].lock().unwrap() = Some(r);
            });
        }
    });
    results.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

fn candidate(tr: &Track, headshot_fn: Option<HeadshotFn>) -> KillCandidate {
    // OCR on the most opaque observations (fully faded-in rows)
    let mut best: Vec<&Obs> = tr.obs.iter().collect();
    best.sort_by(|a, b| b.opacity.partial_cmp(&a.opacity).unwrap_or(std::cmp::Ordering::Equal));
    best.truncate(3);
    let (mut kv, mut vv) = (vec![], vec![]);
    for o in &best {
        if let Some(c) = &o.killer_crop {
            let (t, s) = ocr::read_line(c);
            kv.push((t, sc64(s)));
        }
        if let Some(c) = &o.victim_crop {
            let (t, s) = ocr::read_line(c);
            vv.push((t, sc64(s)));
        }
    }
    let (kt, ks) = names::best_text(&kv);
    let (vt, vs) = names::best_text(&vv);
    let top = best[0];
    let hs_votes: Vec<bool> = match headshot_fn {
        Some(f) => best.iter().filter(|o| o.mods_crop.is_some()).map(|o| f(o.mods_crop.as_ref())).collect(),
        None => vec![],
    };
    let headshot = !hs_votes.is_empty() && hs_votes.iter().filter(|&&v| v).count() * 2 > hs_votes.len();
    let gold = tr.obs.iter().filter(|o| o.bx.gold).count() * 2 >= tr.obs.len();
    let kc = best.iter().find_map(|o| o.killer_crop.clone());
    let vc = best.iter().find_map(|o| o.victim_crop.clone());
    KillCandidate {
        t: tr.first_t(),
        t_last: tr.last_t(),
        n_obs: tr.obs.len(),
        killer_text: kt,
        killer_score: ks,
        victim_text: vt,
        victim_score: vs,
        killer_hash: names::name_hash(kc.as_ref()),
        victim_hash: names::name_hash(vc.as_ref()),
        killer_color: top.bx.killer_color,
        victim_color: top.bx.victim_color,
        gold,
        headshot,
        mods_crop: top.mods_crop.clone(),
        victim_portrait: Some(top.victim_portrait.clone()),
        killer_portrait: top.killer_portrait.clone(),
        killer_crop: kc,
        victim_crop: vc,
        sig: top.sig.clone(),
        width: tr.width,
        row_img: Some(top.row_img.clone()),
        obs_times: tr.obs.iter().map(|o| (o.t * 1000.0).round() as i64).collect(),
        vport: portrait_sig(Some(&top.victim_portrait)),
        first_box: Some(tr.obs[0].bx.clone()),
        first_img: Some(tr.obs[0].patch.clone()),
    }
}

fn same_name(ta: &str, ha: &[u8], tb: &str, hb: &[u8], hash_tol: f64) -> bool {
    let (na, nb) = (names::normalize(ta), names::normalize(tb));
    if !na.is_empty() && !nb.is_empty() {
        if names::similarity(ta, tb) >= 0.8 {
            return true;
        }
        // one reading may contain junk from the portrait/assist icons ("IIKoReSh")
        let (short, long) = if nb.chars().count() < na.chars().count() { (&nb, &na) } else { (&na, &nb) };
        return short.chars().count() >= 3 && long.contains(short.as_str());
    }
    names::hash_distance(ha, hb) <= hash_tol
}

fn pdot(a: &Option<Vec<f32>>, b: &Option<Vec<f32>>) -> Option<f64> {
    Some(dot(a.as_ref()?, b.as_ref()?))
}

/// Merge fragments of the same kill feed row (tracking breaks on occlusion/shift).
///
/// A victim can only die once within a few seconds, so fragments with the same victim and colours are the same
/// row even if one killer reading is junk.
pub fn merge_candidates(cands: Vec<KillCandidate>, window: f64) -> Vec<KillCandidate> {
    let mut kept: Vec<KillCandidate> = vec![];
    for c in cands {
        let mut hit = None;
        for (ki, k) in kept.iter().enumerate().rev() {
            if c.t - k.t > window {
                break;
            }
            if (k.killer_color, k.victim_color) != (c.killer_color, c.victim_color) {
                continue;
            }
            if c.t - k.t_last > 2.5 {
                continue;
            }
            if !k.obs_times.is_disjoint(&c.obs_times) {
                continue; // both rows visible in the same frame -> different kills
            }
            let mut same_victim = same_name(&k.victim_text, &k.victim_hash, &c.victim_text, &c.victim_hash, 0.2);
            if !same_victim && k.victim_score.min(c.victim_score) < 0.75 && pdot(&k.vport, &c.vport).map(|d| d >= 0.85).unwrap_or(false) {
                same_victim = true; // unreadable (e.g. non-Latin) name: same agent portrait
            }
            let killer_same = same_name(&k.killer_text, &k.killer_hash, &c.killer_text, &c.killer_hash, 0.2);
            if !same_victim {
                // Two kills by the same player always overlap on screen (rows live ~5 s), so a non-overlapping
                // fragment with a confidently-read identical killer is the same row whose victim name was obscured.
                let strong = k.killer_score.min(c.killer_score) >= 0.6 && k.victim_score.min(c.victim_score) < 0.6;
                let close = (c.t - k.t_last) <= 1.0;
                let port = pdot(&k.vport, &c.vport).map(|d| d >= 0.6).unwrap_or(true);
                if !(killer_same && strong && close && port) {
                    continue;
                }
            }
            let weak = k.killer_score.min(c.killer_score) < 0.6 || k.n_obs.min(c.n_obs) <= 6;
            if killer_same || weak {
                hit = Some(ki);
                break;
            }
        }
        let Some(hi) = hit else {
            kept.push(c);
            continue;
        };
        let h = &mut kept[hi];
        h.t_last = h.t_last.max(c.t_last);
        h.n_obs += c.n_obs;
        h.obs_times.extend(c.obs_times.iter().copied());
        if c.killer_score > h.killer_score {
            h.killer_text = c.killer_text.clone();
            h.killer_score = c.killer_score;
            h.killer_crop = c.killer_crop.clone();
            h.killer_hash = c.killer_hash.clone();
        }
        if c.victim_score > h.victim_score {
            h.victim_text = c.victim_text.clone();
            h.victim_score = c.victim_score;
            h.victim_crop = c.victim_crop.clone();
            h.victim_hash = c.victim_hash.clone();
        }
        h.gold = h.gold || c.gold;
        h.headshot = h.headshot || c.headshot;
    }
    // drop junk fragments: very short, low-confidence victim reading, overlapping a solid row by the same killer
    let mut out = vec![];
    for (ci, c) in kept.iter().enumerate() {
        let short = (c.t_last - c.t) < 1.0;
        if short
            && c.victim_score < 0.5
            && kept.iter().enumerate().any(|(oi, o)| {
                oi != ci
                    && o.t <= c.t
                    && c.t <= o.t_last + 0.5
                    && same_name(&o.killer_text, &o.killer_hash, &c.killer_text, &c.killer_hash, 0.2)
            })
        {
            continue;
        }
        out.push(c.clone());
    }
    out
}
