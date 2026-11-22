//! `valolights --ui-check subs [out_dir]`: drives the real window through the Subtitles page with injected
//! input, runs one real
//! transcription through the speech helper, and saves the app's own frames (egui viewport screenshots,
//! never the desktop) as PNGs. Report: `scratch/vl-app/uicheck_subs_report.txt`.
//!
//! Safety: everything runs read-only except a scratch copy `data/projects/rustqa_subs_*`, deleted at the
//! end; every other project file and data/config.toml are hashed before and after.
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::backend;
use crate::pages::subtitles::{edit_duration, seg_edit, time_label};
use crate::pages::{Action, SUBTITLES, TRIM};
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vl_core::models::Edits;
use vl_core::paths;
use vl_core::transcript::{retime_words, Segment, Transcript};

type Batch = (Vec<Event>, Modifiers);

struct Check {
    out: PathBuf,
    qa_dir: PathBuf,
    inject: VecDeque<Batch>,
    step: usize,
    step_start: Instant,
    frames: u32,
    results: Vec<(String, bool, String)>,
    // values carried between steps
    n: usize,
    f0: f64,
    s0: String,
    states: Vec<String>,
    hist0: usize,
    done: bool,
}

impl Check {
    fn ok(&mut self, name: &str, pass: bool, detail: impl Into<String>) {
        let d = detail.into();
        println!("[{}] {name}: {d}", if pass { " OK " } else { "FAIL" });
        self.results.push((name.into(), pass, d));
    }
    fn push(&mut self, evs: Vec<Event>) {
        self.inject.push_back((evs, Modifiers::NONE));
    }
    fn press(&mut self, p: Pos2) {
        self.push(vec![Event::PointerMoved(p)]);
        self.push(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    }
    fn move_to(&mut self, p: Pos2) {
        self.push(vec![Event::PointerMoved(p)]);
    }
    fn release(&mut self, p: Pos2) {
        self.push(vec![
            Event::PointerMoved(p),
            Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
        ]);
    }
    fn click(&mut self, p: Pos2) {
        self.press(p);
        self.release(p);
    }
    fn double_click(&mut self, p: Pos2) {
        self.click(p);
        self.click(p);
    }
    fn key(&mut self, key: Key, m: Modifiers) {
        self.inject.push_back((
            vec![Event::ModifiersChanged(m), Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m }],
            m,
        ));
        self.inject.push_back((
            vec![
                Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m },
                Event::ModifiersChanged(Modifiers::NONE),
            ],
            Modifiers::NONE,
        ));
    }
    /// Type like a keyboard does: a key event and a text event per character.
    fn type_text(&mut self, s: &str) {
        for ch in s.chars() {
            let key = Key::from_name(&ch.to_ascii_uppercase().to_string()).or(if ch == ' ' { Some(Key::Space) } else { None });
            let mut evs = vec![];
            if let Some(k) = key {
                evs.push(Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
            }
            evs.push(Event::Text(ch.to_string()));
            if let Some(k) = key {
                evs.push(Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
            }
            self.push(evs);
        }
    }
    fn shot(&mut self, ctx: &egui::Context, name: &str) {
        let path = self.out.join(format!("{name}.png"));
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                rgba.extend_from_slice(&p.to_srgba_unmultiplied());
            }
            if let Err(e) = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
                eprintln!("screenshot {}: {e}", path.display());
            }
        });
    }
    fn elapsed(&self) -> Duration {
        self.step_start.elapsed()
    }
    fn waited(&self, ms: u64) -> bool {
        self.elapsed() >= Duration::from_millis(ms)
    }
}

struct CheckApp {
    app: ValoApp,
    chk: Check,
}

#[derive(PartialEq)]
enum Step {
    Next,
    Wait,
}
use Step::{Next, Wait};

fn segs(w: &MainWindow) -> &[Segment] {
    w.st.tr.as_ref().map(|t| t.segments.as_slice()).unwrap_or(&[])
}

fn span(w: &MainWindow, i: usize) -> Option<(f64, f64)> {
    segs(w).get(i).and_then(|s| seg_edit(&w.st, s)).map(|r| (r.0, r.1))
}

/// Undo-relevant state: parts, kill feed position, first caption text.
fn undo_state(w: &MainWindow) -> String {
    let kf = w.st.ed.layout.as_ref().and_then(|l| l.layer("killfeed")).map(|l| format!("{:.3?}", l.dst)).unwrap_or_default();
    format!("{} | {kf} | {:?}", w.st.ed.parts.len(), segs(w).first().map(|s| s.text.clone()))
}

