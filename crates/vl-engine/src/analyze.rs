//! Full analysis pipeline: video -> events.json.
use crate::detectors::{hud, killfeed as kf, sc64, topbar};
use crate::imgproc::{self, Interp};
use crate::layout::Layout;
use crate::rounds::{self as rounds_mod, most_common};
use crate::tracker::{KillCandidate, KillfeedTracker, Obs};
use crate::{identity, names};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use vl_core::config::Config;
use vl_core::ffmpeg::{self as ff, ProbeInfo, ReadOpts};
use vl_core::image::BgrImage;
use vl_core::jobs::{Cancel, Cancelled, Progress};
use vl_core::models::{Events, GameArea, Kill};

pub const MAX_ANALYSIS_H: i32 = 1080;
/// Low-res video (e.g. 360p VODs) is enlarged for analysis: the kill feed parser's gaps and thresholds are tuned
/// for 720p and up; at 360p a gap between name and gun is under a pixel and names fall apart into single letters.
pub const MIN_ANALYSIS_H: i32 = 720;

/// Round to n decimals, half to even.
fn round_n(x: f64, n: i32) -> f64 {
    let p = 10f64.powi(n);
    (x * p).round_ties_even() / p
}

pub fn game_area(src: &Path, info: &ProbeInfo) -> Result<GameArea> {
    let (x, y, w, h) = ff::detect_crop(src, info, 9)?;
    Ok(GameArea { x, y, w, h, hud_scale: 1.0 })
}

/// Analysis resolution for a game area: (w, h, scale factor applied to source px).
pub fn scaled(ga: &GameArea) -> (i64, i64, f64) {
    if (MIN_ANALYSIS_H..=MAX_ANALYSIS_H).contains(&ga.h) {
        return ((ga.w / 2 * 2) as i64, (ga.h / 2 * 2) as i64, 1.0);
    }
    let target = if ga.h > MAX_ANALYSIS_H { MAX_ANALYSIS_H } else { MIN_ANALYSIS_H };
    let s = target as f64 / ga.h as f64;
    (((ga.w as f64 * s / 2.0).round_ties_even() * 2.0) as i64, target as i64, s)
}

/// Output width for a crop scaled to `scale_h` (crop snapped to even values).
fn scaled_width(crop_w: i32, crop_h: i32, scale_h: u32) -> u32 {
    let (w, h) = (crop_w / 2 * 2, crop_h / 2 * 2);
    ((w as f64 * scale_h as f64 / h as f64 / 2.0).round_ties_even() * 2.0) as u32
}

/// Earliest native frame where the row is (fading) in, found by correlating its fixed screen patch.
pub fn refine_time(src: &Path, ga: &GameArea, lay: &Layout, scale: f64, cand: &KillCandidate, fps: f64) -> f64 {
    let (Some(b), Some(first_img)) = (&cand.first_box, &cand.first_img) else { return cand.t };
    if first_img.is_empty() {
        return cand.t;
    }
    let (rx, ry, rw, rh) = lay.rect("killfeed");
    let (sx, sy) = (ga.x as f64 + rx as f64 / scale, ga.y as f64 + ry as f64 / scale);
    let crop = (sx as i32, sy as i32, (rw as f64 / scale) as i32, (rh as f64 / scale) as i32);
    let x0 = 0.max(b.x1 - (lay.u(0.42)).trunc() as i64);
    let t0 = (cand.t - 0.8).max(0.0);
    let first = cand.t;
    let scale_h = (scale != 1.0).then_some((rh / 2 * 2) as u32);
    let opts = ReadOpts {
        fps: fps.min(30.0),
        crop: Some(crop),
        scale_h,
        scale_w: scale_h.map(|sh| scaled_width(crop.2, crop.3, sh)),
        start: t0,
        duration: Some(cand.t - t0 + 0.02),
        // GPU decode, only the sampled frames leave the GPU (far quicker than CPU decoding 8 at once)
        hwaccel: true,
        matrix: None,
    };
    let frames: Vec<ff::Frame> = match ff::read_frames(src, opts, None) {
        Ok(r) => match r.collect::<Result<Vec<_>>>() {
            Ok(v) => v,
            Err(e) => {
                log::debug!("refine failed at {:.2}: {e}", cand.t);
                return first;
            }
        },
        Err(e) => {
            log::debug!("refine failed at {:.2}: {e}", cand.t);
            return first;
        }
    };
    for fr in frames {
        let mut img = fr.img;
        if img.h as i64 != rh {
            img = imgproc::resize(&img, rw as usize, rh as usize, Interp::Linear);
        }
        let g = imgproc::bgr2gray(&imgproc::slice(&img, b.y0, b.y1, x0, b.x1));
        if g.w == first_img.w && g.h == first_img.h {
            let (a, c): (Vec<f32>, Vec<f32>) =
                (g.data.iter().map(|&v| v as f32).collect(), first_img.data.iter().map(|&v| v as f32).collect());
            if imgproc::ncc(&a, &c) >= 0.6 {
                return first.min(fr.t);
            }
        }
    }
    first
}

