//! Export: render the final video and editor marker files.
//!
//! The page never starts the job itself: it pushes [`Action::Render`] with the in-progress path
//! (`<name>.rendering.mp4`) and the options, the main window runs the worker and reports back through
//! [`Page::on_job`]. When the render ends the page renames the file to its final name, or removes the
//! partial file after a cancel / failure. Questions ("Replace video") are app dialogs whose confirm
//! button pushes [`Action::ExportConfirmed`], which the main window hands back to [`ExportPage::confirmed`].
use super::{Action, JobNotice, Page, PageCtx, Shell};
use crate::backend;
use crate::dialogs::Dialog;
use crate::state::{AppState, StateEvent};
use crate::theme::{self, c, Role, Weight, S1, S2, S3, S4, S5, S6, S8};
use crate::widgets::ui::{self, Button};
use egui::{vec2, Align, Align2, Color32, Layout, Rect, Sense, Stroke};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vl_core::{config, paths};

/// renders go to "<name>.rendering.mp4" and are renamed when they finish
const PART: &str = ".rendering";
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4",
    "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
pub const RESOLUTIONS: [(i32, &str); 4] = [(720, "720p"), (1080, "1080p"), (1440, "1440p"), (2160, "2160p")];
pub const FRAME_RATES: [(i32, &str); 2] = [(60, "60 fps"), (30, "30 fps")];
pub const QUALITIES: [(&str, &str); 3] = [("high", "High"), ("medium", "Balanced"), ("small", "Small file")];
/// (extension, title) of the editor exports, in button order
pub const EDITOR_FILES: [(&str, &str); 3] = [
    (".xml", "Premiere Pro sequence with kill markers"),
    (".edl", "DaVinci Resolve timeline with kill markers"),
    (".json", "Everything ValoLights detected"),
];

fn illegal(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20
}

/// A Windows-safe file stem: illegal characters become _, letters in any language are kept.
pub fn safe_stem(name: &str) -> String {
    let mut s = name.trim().to_string();
    if s.to_lowercase().ends_with(".mp4") {
        let n = s.chars().count();
        s = s.chars().take(n - 4).collect();
    }
    let s: String = s.chars().map(|c| if illegal(c) { '_' } else { c }).collect();
    let s: String = s.trim().trim_end_matches(['.', ' ']).chars().take(150).collect();
    let mut s = s.trim_end_matches(['.', ' ']).to_string();
    let first = s.split('.').next().unwrap_or("").to_uppercase();
    if RESERVED.contains(&first.as_str()) {
        s = format!("_{s}");
    }
    s
}

/// "12.3 MB" / "1.05 GB".
pub fn fmt_size(n: u64) -> String {
    if (n as f64) < 1e9 {
        format!("{:.1} MB", n as f64 / 1e6)
    } else {
        format!("{:.2} GB", n as f64 / 1e9)
    }
}

/// Path relative to the app folder when inside it.
pub fn rel(p: &Path) -> String {
    match p.strip_prefix(paths::app_root()) {
        Ok(r) => r.display().to_string(),
        Err(_) => p.display().to_string(),
    }
}

/// `{dur:.1f} s` below a minute, else "m:ss min".
fn fmt_len(dur: f64) -> String {
    if dur < 60.0 {
        format!("{dur:.1} s")
    } else {
        format!("{}:{:02} min", (dur / 60.0).floor() as i64, (dur % 60.0) as i64)
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n != 1 { "s" } else { "" })
}

/// Explorer with the file selected.
pub fn show_in_folder(p: &Path) {
    #[cfg(windows)]
    {
        if p.exists() {
            use std::os::windows::process::CommandExt;
            let _ = crate::sys::quiet_command("explorer").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
            return;
        }
    }
    crate::sys::open_path(if p.is_file() { p.parent().unwrap_or(p) } else { p });
}

/// Delete a file; while the encoder still holds it, try again every 500 ms for a minute.
fn remove_later(p: PathBuf) {
    if std::fs::remove_file(&p).is_ok() || !p.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..120 {
            std::thread::sleep(Duration::from_millis(500));
            if std::fs::remove_file(&p).is_ok() || !p.exists() {
                return;
            }
        }
    });
}

