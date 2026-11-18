//! `valolights --ui-check layout [out_dir]`: drives the real window through the Layout page with injected
//! input and saves
//! the app's own frames (egui viewport screenshots, never the desktop) as PNGs.
//!
//! Runs on a scratch copy `data/projects/rustqa_layout_*` of the short 4:3 example (deleted at the end);
//! every other project and data/config.toml are hashed before and after.
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::pages::layout::LayoutPage;
use crate::pages::{Action, LAYOUT, SUBTITLES};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vl_core::models::Rect4;
use vl_core::paths;

const SETTLE: Duration = Duration::from_millis(750);

#[derive(PartialEq)]
enum Step {
    Next,
    Wait,
}

#[derive(Default)]
struct Vars {
    n0: isize,
    dst0: Rect4,
    src0: Rect4,
    moved: Rect4,
    y1: f64,
    r0: i64,
    a0: f64,
    sx: f64,
    sw: f64,
    v0: f64,
    look0: String,
    newest: String,
    anim: String,
    fr0_h: f32,
    pos0: f64,
    seen: Vec<String>,
    ids: Vec<String>,
}

struct Check {
    out: PathBuf,
    qa_dir: PathBuf,
    inject: VecDeque<Vec<Event>>,
    step: usize,
    phase: u32,
    sub: usize,
    step_start: Instant,
    phase_start: Instant,
    frames: u32,
    results: Vec<(String, bool, String)>,
    v: Vars,
    reported: Vec<(usize, u32, String)>,
    reveal_last: Option<Rect>,
    reveal_since: Instant,
    done: bool,
}

impl Check {
    fn ok(&mut self, name: &str, pass: bool, detail: impl Into<String>) {
        // a check written before a click that waits for scrolling runs once per step phase
        let key = (self.step, self.phase, name.to_string());
        if self.reported.contains(&key) {
            return;
        }
        self.reported.push(key);
        let d = detail.into();
        println!("[{}] {name}: {d}", if pass { " OK " } else { "FAIL" });
        self.results.push((name.into(), pass, d));
    }
    fn moved_to(&mut self, p: Pos2) {
        self.inject.push_back(vec![Event::PointerMoved(p)]);
    }
    fn click(&mut self, p: Pos2) {
        let m = Modifiers::NONE;
        self.moved_to(p);
        self.inject.push_back(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: m }]);
        self.inject.push_back(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: m }]);
        self.inject.push_back(vec![]);
    }
    fn click_now(&mut self, p: Pos2) {
        let m = Modifiers::NONE;
        self.moved_to(p);
        self.inject.push_back(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: m }]);
        self.inject.push_back(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: m }]);
    }
    fn drag(&mut self, a: Pos2, b: Pos2, steps: usize, esc: bool) {
        let m = Modifiers::NONE;
        self.moved_to(a);
        self.inject.push_back(vec![Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: m }]);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            self.moved_to(a + (b - a) * t);
        }
        if esc {
            self.key(Key::Escape, Modifiers::NONE);
        }
        self.inject.push_back(vec![Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: m }]);
        self.inject.push_back(vec![]);
    }
    fn key(&mut self, key: Key, m: Modifiers) {
        self.inject.push_back(vec![
            Event::ModifiersChanged(m),
            Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m },
        ]);
        self.inject.push_back(vec![
            Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m },
            Event::ModifiersChanged(Modifiers::NONE),
        ]);
    }
    fn wheel(&mut self, p: Pos2, dy: f32) {
        self.moved_to(p);
        self.inject.push_back(vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, dy),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        }]);
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
    fn since(&self) -> Duration {
        self.phase_start.elapsed()
    }
    fn next_phase(&mut self) {
        self.phase += 1;
        self.phase_start = Instant::now();
        self.frames = 0;
    }
}

fn dst(w: &MainWindow, kind: &str) -> Rect4 {
    w.st.ed.layout.as_ref().and_then(|l| l.layer(kind)).map(|l| l.dst).unwrap_or_default()
}
fn src(w: &MainWindow, kind: &str) -> Rect4 {
    w.st.ed.layout.as_ref().and_then(|l| l.layer(kind)).map(|l| l.src).unwrap_or_default()
}
fn cropping(w: &MainWindow) -> String {
    w.layout.crop.as_ref().map(|c| c.kind.clone()).unwrap_or_default()
}
fn visible(w: &MainWindow, kind: &str) -> bool {
    w.st.ed.layout.as_ref().and_then(|l| l.layer(kind)).map(|l| l.visible).unwrap_or(false)
}
fn layer_rect(w: &MainWindow, kind: &str) -> Rect {
    w.st.ed.layout.as_ref().and_then(|l| l.layer(kind)).map(|l| w.layout.canvas.dst_rect(l)).unwrap_or(Rect::NOTHING)
}
fn aspect(w: &MainWindow) -> String {
    w.st.ed.layout.as_ref().map(|l| l.aspect.clone()).unwrap_or_default()
}
fn steps_n(w: &mut MainWindow) -> isize {
    w.st.flush_history();
    w.st.history_pos()
}
fn r3(r: Rect4) -> String {
    format!("({:.3}, {:.3}, {:.3}, {:.3})", r.0, r.1, r.2, r.3)
}
fn gsrc(w: &MainWindow) -> Rect4 {
    let l = w.st.ed.layout.as_ref().unwrap();
    let (cw, ch) = crate::backend::canvas_size(&l.aspect, 1080);
    crate::backend::gameplay_src(l.layer("gameplay").unwrap(), &w.st.h.as_ref().unwrap().project.game_area.unwrap(), cw, ch)
}
/// Where to click on a slider so it lands on `v`.
fn slider_at(r: Rect, v: f64, lo: f64, hi: f64) -> Pos2 {
    let (x0, x1) = (r.left() + 6.0, r.right() - 6.0);
    Pos2::new(x0 + ((v - lo) / (hi - lo)) as f32 * (x1 - x0), r.center().y)
}

struct CheckApp {
    app: ValoApp,
    chk: Check,
}

fn slow_fail() -> (bool, String) {
    std::thread::sleep(Duration::from_millis(1000));
    (false, "no Vulkan-capable GPU found".into())
}
fn works() -> (bool, String) {
    (true, String::new())
}

impl CheckApp {
    /// Is the widget `key` inside the visible side panel? Otherwise scroll the panel towards it.
    fn reveal(&mut self, ctx: &egui::Context, key: &str) -> bool {
        let Some(r) = self.app.win.layout.rects.get(key).copied() else { return false };
        if r.left() >= 328.0 {
            return true; // not in the side panel
        }
        let vp = ctx.content_rect();
        let (top, bottom) = (56.0 + 24.0, vp.bottom() - 24.0);
        if r.top() >= top && r.bottom() <= bottom {
            // wait until smooth scrolling has settled (the panel eats the rest of a wheel scroll over a
            // few frames; a slider clicked meanwhile would take it)
            if self.chk.reveal_last != Some(r) {
                self.chk.reveal_last = Some(r);
                self.chk.reveal_since = Instant::now();
            }
            if self.chk.reveal_since.elapsed() >= Duration::from_millis(500) {
                self.chk.reveal_last = None;
                return true;
            }
            return false;
        }
        if self.chk.frames.is_multiple_of(5) {
            let dy = if r.top() < top { (top - r.top()).min(300.0) } else { -(r.bottom() - bottom).min(300.0) };
            self.chk.wheel(Pos2::new(160.0, 400.0), dy);
        }
        false
    }

