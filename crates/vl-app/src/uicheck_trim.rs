//! `valolights --ui-check trim [out_dir]`: drives the real window through the Trim page and saves
//! the app's own frames (egui screenshots, never the desktop) as PNGs.
//!
//! Works on scratch copies `data/projects/rustqa_trim_*` (deleted at the end). Every other project and
//! data/config.toml are hashed before and after. Input is injected as egui events (mouse, keys, wheel).
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::backend::{self, EditList};
use crate::pages::trim::Mode;
use crate::pages::{LAYOUT, SUBTITLES, TRIM};
use egui::{pos2, vec2, Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vl_core::models::Part;
use vl_core::paths;

struct Check {
    out: PathBuf,
    dirs: HashMap<&'static str, PathBuf>,
    inject: VecDeque<(Vec<Event>, Modifiers)>,
    results: Vec<(String, bool, String)>,
    v: HashMap<&'static str, f64>,
    parts0: Vec<(f64, f64)>,
    s: HashMap<&'static str, String>,
    done: bool,
}

impl Check {
    fn ok(&mut self, name: &str, pass: bool, detail: impl Into<String>) {
        let d = detail.into();
        println!("[{}] {name}: {d}", if pass { " OK " } else { "FAIL" });
        self.results.push((name.into(), pass, d));
    }
    fn push(&mut self, evs: Vec<Event>, m: Modifiers) {
        self.inject.push_back((evs, m));
    }
    fn click_m(&mut self, p: Pos2, button: PointerButton, m: Modifiers) {
        self.push(vec![Event::PointerMoved(p)], m);
        self.push(vec![Event::PointerButton { pos: p, button, pressed: true, modifiers: m }], m);
        self.push(vec![Event::PointerButton { pos: p, button, pressed: false, modifiers: m }], m);
    }
    fn click(&mut self, p: Pos2) {
        self.click_m(p, PointerButton::Primary, Modifiers::NONE);
    }
    fn key(&mut self, key: Key, m: Modifiers) {
        self.push(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m }], m);
        self.push(vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m }], Modifiers::NONE);
    }
    fn drag(&mut self, a: Pos2, b: Pos2, m: Modifiers, steps: usize, release: bool) {
        self.push(vec![Event::PointerMoved(a)], m);
        self.push(vec![Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: m }], m);
        for k in 1..=steps {
            let p = a + (b - a) * (k as f32 / steps as f32);
            self.push(vec![Event::PointerMoved(p)], m);
        }
        if release {
            self.push(vec![Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: m }], m);
        }
    }
    fn release_at(&mut self, b: Pos2) {
        self.push(
            vec![Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }],
            Modifiers::NONE,
        );
    }
    fn wheel(&mut self, p: Pos2, lines: f32) {
        self.push(vec![Event::PointerMoved(p)], Modifiers::NONE);
        self.push(
            vec![Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: vec2(0.0, lines),
                modifiers: Modifiers::NONE,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
            }],
            Modifiers::NONE,
        );
    }
    fn shot(&mut self, ctx: &egui::Context, name: &str) {
        let path = self.out.join(format!("{name}.png"));
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
            if let Err(e) = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
                eprintln!("screenshot {}: {e}", path.display());
            }
        });
    }
    fn var(&self, k: &str) -> f64 {
        self.v.get(k).copied().unwrap_or(f64::NAN)
    }
}

struct Env<'a> {
    w: &'a mut MainWindow,
    k: &'a mut Check,
    ctx: &'a egui::Context,
    /// frames and time since the step started
    f: u32,
    el: Duration,
}

impl Env<'_> {
    fn src_pos(&self) -> f64 {
        let tp = &self.w.trim;
        let p = tp.player.pos();
        match (&tp.el, tp.mode) {
            (Some(el), Mode::Edit) => el.to_source(p),
            _ => p,
        }
    }
    fn spans(&self) -> Vec<(f64, f64)> {
        self.w.st.ed.parts.iter().map(|p| (p.src_in, p.src_out)).collect()
    }
    fn tl(&self, t: f64, row: &str) -> Pos2 {
        self.w.trim.timeline.screen_pos(t, row)
    }
    fn rect(&self, name: &str) -> egui::Rect {
        self.w.trim.rects.get(name).copied().unwrap_or(egui::Rect::NOTHING)
    }
    fn secs(&self, s: f64) -> bool {
        self.el.as_secs_f64() >= s
    }
}

type StepFn = Box<dyn for<'a> FnMut(&mut Env<'a>) -> bool>;

/// Run once.
fn act(mut f: impl for<'a> FnMut(&mut Env<'a>) + 'static) -> StepFn {
    Box::new(move |e| {
        f(e);
        true
    })
}

fn wait(s: f64) -> StepFn {
    Box::new(move |e| e.secs(s))
}

/// Wait until `cond` holds (or `timeout` seconds passed).
fn until(timeout: f64, mut cond: impl for<'a> FnMut(&mut Env<'a>) -> bool + 'static) -> StepFn {
    Box::new(move |e| e.f > 2 && (cond(e) || e.secs(timeout)))
}

fn close_dialogs() -> StepFn {
    Box::new(|e| {
        if !e.w.dialogs.is_empty() {
            e.w.dialogs.clear();
        }
        true
    })
}