pub struct ExportPage {
    /// last progress of the render: Some(p) while running
    pub progress: Option<f64>,
    /// the render status line under the buttons (empty = hidden)
    pub status: String,
    // ---- inputs
    /// the file name field
    pub name: String,
    /// file name typed per project (kept while the app runs)
    names: HashMap<String, String>,
    pub resolution: i32,
    pub fps: i32,
    pub quality: String,
    pub encoder: String,
    // ---- derived text (refreshed every frame)
    pub summary: String,
    pub extras: String,
    pub name_hint: String,
    pub name_bad: bool,
    pub note: String,
    pub render_tip: String,
    pub can_render: bool,
    pub can_export: bool,
    // ---- render
    /// project whose name/status the page shows
    pid: Option<String>,
    /// the last finished video
    pub last_out: Option<PathBuf>,
    /// (final path, in-progress path) of the running render
    pub pending: Option<(PathBuf, PathBuf)>,
    pub running: bool,
    /// Render was asked for this frame; the window answers with a progress notice or refuses (busy)
    requested: bool,
    pub cancel_enabled: bool,
    pub show_result: bool,
    nvenc: Option<bool>,
    nvenc_check: Option<Arc<Mutex<Option<bool>>>>,
    focus_name: bool,
    // ---- editor exports
    pub saved_text: Option<String>,
    saved_file: Option<PathBuf>,
    // ---- rects of the last frame (UI check)
    pub name_rect: Rect,
    pub render_rect: Rect,
    pub cancel_rect: Rect,
    pub open_rect: Rect,
    pub play_rect: Rect,
    pub save_rects: [Rect; 3],
    pub combo_rects: [Rect; 4],
    pub column_rect: Rect,
    card_rect: Rect,
}

impl ExportPage {
    pub fn new(_shell: &Shell) -> Self {
        let mut p = ExportPage {
            progress: None,
            status: String::new(),
            name: String::new(),
            names: HashMap::new(),
            resolution: 1080,
            fps: 60,
            quality: "high".into(),
            encoder: "auto".into(),
            summary: "No clip open".into(),
            extras: String::new(),
            name_hint: String::new(),
            name_bad: false,
            note: String::new(),
            render_tip: String::new(),
            can_render: false,
            can_export: false,
            pid: None,
            last_out: None,
            pending: None,
            running: false,
            requested: false,
            cancel_enabled: true,
            show_result: false,
            nvenc: None,
            nvenc_check: None,
            focus_name: false,
            saved_text: None,
            saved_file: None,
            name_rect: Rect::NOTHING,
            render_rect: Rect::NOTHING,
            cancel_rect: Rect::NOTHING,
            open_rect: Rect::NOTHING,
            play_rect: Rect::NOTHING,
            save_rects: [Rect::NOTHING; 3],
            combo_rects: [Rect::NOTHING; 4],
            column_rect: Rect::NOTHING,
            card_rect: Rect::NOTHING,
        };
        p.load_options();
        p
    }

    // ------------------------------------------------------------------ state
    fn load_options(&mut self) {
        let cfg = config::load().render;
        let pick_i = |v: i32, opts: &[(i32, &str)]| if opts.iter().any(|o| o.0 == v) { v } else { opts[0].0 };
        let pick_s = |v: &str, opts: &[(&str, &str)]| if opts.iter().any(|o| o.0 == v) { v.to_string() } else { opts[0].0.to_string() };
        self.resolution = pick_i(cfg.resolution, &RESOLUTIONS);
        self.fps = pick_i(cfg.fps, &FRAME_RATES);
        self.quality = pick_s(&cfg.quality, &QUALITIES);
        self.encoder = pick_s(&cfg.encoder, &[("auto", ""), ("nvenc", ""), ("x264", "")]);
    }

    /// Remembered for the next render, other projects and the next session.
    fn option_changed(&mut self, st: &AppState) {
        if st.read_only {
            return;
        }
        let mut cfg = config::load();
        cfg.render.resolution = self.resolution;
        cfg.render.fps = self.fps;
        cfg.render.quality = self.quality.clone();
        cfg.render.encoder = self.encoder.clone();
        let _ = cfg.save();
    }

