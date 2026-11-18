//! `valolights --ui-check [out_dir]`: drives the real GUI with scripted input and saves the app's own
//! rendered frames (egui viewport screenshots, never the desktop) as PNGs, then writes a report.
//!
//! Safety: the app runs read-only for every project except a scratch copy `data/projects/rustqa_*`, which
//! is deleted at the end; every other project file and data/config.toml are hashed before and after.
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::dialogs::Dialog;
use crate::pages::{Action, Page, CLIPS, EXPORT, LAYOUT, SUBTITLES, TRIM};
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vl_core::paths;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Next,
    Wait,
}

struct Check {
    out: PathBuf,
    qa_dir: PathBuf,
    qa_id: String,
    other_dir: Option<PathBuf>,
    /// input batches, one per frame
    inject: std::collections::VecDeque<(Vec<Event>, Modifiers)>,
    mods: Modifiers,
    step: usize,
    step_start: Instant,
    frames_in_step: u32,
    results: Vec<(String, bool, String)>,
    shots: Vec<String>,
    // values carried between steps
    pos0: f64,
    frames0: u64,
    rows0: i32,
    done: bool,
}

impl Check {
    fn ok(&mut self, name: &str, pass: bool, detail: impl Into<String>) {
        let d = detail.into();
        println!("[{}] {name}: {d}", if pass { " OK " } else { "FAIL" });
        self.results.push((name.into(), pass, d));
    }
    fn click(&mut self, p: Pos2, button: PointerButton) {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return;
        }
        let m = Modifiers::NONE;
        self.inject.push_back((vec![Event::PointerMoved(p)], m));
        self.inject.push_back((vec![Event::PointerButton { pos: p, button, pressed: true, modifiers: m }], m));
        self.inject.push_back((vec![Event::PointerButton { pos: p, button, pressed: false, modifiers: m }], m));
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.inject.push_back((
            vec![Event::ModifiersChanged(modifiers), Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
            modifiers,
        ));
        self.inject.push_back((
            vec![
                Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers },
                Event::ModifiersChanged(Modifiers::NONE),
            ],
            Modifiers::NONE,
        ));
    }
    fn shot(&mut self, ctx: &egui::Context, name: &str) {
        let path = self.out.join(format!("{name}.png"));
        self.shots.push(path.display().to_string());
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
}

struct CheckApp {
    app: ValoApp,
    chk: Check,
}

fn card_center(win: &MainWindow, pid: &str) -> Option<Pos2> {
    win.library.card(pid).map(|c| c.rect).filter(|r| r.is_positive()).map(|r| r.center())
}