fn steps() -> Vec<StepFn> {
    let mut s: Vec<StepFn> = vec![];
    // ---------------------------------------------------------------- open the short clip
    s.push(act(|e| {
        e.ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1600.0, 950.0)));
        let d = e.k.dirs["short"].clone();
        e.w.open_project(&d, false);
    }));
    s.push(wait(0.5));
    s.push(close_dialogs());
    s.push(until(20.0, |e| e.w.trim.player.duration() > 0.0 && e.w.trim.timeline.thumbs.is_some()));
    // go to Trim by clicking the top-bar tab (from Layout)
    s.push(act(|e| {
        let r = e.w.step_rects.get(LAYOUT).copied().unwrap_or(egui::Rect::NOTHING);
        e.k.click(r.center());
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        let r = e.w.step_rects.get(TRIM).copied().unwrap_or(egui::Rect::NOTHING);
        e.k.click(r.center());
    }));
    s.push(wait(1.5));
    s.push(act(|e| {
        e.w.st.apply_quick_cut("kills", true);
    }));
    s.push(wait(1.2));
    s.push(act(|e| {
        e.k.parts0 = e.spans();
        println!(
            "parts {:?}",
            e.k.parts0.iter().map(|(a, b)| ((a * 100.0).round() / 100.0, (b * 100.0).round() / 100.0)).collect::<Vec<_>>()
        );
        e.k.shot(e.ctx, "01_trim");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        // 1 Space from the top bar
        e.k.key(Key::Space, Modifiers::NONE);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("playing", e.w.trim.player.is_playing() as i32 as f64);
        e.k.key(Key::Space, Modifiers::NONE);
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let ok = e.k.var("playing") == 1.0 && !e.w.trim.player.is_playing();
        e.k.ok("Space plays/pauses right after clicking the Trim tab", ok, "");
        // 2 Space after ticking a kill toggle: plays, never re-ticks it
        let sw = e.w.trim.kill_rects.first().map(|r| r.2).unwrap_or(egui::Rect::NOTHING);
        e.k.click(sw.center());
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.s.insert("dis", format!("{:?}", e.w.st.ed.kills_disabled));
        e.k.key(Key::Space, Modifiers::NONE);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let playing = e.w.trim.player.is_playing();
        e.w.trim.player.pause();
        let same = format!("{:?}", e.w.st.ed.kills_disabled) == e.k.s["dis"];
        let dropped = !e.w.st.ed.kills_disabled.is_empty();
        e.k.ok(
            "Space after clicking a kill toggle plays, toggle untouched",
            playing && same && dropped,
            format!("disabled {:?}", e.w.st.ed.kills_disabled),
        );
        let sw = e.w.trim.kill_rects.first().map(|r| r.2).unwrap_or(egui::Rect::NOTHING);
        e.k.click(sw.center());
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let ok = e.spans() == e.k.parts0 && e.w.st.ed.kills_disabled.is_empty();
        e.k.ok("ticking it again restores the quick cut", ok, format!("{} parts", e.spans().len()));
        // 4 Space after clicking a toolbar button doesn't press it again
        let r = e.rect("fit");
        e.k.click(r.center());
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        let pps = e.w.trim.timeline.pps * 3.0;
        e.w.trim.timeline.set_zoom(pps, None);
        e.k.v.insert("pps", e.w.trim.timeline.pps);
        e.k.key(Key::Space, Modifiers::NONE);
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        let playing = e.w.trim.player.is_playing();
        e.w.trim.player.pause();
        let ok = playing && (e.w.trim.timeline.pps - e.k.var("pps")).abs() < 1e-6;
        e.k.ok("Space after clicking a toolbar button plays (button not re-pressed)", ok, "");
        e.w.trim.timeline.fit();
    }));
    s.push(wait(0.3));
    // 5 click on the ruler inside a kept part seeks (edit mode maps to edit time)
    s.push(act(|e| {
        let (a, b) = e.k.parts0[1];
        let p = e.tl((a + b) / 2.0, "ruler");
        e.k.click(p);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let (a, b) = e.k.parts0[1];
        let t = (a + b) / 2.0;
        let sp = e.src_pos();
        // the click lands on a whole pixel: allow one pixel of time
        let px = 1.0 / e.w.trim.timeline.pps;
        e.k.ok(
            "clicking the ruler in a kept part seeks there",
            e.w.trim.mode == Mode::Edit && (sp - t).abs() < 0.15 + px,
            format!("source {sp:.2} want {t:.2}"),
        );
        // 6 drag-scrub inside the part
        let (p0, p1) = (e.tl(a + 0.3, "ruler"), e.tl(b - 0.3, "ruler"));
        e.k.drag(p0, p1, Modifiers::NONE, 8, true);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let (_, b) = e.k.parts0[1];
        let sp = e.src_pos();
        let px = 1.0 / e.w.trim.timeline.pps;
        e.k.ok("dragging on the ruler scrubs", (sp - (b - 0.3)).abs() < 0.15 + px, format!("{sp:.2}"));
        // 7 click in cut footage while in Edit preview -> Full video at that moment, with a message
        let gap = (e.k.parts0[0].1 + e.k.parts0[1].0) / 2.0;
        e.k.v.insert("gap", gap);
        let p = e.tl(gap, "ruler");
        e.k.click(p);
    }));
    s.push(wait(1.2));
    s.push(act(|e| {
        let gap = e.k.var("gap");
        let px = 1.0 / e.w.trim.timeline.pps;
        let pos = e.w.trim.player.pos();
        let ok = e.w.trim.mode == Mode::Source && (pos - gap).abs() < 0.15 + px && e.w.trim.msg.contains("full video");
        e.k.ok(
            "clicking cut footage shows the full video there",
            ok,
            format!("mode {:?} pos {pos:.2} msg {:?}", e.w.trim.mode, e.w.trim.msg),
        );
        e.k.shot(e.ctx, "02_full_video");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        // 8 back to Edit preview from cut footage -> start of the next kept part
        let r = e.rect("mode");
        e.k.click(pos2(r.left() + r.width() / 4.0, r.center().y));
    }));
    s.push(wait(1.2));
    s.push(act(|e| {
        let sp = e.src_pos();
        let want = e.k.parts0[1].0;
        e.k.ok(
            "back to Edit preview lands on the next kept part",
            e.w.trim.mode == Mode::Edit && (sp - want).abs() < 0.1,
            format!("source {sp:.2} want {want:.2}"),
        );
        let a = e.k.parts0[1].0;
        let st = &mut e.w.st;
        e.w.trim.seek_source(a + 1.0, true, st);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("p_before", e.w.trim.player.pos());
        let r = e.rect("mode");
        e.k.click(pos2(r.left() + r.width() / 4.0, r.center().y));
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let ok = (e.w.trim.player.pos() - e.k.var("p_before")).abs() < 0.05;
        e.k.ok(
            "clicking the current mode again keeps the position",
            ok,
            format!("{:.2} vs {:.2}", e.w.trim.player.pos(), e.k.var("p_before")),
        );
        // 9 kill portrait click: selects the row, jumps to 1.5 s before
        let k = e.w.trim.timeline.kills[1].clone();
        e.k.s.insert("kid", k.id.clone());
        e.k.v.insert("kt", k.t);
        let p = e.tl(k.t, "kills");
        e.k.click(p);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let (kid, kt) = (e.k.s["kid"].clone(), e.k.var("kt"));
        let ph = e.w.trim.timeline.playhead;
        let ok = e.w.trim.cur_kill.as_deref() == Some(kid.as_str()) && (ph - (kt - 1.5)).abs() < 0.15;
        e.k.ok("clicking a portrait selects the kill and jumps just before it", ok, format!("playhead {ph:.2} kill {kt:.2}"));
        // 10 kill list row click: same lead-in
        let (id, r, sw) = e.w.trim.kill_rects[2].clone();
        e.k.s.insert("kid2", id);
        e.k.click(pos2((sw.right() + r.right()) / 2.0, r.center().y));
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let kid = e.k.s["kid2"].clone();
        let kt = e.w.st.ev.as_ref().and_then(|ev| ev.kills.iter().find(|k| k.id == kid)).map(|k| k.t).unwrap_or(f64::NAN);
        let ph = e.w.trim.timeline.playhead;
        e.k.ok("clicking a kill row jumps 1.5 s before it", (ph - (kt - 1.5)).abs() < 0.15, format!("{ph:.2}"));
        e.k.shot(e.ctx, "03_kill_selected");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        // 11 drag a right handle: trims, preview follows the handle, one undo step
        e.w.st.apply_quick_cut("kills", true);
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        let (_, b) = e.spans()[0];
        e.k.v.insert("b", b);
        e.w.trim.trimming_log.clear();
        let (p0, p1) = (e.tl(b, "video"), e.tl(b + 1.5, "video"));
        e.k.drag(p0, p1, Modifiers::NONE, 8, true);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let b = e.k.var("b");
        let nb = e.w.st.ed.parts[0].src_out;
        let px = 1.0 / e.w.trim.timeline.pps;
        e.k.ok("dragging a right handle extends the part", (nb - (b + 1.5)).abs() < 0.15 + px, format!("{b:.2} -> {nb:.2}"));
        let seen = e.w.trim.trimming_log.clone();
        e.k.ok(
            "the preview follows the handle while dragging",
            seen.len() >= 3 && (seen.last().copied().unwrap_or(0.0) - nb).abs() < 0.05,
            format!("{} positions", seen.len()),
        );
        let ph = e.w.trim.timeline.playhead;
        e.k.ok("after trimming an end the playhead sits 2 s before it", (ph - (nb - 2.0)).abs() < 0.2, format!("{ph:.2}"));
        e.k.ok("trim undo label", e.w.st.undo_label() == "trim change", e.w.st.undo_label());
        e.k.key(Key::Z, Modifiers::COMMAND);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let b = e.k.var("b");
        let ob = e.w.st.ed.parts[0].src_out;
        e.k.ok(
            "Ctrl+Z undoes the trim in one step",
            (ob - b).abs() < 1e-3 && e.w.toast == "Undid trim change",
            format!("{ob:.2} · toast {:?}", e.w.toast),
        );
        // 12 Esc cancels a handle drag
        let (p0, p1) = (e.tl(b, "video"), e.tl(b + 2.0, "video"));
        e.k.v.insert("p1x", p1.x as f64);
        e.k.v.insert("p1y", p1.y as f64);
        e.k.drag(p0, p1, Modifiers::NONE, 8, false);
    }));
    s.push(wait(0.2));
    s.push(act(|e| {
        e.k.v.insert("dragged", e.w.trim.timeline.parts[0].1);
        e.k.key(Key::Escape, Modifiers::NONE);
    }));
    s.push(wait(0.2));
    s.push(act(|e| {
        let p1 = pos2(e.k.var("p1x") as f32, e.k.var("p1y") as f32);
        e.k.release_at(p1);
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let b = e.k.var("b");
        let ok = e.k.var("dragged") > b + 1.0
            && (e.w.st.ed.parts[0].src_out - b).abs() < 1e-3
            && (e.w.trim.timeline.parts[0].1 - b).abs() < 1e-3;
        e.k.ok("Esc while dragging a handle cancels the trim", ok, format!("dragged to {:.2}, msg {:?}", e.k.var("dragged"), e.w.trim.msg));
        // 13 Shift-drag a new part across an existing one: stops at the neighbour
        let sp = e.spans();
        let start = sp[0].1 + 2.0;
        let (p0, p1) = (e.tl(start, "video"), e.tl(sp[1].0 + 1.5, "video"));
        e.k.drag(p0, p1, Modifiers::SHIFT, 8, true);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let sp = e.spans();
        let overlap = sp.windows(2).any(|w| w[0].1 > w[1].0 + 1e-6);
        e.k.ok(
            "Shift-drag adds a part that never overlaps its neighbour",
            sp.len() == e.k.parts0.len() + 1 && !overlap,
            format!("{sp:.2?}"),
        );
        e.k.shot(e.ctx, "04_new_part");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    // 14 S splits at the playhead; S at an edge explains
    s.push(act(|e| {
        let (a, b) = e.spans()[1];
        let st = &mut e.w.st;
        e.w.trim.seek_source((a + b) / 2.0, true, st);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("n", e.spans().len() as f64);
        e.k.key(Key::S, Modifiers::NONE);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let n = e.k.var("n") as usize;
        let (a, _) = e.spans()[1];
        let (_, b) = e.spans()[2];
        e.k.ok("S splits the part at the playhead", e.spans().len() == n + 1 && e.w.trim.msg.contains("Split"), e.w.trim.msg.clone());
        let sp = e.src_pos();
        e.k.ok("splitting keeps the playhead where it was", (sp - (a + b) / 2.0).abs() < 0.1, format!("{sp:.2}"));
        let st = &mut e.w.st;
        e.w.trim.seek_source(a + 0.05, true, st);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("n", e.spans().len() as f64);
        e.k.key(Key::S, Modifiers::NONE);
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let ok = e.spans().len() == e.k.var("n") as usize && e.w.trim.msg.contains("Too close");
        e.k.ok("S at the very edge of a part does nothing and says why", ok, e.w.trim.msg.clone());
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    // 15 Delete the selected part; playback continues at the next part
    s.push(act(|e| {
        let (a, b) = e.spans()[0];
        let p = e.tl((a + b) / 2.0, "video");
        e.k.click(p);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("sel", e.w.trim.timeline.selected as f64);
        e.k.v.insert("n", e.spans().len() as f64);
        e.k.v.insert("nxt", e.spans()[1].0);
        e.k.key(Key::Delete, Modifiers::NONE);
    }));
    s.push(wait(1.2));
    s.push(act(|e| {
        let ok = e.k.var("sel") == 0.0 && e.spans().len() == e.k.var("n") as usize - 1 && e.w.trim.msg.contains("Deleted");
        e.k.ok("clicking a part selects it, Delete removes it", ok, e.w.trim.msg.clone());
        let sp = e.src_pos();
        e.k.ok(
            "after deleting the playing part the preview carries on at the next part",
            (sp - e.k.var("nxt")).abs() < 0.1,
            format!("source {sp:.2} want {:.2}", e.k.var("nxt")),
        );
        e.k.ok("delete undo label (ui_undo)", e.w.st.undo_label() == "trim change", e.w.st.undo_label());
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    // 16 A in Edit preview explains; in Full video it adds 4 s
    s.push(act(|e| {
        e.k.v.insert("n", e.spans().len() as f64);
        e.k.key(Key::A, Modifiers::NONE);
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let can_add = e.w.trim.actions_state(&e.w.st)[2].0;
        let ok = e.spans().len() == e.k.var("n") as usize && e.w.trim.msg.contains("Full video") && !can_add;
        e.k.ok("A in Edit preview explains instead of doing nothing", ok, e.w.trim.msg.clone());
        let st = &mut e.w.st;
        e.w.trim.set_mode(Mode::Source, st);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let sp = e.spans();
        let gap = (sp[0].1 + sp[1].0) / 2.0;
        let st = &mut e.w.st;
        e.w.trim.seek_source(gap, true, st);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let can_add = e.w.trim.actions_state(&e.w.st)[2].0;
        e.k.ok("Add part is enabled on cut footage", can_add, "");
        e.k.key(Key::A, Modifiers::NONE);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.ok("A on cut footage adds a part around the playhead", e.spans().len() == e.k.var("n") as usize + 1, e.w.trim.msg.clone());
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    // 17 arrow keys step one frame; held arrow keeps moving
    s.push(act(|e| {
        let st = &mut e.w.st;
        e.w.trim.seek_source(20.0, true, st);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("p0", e.w.trim.player.pos());
        e.k.key(Key::ArrowRight, Modifiers::NONE);
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let fr = e.w.st.h.as_ref().map(|h| 1.0 / h.project.source.fps).unwrap_or(1.0 / 30.0);
        e.k.v.insert("fr", fr);
        let (p0, p1) = (e.k.var("p0"), e.w.trim.player.pos());
        e.k.ok("Right steps one frame", (p1 - (p0 + fr)).abs() < fr * 0.6, format!("{p0:.3} -> {p1:.3}"));
        e.k.v.insert("p0", p1);
        for _ in 0..6 {
            e.k.key(Key::ArrowRight, Modifiers::NONE);
        }
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        let (p0, p1, fr) = (e.k.var("p0"), e.w.trim.player.pos(), e.k.var("fr"));
        e.k.ok("six quick Right presses move six frames", (p1 - (p0 + 6.0 * fr)).abs() < fr * 0.6, format!("{p0:.3} -> {p1:.3}"));
        e.k.v.insert("p0", p1);
        e.k.key(Key::ArrowLeft, Modifiers::SHIFT);
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        let (p0, p1) = (e.k.var("p0"), e.w.trim.player.pos());
        e.k.ok("Shift+Left steps back 1 s", (p1 - (p0 - 1.0)).abs() < 0.05, format!("{p0:.2} -> {p1:.2}"));
        let st = &mut e.w.st;
        e.w.trim.set_mode(Mode::Edit, st);
    }));
    s.push(wait(0.8));
    // 18 dropping a kill after trimming by hand keeps the trim and says so
    s.push(act(|e| e.w.st.apply_quick_cut("kills", true)));
    s.push(wait(1.0));
    s.push(act(|e| {
        let mut p = e.spans();
        p[0].0 -= 1.0;
        e.k.v.insert("hand", p[0].0);
        e.w.st.set_parts(p.iter().map(|(a, b)| Part::new(*a, *b)).collect(), true);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let ok = e.w.trim.stats.custom;
        e.k.ok("hand-trimmed cut: quick cut list shows no quick cut selected", ok, "");
        e.k.shot(e.ctx, "05_custom_trim");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        let sw = e.w.trim.kill_rects.last().map(|r| r.2).unwrap_or(egui::Rect::NOTHING);
        e.k.click(sw.center());
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let ok = (e.w.st.ed.parts[0].src_in - e.k.var("hand")).abs() < 1e-3 && e.w.trim.msg.contains("unchanged");
        e.k.ok("dropping a kill keeps the hand trim", ok, e.w.trim.msg.clone());
        let sw = e.w.trim.kill_rects.last().map(|r| r.2).unwrap_or(egui::Rect::NOTHING);
        e.k.click(sw.center());
    }));
    s.push(wait(0.8));
    // 19 dropping a kill keeps the zoom; one undo step
    s.push(act(|e| e.w.st.apply_quick_cut("kills", true)));
    s.push(wait(1.0));
    s.push(act(|e| {
        e.w.trim.timeline.zoom_to(30.0, 45.0);
        e.k.v.insert("pps", e.w.trim.timeline.pps);
        e.k.v.insert("off", e.w.trim.timeline.offset);
        let sw = e.w.trim.kill_rects[1].2;
        e.k.click(sw.center());
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        let ok = (tl.pps - e.k.var("pps")).abs() < 1e-6 && (tl.offset - e.k.var("off")).abs() < 1e-6;
        e.k.ok("dropping a kill keeps the timeline zoom", ok, format!("pps {:.2} off {:.2}", tl.pps, tl.offset));
        e.k.v.insert("dropped", e.w.st.ed.kills_disabled.len() as f64);
        // the cut changed with the kill selection: the step is named after the parts first
        e.k.ok("kill toggle is one undo step", e.w.st.undo_label() == "trim change", e.w.st.undo_label());
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let ok = e.k.var("dropped") > 0.0 && e.w.st.ed.kills_disabled.is_empty() && e.spans().len() == e.k.parts0.len();
        e.k.ok("one Ctrl+Z brings the kill and the cut back", ok, "");
        e.w.trim.timeline.fit();
        // 20 add a missing kill: one undo step
        e.w.trim.victim_override = Some("Sova".into());
        let st = &mut e.w.st;
        e.w.trim.set_mode(Mode::Source, st);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let st = &mut e.w.st;
        e.w.trim.seek_source(5.0, true, st);
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        e.k.v.insert("nk", e.w.st.ev.as_ref().map(|ev| ev.kills.len()).unwrap_or(0) as f64);
        let r = e.rect("add_kill");
        e.k.click(r.center());
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let ev = e.w.st.ev.as_ref().unwrap();
        let added = ev.kills.len() == e.k.var("nk") as usize + 1 && ev.kills.iter().any(|k| k.manual && (k.t - 5.0).abs() < 0.1);
        let manual_ids: Vec<String> = ev.kills.iter().filter(|k| k.manual).map(|k| k.id.clone()).collect();
        let in_list = e.w.trim.kills.iter().any(|k| manual_ids.contains(&k.id));
        let in_cut = e.spans().iter().any(|(a, b)| *a <= 5.0 && 5.0 <= *b);
        e.k.ok("adding a missing kill puts it in the list and the cut", added && in_list && in_cut, e.w.trim.msg.clone());
        e.k.shot(e.ctx, "06_manual_kill");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let nk = e.w.st.ev.as_ref().map(|ev| ev.kills.len()).unwrap_or(0);
        let in_cut = e.spans().iter().any(|(a, b)| *a <= 5.0 && 5.0 <= *b);
        e.k.ok("one Ctrl+Z removes the added kill and its part", nk == e.k.var("nk") as usize && !in_cut, format!("{nk} kills"));
        e.w.trim.victim_override = None;
        let st = &mut e.w.st;
        e.w.trim.set_mode(Mode::Edit, st);
    }));
    s.push(wait(0.6));
    // 21 undo of a volume change; music controls disabled without music
    s.push(act(|e| {
        e.k.v.insert("vol0", e.w.st.ed.music.volume);
        e.k.s.insert("music0", e.w.st.ed.music.path.clone());
        e.w.st.ed.music.volume = if e.w.st.ed.music.volume < 0.8 { e.w.st.ed.music.volume + 0.2 } else { e.w.st.ed.music.volume - 0.2 };
        e.w.st.touch();
    }));
    s.push(wait(0.9));
    s.push(act(|e| e.w.st.undo()));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.ok(
            "undo of a volume change moves the slider back",
            (e.w.st.ed.music.volume - e.k.var("vol0")).abs() < 1e-9,
            format!("{:.2}", e.w.st.ed.music.volume),
        );
        e.w.st.ed.music.path = String::new();
        e.w.st.touch();
    }));
    s.push(wait(0.9));
    s.push(act(|e| {
        e.k.ok("no music: no music lane", !e.w.trim.music_visible, "");
        e.k.shot(e.ctx, "07_no_music");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.w.st.undo();
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.ok("undo brings the music path back", e.w.st.ed.music.path == e.k.s["music0"], e.w.st.ed.music.path.clone());
    }));
    // 22 tabs: position kept, Trim shortcuts off on other pages
    s.push(act(|e| {
        let a = e.spans()[1].0;
        let st = &mut e.w.st;
        e.w.trim.seek_source(a + 0.8, true, st);
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.v.insert("p_keep", e.w.trim.player.pos());
        e.w.go(LAYOUT);
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        e.k.v.insert("n", e.spans().len() as f64);
        e.k.key(Key::S, Modifiers::NONE);
        e.k.key(Key::Delete, Modifiers::NONE);
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        e.k.ok("Trim shortcuts don't fire on other pages", e.spans().len() == e.k.var("n") as usize, "");
        e.w.go(TRIM);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let (p, keep) = (e.w.trim.player.pos(), e.k.var("p_keep"));
        e.k.ok("position kept after switching tabs", (p - keep).abs() < 0.05, format!("{p:.2} vs {keep:.2}"));
    }));
    // 22b captions edited on another page reach the Trim preview
    s.push(act(|e| {
        if e.w.st.tr.as_ref().map(|t| !t.segments.is_empty()).unwrap_or(false) {
            e.w.st.ed.subtitles.enabled = true;
            e.w.st.touch();
        }
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        e.w.go(SUBTITLES);
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        let parts = e.w.st.ed.parts.clone();
        let mut edited = false;
        if let Some(tr) = e.w.st.tr.as_mut() {
            if let Some(sg) =
                tr.segments.iter_mut().find(|x| !x.words.is_empty() && parts.iter().any(|p| p.src_in <= x.start && x.start <= p.src_out))
            {
                sg.words[0].text = "zebrafish".into();
                sg.hidden = false; // the caption under the cut may be hidden in this clip: show it
                sg.text = sg.words.iter().map(|w| w.text.clone()).collect::<Vec<_>>().join(" ");
                edited = true;
            }
        }
        if edited {
            e.w.st.save_transcript();
        }
        e.k.v.insert("edited", edited as i32 as f64);
    }));
    s.push(wait(0.3));
    s.push(act(|e| e.w.go(TRIM)));
    s.push(wait(1.0));
    s.push(act(|e| {
        if e.k.var("edited") == 1.0 {
            let key = e.w.trim.subs_key(&e.w.st);
            let ass =
                e.w.st
                    .h
                    .as_ref()
                    .map(|h| h.dir.join("cache").join("preview_edit.ass"))
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();
            let same = e.w.trim.subs_sig.as_deref() == Some(key.as_str());
            let lines = ass.lines().filter(|l| l.starts_with("Dialogue")).count();
            e.k.ok(
                "caption edits made on Subtitles show in the Trim preview",
                same && ass.contains("zebrafish"),
                format!(
                    "sig current {same}, {lines} dialogue lines, subs on {}, mode {:?}, parts {:?}, subs {}",
                    e.w.st.ed.subtitles.enabled,
                    e.w.trim.mode,
                    e.spans(),
                    serde_json::to_string(&e.w.st.ed.subtitles).unwrap_or_default()
                ),
            );
        } else {
            // a clip that was never transcribed has no caption to edit: nothing to check (not a failure)
            let untranscribed = e.w.st.tr.as_ref().map(|t| t.segments.is_empty()).unwrap_or(true);
            e.k.ok(
                "caption edits made on Subtitles show in the Trim preview",
                untranscribed,
                format!(
                    "{}nothing edited: tr {:?} segments, parts {:?}",
                    if untranscribed { "skipped, the clip has no captions: " } else { "" },
                    e.w.st.tr.as_ref().map(|t| t.segments.iter().map(|s| (s.start, s.hidden, s.words.len())).collect::<Vec<_>>()),
                    e.spans()
                ),
            );
        }
    }));
    // 23 resize keeps "fit"; zoom buttons zoom around the playhead
    s.push(act(|e| {
        e.w.trim.timeline.fit();
        e.ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1250.0, 900.0)));
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        let d = (tl.screen_pos(e.w.st.duration(), "video").x - tl.rect.right()).abs();
        e.k.ok("fitted timeline stays fitted after a resize", d < 2.0, format!("{d:.1} px, width {:.0}", tl.rect.width()));
        e.k.shot(e.ctx, "08_small_window");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1600.0, 950.0)));
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        let d = (tl.screen_pos(e.w.st.duration(), "video").x - tl.rect.right()).abs();
        e.k.ok("...and after growing again", d < 2.0, format!("{d:.1} px, width {:.0}", tl.rect.width()));
        let st = &mut e.w.st;
        e.w.trim.seek_source(40.0, true, st);
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        let r = e.rect("zoom_in");
        for _ in 0..4 {
            e.k.click(r.center());
        }
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        let (v0, v1) = (tl.offset, tl.offset + tl.view_w() / tl.pps);
        e.k.ok(
            "zooming in keeps the playhead on screen",
            v0 <= tl.playhead && tl.playhead <= v1 && tl.pps > 30.0,
            format!("[{v0:.1},{v1:.1}] ph {:.1} pps {:.1}", tl.playhead, tl.pps),
        );
        e.k.shot(e.ctx, "09_zoomed");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        // Ctrl+wheel zooms out around the pointer
        let pps = tl.pps;
        e.k.v.insert("pps", pps);
        let p = tl.screen_pos(tl.playhead, "video");
        e.k.push(vec![Event::PointerMoved(p)], Modifiers::CTRL);
        e.k.push(
            vec![Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: vec2(0.0, -1.0),
                modifiers: Modifiers::CTRL,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
            }],
            Modifiers::CTRL,
        );
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        e.k.ok("Ctrl+wheel zooms", (tl.pps - e.k.var("pps") * 0.8).abs() < 1e-6, format!("{:.2} -> {:.2}", e.k.var("pps"), tl.pps));
        e.w.trim.timeline.fit();
        e.w.trim.jump(None);
    }));
    // 24 playing to the end and pressing Space plays again from the start
    s.push(wait(0.6));
    s.push(act(|e| e.k.key(Key::Space, Modifiers::NONE)));
    s.push(wait(0.6));
    s.push(act(|e| {
        let p = e.w.trim.player.pos();
        e.k.ok("Space at the end of the edit plays again from the start", e.w.trim.player.is_playing() && p < 1.5, format!("{p:.2}"));
        e.w.trim.player.pause();
        e.w.st.apply_quick_cut("kills", true);
    }));
    s.push(wait(0.8));
    // 25 all kills dropped -> empty states
    s.push(act(|e| {
        e.w.st.ed.kills_disabled = e.w.st.my_kills(true).iter().map(|k| k.id.clone()).collect();
        e.w.st.apply_quick_cut("kills", true);
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        e.k.ok(
            "nothing kept: says what to do",
            e.w.st.ed.parts.is_empty() && e.w.trim.msg.contains("Nothing is kept"),
            e.w.trim.msg.clone(),
        );
        e.k.ok("no highlights: explains instead of an empty list", e.w.trim.stats.segs.is_empty(), "");
        e.k.shot(e.ctx, "10_trim_empty");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        e.w.st.ed.kills_disabled = vec![];
        e.w.st.apply_quick_cut("kills", true);
    }));
    s.push(wait(1.0));
    s.push(act(|e| e.k.shot(e.ctx, "11_trim")));
    s.push(wait(0.3));
    // ---------------------------------------------------------------- 26 long clip
    s.push(act(|e| {
        let d = e.k.dirs["long"].clone();
        e.w.open_project(&d, false);
    }));
    s.push(wait(0.4));
    s.push(close_dialogs());
    s.push(act(|e| e.w.go(TRIM)));
    s.push(wait(1.5));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        let building = !tl.strip_note.is_empty() && tl.thumbs.is_none();
        e.k.ok("long clip: the film strip says it is building", building || tl.thumbs.is_some(), format!("note {:?}", tl.strip_note));
        let d = (tl.screen_pos(e.w.st.duration(), "video").x - tl.rect.right()).abs();
        e.k.ok("long clip opens fitted", d < 2.0 && tl.selected == -1, format!("{d:.1} px"));
        let rounds = e.w.st.ev.as_ref().map(|ev| ev.rounds.clone()).unwrap_or_default();
        let p0 = e.w.st.ed.parts.first().map(|p| p.src_in).unwrap_or(0.0);
        let want = rounds
            .iter()
            .find(|r| r.start <= p0 && p0 < r.end)
            .and_then(|r| r.score_before)
            .map(|(a, b)| format!("Round {} ", a + b + 1))
            .unwrap_or_default();
        let ok = (tl.playhead - p0).abs() < 0.1 && e.w.trim.round_label.starts_with(&want);
        e.k.ok("switching clips updates the round and time labels", ok, format!("{} | {}", e.w.trim.round_label, e.w.trim.src_t_label));
        e.w.trim.victim_override = Some(String::new());
        let st = &mut e.w.st;
        e.w.trim.set_mode(Mode::Source, st);
    }));
    s.push(wait(0.8));
    for t in [300.0, 400.0, 500.0] {
        // more than 8 kills so the list scrolls
        s.push(act(move |e| {
            let st = &mut e.w.st;
            e.w.trim.seek_source(t, true, st);
        }));
        s.push(wait(0.4));
        s.push(act(|e| {
            let (st, sh) = (&mut e.w.st, &mut e.w.shell);
            e.w.trim.add_manual_kill(st, sh);
        }));
        s.push(wait(0.4));
    }
    s.push(act(|e| {
        e.w.trim.victim_override = None;
        let st = &mut e.w.st;
        e.w.trim.set_mode(Mode::Edit, st);
        println!("  long clip kills: {}", e.w.trim.kills.len());
        if let Some((_, r, _)) = e.w.trim.kill_rects.first().cloned() {
            e.k.wheel(r.center(), -40.0);
        }
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        if e.w.trim.kills.len() > 8 {
            // the last row is visible now: drop it and see that the list stays put
            let (id, r, sw) = e.w.trim.kill_rects.last().cloned().unwrap();
            e.k.s.insert("scroll_kid", id);
            e.k.v.insert("scroll_y", r.top() as f64);
            e.k.click(sw.center());
        }
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        if let Some(kid) = e.k.s.get("scroll_kid").cloned() {
            let y = e.w.trim.kill_rects.iter().find(|r| r.0 == kid).map(|r| r.1.top() as f64).unwrap_or(f64::NAN);
            let dropped = e.w.st.ed.kills_disabled.contains(&kid);
            e.k.ok(
                "dropping a kill doesn't scroll the kill list",
                dropped && (y - e.k.var("scroll_y")).abs() < 1.0,
                format!("{y:.0} vs {:.0}", e.k.var("scroll_y")),
            );
            e.k.shot(e.ctx, "12_long_scrolled");
        }
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        if e.k.s.contains_key("scroll_kid") {
            e.w.st.undo();
        }
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        if let Some(kid) = e.k.s.get("scroll_kid").cloned() {
            let y = e.w.trim.kill_rects.iter().find(|r| r.0 == kid).map(|r| r.1.top() as f64).unwrap_or(f64::NAN);
            e.k.ok("undo keeps the kill list scroll too", (y - e.k.var("scroll_y")).abs() <= 1.0, format!("{y:.0}"));
        }
    }));
    s.push(until(180.0, |e| e.w.trim.timeline.thumbs.is_some()));
    s.push(act(|e| {
        let tl = &e.w.trim.timeline;
        e.k.ok("long clip film strip arrives", tl.thumbs.is_some() && tl.strip_note.is_empty(), "");
        e.k.shot(e.ctx, "13_trim_long");
    }));
    s.push(wait(0.3));
    // ---------------------------------------------------------------- music
    s.push(act(|e| {
        let d = e.k.dirs["music"].clone();
        e.w.open_project(&d, false);
    }));
    s.push(wait(0.4));
    s.push(close_dialogs());
    s.push(act(|e| e.w.go(TRIM)));
    s.push(wait(1.5));
    s.push(act(|e| {
        e.k.ok("music: no music lane before a song is chosen", !e.w.trim.music_visible, "");
        // a generated 128 BPM song with a drop (no real music file needed)
        let song = paths::cache_dir().join("qa").join("test128.wav");
        if !song.exists() {
            let _ = vl_render::music::test_song::write_wav(&song);
        }
        e.w.trim.pick_override = Some(song.to_string_lossy().into_owned());
        let r = e.rect("choose_music");
        e.k.click(r.center());
    }));
    s.push(until(20.0, |e| e.w.trim.song.is_some()));
    s.push(wait(1.5));
    s.push(act(|e| {
        let m = e.w.st.ed.music.clone();
        e.k.ok("picked song plays as background", m.enabled && m.song_in == 0.0 && m.start == 0.0 && m.end.is_none(), e.w.trim.msg.clone());
        e.k.ok("music lane shows with the analysed song", e.w.trim.music_visible && e.w.trim.lane.info.is_some(), e.w.trim.mplace.clone());
        e.k.ok(
            "label says what plays where",
            e.w.trim.mplace.contains("song 0:00.0") && e.w.trim.mplace.contains("BPM"),
            e.w.trim.mplace.clone(),
        );
        e.k.ok(
            "edit preview mixes the music in",
            e.w.trim.loaded_music.as_deref().map(|p| p.ends_with(".wav") && Path::new(p).exists()).unwrap_or(false),
            format!("{:?}", e.w.trim.loaded_music),
        );
        e.w.trim.player.play();
    }));
    s.push(wait(1.5));
    s.push(act(|e| {
        let p1 = e.w.trim.player.pos();
        e.k.ok(
            "preview plays with music",
            p1 > 0.5 && e.w.trim.lane.playhead.is_some(),
            format!("pos {p1:.2} song {:?}", e.w.trim.lane.playhead),
        );
        e.k.shot(e.ctx, "14_music_playing");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.w.trim.player.pause();
        // drag the playing part
        let win = e.w.trim.lane.win_screen();
        e.k.v.insert("si0", e.w.st.ed.music.song_in);
        let a = pos2(win.center().x - 30.0, win.center().y);
        e.k.drag(a, a + vec2(90.0, 0.0), Modifiers::NONE, 9, true);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let si = e.w.st.ed.music.song_in;
        e.k.ok("dragging the part moves which bit of the song plays", si > e.k.var("si0") + 1.0, format!("{si:.2}"));
        e.k.ok("music undo label", e.w.st.undo_label() == "music change", e.w.st.undo_label());
        e.w.st.undo();
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let si0 = e.k.var("si0");
        e.k.ok("undo puts it back", (e.w.st.ed.music.song_in - si0).abs() < 1e-6 && (e.w.trim.lane.song_in - si0).abs() < 1e-6, "");
        // trim the start
        let win = e.w.trim.lane.win_screen();
        let a = pos2(win.left(), win.center().y);
        e.k.drag(a, a + vec2(50.0, 0.0), Modifiers::NONE, 5, true);
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let m = e.w.st.ed.music.clone();
        e.k.ok(
            "dragging the left end starts the music later in the edit",
            m.start > 0.5 && (m.song_in - m.start).abs() < 1e-6,
            format!("start {:.2} song_in {:.2}", m.start, m.song_in),
        );
        // Esc cancels a lane drag
        let win = e.w.trim.lane.win_screen();
        e.k.v.insert("si1", m.song_in);
        let a = pos2(win.center().x, win.center().y);
        e.k.drag(a, a + vec2(80.0, 0.0), Modifiers::NONE, 4, false);
    }));
    s.push(wait(0.2));
    s.push(act(|e| e.k.key(Key::Escape, Modifiers::NONE)));
    s.push(wait(0.2));
    s.push(act(|e| {
        let win = e.w.trim.lane.win_screen();
        e.k.release_at(win.center());
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        let ok = (e.w.st.ed.music.song_in - e.k.var("si1")).abs() < 1e-6 && e.w.trim.msg == "Move cancelled";
        e.k.ok("Esc cancels moving the music", ok, e.w.trim.msg.clone());
        let r = e.rect("background");
        e.k.click(r.center());
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let m = e.w.st.ed.music.clone();
        e.k.ok("Background resets the placement", m.song_in == 0.0 && m.start == 0.0 && m.end.is_none(), e.w.trim.msg.clone());
        e.k.parts0 = e.spans();
        let r = e.rect("sync");
        e.k.click(r.center());
    }));
    s.push(wait(1.0));
    s.push(act(|e| {
        let m = e.w.st.ed.music.clone();
        let el = EditList::new(&e.w.st.ed.parts, &e.w.st.ed.effects.transition);
        let song = e.w.trim.song.clone().unwrap();
        let beats: Vec<f64> = song.beats.iter().map(|b| b - m.song_in).collect();
        let mut cuts: Vec<f64> = el.placed.iter().skip(1).map(|p| p.out_start + el.td / 2.0).collect();
        cuts.push(el.duration);
        let errs: Vec<f64> = cuts
            .iter()
            .map(|c| (1000.0 * beats.iter().map(|b| (b - c).abs()).fold(f64::INFINITY, f64::min) * 10.0).round() / 10.0)
            .collect();
        e.k.ok("Sync: placement found and marked", m.synced && e.w.trim.mplace.contains("synced"), e.w.trim.mplace.clone());
        e.k.ok("Sync: every cut on a beat", errs.iter().cloned().fold(0.0, f64::max) < 1.0, format!("errs ms {errs:?} · {}", e.w.trim.msg));
        let kills: Vec<f64> =
            e.w.st.my_kills(false).iter().map(|k| k.t).filter(|t| e.k.parts0.iter().any(|(a, b)| a <= t && t <= b)).collect();
        e.k.ok("Sync: no kill lost", kills.iter().all(|t| e.w.st.ed.parts.iter().any(|p| p.src_in <= *t && *t <= p.src_out)), "");
        let first = kills.iter().filter_map(|t| el.to_output(*t)).fold(f64::INFINITY, f64::min);
        let drop_song = m.song_in + first;
        let near = song.bars.iter().map(|b| (b - drop_song).abs()).fold(f64::INFINITY, f64::min);
        e.k.ok("Sync: the song's bar lands on the first kill", near < 0.05, format!("first kill {first:.2}, song {drop_song:.2}"));
        e.k.ok("Sync undo label", e.w.st.undo_label() == "beat sync", e.w.st.undo_label());
        e.k.shot(e.ctx, "15_synced");
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.w.st.undo();
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        let ok = e.spans() == e.k.parts0 && !e.w.st.ed.music.synced;
        e.k.ok("one Ctrl+Z undoes the whole sync", ok, "");
        let r = e.rect("snap");
        e.k.click(r.center());
    }));
    s.push(wait(0.8));
    s.push(act(|e| {
        e.k.ok(
            "Snap cuts keeps the song and says how many cuts moved",
            !e.w.st.ed.music.synced && e.w.trim.msg.starts_with("Cuts snapped"),
            e.w.trim.msg.clone(),
        );
        e.w.st.undo();
    }));
    // the undo can show/hide the "your own trim" note above, moving the side panel: click once it settled
    s.push(wait(0.3));
    s.push(act(|e| {
        // music off -> the lane goes away; volume slider follows undo
        let r = e.rect("music_toggle");
        e.k.click(pos2(r.left() + 18.0, r.center().y));
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let d = format!(
            "enabled {} lane {} toggle {:?} undo {:?}",
            e.w.st.ed.music.enabled,
            e.w.trim.music_visible,
            e.rect("music_toggle"),
            e.w.st.undo_label()
        );
        e.k.ok("turning the music off hides the lane", !e.w.st.ed.music.enabled && !e.w.trim.music_visible, d);
        e.w.st.undo();
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        e.k.ok("undo turns it back on", e.w.st.ed.music.enabled && e.w.trim.music_visible, "");
        e.k.shot(e.ctx, "16_music_lane");
    }));
    s.push(wait(0.3));
    // ---------------------------------------------------------------- Smart cut
    // the first pick listens to the clip (a background job), then the cut follows with its reasons
    s.push(act(|e| {
        e.k.parts0 = e.spans();
        let ready = e.w.st.smart_ready();
        e.k.ok("Smart cut hasn't listened to this clip yet", !ready, "");
        let r = e.w.trim.quick_rects.iter().find(|(k, _)| k == "smart").map(|x| x.1).unwrap_or(egui::Rect::NOTHING);
        e.k.click(r.center());
    }));
    s.push(wait(0.4));
    s.push(act(|e| {
        let running = e.w.trim.smart_job.clone();
        e.k.ok(
            "picking Smart cut starts listening to the clip, with progress",
            running.is_some() || e.w.st.smart_ready(),
            format!("{running:?}"),
        );
        e.k.shot(e.ctx, "17_smart_listening");
    }));
    s.push(until(90.0, |e| e.w.trim.smart_job.is_none() && e.w.st.smart_ready()));
    s.push(wait(0.6));
    s.push(act(|e| {
        let want: Vec<(f64, f64)> =
            e.w.st.quick_cut_parts(Some("smart")).unwrap_or_default().iter().map(|p| (p.src_in, p.src_out)).collect();
        let ok = e.w.st.ed.quick_cut == "smart" && !want.is_empty() && e.spans() == want && !e.w.trim.stats.custom;
        e.k.ok("when listening is done the Smart cut is applied", ok, format!("{} parts, quick cut {:?}", want.len(), e.w.st.ed.quick_cut));
        let titles: Vec<String> = e.w.trim.stats.segs.iter().map(|s| s.title.clone()).collect();
        e.k.ok(
            "each Smart cut highlight says why it's in",
            !titles.is_empty() && titles.iter().all(|t| !t.is_empty()),
            format!("{titles:?}"),
        );
        let saved = e.w.st.h.as_ref().and_then(|h| h.events()).and_then(|ev| ev.audio).map(|a| a.loud.len()).unwrap_or(0);
        e.k.ok("the sound signals are saved with the clip", saved > 0, format!("{saved} frames"));
        e.k.shot(e.ctx, "18_smart_cut");
    }));
    s.push(wait(0.5));
    s.push(act(|e| {
        e.w.st.undo();
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let back = e.spans() == e.k.parts0;
        e.k.ok("one undo goes back to the cut before Smart cut", back, format!("{} parts", e.spans().len()));
        let kept = e.w.st.smart_ready() && e.w.st.h.as_ref().and_then(|h| h.events()).and_then(|ev| ev.audio).is_some();
        e.k.ok("undo keeps what Smart cut heard (no second listening)", kept, "");
        // picked again: no second listening pass
        let r = e.w.trim.quick_rects.iter().find(|(k, _)| k == "smart").map(|x| x.1).unwrap_or(egui::Rect::NOTHING);
        e.k.click(r.center());
    }));
    s.push(wait(0.6));
    s.push(act(|e| {
        let ok = e.w.trim.smart_job.is_none() && e.w.st.ed.quick_cut == "smart" && e.w.shell.job.is_none();
        e.k.ok("picking Smart cut again applies it straight away", ok, format!("{:?}", e.w.trim.smart_job));
    }));
    s.push(wait(0.3));
    s.push(act(|e| {
        e.k.done = true;
    }));
    s
}