    /// Change one of the four choices like its drop-down does (UI check): 0 resolution, 1 frame rate,
    /// 2 quality, 3 encoder.
    pub fn set_option(&mut self, i: usize, v: &str, st: &AppState) {
        match i {
            0 => self.resolution = v.parse().unwrap_or(self.resolution),
            1 => self.fps = v.parse().unwrap_or(self.fps),
            2 => self.quality = v.to_string(),
            _ => self.encoder = v.to_string(),
        }
        self.option_changed(st);
        self.refresh(st);
    }

    fn encoders(&self) -> [(&'static str, &'static str); 3] {
        let nv = if self.nvenc == Some(false) { "NVIDIA GPU (not found)" } else { "NVIDIA GPU" };
        [("auto", "Automatic"), ("nvenc", nv), ("x264", "CPU")]
    }

    pub fn stem(&self) -> String {
        safe_stem(&self.name)
    }

    pub fn options(&self) -> serde_json::Value {
        json!({"resolution": self.resolution, "fps": self.fps, "quality": self.quality, "encoder": self.encoder})
    }

    fn out_dir(st: &AppState, create: bool) -> PathBuf {
        let d = paths::exports_dir().join(st.h.as_ref().map(|h| h.project.id.as_str()).unwrap_or("misc"));
        if create {
            let _ = std::fs::create_dir_all(&d);
        }
        d
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = text.into();
    }

    /// Refresh the page: summary, extras, notes, project switch.
    pub fn refresh(&mut self, st: &AppState) {
        let (Some(h), Some(_)) = (st.h.as_ref(), st.ev.as_ref()) else {
            self.pid = None;
            self.summary = "No clip open".into();
            self.extras.clear();
            self.note.clear();
            self.can_render = false;
            self.can_export = false;
            return;
        };
        if self.pid.as_deref() != Some(h.project.id.as_str()) {
            let pid = h.project.id.clone();
            self.name = self.names.get(&pid).cloned().unwrap_or_else(|| {
                let s = safe_stem(&format!("{}_highlights", h.project.name));
                if s.is_empty() {
                    "highlights".into()
                } else {
                    s
                }
            });
            self.pid = Some(pid);
            self.saved_text = None;
            self.saved_file = None;
            if let (true, Some((fin, _))) = (self.running, &self.pending) {
                let name = fin.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                self.status = format!("Still rendering {name} from another clip. It is saved in {}", rel(fin.parent().unwrap_or(fin)));
            } else {
                self.show_result = false;
                self.status.clear();
            }
            self.clean_leftovers(st);
        }
        let ed = &st.ed;
        let (n, dur, td) = backend::edit_list_info(&ed.parts, &ed.effects.transition);
        let times: Vec<f64> = st.my_kills(false).iter().map(|k| k.t).collect();
        let kills = backend::count_in_output(&ed.parts, &ed.effects.transition, &times);
        let aspect = backend::output_aspect(ed, h.project.game_area.as_ref());
        let (w, hh) = backend::canvas_size(&aspect, self.resolution as i64);
        self.summary = format!(
            "{aspect} \u{b7} {w} \u{d7} {hh} \u{b7} {} fps \u{b7} {} \u{b7} {} \u{b7} {}",
            self.fps,
            fmt_len(dur),
            plural(n, "part"),
            plural(kills, "kill")
        );
        let mut extras: Vec<String> = vec![];
        if ed.subtitles.enabled && st.tr.as_ref().map(|t| !t.segments.is_empty()).unwrap_or(false) {
            extras.push("subtitles".into());
        }
        if ed.music.enabled && !ed.music.path.is_empty() {
            extras.push(if Path::new(&ed.music.path).exists() { "music".into() } else { "no music (the song file is missing)".into() });
        }
        if ed.effects.zoom {
            extras.push("kill zoom".into());
        }
        if ed.effects.flash_on_kill {
            extras.push("kill flash".into());
        }
        if ed.effects.transition != "none" && td != 0.0 {
            extras.push(format!("{} transitions", ed.effects.transition));
        }
        if ed.effects.look != "none" {
            extras.push(format!("{} look", ed.effects.look));
        }
        if ed.effects.super_smooth {
            extras.push("super smooth (slower render)".into());
        }
        self.extras = if extras.is_empty() { String::new() } else { format!("With {}", extras.join(", ")) };
        let mut notes: Vec<String> = vec![];
        let src_h = h.project.source.height as i32;
        if src_h > 0 && self.resolution > src_h {
            notes.push(format!("{}p is above your recording ({src_h}p), so the video gets bigger but not sharper", self.resolution));
        }
        if self.nvenc == Some(false) && self.encoder == "nvenc" {
            notes.push("No working NVIDIA encoder was found on this PC, the CPU is used instead".into());
        }
        self.note = notes.join("\n");
        self.update_name(st);
    }