    /// Click a recorded widget (after scrolling it into view). False while scrolling.
    fn click_w(&mut self, ctx: &egui::Context, key: &str) -> bool {
        if !self.reveal(ctx, key) {
            if self.chk.since() > Duration::from_secs(8) {
                let k = key.to_string();
                self.chk.ok(&format!("widget {k} reachable"), false, "never scrolled into view");
                return true;
            }
            return false;
        }
        let r = self.app.win.layout.rects[key];
        self.chk.click(r.center());
        true
    }

    fn click_slider(&mut self, ctx: &egui::Context, key: &str, v: f64, lo: f64, hi: f64) -> bool {
        if !self.reveal(ctx, key) {
            return self.chk.since() > Duration::from_secs(8);
        }
        let r = self.app.win.layout.rects[key];
        if std::env::var_os("VL_DEBUG").is_some() {
            let ed = &self.app.win.st.ed;
            println!(
                "  click {key} at {v} ({:?} -> {:?}), rows {} secs {}",
                r,
                slider_at(r, v, lo, hi),
                ed.killfeed.rows,
                ed.killfeed.seconds
            );
        }
        self.chk.click(slider_at(r, v, lo, hi));
        true
    }

    fn list_row(&mut self, ctx: &egui::Context, kind: &str) -> bool {
        let key = format!("layer:{kind}");
        if !self.reveal(ctx, &key) {
            return self.chk.since() > Duration::from_secs(8);
        }
        let r = self.app.win.layout.rects[&key];
        self.chk.click(Pos2::new(r.center().x + 40.0, r.center().y));
        true
    }