struct CheckApp {
    app: ValoApp,
    chk: Check,
    steps: Vec<StepFn>,
    step: usize,
    f: u32,
    start: Instant,
    deadline: Instant,
}

impl eframe::App for CheckApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.win.frame_ui(ui, frame);
        if Instant::now() > self.deadline && !self.chk.done {
            self.chk.ok("finished in time", false, format!("stuck at step {}", self.step));
            self.chk.done = true;
        }
        if !self.chk.inject.is_empty() {
            ctx.request_repaint();
            return;
        }
        if !self.chk.done && self.step < self.steps.len() {
            let mut env = Env { w: &mut self.app.win, k: &mut self.chk, ctx: &ctx, f: self.f, el: self.start.elapsed() };
            let fin = (self.steps[self.step])(&mut env);
            if fin {
                self.step += 1;
                self.f = 0;
                self.start = Instant::now();
            } else {
                self.f += 1;
            }
            ctx.request_repaint();
        } else if self.f < 1000 {
            self.chk.done = true;
            self.f += 1;
            if self.f > 10 {
                self.f = 1000;
                self.app.win.shell.push(crate::pages::Action::Quit);
            }
            ctx.request_repaint();
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some((mut evs, m)) = self.chk.inject.pop_front() {
            // the modifiers held for this batch come first (egui reads them from the event stream)
            raw.events.push(Event::ModifiersChanged(m));
            raw.events.append(&mut evs);
            raw.focused = true;
        }
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        eframe::App::on_exit(&mut self.app, gl);
    }

    fn clear_color(&self, v: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(v)
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

struct Owner {
    inner: CheckApp,
    sink: std::sync::Arc<std::sync::Mutex<Vec<(String, bool, String)>>>,
}

impl eframe::App for Owner {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui, frame);
        *self.sink.lock().unwrap() = self.inner.chk.results.clone();
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.inner.raw_input_hook(ctx, raw);
    }
    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        *self.sink.lock().unwrap() = self.inner.chk.results.clone();
        self.inner.on_exit(gl);
    }
    fn clear_color(&self, v: &egui::Visuals) -> [f32; 4] {
        self.inner.clear_color(v)
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// Hash of every file under data/projects (except rustqa_*) and data/config.toml.
fn fingerprint() -> BTreeMap<PathBuf, (u64, u64)> {
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, (u64, u64)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().map(|n| n.to_string_lossy().starts_with("rustqa_")).unwrap_or(false) {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(p, (bytes.len() as u64, h.finish()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&paths::projects_dir(), &mut out);
    if let Ok(b) = std::fs::read(paths::config_path()) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        b.hash(&mut h);
        out.insert(paths::config_path(), (b.len() as u64, h.finish()));
    }
    out
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &dst.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, dst.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// A writable copy of a project; `music` = start with no music and no transitions.
fn make_copy(src: &str, id: &str, music: bool) -> Option<PathBuf> {
    let from = paths::projects_dir().join(src);
    let to = paths::projects_dir().join(id);
    let _ = std::fs::remove_dir_all(&to);
    copy_dir(&from, &to).ok()?;
    let mut h = vl_core::projects::open_dir(&to).ok()?;
    h.project.id = id.to_string();
    h.project.name = format!("{} (trim QA)", h.project.name);
    h.save().ok()?;
    if music {
        let mut ed = h.edits();
        ed.effects.transition = "none".into();
        ed.music = Default::default();
        h.save_edits(&ed).ok()?;
    }
    Some(to)
}

pub fn run(args: &[String]) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("uicheck-trim", false);
    let out = args.first().map(PathBuf::from).unwrap_or_else(|| paths::app_root().join("scratch").join("vl-app-trim").join("uicheck"));
    let _ = std::fs::create_dir_all(&out);
    let before = fingerprint();
    let stamp = chrono::Local::now().format("%H%M%S").to_string();
    let mut dirs = HashMap::new();
    let id = |stem: &str| {
        crate::uicheck::find_project(stem).and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default()
    };
    let (short, long) = (id("example_clip-short-4-3"), id("example-long-3-4"));
    for (k, src, music) in [("short", short.as_str(), false), ("long", long.as_str(), false), ("music", short.as_str(), true)] {
        match make_copy(src, &format!("rustqa_trim_{k}_{stamp}"), music) {
            Some(d) => {
                dirs.insert(k, d);
            }
            None => {
                eprintln!("could not copy {src}");
                for d in dirs.values() {
                    let _ = std::fs::remove_dir_all(d);
                }
                return 2;
            }
        }
    }
    println!("QA copies: {:?}\nscreenshots: {}", dirs.values().collect::<Vec<_>>(), out.display());
    let opts = Options { read_only: true, writable_prefix: Some("rustqa_trim_".into()), no_resume: true, no_first_run: true };
    let results = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let sink = results.clone();
    let dirs2 = dirs.clone();
    let run = eframe::run_native(
        "ValoLights Trim check",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            let win = MainWindow::new(app::gl_env(cc), opts);
            Ok(Box::new(Owner {
                inner: CheckApp {
                    app: ValoApp { win },
                    chk: Check {
                        out,
                        dirs: dirs2,
                        inject: VecDeque::new(),
                        results: vec![],
                        v: HashMap::new(),
                        parts0: vec![],
                        s: HashMap::new(),
                        done: false,
                    },
                    steps: steps(),
                    step: 0,
                    f: 0,
                    start: Instant::now(),
                    deadline: Instant::now() + Duration::from_secs(600),
                },
                sink,
            }))
        }),
    );
    if let Err(e) = run {
        eprintln!("GUI failed: {e}");
    }
    for d in dirs.values() {
        let _ = std::fs::remove_dir_all(d);
    }
    let _ = std::fs::remove_dir_all(paths::tmp_dir().join("trim"));
    let after = fingerprint();
    let mut results = results.lock().unwrap().clone();
    let changed: Vec<String> = before
        .iter()
        .filter(|(p, v)| after.get(*p) != Some(v))
        .map(|(p, _)| p.display().to_string())
        .chain(after.keys().filter(|p| !before.contains_key(*p)).map(|p| format!("new: {}", p.display())))
        .collect();
    results.push(("user data untouched".into(), changed.is_empty(), format!("{} files changed {changed:?}", changed.len())));
    results.push(("QA copies removed".into(), dirs.values().all(|d| !d.exists()), String::new()));
    let fails = results.iter().filter(|r| !r.1).count();
    let report: String = results.iter().map(|(n, ok, d)| format!("[{}] {n}: {d}\n", if *ok { " OK " } else { "FAIL" })).collect();
    let report = format!("{report}\nRESULT: {} ({} checks, {fails} failed)\n", if fails == 0 { "PASS" } else { "FAIL" }, results.len());
    let _ = std::fs::write(paths::app_root().join("scratch").join("vl-app-trim").join("uicheck_trim_report.txt"), &report);
    println!("{report}");
    let _ = backend::TRANSITION_DUR;
    i32::from(fails > 0)
}