    /// Update the hint under the file name, Render / Save availability, the empty-cut status.
    fn update_name(&mut self, st: &AppState) {
        if st.h.is_none() {
            return;
        }
        let stem = self.stem();
        let raw = self.name.trim();
        let empty_cut = st.ed.parts.is_empty();
        self.name_bad = stem.is_empty();
        if stem.is_empty() {
            self.name_hint = "Type a file name".into();
        } else {
            let fin = Self::out_dir(st, false).join(format!("{stem}.mp4"));
            let mut text = format!("Saved as {stem}.mp4 in {}", rel(fin.parent().unwrap()));
            let typed = raw.strip_suffix(".mp4").unwrap_or(raw);
            let typed = typed.strip_suffix(".MP4").unwrap_or(typed);
            if typed.trim() != stem {
                text += ". Characters that can't be in a file name were replaced";
            }
            if fin.exists() {
                text += ". A video with this name already exists";
            }
            self.name_hint = text;
        }
        self.render_tip = if empty_cut { "Nothing on the timeline. Keep at least one part in Trim".into() } else { String::new() };
        let ok = !stem.is_empty() && !empty_cut;
        self.can_render = ok && !self.running;
        self.can_export = ok;
        if empty_cut && !self.running {
            self.set_status("Nothing on the timeline. Keep at least one part in Trim to render");
        } else if !self.running && self.status.starts_with("Nothing on the timeline") {
            self.set_status("");
        }
    }