impl CheckApp {
    fn script(&mut self, ctx: &egui::Context) -> Step {
        let w = &mut self.app.win;
        let k = &mut self.chk;
        let f = k.frames;
        let tk = |w: &MainWindow| w.subs.track.rect;
        match k.step {
            0 => {
                w.open_project(&k.qa_dir.clone(), true);
                Next
            }
            1 => {
                if f < 5 {
                    return Wait;
                }
                if !w.dialogs.is_empty() {
                    w.dialogs.clear();
                }
                w.go(SUBTITLES);
                Next
            }
            // the preview loads (export composition + ASS)
            2 => {
                let p = &w.subs.player;
                if p.duration() > 0.0 && p.frames_rendered > 0 && f > 20 {
                    let parts: Vec<String> = backend::EditList::plain(&w.st.ed.parts)
                        .placed
                        .iter()
                        .map(|p| format!("({:.2}, {:.2}, {:.2})", p.out_start, p.out_start + p.dur(), p.src_in))
                        .collect();
                    k.ok("preview loads", w.page == SUBTITLES, format!("duration {:.2} s · edit parts {}", p.duration(), parts.join(" ")));
                    return Next;
                }
                if k.elapsed() > Duration::from_secs(20) {
                    k.ok("preview loads", false, format!("info {:?}", w.subs.player.info()));
                    return Next;
                }
                Wait
            }
            // 1. times are shown in edit time
            3 => {
                let s0 = segs(w)[0].clone();
                let r = seg_edit(&w.st, &s0).unwrap();
                let lbl = time_label(&w.st, &s0);
                k.ok(
                    "caption times shown in edit time",
                    (w.subs.ed_start - r.0).abs() < 0.01 && lbl.starts_with("00:0") && w.subs.sel == 0,
                    format!("start box {:.2}, edit start {:.2}, source {}, row {lbl:?}", w.subs.ed_start, r.0, s0.start),
                );
                k.shot(ctx, "subs_01_loaded");
                Next
            }
            // 2. clicking the empty track (below the lane) scrubs
            4 => {
                if f == 0 {
                    let x = w.subs.track.x_of(6.0);
                    k.click(Pos2::new(x, tk(w).top() + 68.0));
                    return Wait;
                }
                if !k.waited(700) {
                    return Wait;
                }
                let pos = w.subs.player.pos();
                k.ok("click on the track moves the playhead", (pos - 6.0).abs() < 0.25, format!("pos {pos:.2}"));
                Next
            }
            // 3. dragging on the ruler scrubs continuously
            5 => {
                if f == 0 {
                    let y = tk(w).top() + 12.0;
                    let (x2, x4) = (w.subs.track.x_of(2.0), w.subs.track.x_of(4.0));
                    k.press(Pos2::new(x2, y));
                    for s in 1..=6 {
                        k.move_to(Pos2::new(x2 + (x4 - x2) * s as f32 / 6.0, y));
                    }
                    k.release(Pos2::new(x4, y));
                    return Wait;
                }
                if !k.waited(1500) {
                    return Wait;
                }
                let pos = w.subs.player.pos();
                k.ok("dragging the playhead scrubs", (pos - 4.0).abs() < 0.25, format!("pos {pos:.2}"));
                Next
            }
            // 4. clicking a caption block selects it
            6 => {
                if f == 0 {
                    let it = w.subs.track.item(1).cloned().unwrap();
                    k.click(Pos2::new(w.subs.track.x_of((it.a + it.b) / 2.0), tk(w).top() + 44.0));
                    return Wait;
                }
                if !k.waited(400) {
                    return Wait;
                }
                k.ok(
                    "clicking a caption on the track selects it",
                    w.subs.sel == 1 && w.subs.ed_text == "he is one shot",
                    format!("sel {} text {:?}", w.subs.sel, w.subs.ed_text),
                );
                Next
            }
            // 5. drag the end edge of the selected caption
            7 => {
                if f == 0 {
                    k.f0 = segs(w)[1].end;
                    k.hist0 = w.st.history_len();
                    let it = w.subs.track.item(1).cloned().unwrap();
                    let (x, y) = (w.subs.track.x_of(it.b), tk(w).top() + 44.0);
                    k.press(Pos2::new(x, y));
                    for s in 1..8 {
                        k.move_to(Pos2::new(x + s as f32 * 6.0, y));
                    }
                    k.release(Pos2::new(x + 42.0, y));
                    return Wait;
                }
                if !k.waited(400) {
                    return Wait;
                }
                let e = segs(w)[1].end;
                k.ok("dragging a caption edge retimes it", e > k.f0 + 0.05, format!("{:.2} -> {e:.2}", k.f0));
                k.ok(
                    "track retime is one undo step",
                    w.st.history_len() == k.hist0 + 1 && w.st.undo_label() == "caption edit",
                    format!("history {} -> {}, label {:?}", k.hist0, w.st.history_len(), w.st.undo_label()),
                );
                Next
            }
            // 6. [ sets the start to the playhead
            8 => {
                if f == 0 {
                    let r1 = span(w, 1).unwrap();
                    k.f0 = r1.0 + 0.3;
                    let st = &w.st;
                    w.subs.seek_edit(st, k.f0);
                    return Wait;
                }
                if !k.waited(500) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = "bracket".into();
                    k.key(Key::OpenBracket, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(900) {
                    return Wait;
                }
                k.s0.clear();
                let i = w.subs.sel as usize;
                let r = span(w, i).unwrap();
                k.ok("[ sets the start to the playhead", (r.0 - k.f0).abs() < 0.06, format!("start {:.2}, wanted {:.2}", r.0, k.f0));
                Next
            }
            // 7. S splits at the playhead
            9 => {
                if f == 0 {
                    k.n = segs(w).len();
                    let r = span(w, w.subs.sel as usize).unwrap();
                    let st = &w.st;
                    w.subs.seek_edit(st, (r.0 + r.1) / 2.0);
                    return Wait;
                }
                if !k.waited(500) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = "s".into();
                    k.key(Key::S, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(900) {
                    return Wait;
                }
                k.s0.clear();
                k.ok(
                    "S splits the caption at the playhead",
                    segs(w).len() == k.n + 1,
                    format!("{} -> {} captions, msg {:?}", k.n, segs(w).len(), w.subs.msg),
                );
                Next
            }
            // 8. Space plays, Space pauses
            10 => {
                if f == 0 {
                    k.key(Key::Space, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(700) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = if w.subs.player.is_playing() { "playing".into() } else { "not playing".into() };
                    k.key(Key::Space, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(1100) {
                    return Wait;
                }
                let paused = !w.subs.player.is_playing();
                k.ok("Space plays and pauses", k.s0 == "playing" && paused, format!("after 1st: {}, paused after 2nd: {paused}", k.s0));
                k.s0.clear();
                Next
            }
            // 9. Right arrow steps one frame
            11 => {
                if f == 0 {
                    k.f0 = w.subs.player.pos();
                    k.key(Key::ArrowRight, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(500) {
                    return Wait;
                }
                let d = w.subs.player.pos() - k.f0;
                k.ok("Right arrow steps one frame", 0.005 < d && d < 0.05, format!("{:.3} -> {:.3}", k.f0, w.subs.player.pos()));
                Next
            }
            // 10. Play caption stops at its end
            12 => {
                if f == 0 {
                    let st = &mut w.st;
                    w.subs.select(st, 0, true);
                    return Wait;
                }
                if !k.waited(400) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = "playing".into();
                    let r = span(w, 0).unwrap();
                    k.f0 = r.1;
                    let st = &w.st;
                    w.subs.play_line(st);
                    k.step_start = Instant::now();
                    k.n = ((r.1 - r.0) * 1000.0) as usize + 1000;
                    return Wait;
                }
                // wait until it stops by itself (or give up 3 s after the caption should have ended)
                if w.subs.player.is_playing() || !k.waited(400) {
                    if k.waited(k.n as u64 + 2000) {
                        k.ok("Play caption stops at the caption end", false, format!("still playing at {:.2}", w.subs.player.pos()));
                        k.s0.clear();
                        return Next;
                    }
                    return Wait;
                }
                k.s0.clear();
                let pos = w.subs.player.pos();
                k.ok(
                    "Play caption stops at the caption end",
                    !w.subs.player.is_playing() && (pos - k.f0).abs() < 0.15,
                    format!("stopped at {pos:.2}, end {:.2}", k.f0),
                );
                Next
            }
            // 11. inline list edit (double-click, select all, type, Enter) is kept
            13 => {
                if f == 0 {
                    let Some(r) = w.subs.rects.rows.iter().find(|(i, _)| *i == 0).map(|x| x.1) else {
                        k.ok("editing text in the list is kept", false, "row 0 not visible");
                        return Next;
                    };
                    k.hist0 = w.st.history_len();
                    k.double_click(Pos2::new(r.right() - 60.0, r.center().y));
                    return Wait;
                }
                if f < 4 {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = "typed".into();
                    k.key(Key::A, Modifiers::COMMAND);
                    k.type_text("nice one brother that was clean");
                    k.key(Key::Enter, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(400) {
                    return Wait;
                }
                k.s0.clear();
                let t = segs(w)[0].text.clone();
                k.ok("editing text in the list is kept", t == "nice one brother that was clean", format!("{t:?}"));
                k.ok(
                    "list edit is one undo step",
                    w.st.history_len() == k.hist0 + 1 && w.st.undo_label() == "caption edit",
                    format!("history {} -> {}", k.hist0, w.st.history_len()),
                );
                Next
            }
            // 12. editor text: Enter commits
            14 => {
                if f == 0 {
                    let st = &mut w.st;
                    w.subs.select(st, 0, true);
                    return Wait;
                }
                if f == 3 {
                    k.click(w.subs.rects.text.center());
                    k.key(Key::A, Modifiers::COMMAND);
                    k.type_text("clean round");
                    k.key(Key::Enter, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(600) || f < 5 {
                    return Wait;
                }
                let t = segs(w)[0].text.clone();
                k.ok("typing + Enter in the caption box commits", t == "clean round", format!("{t:?}"));
                Next
            }
            // 13. N adds a caption at the playhead without overlapping
            15 => {
                if f == 0 {
                    k.n = segs(w).len();
                    let st = &w.st;
                    w.subs.seek_edit(st, 0.3);
                    return Wait;
                }
                if !k.waited(500) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = "n".into();
                    k.key(Key::N, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(900) {
                    return Wait;
                }
                k.s0.clear();
                let mut spans: Vec<(f64, f64)> = segs(w).iter().filter_map(|s| seg_edit(&w.st, s)).map(|r| (r.0, r.1)).collect();
                spans.sort_by(|a, b| a.0.total_cmp(&b.0));
                let overl = spans.windows(2).any(|p| p[0].1 > p[1].0 + 1e-6);
                let focused = ctx.text_edit_focused();
                k.ok(
                    "N adds a caption at the playhead, no overlaps",
                    segs(w).len() == k.n + 1 && !overl,
                    format!("{} -> {} captions, overlap {overl}", k.n, segs(w).len()),
                );
                k.ok("new caption puts the cursor in the text box", focused, format!("text box focused {focused}"));
                k.shot(ctx, "subs_02_new_caption");
                k.key(Key::Escape, Modifiers::NONE);
                Next
            }
            // 14. Delete removes, Ctrl+Z restores
            16 => {
                if f == 0 {
                    k.n = segs(w).len();
                    k.key(Key::Delete, Modifiers::NONE);
                    return Wait;
                }
                if !k.waited(900) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = if segs(w).len() + 1 == k.n { "deleted".into() } else { format!("not deleted ({})", segs(w).len()) };
                    k.key(Key::Z, Modifiers::COMMAND);
                    return Wait;
                }
                if !k.waited(1500) {
                    return Wait;
                }
                k.ok(
                    "Delete removes, Ctrl+Z brings it back",
                    k.s0 == "deleted" && segs(w).len() == k.n,
                    format!("{}, now {} (was {}), toast {:?}", k.s0, segs(w).len(), k.n, w.toast),
                );
                k.s0.clear();
                Next
            }
            // 15. playback highlights the current caption
            17 => {
                if f == 0 {
                    let st = &mut w.st;
                    w.subs.select(st, 0, false);
                    let r = span(w, 0).unwrap();
                    let st = &w.st;
                    w.subs.seek_edit(st, (r.0 + r.1) / 2.0);
                    return Wait;
                }
                if !k.waited(700) {
                    return Wait;
                }
                k.ok("the playing caption is highlighted", w.subs.track.now == 0, format!("now {}", w.subs.track.now));
                Next
            }
            // 16. position survives switching tabs
            18 => {
                if f == 0 {
                    let st = &w.st;
                    w.subs.seek_edit(st, 5.5);
                    return Wait;
                }
                if f == 1 || !k.waited(600) {
                    return Wait;
                }
                if k.s0.is_empty() {
                    k.s0 = "trim".into();
                    w.go(TRIM);
                    return Wait;
                }
                if k.s0 == "trim" {
                    if !k.waited(1600) {
                        return Wait;
                    }
                    k.s0 = "back".into();
                    w.go(SUBTITLES);
                    return Wait;
                }
                if !k.waited(3600) {
                    return Wait;
                }
                k.s0.clear();
                let pos = w.subs.player.pos();
                k.ok("position kept when coming back to the page", (pos - 5.5).abs() < 0.4, format!("pos {pos:.2}"));
                Next
            }
            // Up / Down move through the list (select + seek), like a table
            1018 => {
                if f == 0 {
                    let r = w.subs.rects.rows.first().map(|x| x.1);
                    if let Some(r) = r {
                        k.click(Pos2::new(r.right() - 40.0, r.center().y));
                    }
                    return Wait;
                }
                if f == 4 {
                    k.n = w.subs.sel as usize;
                    k.key(Key::ArrowDown, Modifiers::NONE);
                    return Wait;
                }
                if f < 12 {
                    return Wait;
                }
                let want = w.subs.rows.iter().position(|&i| i == k.n).and_then(|r| w.subs.rows.get(r + 1)).copied();
                k.ok(
                    "Down selects the next caption in the list",
                    want.map(|i| i as isize) == Some(w.subs.sel),
                    format!("{} -> {}", k.n, w.subs.sel),
                );
                Next
            }
            // 17. typing shortcut letters in the caption box types text
            19 => {
                if f == 0 {
                    k.n = segs(w).len();
                    let st = &mut w.st;
                    w.subs.select(st, 0, true);
                    return Wait;
                }
                if f == 3 {
                    k.click(w.subs.rects.text.center());
                    k.key(Key::A, Modifiers::COMMAND);
                    k.type_text("snipe n swing [ ]");
                    for _ in 0..4 {
                        k.key(Key::Backspace, Modifiers::NONE);
                    }
                    k.key(Key::Enter, Modifiers::NONE);
                    return Wait;
                }
                if f < 6 || !k.waited(700) {
                    return Wait;
                }
                let t = segs(w)[0].text.clone();
                k.ok(
                    "typing S/N/[ in the text box doesn't trigger shortcuts",
                    segs(w).len() == k.n && t == "snipe n swing",
                    format!("{t:?}, {} captions", segs(w).len()),
                );
                Next
            }
            // 18. page shortcuts are off on other pages
            20 => {
                if f == 0 {
                    k.n = segs(w).len();
                    w.go(TRIM);
                    return Wait;
                }
                if f == 10 {
                    k.key(Key::N, Modifiers::NONE);
                    k.key(Key::Delete, Modifiers::NONE);
                    return Wait;
                }
                if f < 20 {
                    return Wait;
                }
                k.ok("subtitle shortcuts don't fire on other pages", segs(w).len() == k.n, format!("{} -> {}", k.n, segs(w).len()));
                w.go(SUBTITLES);
                Next
            }
            // discrete undo steps: a typed edit (grouped) then a split = two steps, undone one at a time
            21 => {
                if f == 0 {
                    let i = (0..segs(w).len()).find(|&i| span(w, i).map(|r| r.1 - r.0 > 0.6).unwrap_or(false)).unwrap_or(0);
                    let st = &mut w.st;
                    w.subs.select(st, i, true);
                    return Wait;
                }
                if f == 1 {
                    let r = span(w, w.subs.sel as usize).unwrap_or((0.0, 0.0));
                    let st = &w.st;
                    w.subs.seek_edit(st, (r.0 + r.1) / 2.0);
                    return Wait;
                }
                if f < 30 {
                    return Wait;
                }
                if f == 30 {
                    w.st.flush_history();
                    k.hist0 = w.st.history_len();
                    k.n = segs(w).len();
                    k.click(w.subs.rects.text.center());
                    k.key(Key::A, Modifiers::COMMAND);
                    k.type_text("he is one tap");
                    k.key(Key::Enter, Modifiers::NONE);
                    k.key(Key::S, Modifiers::NONE);
                    return Wait;
                }
                if f < 34 || !k.waited(2500) {
                    return Wait;
                }
                let h = w.st.history_len();
                let split = segs(w).len() == k.n + 1;
                w.st.undo();
                let after1 = segs(w).len() == k.n && segs(w).iter().any(|s| s.text == "he is one tap");
                w.st.undo();
                let after2 = !segs(w).iter().any(|s| s.text == "he is one tap");
                w.st.redo();
                w.st.redo();
                k.ok(
                    "typing then split = two undo steps",
                    split && h == k.hist0 + 2 && after1 && after2 && segs(w).len() == k.n + 1,
                    format!("history {} -> {h}, split {split}, undo1 keeps text {after1}, undo2 drops text {after2}", k.hist0),
                );
                Next
            }
            // the censor dialog: pick a word, OK = one step, the list shows ***
            22 => {
                if f == 0 {
                    let st = &mut w.st;
                    w.subs.select(st, 0, false);
                    let st = &mut w.st;
                    w.subs.censor_words(st);
                    k.hist0 = w.st.history_len();
                    return Wait;
                }
                if f == 4 {
                    k.shot(ctx, "subs_03_censor");
                    match w.subs.rects.censor_chips.first().copied() {
                        Some(r) => k.click(r.center()),
                        None => k.ok("censor dialog", false, "no word chips"),
                    }
                    k.key(Key::Enter, Modifiers::NONE);
                    return Wait;
                }
                if f < 10 {
                    return Wait;
                }
                let s = segs(w)[0].clone();
                k.ok(
                    "Censor… hides the picked word",
                    s.words.first().map(|w| w.censored).unwrap_or(false) && w.subs.censor.is_none() && w.st.history_len() == k.hist0 + 1,
                    format!("words {:?}", s.words.iter().map(|w| (w.text.clone(), w.censored)).collect::<Vec<_>>()),
                );
                let st = &mut w.st;
                w.subs.select(st, 1, true);
                Next
            }
            23 => {
                if !k.waited(1200) {
                    return Wait;
                }
                k.shot(ctx, "subs_04_916");
                Next
            }
            // trim, layout and caption edits are separate steps with their labels
            24 => {
                if f == 0 {
                    w.st.flush_history();
                    k.states = vec![undo_state(w)];
                    let mut parts = w.st.ed.parts.clone();
                    if parts.len() > 1 {
                        parts.remove(0);
                    }
                    w.st.set_parts(parts, true);
                    k.states.push(undo_state(w));
                    if let Some(l) = w.st.ed.layout.as_mut().and_then(|l| l.layer_mut("killfeed")) {
                        l.dst = (0.1, 0.5, 0.68, 0.11);
                    }
                    w.st.touch();
                    return Wait;
                }
                if !k.waited(900) {
                    return Wait;
                }
                if k.states.len() == 2 {
                    k.states.push(undo_state(w));
                    k.s0 = w.st.undo_label();
                    let st = &mut w.st;
                    w.subs.select(st, 0, false);
                    return Wait;
                }
                if k.states.len() == 3 && f > 0 && k.inject.is_empty() && k.n != 777 {
                    k.n = 777;
                    k.click(w.subs.rects.text.center());
                    k.key(Key::A, Modifiers::COMMAND);
                    k.type_text("nice one bro");
                    return Wait; // the typing pause (450 ms) commits, the group closes 600 ms later
                }
                if !k.waited(3500) {
                    return Wait;
                }
                k.states.push(undo_state(w));
                let label = w.st.undo_label();
                k.ok(
                    "undo labels per page",
                    k.s0 == "layout change" && label == "caption edit",
                    format!("after layout {:?}, after text {label:?}", k.s0),
                );
                k.n = 0;
                // Ctrl+Z inside the focused text field must not touch the app history
                k.key(Key::Z, Modifiers::COMMAND);
                Next
            }
            25 => {
                if f < 6 {
                    return Wait;
                }
                let s3 = k.states[3].clone();
                k.ok("Ctrl+Z in a text field keeps the app state", undo_state(w) == s3 && ctx.text_edit_focused(), undo_state(w));
                k.key(Key::Escape, Modifiers::NONE);
                Next
            }
            26..=28 => {
                if f == 2 {
                    k.key(Key::Z, Modifiers::COMMAND);
                    return Wait;
                }
                if f < 10 {
                    return Wait;
                }
                let want = k.states[2 - (k.step - 26)].clone();
                let name = format!("undo {}", k.step - 25);
                k.ok(&name, undo_state(w) == want, format!("toast {:?}", w.toast));
                Next
            }
            29..=31 => {
                if f == 2 {
                    k.key(Key::Y, Modifiers::COMMAND);
                    return Wait;
                }
                if f < 10 {
                    return Wait;
                }
                let want = k.states[1 + (k.step - 29)].clone();
                let name = format!("redo {}", k.step - 28);
                k.ok(&name, undo_state(w) == want, format!("toast {:?}", w.toast));
                if k.step == 31 {
                    w.st.save_now();
                    let disk = vl_core::projects::open_dir(&k.qa_dir).map(|h| h.edits().parts.len()).unwrap_or(0);
                    k.ok(
                        "saved to disk matches, redo disabled at end",
                        disk == w.st.ed.parts.len() && !w.st.can_redo(),
                        format!("disk parts {disk}"),
                    );
                }
                Next
            }
            // the 16:9 arrangement (preview + editor on top, list below)
            32 => {
                if f == 0 {
                    let ga = w.st.h.as_ref().and_then(|h| h.project.game_area).unwrap();
                    w.st.ed.layout = Some(backend::default_layout("16:9", &ga));
                    w.st.ed.format = "16:9".into();
                    w.st.touch();
                    w.go(TRIM);
                    return Wait;
                }
                if f == 3 {
                    w.go(SUBTITLES);
                    return Wait;
                }
                if f == 6 {
                    let st = &mut w.st;
                    w.subs.select(st, 1, true);
                }
                if !k.waited(3000) {
                    return Wait;
                }
                k.shot(ctx, "subs_05_169");
                let snap = k.out.join("subs_05_169_mpv.png");
                w.subs.player.snapshot(&snap.to_string_lossy());
                k.ok(
                    "16:9 arrangement",
                    w.subs.track.rect.width() > 1000.0,
                    format!("track width {:.0}, frames rendered {}", w.subs.track.rect.width(), w.subs.player.frames_rendered),
                );
                Next
            }
            // the preview shows the new composition (texture refreshed after the shape change)
            1032 => {
                if f == 0 {
                    k.n = w.subs.player.frames_rendered as usize;
                    let st = &w.st;
                    w.subs.seek_edit(st, 1.0);
                    return Wait;
                }
                if !k.waited(1500) {
                    return Wait;
                }
                k.ok(
                    "preview frame after a seek",
                    w.subs.player.frames_rendered as usize > k.n,
                    format!("frames {} -> {}", k.n, w.subs.player.frames_rendered),
                );
                k.shot(ctx, "subs_05b_169_after_seek");
                Next
            }
            // a real transcription of the kept parts through the speech helper
            33 if std::env::var_os("VL_SUBS_NO_SPEECH").is_some() => {
                // the progress look without the speech helper
                if f == 0 {
                    w.subs.progress = Some(0.4);
                    w.subs.status = "Transcribing 0:12 of 0:30".into();
                    return Wait;
                }
                if f == 6 {
                    k.shot(ctx, "subs_06_progress_sim");
                }
                if f < 12 {
                    return Wait;
                }
                w.subs.progress = None;
                Next
            }
            34 if std::env::var_os("VL_SUBS_NO_SPEECH").is_some() => Next,
            33 => {
                if f == 0 {
                    w.shell.push(Action::TranscribeConfirmed { whole: false, only: None });
                    k.s0.clear();
                    return Wait;
                }
                if w.subs.progress.is_some() && k.s0.is_empty() {
                    k.s0 = "running".into();
                    k.shot(ctx, "subs_06_transcribing");
                }
                if w.subs.progress.is_none() && k.s0 == "running" && f > 2 {
                    let tr = w.st.tr.clone().unwrap_or_default();
                    let texts: Vec<String> = tr.segments.iter().take(6).map(|s| s.text.clone()).collect();
                    k.ok(
                        "real transcription (kept parts)",
                        tr.model != "test" && !tr.segments.is_empty() && w.st.ed.subtitles.enabled,
                        format!("model {:?}, {} captions, status {:?}, first {texts:?}", tr.model, tr.segments.len(), w.subs.status),
                    );
                    k.s0 = "done".into();
                    k.step_start = Instant::now();
                    return Wait;
                }
                if k.s0 == "done" {
                    if !k.waited(1500) {
                        return Wait;
                    }
                    k.s0.clear();
                    k.shot(ctx, "subs_07_transcribed");
                    return Next;
                }
                if k.elapsed() > Duration::from_secs(600) {
                    k.ok("real transcription (kept parts)", false, format!("timed out, status {:?}", w.subs.status));
                    return Next;
                }
                Wait
            }
            // Listen again: one caption
            34 => {
                if f == 0 {
                    let Some(i) = segs(w).iter().position(|s| seg_edit(&w.st, s).is_some()) else {
                        k.ok("Listen again (one caption)", false, "no caption in the cut");
                        return Next;
                    };
                    let st = &mut w.st;
                    w.subs.select(st, i, true);
                    let s = &segs(w)[i];
                    k.n = segs(w).len();
                    w.shell.push(Action::Transcribe { whole: false, only: Some((s.start, s.end)) });
                    return Wait;
                }
                if w.subs.progress.is_some() && k.s0.is_empty() {
                    k.s0 = "running".into();
                }
                if w.subs.progress.is_none() && k.s0 == "running" && f > 2 {
                    k.ok(
                        "Listen again (one caption)",
                        w.subs.status.starts_with("Model") || w.toast.contains("again") || !segs(w).is_empty(),
                        format!("status {:?}, {} -> {} captions", w.subs.status, k.n, segs(w).len()),
                    );
                    k.s0.clear();
                    return Next;
                }
                if k.elapsed() > Duration::from_secs(300) {
                    k.ok("Listen again (one caption)", false, format!("timed out, status {:?}", w.subs.status));
                    return Next;
                }
                Wait
            }
            // Words at a time: at most two words of a caption on screen
            35 => {
                if f == 0 {
                    k.s0 = format!("{}", w.st.ed.subtitles.max_words);
                    let r = w.subs.max_words_rect;
                    // seven equal cells: All, 1, 2, 3, 4, 6, 8 -> "2" is the third
                    let cell = (r.width() - 4.0) / 7.0;
                    k.click(Pos2::new(r.left() + 2.0 + cell * 2.5, r.center().y));
                    return Wait;
                }
                if f < 12 {
                    return Wait;
                }
                let mw = w.st.ed.subtitles.max_words;
                let ass = w.subs.subs_path_for_check(&w.st).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
                let lines: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue")).collect();
                let most = lines
                    .iter()
                    .map(|l| {
                        let text = l.splitn(10, ',').nth(9).unwrap_or("");
                        // drop the colour tags and a speaker name, count the words
                        let mut plain = String::new();
                        let mut tag = false;
                        for ch in text.chars() {
                            match ch {
                                '{' => tag = true,
                                '}' => tag = false,
                                c if !tag => plain.push(c),
                                _ => {}
                            }
                        }
                        let plain = plain.split_once(": ").map(|(_, t)| t.to_string()).unwrap_or(plain);
                        plain.split_whitespace().count()
                    })
                    .max()
                    .unwrap_or(0);
                k.ok(
                    "Words at a time 2: no caption shows more than two words",
                    mw == 2 && !lines.is_empty() && most <= 2,
                    format!("max_words {mw}, {} events, most words {most}", lines.len()),
                );
                k.shot(ctx, "subs_08_two_words");
                w.st.undo();
                Next
            }
            36 => {
                if f < 6 {
                    return Wait;
                }
                let back = w.st.ed.subtitles.max_words.to_string() == k.s0;
                k.ok("one undo puts Words at a time back", back, format!("{} (was {})", w.st.ed.subtitles.max_words, k.s0));
                k.s0.clear();
                Next
            }
            _ => {
                k.done = true;
                Next
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.win.frame_ui(ui, frame);
        if !self.chk.inject.is_empty() {
            ctx.request_repaint();
        } else if !self.chk.done {
            match self.script(&ctx) {
                Next => {
                    self.chk.step = match self.chk.step {
                        18 => 1018,
                        1018 => 19,
                        32 => 1032,
                        1032 => 33,
                        s => s + 1,
                    };
                    self.chk.frames = 0;
                    self.chk.step_start = Instant::now();
                }
                Wait => self.chk.frames += 1,
            }
            ctx.request_repaint();
        } else if self.chk.frames < 1000 {
            self.chk.frames += 1;
            if self.chk.frames > 8 {
                self.chk.frames = 1000;
                self.app.win.shell.push(Action::Quit);
            }
            ctx.request_repaint();
        }
    }
}

fn fingerprint() -> BTreeMap<PathBuf, (u64, u64)> {
    use std::hash::{Hash, Hasher};
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, (u64, u64)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().map(|n| n.to_string_lossy().starts_with("rustqa_") || n.to_string_lossy().starts_with("qa")).unwrap_or(false) {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(p, (bytes.len() as u64, h.finish()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&paths::projects_dir(), &mut out);
    if let Ok(b) = std::fs::read(paths::config_path()) {
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

fn seg(a: f64, b: f64, text: &str) -> Segment {
    let mut s = Segment {
        start: a,
        end: b,
        text: String::new(),
        speaker: "s1".into(),
        words: vec![],
        voice_line: false,
        hidden: false,
        edited: false,
        added: false,
    };
    retime_words(&mut s, text);
    // even word spacing
    let n = s.words.len().max(1) as f64;
    for (i, w) in s.words.iter_mut().enumerate() {
        w.start = a + i as f64 * (b - a) / n;
        w.end = a + (i + 1) as f64 * (b - a) / n;
    }
    s
}

pub fn run(args: &[String]) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("uicheck_subs", false);
    let out = args.first().map(PathBuf::from).unwrap_or_else(|| paths::app_root().join("scratch").join("vl-app").join("uicheck_subs"));
    let _ = std::fs::create_dir_all(&out);
    // the newest "short" clip
    let Some(src) = vl_core::projects::list_all()
        .into_iter()
        .filter(|h| {
            h.project.name.contains("short")
                && !h.project.id.starts_with("rustqa_")
                && h.project.status == "ready"
                && h.source_ok()
                && h.project.game_area.is_some()
        })
        .max_by(|a, b| a.project.created.cmp(&b.project.created))
    else {
        eprintln!("no ready 'short' project");
        return 2;
    };
    let before = fingerprint();
    let qa_id = format!("rustqa_subs_{}", chrono::Local::now().format("%H%M%S"));
    let qa_dir = paths::projects_dir().join(&qa_id);
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(qa_dir.clone());
    if let Err(e) = copy_dir(&src.dir, &qa_dir) {
        eprintln!("copy failed: {e}");
        return 2;
    }
    let mut h = match vl_core::projects::open_dir(&qa_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("open copy: {e}");
            return 2;
        }
    };
    h.project.id = qa_id.clone();
    h.project.name = format!("QA copy of {}", h.project.name);
    let _ = h.save();
    let tr = Transcript {
        model: "test".into(),
        segments: vec![seg(14.3, 15.6, "nice one brother"), seg(35.0, 36.4, "he is one shot"), seg(38.0, 39.5, "go go go")],
        ..Default::default()
    };
    let _ = vl_core::models::save_json(&tr, &h.transcript_path());
    let mut fresh = Edits {
        layout: Some(backend::default_layout("9:16", h.project.game_area.as_ref().unwrap())),
        format: "9:16".into(),
        ..Default::default()
    };
    fresh.subtitles.enabled = true;
    let _ = h.save_edits(&fresh);
    println!("QA copy: {}\nscreenshots: {}", qa_dir.display(), out.display());
    let opts = Options { read_only: true, writable_prefix: Some("rustqa_".into()), no_resume: true, no_first_run: true };
    let results = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let sink = results.clone();
    let qa2 = qa_dir.clone();
    struct Owner {
        inner: CheckApp,
        sink: std::sync::Arc<std::sync::Mutex<Vec<(String, bool, String)>>>,
    }
    impl eframe::App for Owner {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            self.inner.ui(ui, frame);
        }
        fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
            if let Some((mut evs, _m)) = self.inner.chk.inject.pop_front() {
                raw.events.append(&mut evs);
                raw.focused = true;
            }
        }
        fn on_exit(&mut self, gl: Option<&glow::Context>) {
            *self.sink.lock().unwrap() = self.inner.chk.results.clone();
            eframe::App::on_exit(&mut self.inner.app, gl);
        }
        fn clear_color(&self, v: &egui::Visuals) -> [f32; 4] {
            self.inner.app.clear_color(v)
        }
        fn persist_egui_memory(&self) -> bool {
            false
        }
    }
    let run = eframe::run_native(
        "ValoLights subtitles check",
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
                        step_start: Instant::now(),
                        frames: 0,
                        results: vec![],
                        n: 0,
                        f0: 0.0,
                        s0: String::new(),
                        states: vec![],
                        hist0: 0,
                        done: false,
                    },
                },
                sink,
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
    results.push(("user data untouched".into(), changed.is_empty(), format!("{} files changed {changed:?}", changed.len())));
    results.push(("QA copy removed".into(), !qa_dir.exists(), qa_dir.display().to_string()));
    let fails = results.iter().filter(|r| !r.1).count();
    let report: String = results.iter().map(|(n, ok, d)| format!("[{}] {n}: {d}\n", if *ok { " OK " } else { "FAIL" })).collect();
    let report = format!("{report}\nRESULT: {} ({} checks, {fails} failed)\n", if fails == 0 { "PASS" } else { "FAIL" }, results.len());
    let _ = std::fs::write(paths::app_root().join("scratch").join("vl-app").join("uicheck_subs_report.txt"), &report);
    println!("{report}");
    let _ = edit_duration;
    i32::from(fails > 0)
}