fn fmt_mmss(t: f64) -> String {
    format!("{}:{:02}", (t / 60.0).floor() as i64, (t % 60.0).floor() as i64)
}

/// Per-frame results computed off the main thread.
struct FrameWork {
    t: f64,
    obs: Vec<Obs>,
    hud: Option<HudRead>,
    /// red share of the timer area (spike icon)
    spike: f32,
}

struct HudRead {
    tb: topbar::TopbarReading,
    ammo: Option<i32>,
    spect: Option<(String, f32)>,
}

pub fn analyze(src: &Path, project_dir: &Path, progress: Progress, cancel: &Cancel) -> Result<(Events, GameArea, ProbeInfo)> {
    analyze_with(src, project_dir, progress, cancel, &vl_core::config::load())
}

/// `analyze` with an explicit configuration (the evaluator overrides the aliases).
pub fn analyze_with(
    src: &Path,
    project_dir: &Path,
    progress: Progress,
    cancel: &Cancel,
    cfg: &Config,
) -> Result<(Events, GameArea, ProbeInfo)> {
    let prog = progress;
    let t_start = Instant::now();
    prog(0.01, "Reading video info");
    let info = ff::probe(src)?;
    prog(0.02, "Detecting game area");
    let ga = game_area(src, &info)?;
    let (aw, ah, scale) = scaled(&ga);
    let lay = Layout { width: aw, height: ah, scale: 1.0, soft: scale > 1.0 };
    log::info!("game area {:?} -> analysis {aw}x{ah} (scale {scale:.3})", ga.rect());

    let tracker0 = KillfeedTracker::new(lay, Some(kf::is_headshot));
    let mut tracker: Option<KillfeedTracker> = None;
    let fps = cfg.analysis.sample_fps;
    let hud_every = 1.max((fps / cfg.analysis.hud_fps).round_ties_even() as usize);
    let mut scores: Vec<(f64, i32, i32)> = vec![];
    let mut left_colors: Vec<&'static str> = vec![];
    let mut timer: Vec<(f64, i32)> = vec![];
    let mut ammo: Vec<(f64, i32)> = vec![];
    let mut spect: Vec<(f64, String)> = vec![];
    let mut spike: Vec<(f64, f32)> = vec![];
    let (kx, ky, kw, kh) = lay.rect("killfeed");
    let scale_h = (scale != 1.0).then_some(ah as u32);
    let crop = ga.rect();
    let opts = ReadOpts {
        fps,
        crop: Some(crop),
        scale_h,
        scale_w: scale_h.map(|sh| scaled_width(crop.2, crop.3, sh)),
        hwaccel: true,
        ..Default::default()
    };
    let reader = ff::read_frames(src, opts, Some(cancel.0.clone()))?;

    // Decode on this thread; per-frame detection runs on worker threads (results are consumed in frame order,
    // so the tracker sees exactly the sequential stream).
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<(usize, f64, BgrImage)>(workers * 2);
    let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    let (res_tx, res_rx) = std::sync::mpsc::channel::<(usize, FrameWork)>();
    let mut read_err: Option<anyhow::Error> = None;
    std::thread::scope(|s| {
        for _ in 0..workers {
            let job_rx = job_rx.clone();
            let res_tx = res_tx.clone();
            s.spawn(move || loop {
                let job = { job_rx.lock().unwrap().recv() };
                let Ok((i, t, img)) = job else { break };
                let roi = img.crop(kx, ky, kw, kh);
                let obs = KillfeedTracker::observe(t, &roi, &lay);
                let hud = (i % hud_every == 0).then(|| HudRead {
                    tb: topbar::read(&img, &lay),
                    ammo: hud::read_ammo(&img, &lay),
                    spect: (i % (hud_every * 2) == 0).then(|| hud::read_spectate_name(&img, &lay)),
                });
                let spike = topbar::spike_share(&img, &lay);
                if res_tx.send((i, FrameWork { t, obs, hud, spike })).is_err() {
                    break;
                }
            });
        }
        drop(res_tx);
        let consumer = s.spawn(move || {
            let mut pending: HashMap<usize, FrameWork> = HashMap::new();
            let mut next = 0usize;
            let mut tr = tracker0;
            let (mut sc, mut lc, mut tm, mut am, mut sp) = (vec![], vec![], vec![], vec![], vec![]);
            let mut sk: Vec<(f64, f32)> = vec![];
            for (i, w) in res_rx.iter() {
                pending.insert(i, w);
                while let Some(w) = pending.remove(&next) {
                    tr.feed_obs(w.t, w.obs);
                    sk.push((w.t, w.spike));
                    if let Some(h) = w.hud {
                        if let (Some(l), Some(r)) = (h.tb.left, h.tb.right) {
                            sc.push((w.t, l, r));
                        }
                        if !h.tb.left_color.is_empty() {
                            lc.push(h.tb.left_color);
                        }
                        if let Some(tv) = h.tb.timer {
                            tm.push((w.t, tv));
                        }
                        if let Some(a) = h.ammo {
                            am.push((round_n(w.t, 2), a));
                        }
                        if let Some((name, s)) = h.spect {
                            if sc64(s) >= 0.75 && name.chars().count() >= 2 {
                                sp.push((round_n(w.t, 2), name));
                            }
                        }
                    }
                    next += 1;
                }
            }
            (tr, sc, lc, tm, am, sp, sk)
        });
        let mut i = 0usize;
        for fr in reader {
            let fr = match fr {
                Ok(f) => f,
                Err(e) => {
                    read_err = Some(e);
                    break;
                }
            };
            let t = fr.t;
            if job_tx.send((i, t, fr.img)).is_err() {
                break;
            }
            i += 1;
            if i.is_multiple_of(30) {
                prog(
                    0.03 + 0.75 * (t / info.duration.max(1e-6)).min(1.0),
                    &format!("Scanning {} of {}", fmt_mmss(t), fmt_mmss(info.duration)),
                );
            }
        }
        drop(job_tx);
        let (tr, sc, lc, tm, am, sp, sk) = consumer.join().expect("analysis worker panicked");
        spike = sk;
        tracker = Some(tr);
        scores = sc;
        left_colors = lc;
        timer = tm;
        ammo = am;
        spect = sp;
    });
    if cancel.is_set() {
        return Err(Cancelled.into());
    }
    if let Some(e) = read_err {
        return Err(e);
    }

    let t_scan = t_start.elapsed().as_secs_f64();
    prog(0.80, "Reading names");
    let mut cands = tracker.expect("tracker").finish();
    let t_names = t_start.elapsed().as_secs_f64();
    if cfg.analysis.refine {
        let fps_native = if info.fps > 0.0 { info.fps } else { 60.0 };
        let done = std::sync::atomic::AtomicUsize::new(0);
        let n = cands.len();
        let times = crate::tracker::par_map(&cands, 8, |c| {
            if cancel.is_set() {
                return c.t;
            }
            let t = refine_time(src, &ga, &lay, scale, c, fps_native);
            let j = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if j.is_multiple_of(5) {
                prog(0.85 + 0.10 * j as f64 / 1.max(n) as f64, "Refining kill times");
            }
            t
        });
        if cancel.is_set() {
            return Err(Cancelled.into());
        }
        for (c, t) in cands.iter_mut().zip(times) {
            c.t = t;
        }
    }

    let t_refine = t_start.elapsed().as_secs_f64();
    log::info!("timing: scan {t_scan:.1}s, names {:.1}s, refine {:.1}s ({} candidates)", t_names - t_scan, t_refine - t_names, cands.len());
    if let Ok(dbg) = std::env::var("VL_DUMP_CANDS") {
        // debugging aid: the candidates before identity resolution
        let v: Vec<serde_json::Value> = cands
            .iter()
            .map(|c| {
                serde_json::json!({"t": c.t, "t_last": c.t_last, "n_obs": c.n_obs, "kt": c.killer_text, "ks": c.killer_score,
                "vt": c.victim_text, "vs": c.victim_score, "kc": c.killer_color, "vc": c.victim_color, "gold": c.gold, "hs": c.headshot})
            })
            .collect();
        let _ = std::fs::write(dbg, serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    prog(0.96, "Finishing");
    let mut ev = build_events(&cands, project_dir, lay.soft)?;
    ev.timer = timer;
    ev.ammo = ammo;
    ev.scores = scores.clone();
    ev.spectating = spect;
    let left_color = most_common(left_colors.iter().copied()).map(|x| x.0).unwrap_or("");
    let states = rounds_mod::smooth_scores(&scores, 3, 6);
    ev.rounds = rounds_mod::build_rounds(&states, info.duration, left_color);
    // round ends (post-round countdown) and spike plants (spike icon), then the end-of-round banners
    rounds_mod::attach(&mut ev.rounds, &rounds_mod::round_ends(&ev.timer), &rounds_mod::spike_plants(&spike));
    prog(0.97, "Reading round banners");
    let decided: Vec<(usize, f64)> = ev.rounds.iter().enumerate().filter_map(|(i, r)| r.decided.map(|d| (i, d))).collect();
    let texts = crate::tracker::par_map(&decided, 4, |&(_, d)| read_round_banner(src, &ga, &lay, scale, d));
    for (&(i, d), text) in decided.iter().zip(texts) {
        let tags = banner_tags(&text);
        log::info!("round {} decided at {d:.1}s: banner {text:?} -> {tags:?}", i + 1);
        let rd = &mut ev.rounds[i];
        for t in tags {
            if !rd.banners.contains(&t) {
                rd.banners.push(t);
            }
        }
        ev.banners.push(vl_core::models::Banner { t: d, text: text.clone(), kind: "round_end".into() });
    }
    for rd in &ev.rounds {
        if let Some(p) = rd.planted {
            ev.banners.push(vl_core::models::Banner { t: p, text: "Spike planted".into(), kind: "spike_planted".into() });
        }
    }
    ev.banners.sort_by(|a, b| a.t.total_cmp(&b.t));
    let rounds = ev.rounds.clone();
    for k in &mut ev.kills {
        k.round = rounds_mod::round_of(k.t, &rounds);
    }
    identity::assign_teams(&mut ev);
    if lay.soft {
        soft_identity(&mut ev);
    }
    finalize_identity(&mut ev, &cfg.identity.aliases);
    ev.engine_version = vl_core::VERSION.to_string();
    log::info!("analysis done in {:.1}s: {} kills, {} rounds", t_start.elapsed().as_secs_f64(), ev.kills.len(), ev.rounds.len());
    prog(1.0, "Done");
    Ok((ev, ga, info))
}

/// Low-res video: OCR scraps ("1", "M", a stray "Me") must not name players or claim to be the user.
/// "Me" = the player with the most gold rows (only the local player's kills are gold); unreadable names become
/// Ally / Enemy n (teal = the recording player's team).
pub fn soft_identity(ev: &mut Events) {
    let me = most_common(ev.kills.iter().filter(|k| k.gold && k.killer != k.victim).map(|k| k.killer.clone()))
        .filter(|(_, n)| *n >= 2)
        .map(|(p, _)| p);
    // side counts per player, in first-seen order (Counter semantics)
    let mut side: HashMap<String, Vec<(&'static str, usize)>> = HashMap::new();
    let mut bump = |p: &str, col: &'static str| {
        let e = side.entry(p.to_string()).or_default();
        match e.iter_mut().find(|x| x.0 == col) {
            Some(x) => x.1 += 1,
            None => e.push((col, 1)),
        }
    };
    for k in &ev.kills {
        let ks: &'static str = if k.killer_side == "red" {
            "red"
        } else if k.killer_side == "teal" {
            "teal"
        } else {
            "other"
        };
        bump(&k.killer, ks);
        bump(&k.victim, if k.killer_side == "teal" { "red" } else { "teal" });
    }
    let (mut n_teal, mut n_red) = (0, 0);
    let mut order: Vec<usize> = (0..ev.players.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(ev.players[i].seen));
    for i in order {
        let p = &mut ev.players[i];
        let is_me = me.as_deref() == Some(p.id.as_str());
        p.names.retain(|x| is_me || !names::is_me_text(x));
        let readable: Vec<String> = p.names.iter().filter(|x| names::normalize(x).chars().count() >= 3).cloned().collect();
        if is_me {
            p.me_hint = "gold".into();
            p.display = "Me".into();
        } else if let Some(r) = readable.first() {
            p.display = r.clone();
        } else {
            let col = side
                .get(&p.id)
                .and_then(|v| {
                    let mut best: Option<&(&str, usize)> = None;
                    for x in v {
                        if best.map(|b| x.1 > b.1).unwrap_or(true) {
                            best = Some(x);
                        }
                    }
                    best.map(|b| b.0)
                })
                .unwrap_or("teal");
            if col == "teal" {
                n_teal += 1;
                p.display = format!("Ally {n_teal}");
            } else {
                // the counter is keyed by the colour string; anything but teal is labelled Enemy
                n_red += 1;
                p.display = format!("Enemy {n_red}");
            }
        }
    }
}

fn save_png(img: &BgrImage, path: &Path) -> Result<()> {
    img.save(path)
}

pub fn build_events(cands: &[KillCandidate], project_dir: &Path, soft: bool) -> Result<Events> {
    let mut reg = identity::PlayerRegistry::new();
    let mut kills = vec![];
    let pdir = project_dir.join("cache").join("portraits");
    std::fs::create_dir_all(&pdir)?;
    let ndir = project_dir.join("cache").join("names");
    std::fs::create_dir_all(&ndir)?;
    let mut cands: Vec<&KillCandidate> = cands.iter().collect();
    cands.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    let mut groups: Vec<Option<String>> = vec![None; 2 * cands.len()];
    if soft {
        // names unreadable: players by team colour + agent portrait, across the whole match
        let items: Vec<identity::Sighting> = cands
            .iter()
            .flat_map(|c| {
                [(c.victim_color, c.victim_portrait.as_ref(), false, false), (c.killer_color, c.killer_portrait.as_ref(), true, c.gold)]
            })
            .collect();
        groups = identity::portrait_groups(&items);
    }
    for (n, c) in cands.iter().enumerate() {
        let victim = reg.resolve(
            &c.victim_text,
            c.victim_score,
            Some(&c.victim_hash),
            c.victim_crop.as_ref(),
            false,
            groups[2 * n].as_deref(),
            c.victim_portrait.as_ref(),
        );
        let killer = if c.killer_color == c.victim_color {
            // same colour on both sides: self-death (e.g. Clove's ult expiring, fall damage)
            victim.clone()
        } else {
            reg.resolve(
                &c.killer_text,
                c.killer_score,
                Some(&c.killer_hash),
                c.killer_crop.as_ref(),
                c.gold,
                groups[2 * n + 1].as_deref(),
                c.killer_portrait.as_ref(),
            )
        };
        let kid = format!("k{}", n + 1);
        let (mut vp, mut kp, mut rp) = (None, None, None);
        if let Some(p) = c.victim_portrait.as_ref().filter(|p| !p.is_empty()) {
            save_png(p, &pdir.join(format!("{kid}_v.png")))?;
            vp = Some(format!("cache/portraits/{kid}_v.png"));
        }
        if let Some(p) = c.killer_portrait.as_ref().filter(|p| !p.is_empty()) {
            save_png(p, &pdir.join(format!("{kid}_k.png")))?;
            kp = Some(format!("cache/portraits/{kid}_k.png"));
        }
        if let Some(r) = c.row_img.as_ref().filter(|p| !p.is_empty()) {
            let rdir = project_dir.join("cache").join("rows");
            std::fs::create_dir_all(&rdir)?;
            save_png(r, &rdir.join(format!("{kid}.png")))?;
            rp = Some(format!("cache/rows/{kid}.png"));
        }
        let conf = 1f64.min(0.4 + 0.02 * c.n_obs as f64 + 0.3 * c.victim_score.max(0.0));
        kills.push(Kill {
            id: kid,
            t: round_n(c.t, 3),
            t_conf: 1.0,
            round: -1,
            killer,
            victim,
            killer_side: c.killer_color.to_string(),
            headshot: c.headshot,
            modifiers: vec![],
            gold: c.gold,
            confidence: round_n(conf, 2),
            victim_portrait: vp,
            killer_portrait: kp,
            row_image: rp,
            manual: false,
        });
    }
    let mut players = reg.players();
    for (cl, p) in reg.clusters.iter().zip(players.iter_mut()) {
        let mut img = cl.crop.clone().filter(|c| !c.is_empty());
        if let Some(por) = cl.portrait_img.as_ref().filter(|p| !p.is_empty()) {
            // low-res: the agent portrait is what makes a player recognisable; show it before the blurry name
            img = Some(match img {
                None => por.clone(),
                Some(nm) => {
                    let hh = por.h;
                    let nw = 1.max((nm.w as f64 * hh as f64 / nm.h as f64) as usize);
                    let nm = imgproc::resize(&nm, nw, hh, Interp::Linear);
                    let gap = BgrImage::new(2.max(hh / 4), hh);
                    imgproc::hstack(&[por, &gap, &nm])
                }
            });
        }
        if let Some(img) = img {
            save_png(&img, &ndir.join(format!("{}.png", p.id)))?;
            p.crop = Some(format!("cache/names/{}.png", p.id));
        }
    }
    Ok(Events { players, kills, ..Default::default() })
}

/// The end-of-round banner text: OCR of the banner area over the first seconds after the round was decided
/// (it slides in, so several frames are read; the most text wins).
fn read_round_banner(src: &Path, ga: &GameArea, lay: &Layout, scale: f64, decided: f64) -> String {
    let (rx, ry, rw, rh) = lay.rect("banner");
    let crop = (
        (ga.x as f64 + rx as f64 / scale) as i32,
        (ga.y as f64 + ry as f64 / scale) as i32,
        (rw as f64 / scale) as i32,
        (rh as f64 / scale) as i32,
    );
    let opts = ReadOpts { fps: 2.0, crop: Some(crop), start: decided + 0.6, duration: Some(3.0), hwaccel: true, ..Default::default() };
    let Ok(frames) = ff::read_frames(src, opts, None) else { return String::new() };
    let mut best = String::new();
    for f in frames.flatten() {
        let text = crate::ocr::read_block(&f.img, 0.5).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join(" ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.chars().filter(|c| c.is_alphabetic()).count() > best.chars().filter(|c| c.is_alphabetic()).count() {
            best = text;
        }
    }
    best
}

/// Round tags from the end-of-round banner: badges, and the result when the banner states it (live games
/// show "ROUND WIN" / "ROUND LOSS"; replays name the winning side instead).
pub fn banner_tags(text: &str) -> Vec<String> {
    let up = text.to_uppercase();
    let words: Vec<&str> = up.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |w: &str| words.contains(&w);
    let mut tags: Vec<String> = vec![];
    for badge in ["FLAWLESS", "CLUTCH", "THRIFTY"] {
        if has(badge) {
            tags.push(badge.into());
        }
    }
    if has("ACE") {
        tags.push(if has("TEAM") { "TEAM ACE".into() } else { "ACE".into() });
    }
    if up.contains("ROUND WIN") || up.contains("ROUND WON") {
        tags.push("result:win".into());
    } else if up.contains("ROUND LOSS") || up.contains("ROUND LOST") {
        tags.push("result:loss".into());
    }
    tags
}

pub fn finalize_identity(ev: &mut Events, aliases: &[String]) -> Vec<String> {
    let me = identity::resolve_me(ev, aliases);
    let mine = most_common(ev.kills.iter().filter(|k| me.contains(&k.killer)).map(|k| k.killer_side.clone()));
    // round results from the winner colour relative to the user's (dominant) kill feed colour
    if let Some((mine, _)) = mine {
        for rd in &mut ev.rounds {
            let w = rd.banners.iter().find_map(|b| b.strip_prefix("winner:")).unwrap_or("").to_string();
            if !w.is_empty() {
                rd.result = if w == mine { "win".into() } else { "loss".into() };
            }
        }
    }
    // a "ROUND WIN" / "ROUND LOSS" banner states the result outright
    for rd in &mut ev.rounds {
        if let Some(r) = rd.banners.iter().find_map(|b| b.strip_prefix("result:")) {
            rd.result = r.to_string();
        }
    }
    me
}

#[cfg(test)]
mod banner_tests {
    use super::banner_tags;

    #[test]
    fn banner_tags_from_ocr_text() {
        assert_eq!(banner_tags("FLAWLESS HI"), vec!["FLAWLESS"]);
        assert_eq!(banner_tags("III 0G k CLUTCH IV"), vec!["CLUTCH"]);
        assert_eq!(banner_tags("DEFENDERS WON ATTACKERS ELIMINATED"), Vec::<String>::new());
        assert_eq!(banner_tags("TEAM ACE"), vec!["TEAM ACE"]);
        assert_eq!(banner_tags("ROUND WIN"), vec!["result:win"]);
        assert_eq!(banner_tags("ROUND LOSS"), vec!["result:loss"]);
        assert!(banner_tags("SPACE").is_empty()); // "ACE" only as a word
    }
}