impl CheckApp {
    /// The script. Each arm runs once per frame until it returns Next.
    fn script(&mut self, ctx: &egui::Context) -> Step {
        let w = &mut self.app.win;
        let k = &mut self.chk;
        let f = k.frames_in_step;
        match k.step {
            // 0: Clips shows the existing projects
            0 => {
                if f < 15 {
                    return Step::Wait;
                }
                let on_disk = std::fs::read_dir(paths::projects_dir())
                    .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().join("project.json").exists()).count())
                    .unwrap_or(0);
                let n = w.library.cards.len();
                k.ok("clips list", n == on_disk && n > 0, format!("{n} cards, {on_disk} projects on disk"));
                let laid_out = w.library.cards.iter().filter(|c| c.rect.is_positive()).count();
                k.ok("cards drawn", laid_out > 0, format!("{laid_out} cards visible"));
                k.shot(ctx, "01_clips");
                Step::Next
            }
            // 1: right-click a card -> context menu
            1 => {
                if f == 0 {
                    if let Some(p) = card_center(w, &k.qa_id) {
                        k.click(p, PointerButton::Secondary);
                    }
                    return Step::Wait;
                }
                if f < 6 {
                    return Step::Wait;
                }
                k.shot(ctx, "02_context_menu");
                k.key(Key::Escape, Modifiers::NONE);
                Step::Next
            }
            // 2: open the QA copy with a real click
            2 => {
                if f == 0 {
                    match card_center(w, &k.qa_id) {
                        Some(p) => k.click(p, PointerButton::Primary),
                        None => {
                            k.ok("open clip", false, "QA card not visible");
                            return Step::Next;
                        }
                    }
                    return Step::Wait;
                }
                if f < 8 {
                    return Step::Wait;
                }
                let opened = w.st.pid() == Some(k.qa_id.as_str());
                k.ok(
                    "open clip",
                    opened && w.page == TRIM && w.st.ev.is_some(),
                    format!(
                        "page {} · pid {:?} · {} kills · {} parts",
                        w.page,
                        w.st.pid(),
                        w.st.ev.as_ref().map(|e| e.kills.len()).unwrap_or(0),
                        w.st.ed.parts.len()
                    ),
                );
                if !w.dialogs.is_empty() {
                    k.shot(ctx, "03a_who_are_you_auto");
                }
                Step::Next
            }
            // 3: close an automatic who-are-you; wait for the player to show frames
            3 => {
                if f == 2 && !w.dialogs.is_empty() {
                    k.key(Key::Escape, Modifiers::NONE);
                }
                if w.trim.player.duration() > 0.0 && w.trim.player.frames_rendered > 0 && f > 10 {
                    k.ok(
                        "player loads",
                        true,
                        format!("duration {:.2} s, {} frames rendered", w.trim.player.duration(), w.trim.player.frames_rendered),
                    );
                    k.shot(ctx, "03_trim_loaded");
                    k.pos0 = w.trim.player.pos();
                    k.frames0 = w.trim.player.frames_rendered;
                    w.trim.player.play();
                    return Step::Next;
                }
                if k.elapsed() > Duration::from_secs(15) {
                    k.ok("player loads", false, format!("info: {:?}", w.trim.player.info()));
                    k.shot(ctx, "03_trim_failed");
                    return Step::Next;
                }
                Step::Wait
            }
            // 4: playing: frames advance, position moves
            4 => {
                if k.elapsed() < Duration::from_millis(2000) {
                    return Step::Wait;
                }
                let (p1, fr1) = (w.trim.player.pos(), w.trim.player.frames_rendered);
                k.ok(
                    "player plays",
                    p1 > k.pos0 + 0.5 && fr1 > k.frames0 + 20,
                    format!("position {:.2} -> {:.2} s, frames {} -> {}", k.pos0, p1, k.frames0, fr1),
                );
                k.shot(ctx, "04_trim_playing");
                w.trim.player.pause();
                Step::Next
            }
            // 5: an edit, then undo with the top-bar button and redo with Ctrl+Y
            5 => {
                if f == 0 {
                    k.rows0 = w.st.ed.killfeed.rows;
                    w.st.ed.killfeed.rows = if k.rows0 == 2 { 3 } else { 2 };
                    w.st.touch();
                    return Step::Wait;
                }
                if f == 2 {
                    let grouping = w.st.undo_label();
                    k.ok("undo grouping", grouping == "last change", format!("undo label while grouping: {grouping:?}"));
                }
                if k.elapsed() < Duration::from_millis(900) {
                    return Step::Wait;
                }
                let lbl = w.st.undo_label();
                k.ok("undo step recorded", lbl == "kill feed change" && w.st.can_undo(), format!("undo label {lbl:?}"));
                k.shot(ctx, "05_edit_recorded");
                let r = w.undo_rect;
                k.click(r.center(), PointerButton::Primary);
                Step::Next
            }
            6 => {
                if f < 4 {
                    return Step::Wait;
                }
                let rows = w.st.ed.killfeed.rows;
                k.ok(
                    "undo via button",
                    rows == k.rows0 && w.toast == "Undid kill feed change",
                    format!("rows {rows} (was {}), toast {:?}", k.rows0, w.toast),
                );
                let disk = vl_core::projects::open_dir(&k.qa_dir).map(|h| h.edits().killfeed.rows).unwrap_or(-1);
                k.ok("undo written to disk", disk == k.rows0, format!("edits.json rows {disk}"));
                k.shot(ctx, "06_undone");
                k.key(Key::Y, Modifiers::COMMAND);
                Step::Next
            }
            7 => {
                if f < 4 {
                    return Step::Wait;
                }
                let rows = w.st.ed.killfeed.rows;
                k.ok(
                    "redo via Ctrl+Y",
                    rows != k.rows0 && w.toast == "Redid kill feed change",
                    format!("rows {rows}, toast {:?}", w.toast),
                );
                k.key(Key::Z, Modifiers::COMMAND);
                Step::Next
            }
            8 => {
                if f < 4 {
                    return Step::Wait;
                }
                k.ok("undo via Ctrl+Z", w.st.ed.killfeed.rows == k.rows0, format!("rows {}", w.st.ed.killfeed.rows));
                // settings dialog through the top-bar button
                let r = w.settings_rect;
                k.click(r.center(), PointerButton::Primary);
                Step::Next
            }
            9 => {
                if f < 6 {
                    return Step::Wait;
                }
                k.ok(
                    "settings dialog",
                    w.dialogs.last().map(|d| d.title() == "Settings").unwrap_or(false),
                    format!("{} dialog(s)", w.dialogs.len()),
                );
                k.shot(ctx, "07_settings");
                k.key(Key::Escape, Modifiers::NONE);
                Step::Next
            }
            10 => {
                if f < 3 {
                    return Step::Wait;
                }
                k.ok("settings cancelled", w.dialogs.is_empty(), "");
                w.shell.push(Action::WhoAreYou);
                Step::Next
            }
            11 => {
                if f < 6 {
                    return Step::Wait;
                }
                k.ok("who are you dialog", w.dialogs.last().map(|d| d.title() == "Who are you").unwrap_or(false), "");
                k.shot(ctx, "08_who_are_you");
                k.key(Key::Escape, Modifiers::NONE);
                Step::Next
            }
            12 => {
                if f < 3 {
                    return Step::Wait;
                }
                w.dialogs.push(Dialog::fail(
                    "Export failed",
                    "The video could not be exported",
                    "render failed\n  caused by: ffmpeg exited with code 1",
                ));
                Step::Next
            }
            13 => {
                if f == 3 {
                    // press "Show details"
                    k.shot(ctx, "09_fail");
                }
                if f < 6 {
                    return Step::Wait;
                }
                k.key(Key::Enter, Modifiers::NONE);
                Step::Next
            }
            14 => {
                if f < 3 {
                    return Step::Wait;
                }
                k.ok("fail dialog closes with Enter", w.dialogs.is_empty(), "");
                w.shell.push(Action::Dialog(Dialog::Vocabulary(crate::dialogs::Vocabulary { text: crate::dialogs::DEFAULT_VOCAB.into() })));
                Step::Next
            }
            15 => {
                if f < 6 {
                    return Step::Wait;
                }
                k.shot(ctx, "10_vocabulary");
                k.key(Key::Escape, Modifiers::NONE);
                Step::Next
            }
            // 16..19: the other steps through the tabs
            16..=19 => {
                let target = [LAYOUT, SUBTITLES, EXPORT, TRIM][k.step - 16];
                if f == 0 {
                    let r = w.step_rects.get(target).copied();
                    if let Some(r) = r {
                        k.click(r.center(), PointerButton::Primary);
                    }
                    return Step::Wait;
                }
                if f < 6 {
                    return Step::Wait;
                }
                k.ok(&format!("tab {}", crate::pages::STEPS[target]), w.page == target, format!("page {}", w.page));
                k.shot(ctx, &format!("11_{}", crate::pages::STEPS[target].to_lowercase()));
                Step::Next
            }
            // 20: Ctrl+1 back to Clips; the open clip carries the accent outline
            20 => {
                if f == 0 {
                    k.key(Key::Num1, Modifiers::COMMAND);
                    return Step::Wait;
                }
                if f < 6 {
                    return Step::Wait;
                }
                k.ok(
                    "Ctrl+1 -> Clips",
                    w.page == CLIPS && w.library.current_pid.as_deref() == Some(k.qa_id.as_str()),
                    format!("page {}", w.page),
                );
                k.shot(ctx, "12_clips_current");
                Step::Next
            }
            // 21: open a project without edits.json read-only (switching clips)
            21 => {
                let Some(other) = k.other_dir.clone() else { return Step::Next };
                if f == 0 {
                    w.shell.push(Action::OpenProject(other.clone()));
                    return Step::Wait;
                }
                if f == 3 && !w.dialogs.is_empty() {
                    k.key(Key::Escape, Modifiers::NONE);
                }
                if f < 10 {
                    return Step::Wait;
                }
                let pid = other.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                k.ok(
                    "open a project without edits.json read-only",
                    w.st.pid() == Some(pid.as_str()) && w.st.read_only && w.page == TRIM,
                    format!("pid {:?}, read_only {}", w.st.pid(), w.st.read_only),
                );
                Step::Next
            }
            22 => {
                if w.trim.player.duration() > 0.0 && f > 20 || k.elapsed() > Duration::from_secs(10) {
                    k.shot(ctx, "13_no_edits_project");
                    w.trim.stop_players();
                    return Step::Next;
                }
                Step::Wait
            }
            // 23: back to the QA copy, Subtitles page
            23 => {
                if f == 0 {
                    w.shell.push(Action::OpenProject(k.qa_dir.clone()));
                    return Step::Wait;
                }
                if f == 3 && !w.dialogs.is_empty() {
                    k.key(Key::Escape, Modifiers::NONE);
                }
                if f < 10 {
                    return Step::Wait;
                }
                w.go(SUBTITLES);
                w.shell.push(Action::TranscribeConfirmed { whole: false, only: None });
                Step::Next
            }
            // 24: a transcription job reports progress; cancel kills the worker tree
            24 => {
                let p = w.subs.progress.unwrap_or(0.0);
                if p > 0.0 || k.elapsed() > Duration::from_secs(30) {
                    k.ok("transcribe job progress", p > 0.0, format!("{p:.2} {:?}", w.subs.status));
                    k.shot(ctx, "14_transcribing");
                    k.frames0 = w.job.as_ref().map(|j| j.pid() as u64).unwrap_or(0);
                    w.shell.push(Action::CancelJob);
                    return Step::Next;
                }
                Step::Wait
            }
            25 => {
                if w.job.is_some() && k.elapsed() < Duration::from_secs(10) {
                    return Step::Wait;
                }
                let alive = crate::sys::quiet_command("tasklist")
                    .args(["/FI", &format!("PID eq {}", k.frames0), "/NH"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).contains(&k.frames0.to_string()))
                    .unwrap_or(true);
                k.ok(
                    "cancel kills the worker",
                    w.job.is_none() && w.subs.status == "Cancelled" && !alive,
                    format!("status {:?}, worker {} alive {alive}", w.subs.status, k.frames0),
                );
                w.shell.push(Action::TranscribeConfirmed { whole: false, only: None });
                Step::Next
            }
            // 26: a full transcription: the transcript is loaded into the state, captions counted
            26 => {
                if w.job.is_some() && k.elapsed() < Duration::from_secs(180) {
                    return Step::Wait;
                }
                let n = w.st.tr.as_ref().map(|t| t.segments.len()).unwrap_or(0);
                k.ok(
                    "transcribe job finishes",
                    n > 0 && (w.subs.status.ends_with("captions") || w.subs.status.starts_with("Model ")) && w.st.ed.subtitles.enabled,
                    format!("{n} segments, status {:?}", w.subs.status),
                );
                k.shot(ctx, "15_transcribed");
                Step::Next
            }
            _ => {
                if !k.done {
                    k.done = true;
                }
                Step::Wait
            }
        }
    }
}

