//! Build and run the ffmpeg render for a project's edit.: the filter graph and command are written to
//! graph.txt / cmd.txt in the work folder.
use crate::ass;
use crate::editlist::EditList;
use crate::highlights;
use crate::killfeed_plan::{enter_progress, plan_rows, slot_expr, EXIT};
use crate::layoutspec::{canvas_size, crops_full_frame, effective_layout, gameplay_src, is_overlay, px_rect};
use crate::looks::look_filter;
use crate::music;
use crate::numfmt::{f, list2cmdline, trunc_i};
use crate::overlays;
use crate::smooth;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use vl_core::config;
use vl_core::ffmpeg as ff;
use vl_core::jobs::{Cancel, Cancelled, Progress};
use vl_core::models::{Edits, GameArea, Kill, LayoutSpec, Part};
use vl_core::paths;
use vl_core::projects::ProjectHandle;

pub const FEED_SECONDS: f64 = 4.5;

/// transition name -> xfade transition
pub fn xfade_name(transition: &str) -> &'static str {
    match transition {
        "flash" => "fadewhite",
        "blur" => "hblur",
        _ => "fade",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutSpec {
    pub w: i64,
    pub h: i64,
    pub fps: i64,
}

/// `max(2, int(v) // 2 * 2)`
pub fn even_t(v: f64) -> i64 {
    2.max(crate::numfmt::floordiv(trunc_i(v), 2) * 2)
}

/// A crop position: even, never negative (0 stays 0, unlike `even_t`).
fn even_pos(v: f64) -> i64 {
    (v.max(0.0) as i64) / 2 * 2
}

pub fn out_spec(fmt: &str, ga: &GameArea, resolution: i64, fps: i64, proxy: bool) -> OutSpec {
    let r = if proxy { 540 } else { resolution };
    let fps = if proxy { 30 } else { fps };
    if fmt == "source" {
        return OutSpec { w: even_t(r as f64 * ga.w as f64 / ga.h as f64), h: even_t(r as f64), fps };
    }
    let (w, h) = canvas_size(fmt, r);
    OutSpec { w, h, fps }
}

/// Per-frame zoom factor: quick punch-in at each kill, eased back out. `amount` = (kill time, amount);
///, a later entry for the same time wins.
pub fn zoom_expr(kill_times: &[f64], amount: &[(f64, f64)]) -> String {
    let mut terms = vec![];
    for &tk in kill_times {
        let a = amount.iter().rev().find(|(t, _)| *t == tk).map(|(_, a)| *a).unwrap_or(0.0);
        if a <= 0.0 {
            continue;
        }
        let x = format!("(t-{})", f(tk, 3));
        terms.push(format!("{}*between({x},-0.12,0.9)*if(lt({x},0),({x}+0.12)/0.12,1-pow({x}/0.9,2))", f(a, 3)));
    }
    if terms.is_empty() {
        return String::new();
    }
    format!("1+{}", terms.join("+"))
}

pub fn flash_expr(kill_times: &[f64]) -> String {
    kill_times
        .iter()
        .map(|tk| {
            let t = f(*tk, 3);
            format!("between(t-{t},0,0.18)*(1-(t-{t})/0.18)")
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Filter lines composing one video stream onto the layout canvas.
///
/// `src` is the input pad label ("" = the graph's unlabeled input, used by the live mpv preview).
/// Returns (lines, label of the composed output). Shared by the render and the subtitle preview, so what
/// you preview is what you export.
#[allow(clippy::too_many_arguments)]
pub fn compose_lines(
    src: &str,
    tag: &str,
    ga: &GameArea,
    w: i64,
    h: i64,
    l: &LayoutSpec,
    zexpr: &str,
    game_input: Option<&str>,
) -> (Vec<String>, String) {
    let (gw, gh) = (ga.w as f64, ga.h as f64);
    let game = l.layer("gameplay");
    let overlays_: Vec<_> = l.layers.iter().filter(|x| x.visible && is_overlay(&x.kind)).collect();
    // the webcam may sit outside the game area: it crops the whole frame, the others the game area
    let n_full = overlays_.iter().filter(|x| crops_full_frame(&x.kind)).count();
    let n_src = 2 + overlays_.len() - n_full;
    let mut head = if src.is_empty() { String::new() } else { format!("[{src}]") };
    let mut g = vec![];
    if n_full > 0 {
        g.push(format!("{head}split={}[ga{tag}]{}", n_full + 1, (0..n_full).map(|k| format!("[f{tag}_{k}]")).collect::<String>()));
        head = format!("[ga{tag}]");
    }
    g.push(format!(
        "{head}crop={}:{}:{}:{},setsar=1,split={n_src}{}",
        even_t(gw),
        even_t(gh),
        ga.x,
        ga.y,
        (0..n_src).map(|k| format!("[s{tag}_{k}]")).collect::<String>()
    ));
    // background: the game itself, blurred and darkened, so no area of the frame is ever empty
    let (bw, bh) = (even_t(w as f64 / 8.0), even_t(h as f64 / 8.0));
    if l.background == "blur" {
        g.push(format!(
            "[s{tag}_0]scale={bw}:{bh}:force_original_aspect_ratio=increase,crop={bw}:{bh},\
             boxblur=6:2,scale={w}:{h}:flags=bicubic,eq=brightness=-0.12:saturation=0.85[bg{tag}]"
        ));
    } else {
        g.push(format!("[s{tag}_0]scale={w}:{h},drawbox=x=0:y=0:w=iw:h=ih:color=black:t=fill[bg{tag}]"));
    }
    let mut cur = format!("bg{tag}");
    match game {
        Some(game) if game.visible => {
            let (dx, dy, dw, dh) = px_rect(game.dst, w, h);
            let (sx, sy, sw, sh) = gameplay_src(game, ga, w, h);
            let (cx, cy, cw, ch) = (even_t(sx * gw), even_t(sy * gh), even_t(sw * gw), even_t(sh * gh));
            let mut chain = if let Some(gi) = game_input {
                g.push(format!("[s{tag}_1]nullsink"));
                format!("[{gi}]setsar=1")
            } else {
                format!("[s{tag}_1]crop={}:{}:{cx}:{cy}", cw.min(even_t(gw)), ch.min(even_t(gh)))
            };
            if !zexpr.is_empty() {
                chain += &format!(
                    ",scale=w='trunc({dw}*({zexpr})/2)*2':h='trunc({dh}*({zexpr})/2)*2':eval=frame:flags=bicubic,\
                     crop={dw}:{dh}:'(trunc({dw}*({zexpr})/2)*2-{dw})/2':'(trunc({dh}*({zexpr})/2)*2-{dh})/2'"
                );
            } else {
                chain += &format!(",scale={dw}:{dh}:flags=bicubic");
            }
            g.push(chain + &format!("[gp{tag}]"));
            g.push(format!("[{cur}][gp{tag}]overlay={dx}:{dy}[c{tag}g]"));
            cur = format!("c{tag}g");
        }
        _ => g.push(format!("[s{tag}_1]nullsink")),
    }
    let (mut n_ga, mut n_fr) = (0, 0);
    for (k, lyr) in overlays_.iter().enumerate() {
        // live crops: score bar, minimap, health/ammo (of the game area), webcam (of the whole frame)
        let (sx, sy, sw, sh) = lyr.src;
        let (dx, dy, dw, dh) = px_rect(lyr.dst, w, h);
        if crops_full_frame(&lyr.kind) {
            g.push(format!(
                "[f{tag}_{n_fr}]crop={}:{}:{}:{},setsar=1,scale={dw}:{dh}:flags=bicubic[ov{tag}_{k}]",
                even_t(sw * gw),
                even_t(sh * gh),
                even_pos(ga.x as f64 + sx * gw),
                even_pos(ga.y as f64 + sy * gh)
            ));
            n_fr += 1;
        } else {
            g.push(format!(
                "[s{tag}_{}]crop={}:{}:{}:{},scale={dw}:{dh}:flags=bicubic[ov{tag}_{k}]",
                n_ga + 2,
                even_t(sw * gw),
                even_t(sh * gh),
                even_t(sx * gw),
                even_t(sy * gh)
            ));
            n_ga += 1;
        }
        g.push(format!("[{cur}][ov{tag}_{k}]overlay={dx}:{dy}[c{tag}o{k}]"));
        cur = format!("c{tag}o{k}");
    }
    (g, cur)
}

/// Single-input/single-output lavfi graph for mpv (`vf=lavfi=[...]`) + its canvas size (W, H).
pub fn preview_graph(ga: &GameArea, l: &LayoutSpec, look: &str, look_intensity: f64, short_side: i64) -> (String, i64, i64) {
    let (w, h) = canvas_size(&l.aspect, short_side);
    let (mut lines, cur) = compose_lines("", "p", ga, w, h, l, "", None);
    let lf = look_filter(look, look_intensity);
    lines.push(format!("[{cur}]{}format=yuv420p", if lf.is_empty() { String::new() } else { lf + "," }));
    (lines.join(";"), w, h)
}

/// Compose input i into [v{i}] on the layout canvas, with kill zoom/flash and the colour look.
fn video_chain(
    i: usize,
    ga: &GameArea,
    spec: &OutSpec,
    ed: &Edits,
    l: &LayoutSpec,
    local_kills: &[(f64, String)],
    game_input: Option<&str>,
    dur: f64,
) -> Vec<String> {
    let fx = &ed.effects;
    let times: Vec<f64> = local_kills.iter().map(|(t, _)| *t).collect();
    let zexpr = if fx.zoom {
        let amt: Vec<(f64, f64)> = local_kills
            .iter()
            .map(|(tk, kid)| (*tk, (fx.zoom_per_kill.get(kid).copied().unwrap_or(fx.zoom_amount) - 1.0).max(0.0)))
            .collect();
        zoom_expr(&times, &amt)
    } else {
        String::new()
    };
    let fexpr = if fx.flash_on_kill { flash_expr(&times) } else { String::new() };
    let (mut g, cur) = compose_lines(&format!("{i}:v"), &i.to_string(), ga, spec.w, spec.h, l, &zexpr, game_input);
    let mut post = vec![format!("fps={}", spec.fps), "format=yuv420p".to_string()];
    if game_input.is_some() {
        // a super smooth part: the overlay can hand `fps` an end time from the smoothed file's own time base
        // (1/15360 against e.g. a 1/60 source), and `fps` then pads the part with copies until the output's
        // -t; with more than one part the concat waits forever for audio. Cut the part at its length.
        post.insert(1, format!("trim=duration={dur:.3}"));
    }
    let lf = look_filter(&fx.look, fx.look_intensity);
    if !lf.is_empty() {
        post.insert(0, lf);
    }
    if !fexpr.is_empty() {
        post.insert(0, format!("eq=brightness='0.35*({fexpr})':eval=frame"));
    }
    g.push(format!("[{cur}]{},settb=AVTB,setpts=PTS-STARTPTS[v{i}]", post.join(",")));
    g
}

/// Stack rows like the in-game feed: each new row takes the first free slot from the top.
pub fn feed_slots(times: &[f64], dur: f64, rows: usize) -> Vec<usize> {
    let mut active: Vec<(f64, usize)> = vec![];
    let mut out = vec![];
    for &t in times {
        active.retain(|(e, _)| *e > t);
        let slot = (0..rows).find(|s| !active.iter().any(|(_, u)| u == s)).unwrap_or(rows.saturating_sub(1));
        active.push((t + dur, slot));
        out.push(slot);
    }
    out
}

/// The kill's killfeed row at the target height: the in-game crop when we have it, else a drawn row.
/// Returns the row width.
fn feed_png(h: &ProjectHandle, k: &Kill, names: &HashMap<String, String>, row_h: i64, max_w: i64, out: &Path) -> Result<i64> {
    let row = k.row_image.as_ref().map(|r| h.dir.join(r)).filter(|p| p.exists());
    let img = match row {
        Some(p) => image::open(&p).with_context(|| format!("unreadable {}", p.display()))?.to_rgba8(),
        None => {
            let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let tmp = out.with_file_name(format!("{stem}_draw.png"));
            let q = "?".to_string();
            let kp = k.killer_portrait.as_ref().map(|p| h.dir.join(p));
            let vp = k.victim_portrait.as_ref().map(|p| h.dir.join(p));
            overlays::feed_row(
                &tmp,
                names.get(&k.killer).unwrap_or(&q),
                names.get(&k.victim).unwrap_or(&q),
                kp.as_deref(),
                vp.as_deref(),
                k.headshot,
                &k.killer_side,
                row_h.max(48),
            )?;
            image::open(&tmp)?.to_rgba8()
        }
    };
    let mut w = 2.max(trunc_i(img.width() as f64 * row_h as f64 / img.height().max(1) as f64));
    let mut rh = row_h;
    if w > max_w {
        rh = 2.max(trunc_i(row_h as f64 * max_w as f64 / w as f64));
        w = max_w;
    }
    let img = overlays::pil_resize(&img, w.max(1) as u32, rh.max(1) as u32, overlays::PilFilter::Lanczos);
    img.save(out)?;
    Ok(w)
}

fn opt_i64(options: &Value, key: &str, default: i64) -> i64 {
    match options.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|v| v as i64)).unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn opt_str(options: &Value, key: &str, default: &str) -> String {
    options.get(key).and_then(|v| v.as_str()).unwrap_or(default).to_string()
}

fn opt_bool(options: &Value, key: &str) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Render the project's edit to `out`.
///
/// `options` (all optional): `resolution`, `fps`, `encoder` (auto|nvenc|x264), `quality` (high|medium|small),
/// `keep_work` (keep the work folder with graph.txt / cmd.txt), plus two Rust-only ones for tooling:
/// `work_dir` (use this work folder) and `dry_run` (write graph.txt / cmd.txt, don't encode).
pub fn render_project(h: &ProjectHandle, out: &Path, progress: Progress, proxy: bool, options: &Value, cancel: &Cancel) -> Result<PathBuf> {
    let prog = progress;
    let cfg = config::load();
    let ev = h.events();
    let ed = h.edits();
    let proj = &h.project;
    let (Some(ev), Some(ga)) = (ev, proj.game_area) else { bail!("Project has not been analysed yet") };
    if !h.source_ok() {
        bail!("Source video not found: {}", proj.source.path);
    }
    let src = proj.source.path.clone();
    let duration = proj.source.duration;
    let me = &proj.identity.me;
    let mut parts: Vec<Part> = if ed.parts.is_empty() {
        highlights::quick_cut("segments", &ev, me, &cfg.highlights, duration, &ed.kills_disabled)?
    } else {
        ed.parts.clone()
    };
    if parts.is_empty() {
        parts = vec![Part::new(0.0, duration.min(30.0))];
    }
    let el = EditList::new(&parts, &ed.effects);
    if el.placed.is_empty() {
        bail!("Nothing to render");
    }
    let l = effective_layout(&ed, &ga);
    let spec = out_spec(
        &l.aspect,
        &ga,
        opt_i64(options, "resolution", cfg.render.resolution as i64),
        opt_i64(options, "fps", cfg.render.fps as i64),
        proxy,
    );
    let work = match options.get("work_dir").and_then(|v| v.as_str()) {
        Some(d) => PathBuf::from(d),
        None => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            paths::tmp_dir().join(format!("render_{}_{now}", proj.id))
        }
    };
    let work = std::path::absolute(&work)?;
    std::fs::create_dir_all(&work)?;
    copy_dir(&paths::assets_dir().join("fonts"), &work.join("fonts"))?;

    let kills = highlights::my_kills(&ev, me, cfg.highlights.include_team_kills, &ed.kills_disabled);
    let names: HashMap<String, String> = ev.players.iter().map(|p| (p.id.clone(), p.display.clone())).collect();

    // ------------------------------------------------------------- super smooth (gameplay pre-pass)
    let mut smoothed: Vec<PathBuf> = vec![];
    let mut render_span = (0.0, 1.0);
    let game = l.layer("gameplay");
    if ed.effects.super_smooth && !proxy && game.map(|g| g.visible).unwrap_or(false) {
        let mut params = smooth::params(&ed.effects.smooth_amount, &ed.effects.smooth_custom);
        if !params.flow {
            let (ok, why) = smooth::rife_works();
            if !ok {
                // the optical-flow blur needs no GPU: use it rather than dropping super smooth
                log::warn!("AI frame interpolation unavailable ({why}), using optical-flow blur");
                prog(0.01, &format!("AI frames unavailable ({why}), using Faster super smooth"));
                params = smooth::faster();
            }
        }
        {
            let game = game.unwrap();
            let (sx, sy, sw, sh) = gameplay_src(game, &ga, spec.w, spec.h);
            let (gw, gh) = (ga.w as f64, ga.h as f64);
            let crop = (ga.x + even_t(sx * gw) as i32, ga.y + even_t(sy * gh) as i32, even_t(sw * gw) as i32, even_t(sh * gh) as i32);
            let in_fps = if proj.source.fps > 0.0 { proj.source.fps } else { 60.0 };
            let total: f64 = el.placed.iter().map(|p| p.dur()).sum();
            let total = if total > 0.0 { total } else { 1.0 };
            let mut done = 0.0;
            for (pi, pl) in el.placed.iter().enumerate() {
                let (d, dur) = (done, pl.dur());
                let what = if params.flow { "Super smooth: motion blur" } else { "Super smooth: generating in-between frames" };
                let part_prog = |fr: f64| prog(0.01 + 0.74 * (d + fr * dur) / total, what);
                smoothed.push(smooth::smooth_part(
                    &src,
                    pl.src_in,
                    pl.dur(),
                    crop,
                    in_fps,
                    spec.fps as f64,
                    &params,
                    &work.join(format!("smooth_{pi}.mp4")),
                    &part_prog,
                    cancel,
                )?);
                done += pl.dur();
            }
            render_span = (0.75, 1.0);
        }
    }

    // ------------------------------------------------------------- inputs
    let mut args: Vec<String> = vec![
        ff::ffmpeg_exe()?,
        "-hide_banner".into(),
        "-y".into(),
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
    ];
    for pl in &el.placed {
        args.extend(ff::hw_decode_args().iter().map(|s| s.to_string()));
        args.extend(["-ss".into(), f(pl.src_in, 3), "-t".into(), f(pl.dur(), 3), "-i".into(), src.clone()]);
    }
    let n = el.placed.len();
    let mut extra = n;
    let mut smooth_inputs: Vec<String> = vec![];
    for sp in &smoothed {
        args.extend(["-i".into(), sp.to_string_lossy().into_owned()]);
        smooth_inputs.push(format!("{extra}:v"));
        extra += 1;
    }
    let mut graph: Vec<String> = vec![];

    // ------------------------------------------------------------- per-part video/audio
    let has_audio = proj.source.has_audio;
    for (i, pl) in el.placed.iter().enumerate() {
        let local: Vec<(f64, String)> =
            kills.iter().filter(|k| pl.src_in <= k.t && k.t <= pl.src_out).map(|k| (k.t - pl.src_in, k.id.clone())).collect();
        graph.extend(video_chain(i, &ga, &spec, &ed, &l, &local, smooth_inputs.get(i).map(|s| s.as_str()), pl.dur()));
        if has_audio {
            graph.push(format!(
                "[{i}:a]aresample=48000,aformat=channel_layouts=stereo,volume={},asetpts=PTS-STARTPTS[a{i}]",
                f(ed.music.game_volume, 2)
            ));
        } else {
            graph.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{}[a{i}]", f(pl.dur(), 3)));
        }
    }

    // ------------------------------------------------------------- join
    if n == 1 {
        graph.push("[v0]null[vj]".into());
        graph.push("[a0]anull[aj]".into());
    } else if el.td > 0.0 {
        let tr = xfade_name(&ed.effects.transition);
        let (mut prev_v, mut prev_a) = ("v0".to_string(), "a0".to_string());
        for i in 1..n {
            let off = el.placed[i].out_start;
            let (nv, na) = (format!("vx{i}"), format!("ax{i}"));
            graph.push(format!("[{prev_v}][v{i}]xfade=transition={tr}:duration={}:offset={}[{nv}]", f(el.td, 3), f(off, 3)));
            graph.push(format!("[{prev_a}][a{i}]acrossfade=d={}[{na}]", f(el.td, 3)));
            prev_v = nv;
            prev_a = na;
        }
        graph.push(format!("[{prev_v}]null[vj]"));
        graph.push(format!("[{prev_a}]anull[aj]"));
    } else {
        let ins: String = (0..n).map(|i| format!("[v{i}][a{i}]")).collect();
        graph.push(format!("{ins}concat=n={n}:v=1:a=1[vj][aj]"));
    }
    let mut vcur = "vj".to_string();
    let mut acur = "aj".to_string();

    // ------------------------------------------------------------- kill feed (your kills, in-game rows)
    let kfs = &ed.killfeed;
    if let Some(feed) = l.layer("killfeed").filter(|x| x.visible) {
        let (fx_, fy_, fw_, fh_) = px_rect(feed.dst, spec.w, spec.h);
        let n_rows = kfs.rows.clamp(1, 6);
        let gap = 0.12;
        let row_h = even_t(fh_ as f64 / (n_rows as f64 + gap * (n_rows - 1) as f64));
        let pitch = row_h as f64 * (1.0 + gap);
        let by_id: HashMap<&str, &Kill> = kills.iter().map(|k| (k.id.as_str(), k)).collect();
        let items: Vec<(String, f64, i32)> = kills.iter().filter_map(|k| el.to_output(k.t).map(|t| (k.id.clone(), t, k.round))).collect();
        let round_end = |rnd: i32, _t: f64| -> Option<f64> {
            // the in-game feed clears when the next round's buy phase starts
            if rnd >= 0 && (rnd as usize) < ev.rounds.len() {
                Some(el.out_at_or_before(ev.rounds[rnd as usize].end + 7.0))
            } else {
                None
            }
        };
        let plans = plan_rows(&items, &kfs.keep, kfs.seconds, n_rows, el.duration, &round_end);
        for (i, pl) in plans.iter().enumerate() {
            let k = by_id[pl.key.as_str()];
            let png = work.join(format!("row_{}.png", k.id));
            let rw = feed_png(h, k, &names, row_h, fw_, &png)?;
            args.extend(["-loop".into(), "1".into(), "-t".into(), f(el.duration, 3), "-i".into(), png.to_string_lossy().into_owned()]);
            let right = fx_ + fw_;
            let slot = slot_expr(&plans, i, &kfs.newest);
            let mut y = format!("{fy_}+{}*({slot})", f(pitch, 2));
            let mut x = format!("{right}-w");
            let ease = enter_progress(pl.start);
            let mut chain = format!("[{extra}:v]format=rgba");
            match kfs.animation.as_str() {
                "pop" => {
                    chain += &format!(
                        ",scale=w='max(2,trunc(iw*(0.55+0.45*{ease})/2)*2)':h='max(2,trunc(ih*(0.55+0.45*{ease})/2)*2)'\
                         :eval=frame,fade=t=in:st={}:d=0.1:alpha=1",
                        f(pl.start, 3)
                    );
                    y = format!("{fy_}+{}*({slot})+({row_h}-h)/2", f(pitch, 2));
                    x = format!("{right}-w-({rw}-w)/2");
                }
                "slide" => {
                    x = format!("{right}-w+{}*(1-{ease})", f(40f64.max(rw as f64 * 0.6), 0));
                    chain += &format!(",fade=t=in:st={}:d=0.15:alpha=1", f(pl.start, 3));
                }
                "fade" => chain += &format!(",fade=t=in:st={}:d=0.28:alpha=1", f(pl.start, 3)),
                _ => {}
            }
            if !pl.keep_to_end {
                chain += &format!(",fade=t=out:st={}:d={EXIT}:alpha=1", f(pl.end, 3));
            }
            graph.push(chain + &format!("[ck{extra}]"));
            let t1 = if pl.keep_to_end { el.duration } else { el.duration.min(pl.end + EXIT) };
            graph.push(format!(
                "[{vcur}][ck{extra}]overlay=x='{x}':y='{y}':eval=frame:enable='between(t,{},{})'[vk{extra}]",
                f(pl.start, 3),
                f(t1, 3)
            ));
            vcur = format!("vk{extra}");
            extra += 1;
        }
    }

    // ------------------------------------------------------------- subtitles + censor
    let st = &ed.subtitles;
    if st.enabled && h.transcript_path().exists() {
        let trn = h.transcript().ok_or_else(|| anyhow!("transcript.json is unreadable"))?;
        let sl = l.layer("subtitles");
        let bx = sl.filter(|s| s.visible).map(|s| px_rect(s.dst, spec.w, spec.h));
        ass::build_ass(&trn, st, &el, spec.w, spec.h, &ed.highlight_color, &work.join("subs.ass"), bx)?;
        graph.push(format!("[{vcur}]ass=subs.ass:fontsdir=fonts[vs]"));
        vcur = "vs".into();
        if st.censor_audio {
            let mut ranges = vec![];
            for (a, b) in ass::censor_ranges(&trn, st) {
                ranges.extend(el.map_range(a - 0.04, b + 0.04));
            }
            if !ranges.is_empty() {
                let en = ranges.iter().map(|(a, b)| format!("between(t,{},{})", f(*a, 3), f(*b, 3))).collect::<Vec<_>>().join("+");
                graph.push(format!("[{acur}]volume=0:enable='{en}'[acz]"));
                graph.push(format!(
                    "sine=f=1000:r=48000:d={},volume=0.25,volume=0:enable='not({en})',aformat=channel_layouts=stereo[beep]",
                    f(el.duration, 3)
                ));
                graph.push("[acz][beep]amix=inputs=2:normalize=0:duration=first[acc]".into());
                acur = "acc".into();
            }
        }
    }

    // ------------------------------------------------------------- music
    let mus = &ed.music;
    if mus.enabled && !mus.path.is_empty() && Path::new(&mus.path).exists() {
        args.extend(["-stream_loop".into(), "-1".into(), "-i".into(), mus.path.clone()]);
        let mi = extra;
        extra += 1;
        // the chosen part of the song, placed on the edit timeline (same chain as the previews)
        graph.push(format!("[{mi}:a]aresample=48000,aformat=channel_layouts=stereo,{}[mus]", music::shape_filter(mus, el.duration, true)));
        if mus.duck {
            graph.push(format!("[{acur}]asplit=2[gam][gsc]"));
            graph.push("[mus][gsc]sidechaincompress=threshold=0.08:ratio=5:attack=20:release=300[mduck]".into());
            graph.push("[gam][mduck]amix=inputs=2:normalize=0:duration=first[amx]".into());
        } else {
            graph.push(format!("[{acur}][mus]amix=inputs=2:normalize=0:duration=first[amx]"));
        }
        acur = "amx".into();
    }
    let _ = extra;

    graph.push(format!("[{vcur}]format=yuv420p[vout]"));
    graph.push(format!("[{acur}]alimiter=limit=0.95[aout]"));
    crate::numfmt::write_text(&work.join("graph.txt"), &graph.join(";\n"))?;

    // ------------------------------------------------------------- encode
    let enc = opt_str(options, "encoder", &cfg.render.encoder);
    let use_nvenc = (enc == "auto" || enc == "nvenc") && ff::nvenc_works();
    let q = opt_str(options, "quality", &cfg.render.quality);
    let venc: Vec<String> = if use_nvenc {
        let cq = match q.as_str() {
            "medium" => 23,
            "small" => 28,
            _ => 19,
        };
        [
            "-c:v",
            "h264_nvenc",
            "-preset",
            if proxy { "p1" } else { "p5" },
            "-rc",
            "vbr",
            "-cq",
            &(if proxy { 30 } else { cq }).to_string(),
            "-b:v",
            "0",
            "-profile:v",
            "high",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    } else {
        let crf = match q.as_str() {
            "medium" => 22,
            "small" => 26,
            _ => 18,
        };
        ["-c:v", "libx264", "-preset", if proxy { "ultrafast" } else { "medium" }, "-crf", &(if proxy { 30 } else { crf }).to_string()]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    let out = std::path::absolute(out)?;
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    args.extend(["-/filter_complex".into(), "graph.txt".into(), "-map".into(), "[vout]".into(), "-map".into(), "[aout]".into()]);
    args.extend(venc);
    args.extend(
        ["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart", "-t"]
            .iter()
            .map(|s| s.to_string())
            .chain([f(el.duration, 3), out.to_string_lossy().into_owned()]),
    );
    crate::numfmt::write_text(&work.join("cmd.txt"), &list2cmdline(&args))?;
    log::info!("render {} -> {} ({}x{}, {} parts, {:.1}s)", proj.id, out.display(), spec.w, spec.h, n, el.duration);
    if opt_bool(options, "dry_run") {
        return Ok(out);
    }
    let (lo, hi) = render_span;
    prog(lo + 0.01, "Rendering");
    let res = run_ffmpeg(&args, &work, el.duration, &|p, m| prog(lo + (hi - lo) * p, m), cancel);
    if !opt_bool(options, "keep_work") {
        let _ = std::fs::remove_dir_all(&work);
    }
    res?;
    prog(1.0, "Done");
    Ok(out)
}

/// Run the render, reporting progress from `-progress pipe:1`; kills ffmpeg when `cancel` is set.
pub fn run_ffmpeg(args: &[String], cwd: &Path, duration: f64, prog: &dyn Fn(f64, &str), cancel: &Cancel) -> Result<()> {
    let mut child = ff::command(&args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start ffmpeg")?;
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_th = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stderr.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    });
    let child = Arc::new(Mutex::new(child));
    let finished = Arc::new(AtomicBool::new(false));
    let killed = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (child, finished, killed, cancel) = (child.clone(), finished.clone(), killed.clone(), cancel.clone());
        std::thread::spawn(move || {
            while !finished.load(Ordering::Relaxed) {
                if cancel.is_set() {
                    killed.store(true, Ordering::Relaxed);
                    let _ = child.lock().unwrap().kill();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
    };
    let mut r = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
            if let Ok(us) = v.parse::<i64>() {
                prog((0.01 + 0.98 * us as f64 / 1e6 / duration.max(0.01)).min(0.99), "Rendering");
            }
        }
    }
    let status = loop {
        if let Some(s) = child.lock().unwrap().try_wait()? {
            break s;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    finished.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    let msg = err_th.join().unwrap_or_default();
    if killed.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    if !status.success() {
        bail!("ffmpeg render failed ({}):\n{}", status.code().unwrap_or(-1), &msg[msg.len().saturating_sub(3000)..]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_and_specs() {
        let ga = GameArea { x: 160, y: 0, w: 960, h: 720, hud_scale: 1.0 };
        let s = out_spec("9:16", &ga, 1080, 60, false);
        assert_eq!((s.w, s.h), (1080, 1920));
        assert_eq!(out_spec("16:9", &ga, 1080, 60, false).w, 1920);
        assert_eq!(out_spec("16:9", &ga, 1080, 60, true).h, 540);
        assert_eq!(out_spec("source", &ga, 1080, 60, false), OutSpec { w: 1440, h: 1080, fps: 60 });
        assert_eq!(zoom_expr(&[], &[]), "");
        assert!(zoom_expr(&[1.0], &[(1.0, 0.1)]).starts_with("1+"));
        assert_eq!(feed_slots(&[0.0, 1.0, 10.0], 4.5, 4), vec![0, 1, 0]);
    }

    #[test]
    fn preview_graph_shape() {
        let ga = GameArea { x: 0, y: 0, w: 1440, h: 1080, hud_scale: 1.0 };
        let l = crate::layoutspec::default_layout("9:16", &ga);
        let (g, w, h) = preview_graph(&ga, &l, "vivid", 0.5, 720);
        assert_eq!((w, h), (720, 1280));
        assert!(g.starts_with("crop=1440:1080:0:0,setsar=1,split=4[sp_0][sp_1][sp_2][sp_3]"));
        assert!(g.ends_with("eq=saturation=1.225:contrast=1.060,format=yuv420p"));
    }

    #[test]
    fn webcam_crops_the_whole_frame() {
        // a 4:3 game in a 16:9 recording, the webcam in the left border
        let ga = GameArea { x: 240, y: 0, w: 1440, h: 1080, hud_scale: 1.0 };
        let mut l = crate::layoutspec::default_layout("16:9", &ga);
        let cam = l.layer_mut("webcam").unwrap();
        cam.visible = true;
        cam.src = (-240.0 / 1440.0, 0.5, 200.0 / 1440.0, 200.0 / 1080.0);
        let (g, _, _) = preview_graph(&ga, &l, "none", 0.0, 720);
        assert!(g.starts_with("split=2[gap][fp_0];[gap]crop=1440:1080:240:0,setsar=1,split=2[sp_0][sp_1]"), "{g}");
        assert!(g.contains("[fp_0]crop=200:200:0:540,setsar=1,scale="), "{g}");
        // without a visible webcam the graph is what it always was
        l.layer_mut("webcam").unwrap().visible = false;
        assert!(preview_graph(&ga, &l, "none", 0.0, 720).0.starts_with("crop=1440:1080:240:0,setsar=1,split=2[sp_0][sp_1]"));
    }
}