    /// The script: each arm runs once per frame (when no input is pending) until it returns Next.
    fn script(&mut self, ctx: &egui::Context) -> Step {
        use Step::*;
        let f = self.chk.frames;
        let ph = self.chk.phase;
        let ctrl = Modifiers::COMMAND;
        macro_rules! w {
            () => {
                self.app.win
            };
        }
        macro_rules! k {
            () => {
                self.chk
            };
        }
        macro_rules! wait_frames {
            ($n:expr) => {
                if f < $n {
                    return Wait;
                }
            };
        }
        macro_rules! wait_settle {
            () => {
                if k!().since() < SETTLE {
                    return Wait;
                }
            };
        }
        match k!().step {
            // 0: open the QA copy on the Layout page; wait for the still frame
            0 => {
                if f == 0 {
                    let d = k!().qa_dir.clone();
                    w!().shell.push(Action::OpenProject(d));
                    return Wait;
                }
                if f == 6 && !w!().dialogs.is_empty() {
                    k!().key(Key::Escape, Modifiers::NONE);
                }
                if f == 10 {
                    w!().go(LAYOUT);
                }
                if f < 12 || (!w!().layout.canvas.has_still() && k!().since() < Duration::from_secs(20)) {
                    return Wait;
                }
                if f < 20 {
                    return Wait;
                }
                let (pg, asp, still, rows) = (w!().page, aspect(&w!()), w!().layout.canvas.has_still(), w!().layout.canvas.rows_len());
                k!().ok(
                    "layout page opens with the 9:16 layout, a still frame and kill rows",
                    pg == LAYOUT && asp == "9:16" && still && rows > 0,
                    format!("page {pg}, aspect {asp}, still {still}, {rows} rows"),
                );
                k!().shot(ctx, "01_start");
                Next
            }
            // 1: the bottom of the side panel
            1 => {
                if f == 0 {
                    for _ in 0..8 {
                        k!().wheel(Pos2::new(160.0, 400.0), -400.0);
                    }
                    return Wait;
                }
                if f == 20 {
                    k!().shot(ctx, "01b_scrolled");
                }
                if f == 24 {
                    for _ in 0..8 {
                        k!().wheel(Pos2::new(160.0, 400.0), 400.0);
                    }
                }
                wait_frames!(40);
                Next
            }
            // 2: click selects
            2 => {
                if ph == 0 {
                    let c = layer_rect(&w!(), "killfeed").center();
                    k!().click(c);
                    k!().next_phase();
                    return Wait;
                }
                wait_frames!(3);
                let sel = w!().layout.canvas.sel.clone();
                let hint = LayoutPage::layer_hint(&w!().st, &sel);
                k!().ok("clicking a layer on the canvas selects it and the list row", sel == "killfeed", hint);
                Next
            }
            3 => {
                if ph == 0 {
                    wait_settle!();
                    k!().v.n0 = steps_n(&mut w!());
                    let c = layer_rect(&w!(), "killfeed").center();
                    k!().click(c);
                    k!().next_phase();
                    return Wait;
                }
                wait_settle!();
                let n = steps_n(&mut w!());
                let n0 = k!().v.n0;
                k!().ok("a plain click records no undo step", n == n0, format!("{n0} -> {n}"));
                Next
            }
            // 4: drag off canvas keeps a sliver on
            4 => {
                if ph == 0 {
                    k!().v.dst0 = dst(&w!(), "killfeed");
                    let fr = w!().layout.canvas.frame_rect();
                    let c = layer_rect(&w!(), "killfeed").center();
                    k!().drag(c, Pos2::new(fr.right() + 600.0, fr.bottom() + 600.0), 10, false);
                    k!().next_phase();
                    return Wait;
                }
                wait_frames!(2);
                let (x, y, _, _) = dst(&w!(), "killfeed");
                k!().ok(
                    "dragging a layer off the canvas keeps part of it on",
                    x <= 1.0 - 0.049 && y <= 1.0 - 0.049 && x > 0.9,
                    r3(dst(&w!(), "killfeed")),
                );
                let fr = w!().layout.canvas.frame_rect();
                let inside = layer_rect(&w!(), "killfeed").intersect(fr);
                let l = w!().st.ed.layout.clone().unwrap();
                let at = w!().layout.canvas.layer_at(inside.center(), &l);
                k!().ok(
                    "the off-canvas layer can still be grabbed",
                    inside.width() > 4.0 && inside.height() > 4.0 && at == "killfeed",
                    format!("{:.0}x{:.0} px on canvas, {at}", inside.width(), inside.height()),
                );
                Next
            }
            // 5: a drag is one undo step, labelled
            5 => {
                if ph == 0 {
                    wait_settle!();
                    w!().st.flush_history();
                    let lbl = w!().st.undo_label();
                    k!().ok("a layer move is an undo step named \"layout change\"", lbl == "layout change", format!("{lbl:?}"));
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    return Wait;
                }
                if ph == 1 {
                    wait_frames!(4);
                    let d = dst(&w!(), "killfeed");
                    let toast = w!().toast.clone();
                    k!().ok(
                        "undo puts the dragged layer back in one step",
                        d == k!().v.dst0 && toast == "Undid layout change",
                        format!("{} · toast {toast:?}", r3(d)),
                    );
                    let c = layer_rect(&w!(), "killfeed").center();
                    k!().drag(c, c + Vec2::new(0.0, 60.0), 10, false);
                    k!().next_phase();
                    return Wait;
                }
                if ph == 2 {
                    wait_frames!(2);
                    let y = dst(&w!(), "killfeed").1;
                    k!().ok(
                        "dragging after undo is saved to the edits",
                        y > k!().v.dst0.1 + 0.02,
                        format!("y {:.3} -> {y:.3}", k!().v.dst0.1),
                    );
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    return Wait;
                }
                wait_frames!(4);
                Next
            }
            // 6: Esc cancels a drag
            6 => {
                if ph == 0 {
                    wait_settle!();
                    k!().v.dst0 = dst(&w!(), "killfeed");
                    k!().v.n0 = steps_n(&mut w!());
                    let c = layer_rect(&w!(), "killfeed").center();
                    k!().drag(c, c + Vec2::new(-80.0, 120.0), 10, true);
                    k!().next_phase();
                    return Wait;
                }
                wait_settle!();
                let (d, n) = (dst(&w!(), "killfeed"), steps_n(&mut w!()));
                k!().ok(
                    "Esc during a drag puts the layer back, no undo step",
                    d == k!().v.dst0 && n == k!().v.n0,
                    format!("{} · steps {} -> {n}", r3(d), k!().v.n0),
                );
                Next
            }
            // 7: aspect-locked overlay corner drags
            7 => {
                if ph == 0 {
                    let c = layer_rect(&w!(), "scorebar").center();
                    k!().click(c);
                    k!().next_phase();
                    return Wait;
                }
                let deltas =
                    [("sideways", Vec2::new(60.0, 0.0)), ("down", Vec2::new(0.0, 40.0)), ("along the diagonal", Vec2::new(40.0, 6.0))];
                let i = k!().sub;
                if ph == 1 {
                    wait_frames!(3);
                    k!().v.dst0 = dst(&w!(), "scorebar");
                    let r = layer_rect(&w!(), "scorebar");
                    k!().drag(r.left_bottom(), r.left_bottom() + deltas[i].1, 10, false);
                    k!().next_phase();
                    return Wait;
                }
                if ph == 2 {
                    wait_frames!(2);
                    let (sb0, sb) = (k!().v.dst0, dst(&w!(), "scorebar"));
                    let (cw, ch) = crate::backend::canvas_size("9:16", 1080);
                    let asp = (sb.2 * cw as f64) / (sb.3 * ch as f64);
                    let want = crate::backend::layer_src_aspect_px("scorebar", &w!().st.h.as_ref().unwrap().project.game_area.unwrap());
                    let pass = (sb.2 - sb0.2).abs() > 0.0005
                        && (sb.2 - sb0.2).abs() < 0.3
                        && (asp - want).abs() < 0.01
                        && ((sb.0 + sb.2) - (sb0.0 + sb0.2)).abs() < 1e-6
                        && (sb.1 - sb0.1).abs() < 1e-6;
                    k!().ok(
                        &format!("score bar corner drag {} resizes it in proportion", deltas[i].0),
                        pass,
                        format!("w {:.3} -> {:.3}, aspect {asp:.3} (crop {want:.3})", sb0.2, sb.2),
                    );
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    return Wait;
                }
                wait_frames!(4);
                k!().sub += 1;
                if k!().sub < deltas.len() {
                    k!().phase = 1;
                    k!().frames = 0;
                    return Wait;
                }
                Next
            }
            // 8: gameplay resize snaps its bottom edge to the middle
            8 => {
                if ph == 0 {
                    let fr = w!().layout.canvas.frame_rect();
                    k!().click(fr.center() + Vec2::new(0.0, 60.0));
                    k!().next_phase();
                    return Wait;
                }
                if ph == 1 {
                    wait_frames!(3);
                    let sel = w!().layout.canvas.sel.clone();
                    k!().ok("clicking open gameplay selects gameplay", sel == "gameplay", sel);
                    let fr = w!().layout.canvas.frame_rect();
                    let g = layer_rect(&w!(), "gameplay");
                    k!().drag(g.right_bottom(), Pos2::new(g.right(), fr.top() + fr.height() * 0.5 + 3.0), 10, false);
                    k!().next_phase();
                    return Wait;
                }
                if ph == 2 {
                    wait_frames!(2);
                    let d = dst(&w!(), "gameplay");
                    k!().ok(
                        "resizing snaps a horizontal edge to the centre line",
                        (d.1 + d.3 - 0.5).abs() < 1e-6,
                        format!("bottom {:.4}", d.1 + d.3),
                    );
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    return Wait;
                }
                wait_frames!(4);
                Next
            }
            // 9: keyboard nudges wherever the user clicked
            9 => match ph {
                0 => {
                    if self.list_row(ctx, "hud") {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    k!().v.dst0 = dst(&w!(), "hud");
                    k!().key(Key::ArrowUp, Modifiers::NONE);
                    k!().key(Key::ArrowUp, Modifiers::SHIFT);
                    k!().next_phase();
                    Wait
                }
                2 => {
                    wait_frames!(3);
                    let (y0, y) = (k!().v.dst0.1, dst(&w!(), "hud").1);
                    let sel = w!().layout.canvas.sel.clone();
                    k!().ok(
                        "arrow keys nudge the layer after clicking the layers list (Shift = bigger)",
                        (y - (y0 - 0.012)).abs() < 1e-6 && sel == "hud",
                        format!("y {y0:.3} -> {y:.3}"),
                    );
                    if self.click_w(ctx, "bg:black") {
                        k!().next_phase();
                    }
                    Wait
                }
                3 => {
                    wait_frames!(3);
                    k!().v.y1 = dst(&w!(), "hud").1;
                    k!().key(Key::ArrowDown, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                4 => {
                    wait_frames!(3);
                    let y = dst(&w!(), "hud").1;
                    k!().ok(
                        "arrow keys nudge after clicking a side panel button",
                        (y - (k!().v.y1 + 0.002)).abs() < 1e-6,
                        format!("y {:.4} -> {y:.4}", k!().v.y1),
                    );
                    if self.click_w(ctx, "bg:blur") {
                        k!().next_phase();
                    }
                    Wait
                }
                5 => {
                    wait_frames!(3);
                    let rows = w!().st.ed.killfeed.rows as f64;
                    if self.click_slider(ctx, "slider:rows", rows, 1.0, 6.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                6 => {
                    wait_frames!(3);
                    k!().v.r0 = w!().st.ed.killfeed.rows as i64;
                    k!().v.y1 = dst(&w!(), "hud").1;
                    k!().key(Key::ArrowRight, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                7 => {
                    wait_frames!(3);
                    let (r, y) = (w!().st.ed.killfeed.rows as i64, dst(&w!(), "hud").1);
                    k!().ok(
                        "a focused slider keeps its arrow keys",
                        r == k!().v.r0 + 1 && y == k!().v.y1,
                        format!("rows {} -> {r}, hud y {y:.4}", k!().v.r0),
                    );
                    k!().key(Key::ArrowLeft, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                _ => {
                    wait_settle!();
                    w!().st.flush_history();
                    Next
                }
            },
            // 10: hidden layers
            10 => match ph {
                0 => {
                    if self.list_row(ctx, "minimap") {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    let sel = w!().layout.canvas.sel.clone();
                    let hint = LayoutPage::layer_hint(&w!().st, &sel);
                    k!().ok("selecting a hidden layer explains it", sel == "minimap" && hint.contains("hidden"), hint);
                    k!().v.dst0 = dst(&w!(), "minimap");
                    k!().key(Key::ArrowLeft, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                2 => {
                    wait_frames!(3);
                    let msg = w!().layout.msg.clone();
                    k!().ok(
                        "arrow keys leave a hidden layer alone and say why",
                        dst(&w!(), "minimap") == k!().v.dst0 && msg.contains("hidden"),
                        msg,
                    );
                    k!().next_phase();
                    Wait
                }
                3 => {
                    wait_settle!();
                    k!().v.n0 = steps_n(&mut w!());
                    if self.list_row(ctx, "hud") {
                        k!().next_phase();
                    }
                    Wait
                }
                4 => {
                    wait_frames!(3);
                    if self.click_w(ctx, "toggle:hud") {
                        k!().next_phase();
                    }
                    Wait
                }
                5 => {
                    wait_frames!(4);
                    let sel = w!().layout.canvas.sel.clone();
                    k!().ok("hiding the selected layer moves the selection to gameplay", !visible(&w!(), "hud") && sel == "gameplay", sel);
                    if self.click_w(ctx, "toggle:killfeed") {
                        k!().next_phase();
                    }
                    Wait
                }
                6 => {
                    wait_frames!(4);
                    let n = steps_n(&mut w!());
                    k!().ok("two quick visibility toggles are two undo steps", n == k!().v.n0 + 2, format!("{} -> {n}", k!().v.n0));
                    k!().ok("hidden kill feed greys out its settings with a note", !visible(&w!(), "killfeed"), "");
                    k!().shot(ctx, "02a_hidden_layers");
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                7 => {
                    wait_frames!(4);
                    k!().ok("undo shows the kill feed again", visible(&w!(), "killfeed") && !visible(&w!(), "hud"), "");
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                _ => {
                    wait_frames!(4);
                    k!().ok("undo shows the HUD again", visible(&w!(), "hud"), "");
                    Next
                }
            },
            // 11: shape switch remembers arrangements, each switch one undo step; reset
            11 => match ph {
                0 => {
                    wait_settle!();
                    k!().v.n0 = steps_n(&mut w!());
                    let c = layer_rect(&w!(), "killfeed").center();
                    k!().drag(c, c + Vec2::new(-60.0, 200.0), 10, false);
                    k!().next_phase();
                    Wait
                }
                1 => {
                    wait_frames!(2);
                    k!().v.moved = dst(&w!(), "killfeed");
                    k!().next_phase();
                    Wait
                }
                2 => {
                    wait_settle!();
                    w!().st.flush_history();
                    if self.click_w(ctx, "shape:1:1") {
                        k!().next_phase();
                    }
                    Wait
                }
                3 => {
                    wait_frames!(4);
                    let msg = w!().layout.msg.clone();
                    k!().ok("switching the shape says what happened", aspect(&w!()) == "1:1" && msg.contains("Ctrl+Z"), msg);
                    k!().shot(ctx, "02_square");
                    if self.click_w(ctx, "shape:9:16") {
                        k!().next_phase();
                    }
                    Wait
                }
                4 => {
                    wait_frames!(4);
                    let msg = w!().layout.msg.clone();
                    k!().ok(
                        "switching back brings back the earlier arrangement",
                        aspect(&w!()) == "9:16" && dst(&w!(), "killfeed") == k!().v.moved,
                        msg,
                    );
                    k!().key(Key::Z, ctrl);
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                5 => {
                    wait_frames!(4);
                    let n = steps_n(&mut w!());
                    k!().ok(
                        "each shape switch is one undo step",
                        aspect(&w!()) == "9:16" && dst(&w!(), "killfeed") == k!().v.moved && n == k!().v.n0 + 1,
                        format!("steps {} -> {n}", k!().v.n0),
                    );
                    if self.click_w(ctx, "shape:16:9") {
                        k!().next_phase();
                    }
                    Wait
                }
                6 => {
                    if f == 8 {
                        k!().shot(ctx, "02b_landscape");
                    }
                    wait_frames!(14);
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                7 => {
                    wait_frames!(4);
                    k!().ok(
                        "undo of a shape switch restores shape and arrangement",
                        aspect(&w!()) == "9:16" && dst(&w!(), "killfeed") == k!().v.moved,
                        aspect(&w!()),
                    );
                    k!().next_phase();
                    Wait
                }
                8 => {
                    wait_settle!();
                    w!().st.flush_history();
                    if self.click_w(ctx, "reset") {
                        k!().next_phase();
                    }
                    Wait
                }
                9 => {
                    wait_frames!(4);
                    let msg = w!().layout.msg.clone();
                    k!().ok(
                        "reset says what happened",
                        msg.to_lowercase().contains("reset") && dst(&w!(), "killfeed") != k!().v.moved,
                        msg,
                    );
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                10 => {
                    wait_frames!(4);
                    k!().ok("reset is undoable in one step", dst(&w!(), "killfeed") == k!().v.moved, r3(dst(&w!(), "killfeed")));
                    if self.click_w(ctx, "reset") {
                        k!().next_phase();
                    }
                    Wait
                }
                _ => {
                    wait_frames!(4);
                    Next
                }
            },
            // 12: gameplay pan sliders
            12 => match ph {
                0 => {
                    if self.list_row(ctx, "gameplay") {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    let (sw, sh) = LayoutPage::pan_span(&w!().st);
                    let note = LayoutPage::pan_note(&w!().st);
                    k!().ok(
                        "9:16 fill at zoom 1: left/right works, up/down explains why it can't",
                        sw < 0.999 && sh >= 0.999 && !note.is_empty(),
                        note,
                    );
                    if self.click_slider(ctx, "slider:panx", 0.0, 0.0, 100.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                2 => {
                    wait_frames!(3);
                    k!().v.a0 = gsrc(&w!()).0;
                    if self.click_slider(ctx, "slider:panx", 100.0, 0.0, 100.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                3 => {
                    wait_frames!(3);
                    let s = gsrc(&w!());
                    k!().v.sx = s.0;
                    k!().v.sw = s.2;
                    if self.click_slider(ctx, "slider:panx", 25.0, 0.0, 100.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                4 => {
                    wait_frames!(3);
                    let s25 = gsrc(&w!()).0;
                    let (a0, sx, sw) = (k!().v.a0, k!().v.sx, k!().v.sw);
                    k!().ok(
                        "left/right runs edge to edge with no dead zone",
                        a0 == 0.0 && (sx + sw - 1.0).abs() < 1e-6 && (s25 - 0.25 * (1.0 - sw)).abs() < 0.01,
                        format!("0->{a0:.3} 25->{s25:.3} 100->{:.3}", sx + sw),
                    );
                    if self.click_slider(ctx, "slider:zoom", 150.0, 100.0, 250.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                5 => {
                    wait_frames!(3);
                    let (_, sh) = LayoutPage::pan_span(&w!().st);
                    let note = LayoutPage::pan_note(&w!().st);
                    k!().ok(
                        "zooming in unlocks up/down",
                        sh < 0.999 && note.is_empty(),
                        format!("zoom {:.2}", w!().st.ed.layout.as_ref().unwrap().layer("gameplay").unwrap().zoom),
                    );
                    if self.click_w(ctx, "fit:stretch") {
                        k!().next_phase();
                    }
                    Wait
                }
                6 => {
                    wait_frames!(3);
                    if self.click_slider(ctx, "slider:zoom", 100.0, 100.0, 250.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                7 => {
                    wait_frames!(3);
                    let (sw, sh) = LayoutPage::pan_span(&w!().st);
                    let note = LayoutPage::pan_note(&w!().st);
                    k!().ok(
                        "stretch at zoom 1 locks both pans with a note",
                        sw >= 0.999 && sh >= 0.999 && note.contains("whole game"),
                        note,
                    );
                    k!().shot(ctx, "02c_stretch");
                    if self.click_w(ctx, "fit:cover") {
                        k!().next_phase();
                    }
                    Wait
                }
                _ => {
                    wait_settle!();
                    w!().st.flush_history();
                    Next
                }
            },
            // 13: the wheel over an unfocused slider is left to the panel
            13 => match ph {
                0 => {
                    k!().key(Key::Escape, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                1 => {
                    wait_frames!(2);
                    if self.click_w(ctx, "keep:timed") {
                        k!().next_phase();
                    }
                    Wait
                }
                2 => {
                    wait_frames!(3);
                    if !self.reveal(ctx, "slider:secs") {
                        return Wait;
                    }
                    k!().v.v0 = w!().st.ed.killfeed.seconds;
                    let r = w!().layout.rects["slider:secs"];
                    k!().wheel(r.center(), -40.0);
                    k!().next_phase();
                    Wait
                }
                3 => {
                    wait_frames!(10);
                    let s = w!().st.ed.killfeed.seconds;
                    k!().ok(
                        "mouse wheel over an unfocused slider passes to the panel, value unchanged",
                        s == k!().v.v0,
                        format!("{} -> {s}", k!().v.v0),
                    );
                    let v = (k!().v.v0).round_ties_even();
                    if self.click_slider(ctx, "slider:secs", v, 2.0, 12.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                4 => {
                    wait_frames!(3);
                    let r = w!().layout.rects["slider:secs"];
                    k!().v.v0 = w!().st.ed.killfeed.seconds;
                    k!().wheel(r.center(), -40.0);
                    k!().next_phase();
                    Wait
                }
                5 => {
                    wait_frames!(10);
                    let s = w!().st.ed.killfeed.seconds;
                    k!().ok("a focused slider still takes the wheel", s != k!().v.v0, format!("{} -> {s}", k!().v.v0));
                    let v = k!().v.v0.round_ties_even();
                    if self.click_slider(ctx, "slider:secs", v, 2.0, 12.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                6 => {
                    wait_settle!();
                    let s = w!().st.ed.killfeed.seconds;
                    //: the slider holds whole seconds, so 4.5 s comes back as 4 s
                    let want = k!().v.v0.round_ties_even();
                    k!().ok("slider value and edits agree after wheel + reset", s == want, format!("{s} s (slider at {want})"));
                    if self.click_w(ctx, "keep:clip") {
                        k!().next_phase();
                    }
                    Wait
                }
                _ => {
                    wait_settle!();
                    w!().st.flush_history();
                    Next
                }
            },
            // 14: discrete clicks are separate undo steps
            14 => match ph {
                0 => {
                    wait_settle!();
                    k!().v.n0 = steps_n(&mut w!());
                    let ed = &w!().st.ed;
                    let newest = if ed.killfeed.newest == "top" { "bottom" } else { "top" };
                    let anim = if ed.killfeed.animation == "fade" { "pop" } else { "fade" };
                    let look = if ed.effects.look == "warm" { "cool" } else { "warm" };
                    k!().v.look0 = ed.effects.look.clone();
                    k!().v.newest = newest.into();
                    k!().v.anim = anim.into();
                    k!().v.ids = vec![format!("newest:{newest}"), format!("anim:{anim}"), format!("look:{look}")];
                    k!().sub = 0;
                    k!().next_phase();
                    Wait
                }
                1 => {
                    let i = k!().sub;
                    if i < 3 {
                        let key = k!().v.ids[i].clone();
                        if self.click_w(ctx, &key) {
                            k!().sub += 1;
                        }
                        return Wait;
                    }
                    wait_frames!(4);
                    let n = steps_n(&mut w!());
                    k!().ok("three quick option clicks are three undo steps", n == k!().v.n0 + 3, format!("{} -> {n}", k!().v.n0));
                    let fnew = w!().layout.canvas.feed_newest.clone();
                    k!().ok("newest on top/bottom updates the kill feed hint and canvas", fnew == k!().v.newest, fnew);
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                _ => {
                    wait_frames!(4);
                    let ed = &w!().st.ed;
                    let pass =
                        ed.effects.look == k!().v.look0 && ed.killfeed.animation == k!().v.anim && ed.killfeed.newest == k!().v.newest;
                    let d = format!("look {}, animation {}, newest {}", ed.effects.look, ed.killfeed.animation, ed.killfeed.newest);
                    k!().ok("undo reverts only the last click", pass, d);
                    Next
                }
            },
            // 15: per-kill zoom
            15 => {
                let ids: Vec<String> = LayoutPage::kills_in_cut(&w!().st).iter().map(|(k, _)| k.id.clone()).collect();
                match ph {
                    0 => {
                        let total: f64 = w!().st.ed.parts.iter().map(|p| p.src_out - p.src_in).sum();
                        let kills = LayoutPage::kills_in_cut(&w!().st);
                        let mx = kills.iter().map(|(_, t)| *t).fold(0.0, f64::max);
                        k!().ok(
                            "per-kill list shows kills in the cut with edit times",
                            !ids.is_empty() && mx <= total + 0.01,
                            format!("{} rows, last at {mx:.2} s of {total:.2}", ids.len()),
                        );
                        if ids.is_empty() {
                            return Next;
                        }
                        if self.click_slider(ctx, &format!("slider:pk:{}", ids[0]), 120.0, 100.0, 130.0) {
                            k!().next_phase();
                        }
                        Wait
                    }
                    1 => {
                        wait_frames!(3);
                        if self.click_slider(ctx, "slider:kzoom", 112.0, 101.0, 130.0) {
                            k!().next_phase();
                        }
                        Wait
                    }
                    2 => {
                        wait_frames!(3);
                        let fx = &w!().st.ed.effects;
                        let vals: Vec<i64> = ids
                            .iter()
                            .map(|id| (fx.zoom_per_kill.get(id).copied().unwrap_or(fx.zoom_amount) * 100.0).round() as i64)
                            .collect();
                        k!().ok(
                            "global amount moves the kills that follow it, not the tuned one",
                            vals[0] == 120 && vals[1..].iter().all(|v| *v == 112),
                            format!("{vals:?}"),
                        );
                        let last = ids.last().unwrap().clone();
                        if self.click_slider(ctx, &format!("slider:pk:{last}"), 100.0, 100.0, 130.0) {
                            k!().next_phase();
                        }
                        Wait
                    }
                    3 => {
                        wait_frames!(3);
                        let fx = &w!().st.ed.effects;
                        let v = fx.zoom_per_kill.get(ids.last().unwrap()).copied().unwrap_or(fx.zoom_amount);
                        k!().ok("a kill at ×1.00 reads off", v <= 1.0, format!("{v:.2}"));
                        k!().shot(ctx, "03a_per_kill");
                        if self.click_w(ctx, "punch") {
                            k!().next_phase();
                        }
                        Wait
                    }
                    4 => {
                        wait_frames!(3);
                        k!().ok("punch-in off greys out amount and per-kill list", !w!().st.ed.effects.zoom, "");
                        if self.click_w(ctx, "punch") {
                            k!().next_phase();
                        }
                        Wait
                    }
                    _ => {
                        wait_settle!();
                        w!().st.flush_history();
                        Next
                    }
                }
            }
            // 16: super smooth with the interpolator check
            16 => match ph {
                0 => {
                    if w!().st.ed.effects.super_smooth {
                        // the project has it on: turn it off first
                        if self.click_w(ctx, "smooth") {
                            k!().phase_start = Instant::now();
                        }
                        return Wait;
                    }
                    w!().layout.smooth_probe = slow_fail;
                    w!().layout.smooth_ok = None;
                    // an AI-frames preset, so turning it on runs the interpolator check
                    w!().st.ed.effects.smooth_amount = "subtle".into();
                    if self.click_w(ctx, "smooth") {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    let note = w!().layout.smooth_note(&w!().st);
                    let checking = w!().layout.smooth_checking();
                    k!().ok(
                        "checking super smooth doesn't freeze the window",
                        checking && note.contains("Checking") && k!().since() < Duration::from_millis(500),
                        format!("{:?} after {} ms", note, k!().since().as_millis()),
                    );
                    k!().next_phase();
                    Wait
                }
                2 => {
                    if w!().layout.smooth_checking() && k!().since() < Duration::from_secs(5) {
                        return Wait;
                    }
                    let note = w!().layout.smooth_note(&w!().st);
                    let fx = &w!().st.ed.effects;
                    let pass =
                        fx.super_smooth && fx.smooth_amount == "faster" && note.contains("can't run") && !w!().layout.smooth_checking();
                    k!().ok(
                        "unavailable AI frames fall back to Faster and say why inline",
                        pass,
                        format!("{note} ({}, {})", fx.super_smooth, fx.smooth_amount),
                    );
                    k!().shot(ctx, "03b_smooth_unavailable");
                    w!().layout.smooth_probe = works;
                    w!().layout.smooth_ok = None;
                    // back to off with an AI-frames preset for the next step
                    w!().st.ed.effects.super_smooth = false;
                    w!().st.ed.effects.smooth_amount = "subtle".into();
                    w!().st.touch();
                    k!().next_phase();
                    Wait
                }
                3 => {
                    wait_settle!();
                    k!().v.n0 = steps_n(&mut w!());
                    if self.click_w(ctx, "smooth") {
                        k!().next_phase();
                    }
                    Wait
                }
                4 => {
                    if k!().since() < Duration::from_millis(500) || w!().layout.smooth_checking() {
                        return Wait;
                    }
                    let n = steps_n(&mut w!());
                    let msg = w!().layout.msg.clone();
                    k!().ok(
                        "available super smooth turns on as one undo step",
                        w!().st.ed.effects.super_smooth && n == k!().v.n0 + 1,
                        format!("steps {} -> {n}, {msg:?}", k!().v.n0),
                    );
                    k!().shot(ctx, "03_after_edits");
                    if self.click_w(ctx, "smooth") {
                        k!().next_phase();
                    }
                    Wait
                }
                _ => {
                    wait_settle!();
                    w!().st.flush_history();
                    Next
                }
            },
            // 17: tab switching keeps the selection; arrows don't act on other pages
            17 => match ph {
                0 => {
                    if self.list_row(ctx, "killfeed") {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    k!().v.dst0 = dst(&w!(), "killfeed");
                    w!().go(SUBTITLES);
                    k!().next_phase();
                    Wait
                }
                2 => {
                    wait_frames!(10);
                    k!().key(Key::ArrowLeft, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                3 => {
                    wait_frames!(4);
                    k!().ok("arrow keys on another page leave the layout alone", dst(&w!(), "killfeed") == k!().v.dst0, "");
                    w!().go(LAYOUT);
                    k!().next_phase();
                    Wait
                }
                _ => {
                    wait_frames!(6);
                    let sel = w!().layout.canvas.sel.clone();
                    k!().ok("selection kept across tabs", sel == "killfeed", sel);
                    Next
                }
            },
            // 18: window resize keeps layers in place
            18 => match ph {
                0 => {
                    k!().v.fr0_h = w!().layout.canvas.frame_rect().height();
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1200.0, 760.0)));
                    k!().next_phase();
                    Wait
                }
                1 => {
                    if k!().since() < Duration::from_millis(800) {
                        return Wait;
                    }
                    let fr1 = w!().layout.canvas.frame_rect();
                    let r = layer_rect(&w!(), "killfeed");
                    let x = dst(&w!(), "killfeed").0;
                    let pass = fr1.height() < k!().v.fr0_h && (((r.left() - fr1.left()) / fr1.width()) as f64 - x).abs() < 1e-4;
                    k!().ok(
                        "window resize rescales the canvas, layers keep their spot",
                        pass,
                        format!("frame {:.0} -> {:.0} px high", k!().v.fr0_h, fr1.height()),
                    );
                    k!().shot(ctx, "04_small_window");
                    k!().next_phase();
                    Wait
                }
                2 => {
                    wait_frames!(3);
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1500.0, 920.0)));
                    k!().next_phase();
                    Wait
                }
                _ => {
                    if k!().since() < Duration::from_millis(1500) {
                        return Wait;
                    }
                    Next
                }
            },
            // 19: preview flow
            19 => match ph {
                0 => {
                    if self.click_w(ctx, "view:preview") {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    if w!().layout.view != "preview" && k!().sub < 3 {
                        // the first click after the window was resized back can get lost
                        println!("  view switch click retried ({:?})", w!().layout.rects.get("view:preview"));
                        k!().sub += 1;
                        k!().phase = 0;
                        k!().frames = 0;
                        return Wait;
                    }
                    let pass = w!().layout.view == "preview" && w!().layout.preview_path.is_empty();
                    k!().ok("Preview video before any render shows an empty state", pass, w!().layout.view.clone());
                    k!().shot(ctx, "05_preview_empty");
                    k!().v.y1 = dst(&w!(), "killfeed").1;
                    k!().key(Key::ArrowDown, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                2 => {
                    wait_frames!(3);
                    k!().ok("arrow keys don't move layers in preview view", dst(&w!(), "killfeed").1 == k!().v.y1, "");
                    if self.click_w(ctx, "view:arrange") {
                        k!().next_phase();
                    }
                    Wait
                }
                // Render preview, then Cancel while it runs
                3 => {
                    wait_frames!(3);
                    if self.click_w(ctx, "render") {
                        k!().next_phase();
                    }
                    Wait
                }
                4 => {
                    let p = w!().layout.progress.unwrap_or(0.0);
                    if p <= 0.0 && k!().since() < Duration::from_secs(60) {
                        return Wait;
                    }
                    let status = w!().layout.status.clone();
                    k!().ok("preview shows progress in percent", status.contains('%'), status);
                    k!().shot(ctx, "05b_rendering");
                    let r = w!().layout.rects["cancel"];
                    k!().click_now(r.center());
                    k!().next_phase();
                    Wait
                }
                5 => {
                    if f == 0 {
                        let s = w!().layout.status.clone();
                        k!().ok("Cancel says it is cancelling", s == "Cancelling…" && !w!().layout.stop_enabled, s);
                    }
                    if w!().job.is_some() && k!().since() < Duration::from_secs(15) {
                        return Wait;
                    }
                    let s = w!().layout.status.clone();
                    k!().ok(
                        "a cancelled preview ends quietly",
                        w!().job.is_none() && s == "Cancelled" && w!().layout.progress.is_none(),
                        s,
                    );
                    k!().next_phase();
                    Wait
                }
                6 => {
                    wait_frames!(3);
                    if self.click_w(ctx, "render") {
                        k!().v.seen.clear();
                        k!().next_phase();
                    }
                    Wait
                }
                7 => {
                    let s = w!().layout.status.clone();
                    if !k!().v.seen.contains(&s) {
                        k!().v.seen.push(s);
                    }
                    if (w!().job.is_some() || f < 5) && k!().since() < Duration::from_secs(300) {
                        return Wait;
                    }
                    println!("  progress labels: {:?}", &k!().v.seen[..k!().v.seen.len().min(5)]);
                    k!().next_phase();
                    Wait
                }
                8 => {
                    let p = &w!().layout.player;
                    if !(p.duration() > 0.0 && p.frames_rendered > 0 && f > 10) && k!().since() < Duration::from_secs(15) {
                        return Wait;
                    }
                    let (view, status, path) = (w!().layout.view.clone(), w!().layout.status.clone(), w!().layout.preview_path.clone());
                    let pass = view == "preview"
                        && status == "Preview ready · 540p"
                        && Path::new(&path).exists()
                        && w!().layout.player.duration() > 0.0;
                    k!().ok(
                        "finished preview plays in Preview video",
                        pass,
                        format!("{view}, {status:?}, {:.2} s", w!().layout.player.duration()),
                    );
                    k!().v.pos0 = w!().layout.player.pos();
                    w!().layout.player.play();
                    k!().next_phase();
                    Wait
                }
                9 => {
                    if k!().since() < Duration::from_secs(2) {
                        return Wait;
                    }
                    let p1 = w!().layout.player.pos();
                    k!().ok("the preview plays", p1 > k!().v.pos0 + 0.5, format!("{:.2} -> {p1:.2} s", k!().v.pos0));
                    k!().shot(ctx, "08_preview_playing");
                    w!().layout.player.pause();
                    k!().v.r0 = w!().st.ed.killfeed.rows as i64;
                    let to = if k!().v.r0 < 6 { k!().v.r0 + 1 } else { k!().v.r0 - 1 };
                    if self.click_slider(ctx, "slider:rows", to as f64, 1.0, 6.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                10 => {
                    wait_frames!(3);
                    let s = w!().layout.status.clone();
                    k!().ok("editing after a preview marks it out of date", s.contains("out of date"), s);
                    let r0 = k!().v.r0 as f64;
                    if self.click_slider(ctx, "slider:rows", r0, 1.0, 6.0) {
                        k!().next_phase();
                    }
                    Wait
                }
                11 => {
                    wait_frames!(3);
                    let s = w!().layout.status.clone();
                    k!().ok("undoing the change makes the preview current again", s == "Preview ready · 540p", s);
                    if self.click_w(ctx, "view:arrange") {
                        k!().next_phase();
                    }
                    Wait
                }
                _ => {
                    wait_frames!(3);
                    Next
                }
            },
            // 20: no frame yet: labelled boxes; then the frame again
            20 => match ph {
                0 => {
                    w!().layout.canvas.clear_still();
                    k!().next_phase();
                    Wait
                }
                1 => {
                    wait_frames!(3);
                    k!().shot(ctx, "06_no_still");
                    w!().layout.reload_still();
                    k!().next_phase();
                    Wait
                }
                _ => {
                    if !w!().layout.canvas.has_still() && k!().since() < Duration::from_secs(10) {
                        return Wait;
                    }
                    wait_frames!(4);
                    k!().ok("the still frame comes back", w!().layout.canvas.has_still(), "");
                    k!().shot(ctx, "07_canvas");
                    Next
                }
            },
            // 21: layer crops: the editor, one undo step per drag, the box height follows, bounds, reset,
            //     and a newly shown webcam opens the editor
            21 => match ph {
                0 => {
                    wait_settle!();
                    w!().st.flush_history();
                    let done = if visible(&w!(), "minimap") { self.list_row(ctx, "minimap") } else { self.click_w(ctx, "toggle:minimap") };
                    if done {
                        k!().next_phase();
                    }
                    Wait
                }
                1 => {
                    wait_frames!(4);
                    if self.click_w(ctx, "crop:minimap") {
                        k!().next_phase();
                    }
                    Wait
                }
                2 => {
                    wait_frames!(4);
                    let kind = cropping(&w!());
                    k!().ok("the crop icon on the minimap row opens the crop editor", kind == "minimap", format!("{kind:?}"));
                    w!().st.flush_history();
                    k!().v.n0 = steps_n(&mut w!());
                    k!().v.dst0 = dst(&w!(), "minimap");
                    k!().v.src0 = src(&w!(), "minimap");
                    let b = w!().layout.rects.get("crop_box").copied().unwrap_or(Rect::NOTHING);
                    k!().drag(b.right_bottom(), b.right_bottom() + Vec2::new(50.0, 20.0), 10, false);
                    k!().next_phase();
                    Wait
                }
                3 => {
                    wait_settle!();
                    w!().st.flush_history();
                    let (s0, s, d0, d, n) = (k!().v.src0, src(&w!(), "minimap"), k!().v.dst0, dst(&w!(), "minimap"), steps_n(&mut w!()));
                    let ga = w!().st.h.as_ref().unwrap().project.game_area.unwrap();
                    let (cw, ch) = crate::backend::canvas_size(&aspect(&w!()), 1080);
                    let box_aspect = d.2 * cw as f64 / (d.3 * ch as f64);
                    let crop_aspect = crate::backend::crop_aspect_px(s, &ga);
                    let pass = s.2 > s0.2
                        && s.3 > s0.3
                        && s.0 == s0.0
                        && n == k!().v.n0 + 1
                        && d.2 == d0.2
                        && (box_aspect - crop_aspect).abs() < 0.01;
                    k!().ok(
                        "dragging the crop corner is one undo step; the layer keeps its width and its shape follows the crop",
                        pass,
                        format!(
                            "src {} -> {}, steps {} -> {n}, box aspect {box_aspect:.3} crop {crop_aspect:.3}",
                            r3(s0),
                            r3(s),
                            k!().v.n0
                        ),
                    );
                    k!().shot(ctx, "08_crop_editor");
                    k!().key(Key::Z, ctrl);
                    k!().next_phase();
                    Wait
                }
                4 => {
                    wait_frames!(4);
                    let (s0, s) = (k!().v.src0, src(&w!(), "minimap"));
                    k!().ok("undo puts the crop back", s == s0 && dst(&w!(), "minimap") == k!().v.dst0, r3(s));
                    // drag the box far past the top-left corner: it stops at the game area
                    let b = w!().layout.rects.get("crop_box").copied().unwrap_or(Rect::NOTHING);
                    k!().drag(b.center(), b.center() + Vec2::new(-900.0, -900.0), 12, false);
                    k!().next_phase();
                    Wait
                }
                5 => {
                    wait_settle!();
                    let s = src(&w!(), "minimap");
                    k!().ok(
                        "the minimap crop can't leave the game area",
                        s.0.abs() < 1e-6 && s.1.abs() < 1e-6 && (s.2 - k!().v.src0.2).abs() < 1e-6,
                        r3(s),
                    );
                    if self.click_w(ctx, "crop_reset:minimap") {
                        k!().next_phase();
                    }
                    Wait
                }
                6 => {
                    wait_settle!();
                    let ga = w!().st.h.as_ref().unwrap().project.game_area.unwrap();
                    let s = src(&w!(), "minimap");
                    let want = crate::backend::overlay_default_src("minimap", &ga);
                    k!().ok("the reset icon goes back to the detected minimap", s == want, format!("{} vs {}", r3(s), r3(want)));
                    k!().key(Key::Escape, Modifiers::NONE);
                    k!().next_phase();
                    Wait
                }
                7 => {
                    wait_frames!(4);
                    let kind = cropping(&w!());
                    k!().ok("Esc closes the crop editor", kind.is_empty(), format!("{kind:?}"));
                    if self.click_w(ctx, "toggle:webcam") {
                        k!().next_phase();
                    }
                    Wait
                }
                8 => {
                    wait_frames!(6);
                    let kind = cropping(&w!());
                    let msg = w!().layout.msg.clone();
                    k!().ok(
                        "showing the webcam opens the crop editor on it",
                        visible(&w!(), "webcam") && kind == "webcam" && msg.contains("webcam"),
                        format!("{kind:?} · {msg:?}"),
                    );
                    k!().shot(ctx, "09_webcam_crop");
                    if self.click_w(ctx, "crop:webcam") {
                        k!().next_phase();
                    }
                    Wait
                }
                9 => {
                    wait_frames!(4);
                    let kind = cropping(&w!());
                    k!().ok("the tick on the webcam row closes the crop editor", kind.is_empty(), format!("{kind:?}"));
                    k!().shot(ctx, "10_webcam_layer");
                    k!().next_phase();
                    Wait
                }
                _ => {
                    wait_settle!();
                    w!().st.flush_history();
                    Next
                }
            },
            // 22: everything was written to disk
            22 => {
                if f == 0 {
                    w!().st.save_now();
                    return Wait;
                }
                let disk = vl_core::projects::open_dir(&k!().qa_dir).map(|h| h.edits()).ok();
                let same = disk.as_ref().map(|d| serde_json::to_value(d).ok() == serde_json::to_value(&w!().st.ed).ok()).unwrap_or(false);
                k!().ok("edits on disk match the page", same, "");
                k!().done = true;
                Next
            }
            _ => Wait,
        }
    }
}

impl eframe::App for CheckApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.win.frame_ui(ui, frame);
        if !self.chk.inject.is_empty() {
            ctx.request_repaint();
        } else if !self.chk.done {
            if self.chk.step_start.elapsed() > Duration::from_secs(400) {
                let s = self.chk.step;
                self.chk.ok(&format!("step {s} finishes"), false, "timed out");
                self.chk.done = true;
            } else if self.script(&ctx) == Step::Next {
                self.chk.step += 1;
                self.chk.phase = 0;
                self.chk.sub = 0;
                self.chk.frames = 0;
                self.chk.step_start = Instant::now();
                self.chk.phase_start = Instant::now();
            } else {
                self.chk.frames += 1;
            }
            ctx.request_repaint();
        } else if self.chk.frames < 1000 {
            self.chk.frames += 1;
            if self.chk.frames > 10 {
                self.chk.frames = 1000;
                self.app.win.shell.push(Action::Quit);
            }
            ctx.request_repaint();
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(mut evs) = self.chk.inject.pop_front() {
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

/// Hash of every file under data/projects (except rustqa_*) and data/config.toml.
fn fingerprint() -> BTreeMap<PathBuf, (u64, u64)> {
    use std::hash::{Hash, Hasher};
    fn h(b: &[u8]) -> u64 {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        b.hash(&mut s);
        s.finish()
    }
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, (u64, u64)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().map(|n| n.to_string_lossy().starts_with("rustqa_")).unwrap_or(false) {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(b) = std::fs::read(&p) {
                out.insert(p, (b.len() as u64, h(&b)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&paths::projects_dir(), &mut out);
    if let Ok(b) = std::fs::read(paths::config_path()) {
        out.insert(paths::config_path(), (b.len() as u64, h(&b)));
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

pub fn run(args: &[String]) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("uicheck", false);
    let out = args.first().map(PathBuf::from).unwrap_or_else(|| paths::app_root().join("scratch").join("vl-app-layout").join("uicheck"));
    let _ = std::fs::create_dir_all(&out);
    let src = crate::uicheck::find_project("example_clip-short-4-3").unwrap_or_default();
    let Ok(sh) = vl_core::projects::open_dir(&src) else {
        eprintln!("source project {} missing", src.display());
        return 2;
    };
    if !sh.source_ok() {
        eprintln!("the source video of {} is missing", src.display());
        return 2;
    }
    let before = fingerprint();
    let qa_id = format!("rustqa_layout_{}", chrono::Local::now().format("%H%M%S"));
    let qa_dir = paths::projects_dir().join(&qa_id);
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(qa_dir.clone());
    if let Err(e) = copy_dir(&src, &qa_dir) {
        eprintln!("copy failed: {e}");
        return 2;
    }
    // the default 9:16 arrangement, punch-in on, no per-kill zoom, super smooth off
    match vl_core::projects::open_dir(&qa_dir) {
        Ok(mut h) => {
            h.project.id = qa_id.clone();
            h.project.name = "QA copy (layout)".into();
            let _ = h.save();
            let mut ed = h.edits();
            let ga = h.project.game_area.unwrap_or(vl_core::models::GameArea { x: 0, y: 0, w: 1440, h: 1080, hud_scale: 1.0 });
            ed.layout = Some(crate::backend::default_layout("9:16", &ga));
            ed.format = "9:16".into();
            ed.effects.super_smooth = false;
            ed.effects.zoom = true;
            ed.effects.zoom_per_kill.clear();
            let _ = h.save_edits(&ed);
            // a fresh still is made by the page itself
            let _ = std::fs::remove_file(h.dir.join("cache").join("still_full.png"));
        }
        Err(e) => {
            eprintln!("QA copy unreadable: {e}");
            return 2;
        }
    }
    println!("QA copy: {}\nscreenshots: {}", qa_dir.display(), out.display());
    let opts = Options { read_only: true, writable_prefix: Some("rustqa_".into()), no_resume: true, no_first_run: true };
    let results = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let res2 = results.clone();
    let qa2 = qa_dir.clone();
    let run = eframe::run_native(
        "ValoLights UI check (layout)",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            let win = MainWindow::new(app::gl_env(cc), opts);
            Ok(Box::new(Owner {
                inner: CheckApp {
                    app: ValoApp { win },
                    chk: Check {
                        out,
                        qa_dir: qa2,
                        inject: VecDeque::new(),
                        step: 0,
                        phase: 0,
                        sub: 0,
                        step_start: Instant::now(),
                        phase_start: Instant::now(),
                        frames: 0,
                        results: vec![],
                        v: Vars::default(),
                        reported: vec![],
                        reveal_last: None,
                        reveal_since: Instant::now(),
                        done: false,
                    },
                },
                sink: res2,
            }))
        }),
    );
    if let Err(e) = run {
        eprintln!("GUI failed: {e}");
    }
    let _ = std::fs::remove_dir_all(&qa_dir);
    let after = fingerprint();
    let mut results = results.lock().unwrap().clone();
    let changed: Vec<String> = before
        .iter()
        .filter(|(p, v)| after.get(*p) != Some(v))
        .map(|(p, _)| p.display().to_string())
        .chain(after.keys().filter(|p| !before.contains_key(*p)).map(|p| format!("new: {}", p.display())))
        .collect();
    let line = format!("{} files changed", changed.len());
    println!("[{}] user data untouched: {line} {changed:?}", if changed.is_empty() { " OK " } else { "FAIL" });
    results.push(("user data untouched".into(), changed.is_empty(), format!("{line} {changed:?}")));
    results.push(("QA copy removed".into(), !qa_dir.exists(), qa_dir.display().to_string()));
    let fails = results.iter().filter(|r| !r.1).count();
    let report: String = results.iter().map(|(n, ok, d)| format!("[{}] {n}: {d}\n", if *ok { " OK " } else { "FAIL" })).collect();
    let report = format!("{report}\nRESULT: {} ({} checks, {fails} failed)\n", if fails == 0 { "PASS" } else { "FAIL" }, results.len());
    let _ = std::fs::write(paths::app_root().join("scratch").join("vl-app-layout").join("uicheck_layout_report.txt"), &report);
    println!("{report}");
    i32::from(fails > 0)
}

/// Owns the check app and hands the results out when the window closes.
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