    /// Remove half-written renders left by a crash or a closed app.
    fn clean_leftovers(&self, st: &AppState) {
        if self.running {
            return;
        }
        let d = Self::out_dir(st, false);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(&format!("{PART}.mp4")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    // ------------------------------------------------------------------ render
    /// Start the render. `confirmed` = the user already agreed to replace an existing video.
    pub fn render(&mut self, st: &mut AppState, shell: &mut Shell, confirmed: bool) {
        if self.running || self.requested {
            return;
        }
        if st.h.is_none() || st.ed.parts.is_empty() {
            shell.dialog(Dialog::message("Export", "Nothing to render. Keep at least one part on the timeline"));
            return;
        }
        let stem = self.stem();
        if stem.is_empty() {
            self.focus_name = true;
            self.update_name(st);
            return;
        }
        let d = Self::out_dir(st, true);
        let fin = d.join(format!("{stem}.mp4"));
        if fin.exists() && !confirmed {
            shell.dialog(Dialog::confirm(
                "Replace video",
                &format!("{stem}.mp4 already exists in {}.\n\nReplace it?", rel(&d)),
                "Replace",
                false,
                vec![Action::ExportConfirmed("render".into())],
            ));
            return;
        }
        let tmp = d.join(format!("{stem}{PART}.mp4"));
        let _ = std::fs::remove_file(&tmp);
        self.show_result = false;
        self.pending = Some((fin, tmp.clone()));
        self.requested = true;
        shell.push(Action::Render { out: tmp.to_string_lossy().into_owned(), options: self.options() });
    }

    /// Render progress (`p` = None when it ended).
    fn set_progress(&mut self, p: Option<f64>, msg: &str, st: &AppState) {
        self.requested = false;
        if let Some(p) = p {
            self.running = true;
            self.progress = Some(p);
            self.can_render = false;
            if let (Some((fin, _)), Some(h)) = (&self.pending, &st.h) {
                if fin.parent().and_then(Path::file_name).map(|n| n.to_string_lossy() != h.project.id.as_str()).unwrap_or(false) {
                    return; // keep the "still rendering ... from another clip" line
                }
            }
            let name = self.pending.as_ref().and_then(|(f, _)| f.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            self.status = if name.is_empty() { msg.to_string() } else { format!("{name} \u{b7} {msg}") };
            return;
        }
        let was = self.running;
        self.running = false;
        self.progress = None;
        self.cancel_enabled = true;
        if was && self.pending.is_some() {
            self.finish(msg);
        } else {
            self.set_status(msg);
        }
        self.update_name(st);
    }

    /// The render ended: rename the finished file, or remove the partial one.
    fn finish(&mut self, msg: &str) {
        let (fin, tmp) = self.pending.take().unwrap();
        let cancelled = msg.trim().to_lowercase().starts_with("cancel");
        let failed = cancelled || msg.to_lowercase().starts_with("render failed");
        let size_ok = std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false);
        if !failed && size_ok {
            let mut target = fin.clone();
            if std::fs::rename(&tmp, &fin).is_err() {
                // the old video is open in a player: keep both
                let stem = fin.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let mut k = 2;
                loop {
                    target = fin.with_file_name(format!("{stem} ({k}).mp4"));
                    if !target.exists() {
                        break;
                    }
                    k += 1;
                }
                if std::fs::rename(&tmp, &target).is_err() {
                    target = tmp.clone();
                }
            }
            let tname = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let fname = fin.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let note = if target == fin { String::new() } else { format!(" (could not replace {fname}, it may be open in another app)") };
            let size = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
            self.status = format!("Saved {tname} \u{b7} {} \u{b7} {}{note}", fmt_size(size), rel(target.parent().unwrap_or(&target)));
            self.last_out = Some(target);
            self.show_result = true;
            return;
        }
        remove_later(tmp);
        self.show_result = false;
        if cancelled {
            self.set_status("Cancelled. Nothing was saved");
        } else {
            self.set_status(format!("Render failed. The full log is in {}", rel(&paths::logs_dir())));
        }
    }

    fn cancel(&mut self, shell: &mut Shell) {
        if !self.running {
            return;
        }
        self.cancel_enabled = false;
        self.set_status("Cancelling\u{2026}");
        shell.push(Action::CancelJob);
    }

    fn play(&mut self) {
        match &self.last_out {
            Some(p) if p.exists() => crate::sys::open_path(p),
            _ => {
                self.set_status("The video was moved or deleted");
                self.show_result = false;
            }
        }
    }

    fn open_folder(&mut self, st: &AppState) {
        if st.h.is_none() {
            return;
        }
        let d = Self::out_dir(st, true);
        if crate::sys::quiet_command("explorer").arg(&d).spawn().is_err() {
            self.set_status(format!("Could not open {}", d.display()));
        }
    }

    // ------------------------------------------------------------------ editor exports
    /// Write an editor file (`ext`). `confirmed` = the user already agreed to replace existing files.
    pub fn export(&mut self, ext: &str, st: &mut AppState, shell: &mut Shell, confirmed: bool) {
        if st.h.is_none() || st.ev.is_none() {
            return;
        }
        if st.ed.parts.is_empty() {
            self.saved("Nothing on the timeline. Keep at least one part in Trim", None);
            return;
        }
        let stem = self.stem();
        if stem.is_empty() {
            self.focus_name = true;
            self.update_name(st);
            return;
        }
        let d = Self::out_dir(st, true);
        let out = d.join(format!("{stem}{ext}"));
        let extra = (ext == ".edl").then(|| d.join(format!("{stem}_markers.edl")));
        let existing: Vec<String> = [Some(&out), extra.as_ref()]
            .into_iter()
            .flatten()
            .filter(|p| p.exists())
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        if !existing.is_empty() && !confirmed {
            shell.dialog(Dialog::confirm(
                "Replace file",
                &format!(
                    "{} already exist{} in {}.\n\nReplace?",
                    existing.join(" and "),
                    if existing.len() == 1 { "s" } else { "" },
                    rel(&d)
                ),
                "Replace",
                false,
                vec![Action::ExportConfirmed(ext.to_string())],
            ));
            return;
        }
        st.save_now(); // exporters read the saved edit
        let name = format!("{stem}{ext}");
        if let Err(e) = backend::export_for_editors(ext, st.h.as_ref().unwrap(), &out) {
            self.saved(&format!("Could not save {name}"), None);
            shell.dialog(Dialog::message("Export failed", &format!("Could not save {name}\n\n{e}")));
            return;
        }
        let names = match &extra {
            Some(x) if x.exists() => format!("{name} and {}", x.file_name().unwrap().to_string_lossy()),
            _ => name,
        };
        self.saved(&format!("Saved {names} \u{b7} {}", rel(&d)), Some(out));
    }

    fn saved(&mut self, text: &str, path: Option<PathBuf>) {
        self.saved_file = path;
        self.saved_text = Some(text.to_string());
    }

    /// The confirm button of a "Replace" question (pushed as `Action::ExportConfirmed(tag)`).
    pub fn confirmed(&mut self, tag: &str, st: &mut AppState, shell: &mut Shell) {
        if tag == "render" {
            self.render(st, shell, true);
        } else {
            self.export(tag, st, shell, true);
        }
    }

    fn poll_nvenc(&mut self, ctx: &egui::Context) {
        if self.nvenc.is_none() && self.nvenc_check.is_none() {
            let slot = Arc::new(Mutex::new(None));
            let (s2, c2) = (slot.clone(), ctx.clone());
            std::thread::spawn(move || {
                let ok = vl_core::ffmpeg::nvenc_works();
                *s2.lock().unwrap() = Some(ok);
                c2.request_repaint();
            });
            self.nvenc_check = Some(slot);
        }
        let done = self.nvenc_check.as_ref().and_then(|slot| *slot.lock().unwrap());
        if let Some(ok) = done {
            self.nvenc = Some(ok);
            self.nvenc_check = None;
        }
    }

    /// For tests: pretend the NVIDIA check finished.
    pub fn apply_nvenc(&mut self, ok: bool) {
        self.nvenc = Some(ok);
        self.nvenc_check = None;
    }

    // ------------------------------------------------------------------ view
    fn form_label(ui: &mut egui::Ui, text: &str) {
        let (r, _) = ui.allocate_exact_size(vec2(96.0, 32.0), Sense::hover());
        let color = if ui.is_enabled() { c::SECONDARY } else { c::DISABLED };
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, text, theme::ui_font(13.0, Weight::Regular), color);
    }

    fn text(ui: &mut egui::Ui, text: &str, role: Role, color: Option<Color32>, selectable: bool) -> egui::Response {
        let col = color.unwrap_or(role.color());
        ui.add(egui::Label::new(egui::RichText::new(text).font(role.font()).color(col)).wrap().selectable(selectable))
    }

    fn video_card(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx) {
        let has = cx.st.h.is_some() && cx.st.ev.is_some();
        ui::section(ui, "Video");
        ui.add_space(S4);
        let field_w = (ui.available_width() - 96.0 - S4).max(120.0);
        let mut opts_changed = false;
        ui.spacing_mut().item_spacing = vec2(S4, 0.0);
        // File name
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            Self::form_label(ui, "File name");
            ui.allocate_ui_with_layout(vec2(field_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing = vec2(S2, 0.0);
                ui.horizontal(|ui| {
                    let suffix_w = ui::text_width(ui, ".mp4", &Role::Mono.font());
                    let w = (field_w - suffix_w - S2).max(60.0);
                    let r = ui.add_enabled_ui(has, |ui| ui::text_input(ui, &mut self.name, "Name of the video", w)).inner;
                    self.name_rect = r.rect;
                    if self.focus_name {
                        r.request_focus();
                        self.focus_name = false;
                    }
                    if r.changed() {
                        if let Some(pid) = &self.pid {
                            self.names.insert(pid.clone(), self.name.clone());
                        }
                        self.update_name(cx.st);
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.render(cx.st, cx.shell, false);
                    }
                    ui.allocate_ui_with_layout(vec2(suffix_w, 32.0), Layout::left_to_right(Align::Center), |ui| {
                        ui::label(ui, ".mp4", Role::Mono)
                    });
                });
                if has && !self.name_hint.is_empty() {
                    ui.add_space(S1);
                    let col = if self.name_bad { Some(c::LOSS) } else { None };
                    Self::text(ui, &self.name_hint.clone(), Role::Muted, col, true);
                }
            });
        });
        ui.add_space(S3);
        // the four choices
        let encoders = self.encoders();
        for (i, title) in ["Resolution", "Frame rate", "Quality", "Encoder"].iter().enumerate() {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                Self::form_label(ui, title);
                let w = field_w;
                let r = ui
                    .add_enabled_ui(has, |ui| match i {
                        0 => ui::combo(ui, "exp-res", &mut self.resolution, &RESOLUTIONS, w),
                        1 => ui::combo(ui, "exp-fps", &mut self.fps, &FRAME_RATES, w),
                        2 => {
                            let mut k = self.quality.as_str();
                            let r = ui::combo(ui, "exp-q", &mut k, &QUALITIES, w);
                            self.quality = k.to_string();
                            r
                        }
                        _ => {
                            let mut k = self.encoder.as_str();
                            let mut r = ui::combo(ui, "exp-enc", &mut k, &encoders, w);
                            self.encoder = k.to_string();
                            if self.nvenc == Some(false) && self.encoder == "nvenc" {
                                r = r.on_hover_text("No working NVIDIA encoder on this PC, the CPU is used instead");
                            }
                            r
                        }
                    })
                    .inner;
                self.combo_rects[i] = r.rect;
                opts_changed |= r.changed();
            });
            if i < 3 {
                ui.add_space(S3);
            }
        }
        if opts_changed {
            self.option_changed(cx.st);
            self.refresh(cx.st);
        }
        ui.spacing_mut().item_spacing = vec2(S2, 0.0);
        if has && !self.note.is_empty() {
            ui.add_space(S4);
            Self::text(ui, &self.note.clone(), Role::Muted, None, false);
        }
        ui.add_space(S4 + S1);
        // buttons
        ui.horizontal(|ui| {
            let r = ui.add(
                Button::primary("Render video")
                    .icon("download")
                    .min_height(36.0)
                    .enabled(has && self.can_render && !self.requested)
                    .tooltip_opt(&self.render_tip),
            );
            self.render_rect = r.rect;
            if r.clicked() {
                self.render(cx.st, cx.shell, false);
            }
            if self.running {
                let r = ui.add(Button::ghost("Cancel").icon("x").min_height(36.0).enabled(self.cancel_enabled));
                self.cancel_rect = r.rect;
                if r.clicked() {
                    self.cancel(cx.shell);
                }
            } else {
                self.cancel_rect = Rect::NOTHING;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let r = ui.add(Button::ghost("Open folder").icon("folder-open").enabled(has));
                self.open_rect = r.rect;
                if r.clicked() {
                    self.open_folder(cx.st);
                }
            });
        });
        if self.running {
            ui.add_space(S4);
            ui::progress_bar(ui, self.progress.unwrap_or(0.0) as f32, ui.available_width());
        }
        if !self.status.is_empty() {
            ui.add_space(S4);
            Self::text(ui, &self.status.clone(), Role::Mono, None, true);
        }
        if self.show_result {
            ui.add_space(S4);
            ui.horizontal(|ui| {
                let r = ui.add(Button::new("Play video").icon("play"));
                self.play_rect = r.rect;
                if r.clicked() {
                    self.play();
                }
                if ui.add(Button::ghost("Show in folder").icon("folder-open")).clicked() {
                    if let Some(p) = &self.last_out {
                        show_in_folder(p);
                    }
                }
            });
        } else {
            self.play_rect = Rect::NOTHING;
        }
    }

    fn editors(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx) {
        let has = cx.st.h.is_some() && cx.st.ev.is_some();
        ui::section(ui, "For editors");
        ui.add_space(S1);
        ui::hint(ui, "Marker files point at your original recording, so you can finish the edit elsewhere. They use the file name above");
        ui.add_space(S3);
        for (i, (ext, title)) in EDITOR_FILES.iter().enumerate() {
            ui::hline(ui);
            ui.add_space(S2);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = S3;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let r = ui.add(Button::new("Save").enabled(has && self.can_export).min_height(32.0));
                    self.save_rects[i] = r.rect;
                    if r.clicked() {
                        self.export(ext, cx.st, cx.shell, false);
                    }
                    ui::label(ui, *ext, Role::Mono);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| ui::label_elided(ui, title, Role::Body));
                });
            });
            ui.add_space(S2);
        }
        ui::hline(ui);
        if let Some(text) = self.saved_text.clone() {
            ui.add_space(S3);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(p) = self.saved_file.clone() {
                        if ui.add(Button::ghost("Show in folder").icon("folder-open")).clicked() {
                            show_in_folder(&p);
                        }
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| Self::text(ui, &text, Role::Mono, None, true));
                });
            });
        }
    }
}