impl eframe::App for CheckApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.win.frame_ui(ui, frame);
        if !self.chk.inject.is_empty() {
            ctx.request_repaint(); // input still being delivered
        } else if !self.chk.done {
            match self.script(&ctx) {
                Step::Next => {
                    self.chk.step += 1;
                    self.chk.frames_in_step = 0;
                    self.chk.step_start = Instant::now();
                }
                Step::Wait => self.chk.frames_in_step += 1,
            }
            ctx.request_repaint();
        } else if self.chk.frames_in_step < 1000 {
            // let the last screenshots land, then close
            self.chk.frames_in_step += 1;
            if self.chk.frames_in_step > 8 {
                self.chk.frames_in_step = 1000;
                self.app.win.shell.push(Action::Quit);
            }
            ctx.request_repaint();
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some((mut evs, m)) = self.chk.inject.pop_front() {
            raw.events.append(&mut evs);
            raw.focused = true;
            self.chk.mods = m;
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

/// Pick a ready project with a playable source and kills, preferring a short one.
/// The newest analysed project of a test video (by the source file's name, e.g. "example_clip-short-4-3"):
/// the checks follow the user's projects instead of hard-coding project ids that get deleted.
pub fn find_project(source_stem: &str) -> Option<PathBuf> {
    vl_core::projects::list_all()
        .into_iter()
        .filter(|h| !h.project.id.starts_with("rustqa_") && h.project.status == "ready" && h.source_ok() && h.events().is_some())
        .filter(|h| Path::new(&h.project.source.path).file_stem().map(|s| s.to_string_lossy() == source_stem).unwrap_or(false))
        .max_by(|a, b| a.project.id.cmp(&b.project.id))
        .map(|h| h.dir)
}

fn pick_source_project() -> Option<vl_core::projects::ProjectHandle> {
    let mut all: Vec<_> = vl_core::projects::list_all()
        .into_iter()
        .filter(|h| !h.project.id.starts_with("rustqa_") && h.project.status == "ready" && h.source_ok() && h.events().is_some())
        .collect();
    all.sort_by(|a, b| a.project.source.duration.total_cmp(&b.project.source.duration));
    all.into_iter().next()
}

pub fn run(args: &[String]) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("uicheck", false);
    let out = args.first().map(PathBuf::from).unwrap_or_else(|| paths::app_root().join("scratch").join("vl-app").join("uicheck"));
    let _ = std::fs::create_dir_all(&out);
    if vl_core::projects::list_all().is_empty() {
        return run_empty(out);
    }
    let Some(src) = pick_source_project() else {
        eprintln!("no ready project with a source video in {}", paths::projects_dir().display());
        return 2;
    };
    let before = fingerprint();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    // scratch copy that may be written
    let qa_id = format!("rustqa_{}_{}", chrono::Local::now().format("%H%M%S"), src.project.id.chars().take(40).collect::<String>());
    let qa_dir = paths::projects_dir().join(&qa_id);
    let _cleanup = Cleanup(qa_dir.clone());
    if let Err(e) = copy_dir(&src.dir, &qa_dir) {
        eprintln!("copy failed: {e}");
        return 2;
    }
    if let Ok(mut h) = vl_core::projects::open_dir(&qa_dir) {
        h.project.id = qa_id.clone();
        h.project.name = format!("QA copy of {}", h.project.name);
        let _ = h.save();
    }
    // a project without edits.json is the strictest read-only test (opening it would create one)
    let other = vl_core::projects::list_all()
        .into_iter()
        .filter(|h| !h.project.id.starts_with("rustqa_") && h.project.status == "ready" && h.source_ok())
        .max_by_key(|h| (!h.edits_path().exists(), h.project.id != src.project.id))
        .map(|h| h.dir);
    println!("QA copy: {}\nread-only project: {:?}\nscreenshots: {}", qa_dir.display(), other, out.display());
    let opts = Options { read_only: true, writable_prefix: Some("rustqa_".into()), no_resume: true, no_first_run: true };
    let results = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let res2 = results.clone();
    let qa2 = qa_dir.clone();
    let run = eframe::run_native(
        "ValoLights UI check",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            let win = MainWindow::new(app::gl_env(cc), opts);
            Ok(Box::new(CheckOwner {
                inner: CheckApp {
                    app: ValoApp { win },
                    chk: Check {
                        out,
                        qa_dir: qa2,
                        qa_id,
                        other_dir: other,
                        inject: Default::default(),
                        mods: Modifiers::NONE,
                        step: 0,
                        step_start: Instant::now(),
                        frames_in_step: 0,
                        results: vec![],
                        shots: vec![],
                        pos0: 0.0,
                        frames0: 0,
                        rows0: 0,
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
    println!("[{}] user data untouched: {line} {:?}", if changed.is_empty() { " OK " } else { "FAIL" }, changed);
    results.push(("user data untouched".into(), changed.is_empty(), format!("{line} {changed:?}")));
    results.push(("QA copy removed".into(), !qa_dir.exists(), qa_dir.display().to_string()));
    let fails = results.iter().filter(|r| !r.1).count();
    let report: String = results.iter().map(|(n, ok, d)| format!("[{}] {n}: {d}\n", if *ok { " OK " } else { "FAIL" })).collect();
    let report = format!("{report}\nRESULT: {} ({} checks, {fails} failed)\n", if fails == 0 { "PASS" } else { "FAIL" }, results.len());
    let _ = std::fs::write(paths::app_root().join("scratch").join("vl-app").join("uicheck_report.txt"), &report);
    println!("{report}");
    i32::from(fails > 0)
}

/// Owns the check app and hands the results out when the window closes.
struct CheckOwner {
    inner: CheckApp,
    sink: std::sync::Arc<std::sync::Mutex<Vec<(String, bool, String)>>>,
}

impl eframe::App for CheckOwner {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui, frame);
        if self.inner.chk.done {
            *self.sink.lock().unwrap() = self.inner.chk.results.clone();
        }
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

/// No projects at all (run with VALOLIGHTS_ROOT pointing at an empty scratch root): the Clips page shows
/// the drop zone; hovering files changes its text.
fn run_empty(out: PathBuf) -> i32 {
    struct Empty {
        app: ValoApp,
        out: PathBuf,
        frame: u32,
        ok: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
    }
    impl eframe::App for Empty {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            self.app.win.frame_ui(ui, frame);
            self.frame += 1;
            let shot = |name: &str| {
                let path = self.out.join(format!("{name}.png"));
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
                    let _ = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8);
                });
            };
            match self.frame {
                10 => {
                    *self.ok.lock().unwrap() = Some(self.app.win.library.cards.is_empty() && self.app.win.page == CLIPS);
                    shot("00_empty_clips");
                }
                18 => shot("00_empty_drag_hover"),
                24 => self.app.win.shell.push(Action::Quit),
                _ => {}
            }
            ctx.request_repaint();
        }
        fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
            if (14..=19).contains(&self.frame) {
                for f in ["a.mp4", "b.mkv"] {
                    raw.hovered_files.push(egui::HoveredFile { path: Some(PathBuf::from(f)), ..Default::default() });
                }
            }
        }
    }
    let ok = std::sync::Arc::new(std::sync::Mutex::new(None));
    let ok2 = ok.clone();
    let opts = Options { read_only: true, writable_prefix: None, no_resume: true, no_first_run: true };
    let _ = eframe::run_native(
        "ValoLights UI check",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            Ok(Box::new(Empty { app: ValoApp { win: MainWindow::new(app::gl_env(cc), opts) }, out, frame: 0, ok: ok2 }))
        }),
    );
    let pass = ok.lock().unwrap().unwrap_or(false);
    println!("[{}] empty Clips shows the drop zone\n\nRESULT: {}", if pass { " OK " } else { "FAIL" }, if pass { "PASS" } else { "FAIL" });
    i32::from(!pass)
}