/// `Button::tooltip` only when there is something to say.
trait TooltipOpt {
    fn tooltip_opt(self, t: &str) -> Self;
}
impl TooltipOpt for Button<'_> {
    fn tooltip_opt(self, t: &str) -> Self {
        if t.is_empty() {
            self
        } else {
            self.tooltip(t)
        }
    }
}

impl Page for ExportPage {
    fn on_state(&mut self, ev: &StateEvent, st: &mut AppState, _shell: &mut Shell) {
        if matches!(
            ev,
            StateEvent::ProjectChanged
                | StateEvent::EditsChanged
                | StateEvent::EventsChanged
                | StateEvent::TranscriptChanged
                | StateEvent::Restored(_)
        ) {
            self.refresh(st);
        }
    }

    fn on_job(&mut self, n: &JobNotice, st: &mut AppState, _shell: &mut Shell) {
        self.set_progress(n.progress, &n.msg, st);
    }

    fn on_show(&mut self, st: &mut AppState, _shell: &mut Shell) {
        self.refresh(st);
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx) {
        self.poll_nvenc(ui.ctx());
        if self.requested && !self.running {
            // the window refused (another task is running)
            self.requested = false;
            self.pending = None;
        }
        self.refresh(cx.st);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let avail = ui.available_width();
            let col_w = (avail - 2.0 * S8).clamp(480.0_f32.min(avail), 600.0);
            let left = ((avail - col_w) / 2.0).max(0.0);
            ui.add_space(S8 + S2);
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                ui.add_space(left);
                ui.allocate_ui_with_layout(vec2(col_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(col_w);
                    ui.spacing_mut().item_spacing = vec2(S2, 0.0);
                    ui::label(ui, "Export", Role::Title);
                    ui.add_space(S2);
                    Self::text(ui, &self.summary.clone(), Role::Mono, None, false);
                    ui.add_space(S1);
                    if !self.extras.is_empty() {
                        Self::text(ui, &self.extras.clone(), Role::Muted, None, false);
                    }
                    ui.add_space(S6);
                    let border =
                        if self.card_rect.is_positive() && ui.rect_contains_pointer(self.card_rect) { c::HAIR_STRONG } else { c::HAIR };
                    let card = egui::Frame::new()
                        .fill(c::CARD)
                        .stroke(Stroke::new(1.0, border))
                        .corner_radius(12)
                        .inner_margin(egui::Margin { left: S6 as i8, right: S6 as i8, top: S5 as i8, bottom: S6 as i8 })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            self.video_card(ui, cx);
                        });
                    self.card_rect = card.response.rect;
                    ui.add_space(S8);
                    self.editors(ui, cx);
                    ui.add_space(S8);
                    self.column_rect = ui.min_rect();
                });
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert_eq!(safe_stem("my:clip?"), "my_clip_");
        assert_eq!(safe_stem("Ränkéd ace.mp4"), "Ränkéd ace");
        assert_eq!(safe_stem("  clip.MP4 "), "clip");
        assert_eq!(safe_stem("con"), "_con");
        assert_eq!(safe_stem("lpt1.final"), "_lpt1.final");
        assert_eq!(safe_stem("name. . "), "name");
        assert_eq!(safe_stem(""), "");
        assert_eq!(safe_stem("a\tb"), "a_b");
        assert_eq!(safe_stem(&"x".repeat(200)).chars().count(), 150);
    }

    #[test]
    fn sizes_and_lengths() {
        assert_eq!(fmt_size(12_345_678), "12.3 MB");
        assert_eq!(fmt_size(1_050_000_000), "1.05 GB");
        assert_eq!(fmt_len(17.25), "17.2 s");
        assert_eq!(fmt_len(75.9), "1:15 min");
        assert_eq!(plural(1, "kill"), "1 kill");
        assert_eq!(plural(0, "part"), "0 parts");
    }
}
