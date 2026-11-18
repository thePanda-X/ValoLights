//! Main window and application entry.
use crate::dialogs::Dialog;
use crate::jobs::{Job, JobEvent};
use crate::pages::export::ExportPage;
use crate::pages::layout::LayoutPage;
use crate::pages::library::{self, LibraryPage};
use crate::pages::subtitles::SubtitlesPage;
use crate::pages::trim::TrimPage;
use crate::pages::{Action, JobInfo, JobNotice, Page, PageCtx, Shell, CLIPS, EXPORT, LAYOUT, STEPS, SUBTITLES, TRIM};
use crate::state::{AppState, StateEvent};
use crate::theme::{self, c, Role, Weight};
use crate::widgets::player::GlEnv;
use crate::widgets::ui::{self, Button, IconButton};
use egui::{pos2, vec2, Key, Rect, Sense, Stroke, Ui};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vl_core::{config, paths, projects};

pub fn job_name(kind: &str) -> &'static str {
    match kind {
        "transcribe" => "transcription",
        "proxy" => "preview",
        "render" => "export",
        "analyze" => "analysis",
        "smart" => "Smart cut listening",
        _ => "task",
    }
}

#[derive(Clone, Default)]
pub struct Options {
    /// never write projects or settings (QA)
    pub read_only: bool,
    /// QA: projects whose id starts with this prefix are writable even in read-only mode
    pub writable_prefix: Option<String>,
    /// don't resume interrupted analyses on start (QA)
    pub no_resume: bool,
    /// don't show the first-run dialog (QA)
    pub no_first_run: bool,
}

pub struct MainWindow {
    pub st: AppState,
    pub shell: Shell,
    pub opts: Options,
    pub page: usize,
    pub library: LibraryPage,
    pub trim: TrimPage,
    pub layout: LayoutPage,
    pub subs: SubtitlesPage,
    pub export: ExportPage,
    pub analysis_jobs: HashMap<String, Job>,
    pub queue: Vec<String>,
    /// transcribe / render / proxy (one at a time) and the clip it belongs to
    pub job: Option<Job>,
    pub job_pid: Option<String>,
    /// clicked while analysing: open once the scan finishes
    open_when_done: Option<String>,
    /// where to return when the open clip is clicked again
    last_step: usize,
    pub dialogs: Vec<Dialog>,
    pub toast: String,
    toast_until: Option<Instant>,
    title: String,
    allow_close: bool,
    dialog_counter: usize,
    /// rects of top-bar controls from the last frame (UI check)
    pub step_rects: Vec<Rect>,
    pub undo_rect: Rect,
    pub redo_rect: Rect,
    pub settings_rect: Rect,
    /// the launch / last update check (see update_ui)
    pub update: Option<crate::update_ui::Shared>,
    update_checked: bool,
}

impl MainWindow {
    pub fn new(gl: Option<GlEnv>, opts: Options) -> Self {
        let shell = Shell::new(gl);
        let mut w = MainWindow {
            st: AppState::new(opts.read_only),
            library: LibraryPage::new(&shell),
            trim: TrimPage::new(&shell),
            layout: LayoutPage::new(&shell),
            subs: SubtitlesPage::new(&shell),
            export: ExportPage::new(&shell),
            shell,
            opts,
            page: 0,
            analysis_jobs: HashMap::new(),
            queue: vec![],
            job: None,
            job_pid: None,
            open_when_done: None,
            last_step: 1,
            dialogs: vec![],
            toast: String::new(),
            toast_until: None,
            title: String::new(),
            allow_close: false,
            update: None,
            update_checked: false,
            dialog_counter: 0,
            step_rects: vec![],
            undo_rect: Rect::NOTHING,
            redo_rect: Rect::NOTHING,
            settings_rect: Rect::NOTHING,
        };
        w.library.reload();
        w.go(0);
        if !w.opts.no_resume && !w.opts.read_only {
            w.resume_interrupted();
        }
        if !config::load().first_run_done && !w.opts.no_first_run {
            w.dialogs.push(Dialog::first_run());
        }
        w
    }

    fn read_only_now(&self) -> bool {
        self.opts.read_only
    }

    /// Refuse a write in read-only mode (QA). Returns true when blocked.
    fn blocked(&mut self, what: &str) -> bool {
        if self.read_only_now() {
            self.show_toast(format!("Read-only mode: {what} is disabled"));
            return true;
        }
        false
    }

    // ------------------------------------------------------------ toast, title
    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = msg.into();
        self.toast_until = Some(Instant::now() + Duration::from_secs(4));
    }

    fn window_title(&self) -> String {
        match &self.st.h {
            Some(h) => format!("{} · ValoLights {}", h.project.name, vl_core::VERSION),
            None => format!("ValoLights {}", vl_core::VERSION),
        }
    }

    // ------------------------------------------------------------ undo / redo
    pub fn undo(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() || self.page == CLIPS {
            return; // text fields keep Ctrl+Z for their own typing; on Clips the history is hidden
        }
        self.st.undo();
    }

    pub fn redo(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() || self.page == CLIPS {
            return;
        }
        self.st.redo();
    }

    // ------------------------------------------------------------ navigation
    fn step_key(&mut self, i: isize, absolute: bool) {
        let target = if absolute { i } else { self.page as isize + i };
        if target > 0 && self.st.ev.is_none() {
            self.show_toast("Open a clip first");
            return;
        }
        self.go(target.max(0) as usize);
    }

    fn next(&mut self) {
        let i = self.page;
        self.go(if i == 0 { self.last_step } else { i + 1 });
    }

    pub fn go(&mut self, i: usize) {
        let mut i = i.min(STEPS.len() - 1);
        if i > 0 && (self.st.h.is_none() || self.st.ev.is_none()) {
            i = 0;
        }
        if i != self.page {
            let old = self.page;
            let (st, shell) = (&mut self.st, &mut self.shell);
            match old {
                CLIPS => self.library.on_hide(st, shell),
                TRIM => self.trim.on_hide(st, shell),
                LAYOUT => self.layout.on_hide(st, shell),
                SUBTITLES => self.subs.on_hide(st, shell),
                _ => self.export.on_hide(st, shell),
            }
        }
        let changed = i != self.page;
        self.page = i;
        if i > 0 {
            self.last_step = i;
        }
        if i == 0 {
            self.library.reload();
        }
        if changed {
            let (st, shell) = (&mut self.st, &mut self.shell);
            match i {
                CLIPS => self.library.on_show(st, shell),
                TRIM => self.trim.on_show(st, shell),
                LAYOUT => self.layout.on_show(st, shell),
                SUBTITLES => self.subs.on_show(st, shell),
                _ => self.export.on_show(st, shell),
            }
        }
    }

    // ------------------------------------------------------------ projects
    /// A clip already made from this exact file (same path and size), if any.
    fn existing(f: &Path) -> Option<projects::ProjectHandle> {
        let p = paths::clean(f.canonicalize().ok()?);
        let size = std::fs::metadata(&p).ok()?.len();
        projects::list_all().into_iter().find(|h| h.project.source.size == size && Path::new(&h.project.source.path) == p)
    }

    pub fn add_videos(&mut self, files: Vec<PathBuf>, confirmed: bool) {
        if self.blocked("adding videos") {
            return;
        }
        let mut added = 0;
        for f in files {
            let fname = f.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if !confirmed {
                if let Some(dup) = Self::existing(&f) {
                    self.dialogs.push(Dialog::confirm(
                        "Add video",
                        &format!("\u{201c}{fname}\u{201d} is already in Clips as \u{201c}{}\u{201d}.\n\nAdd another copy? Each copy has its own edits", dup.project.name),
                        "Add copy",
                        false,
                        vec![Action::AddVideosConfirmed(vec![f.clone()])],
                    ));
                    continue;
                }
            }
            match projects::create(&f) {
                Ok(h) => {
                    added += 1;
                    self.library.reload();
                    self.start_analysis(&h.project.id, &h.dir);
                }
                Err(e) => {
                    self.dialogs.push(Dialog::fail("Add video", &format!("Could not read {fname}. Is it a video file?"), &e.to_string()))
                }
            }
        }
        if added > 0 {
            if self.page != 0 {
                self.go(0);
            }
            self.show_toast(format!("Added {added} video{} · analysing on this PC", if added != 1 { "s" } else { "" }));
        }
    }

    fn analysis_running(&self) -> bool {
        self.analysis_jobs.values().any(Job::running)
    }

    fn pump_queue(&mut self) {
        // analyse one video at a time (GPU decode + CPU OCR saturate the machine)
        if self.analysis_running() {
            return;
        }
        while !self.queue.is_empty() {
            let pid = self.queue.remove(0);
            let d = paths::projects_dir().join(&pid);
            if d.exists() {
                self.start_analysis(&pid, &d);
                return;
            }
        }
    }

    pub fn start_analysis(&mut self, pid: &str, d: &Path) {
        if self.analysis_jobs.get(pid).map(Job::running).unwrap_or(false) {
            return;
        }
        if self.analysis_running() {
            if !self.queue.iter().any(|q| q == pid) {
                self.queue.push(pid.to_string());
            }
            self.show_queue();
            return;
        }
        let waker = self.shell.gl.as_ref().map(|g| {
            let c = g.egui.clone();
            std::sync::Arc::new(move || c.request_repaint()) as crate::jobs::Waker
        });
        match Job::start("analyze", json!({"project_dir": d.to_string_lossy()}), waker) {
            Ok(job) => {
                self.analysis_jobs.insert(pid.to_string(), job);
                self.library.progress(pid, 0.0, "Starting…");
                self.show_queue();
            }
            Err(e) => self.dialogs.push(Dialog::fail("Analysis failed", "The analysis could not be started", &e.to_string())),
        }
    }

    fn show_queue(&mut self) {
        let q = self.queue.clone();
        for (n, pid) in q.iter().enumerate() {
            let msg = if n == 0 { "Queued, next up".to_string() } else { format!("Queued, {} in line", n + 1) };
            self.library.progress(pid, 0.0, &msg);
        }
    }

    fn name_of(pid: &str) -> String {
        projects::open_dir(&paths::projects_dir().join(pid)).map(|h| h.project.name).unwrap_or_else(|_| "the clip".into())
    }

    /// The folder of a project id: projects live in data/projects/<id> (the folder name is the id).
    fn dir_of(&self, pid: &str) -> PathBuf {
        self.library.card(pid).map(|c| c.h.dir.clone()).unwrap_or_else(|| paths::projects_dir().join(pid))
    }

    fn poll_analysis(&mut self) {
        let pids: Vec<String> = self.analysis_jobs.keys().cloned().collect();
        for pid in pids {
            let evs = match self.analysis_jobs.get_mut(&pid) {
                Some(j) => j.poll(),
                None => continue,
            };
            for ev in evs {
                match ev {
                    JobEvent::Progress { p, msg } => {
                        let eta = self.analysis_jobs.get(&pid).and_then(Job::eta);
                        let m = match eta {
                            Some(e) if e as i64 > 0 => format!("{msg} · {} s left", e as i64),
                            _ => msg,
                        };
                        self.library.progress(&pid, p, &m);
                    }
                    JobEvent::Finished(_) => self.analysis_done(&pid),
                    JobEvent::Failed(err) => self.analysis_failed(&pid, &err),
                }
            }
        }
    }

    fn analysis_done(&mut self, pid: &str) {
        self.analysis_jobs.remove(pid);
        self.library.refresh_card(pid);
        self.pump_queue();
        if self.st.pid() == Some(pid) {
            let dir = self.st.h.as_ref().unwrap().dir.clone();
            self.open_project(&dir, true);
        } else if self.open_when_done.as_deref() == Some(pid) && self.st.h.is_none() && self.page == 0 {
            self.open_when_done = None;
            let d = self.dir_of(pid);
            self.open_project(&d, false);
        } else {
            self.show_toast(format!("Finished analysing {}", Self::name_of(pid)));
        }
    }

    fn analysis_failed(&mut self, pid: &str, err: &str) {
        self.analysis_jobs.remove(pid);
        self.library.refresh_card(pid);
        if self.open_when_done.as_deref() == Some(pid) {
            self.open_when_done = None;
        }
        self.pump_queue();
        if err == "Cancelled" {
            self.show_toast(format!("Stopped analysing {}", Self::name_of(pid)));
        } else {
            self.dialogs.push(Dialog::fail(
                "Analysis failed",
                &format!("Could not analyse {}. Right-click the clip and choose Analyse to try again", Self::name_of(pid)),
                err,
            ));
        }
    }

    fn cancel_analysis(&mut self, dir: &Path) {
        let Ok(h) = projects::open_dir(dir) else { return };
        let pid = h.project.id.clone();
        if let Some(i) = self.queue.iter().position(|q| *q == pid) {
            self.queue.remove(i);
            self.library.refresh_card(&pid);
            self.show_queue();
            self.show_toast(format!("Removed {} from the queue", h.project.name));
        }
        if self.open_when_done.as_deref() == Some(pid.as_str()) {
            self.open_when_done = None;
        }
        if let Some(j) = self.analysis_jobs.get_mut(&pid) {
            j.cancel();
        }
    }

    fn resume_interrupted(&mut self) {
        for h in projects::list_all() {
            if h.project.status == "new" && h.source_ok() {
                self.queue.push(h.project.id.clone());
            }
        }
        self.pump_queue();
        self.show_queue();
    }

    pub fn reanalyse(&mut self, dir: &Path, confirmed: bool) {
        if self.blocked("analysing") {
            return;
        }
        let Ok(h) = projects::open_dir(dir) else { return };
        let pid = h.project.id.clone();
        if h.project.status == "ready" && !confirmed {
            self.dialogs.push(Dialog::confirm(
                "Analyse again",
                &format!(
                    "Analyse \u{201c}{}\u{201d} again?\n\nKills you added or fixed by hand in this clip are replaced by what the new scan finds. Your cut is kept",
                    h.project.name
                ),
                "Analyse again",
                false,
                vec![Action::ReanalyseConfirmed(dir.to_path_buf())],
            ));
            return;
        }
        if self.st.pid() == Some(pid.as_str()) {
            // the scan rewrites this clip's kills: close it now and open it again when the scan finishes
            if !self.stop_job_for_switch(vec![Action::ReanalyseConfirmed(dir.to_path_buf())]) {
                return;
            }
            self.close_project();
            self.go(0);
            self.open_when_done = Some(pid.clone());
        }
        self.start_analysis(&pid, dir);
    }

    fn close_project(&mut self) {
        self.trim.stop_players();
        self.layout.stop_players();
        self.subs.stop_players();
        self.export.stop_players();
        self.st.close();
    }

    pub fn remove_project(&mut self, dir: &Path) {
        if self.blocked("removing clips") {
            return;
        }
        let Ok(h) = projects::open_dir(dir) else { return };
        let pid = h.project.id.clone();
        self.queue.retain(|q| *q != pid);
        if let Some(mut job) = self.analysis_jobs.remove(&pid) {
            if job.running() {
                job.cancel();
                job.wait(Duration::from_secs(5)); // its files must be released before the folder can go
            }
        }
        if self.open_when_done.as_deref() == Some(pid.as_str()) {
            self.open_when_done = None;
        }
        if self.st.pid() == Some(pid.as_str()) {
            if let Some(j) = self.job.as_mut() {
                if j.running() {
                    j.cancel();
                    j.wait(Duration::from_secs(5));
                }
            }
            self.close_project();
        }
        projects::delete(&h);
        if h.dir.exists() {
            // a file was still in use: try once more, then say so
            std::thread::sleep(Duration::from_millis(200));
            projects::delete(&h);
        }
        self.library.reload();
        self.pump_queue();
        self.go(0);
        if h.dir.exists() {
            self.dialogs.push(Dialog::fail(
                "Remove clip",
                &format!("Some files of \u{201c}{}\u{201d} are still in use and were not deleted. Close other programs using them and remove the clip again", h.project.name),
                &h.dir.to_string_lossy(),
            ));
        } else {
            self.show_toast(format!("Removed {}", h.project.name));
        }
    }

    /// Leaving the open clip while its preview, transcription or export runs: ask, then stop it.
    /// Returns true when nothing runs; otherwise a question is shown and `then` runs if the user agrees.
    fn stop_job_for_switch(&mut self, then: Vec<Action>) -> bool {
        let Some(j) = &self.job else { return true };
        if !j.running() {
            return true;
        }
        let what = job_name(&j.kind);
        let name = self.st.h.as_ref().map(|h| h.project.name.clone()).unwrap_or_else(|| "this clip".into());
        let mut on_yes = vec![Action::CancelJob];
        on_yes.extend(then);
        self.dialogs.push(Dialog::confirm(
            "Task running",
            &format!("The {what} of \u{201c}{name}\u{201d} is still running.\n\nStop it and continue?"),
            &format!("Stop {what}"),
            false,
            on_yes,
        ));
        false
    }

    pub fn open_project(&mut self, dir: &Path, force: bool) {
        let h = match projects::open_dir(dir) {
            Ok(h) => h,
            Err(e) => {
                self.dialogs.push(Dialog::fail(
                    "Open clip",
                    "This clip could not be opened. Its project files may be damaged",
                    &e.to_string(),
                ));
                return;
            }
        };
        let pid = h.project.id.clone();
        if !force && self.st.pid() == Some(pid.as_str()) && self.st.ev.is_some() {
            self.go(self.last_step); // already open: back to where you were, undo history intact
            return;
        }
        let running = self.analysis_jobs.get(&pid).map(Job::running).unwrap_or(false);
        if h.project.status != "ready" || running {
            if running || self.queue.contains(&pid) {
                self.open_when_done = Some(pid);
                self.show_toast(format!("{} opens when the analysis finishes", h.project.name));
                return;
            }
            if !h.source_ok() {
                self.source_missing(&h);
                return;
            }
            let text = match h.project.status.as_str() {
                "error" => "The last analysis of this clip failed. Try again?",
                "analyzing" => "The analysis of this clip did not finish. Analyse it now?",
                _ => "This clip hasn't been analysed yet. Analyse it now?",
            };
            self.dialogs.push(Dialog::confirm("Analyse", text, "Analyse", true, vec![Action::AnalyseAndOpen(dir.to_path_buf())]));
            return;
        }
        if !h.source_ok() {
            self.source_missing(&h);
            return;
        }
        if self.st.h.is_some()
            && self.st.pid() != Some(pid.as_str())
            && !self.stop_job_for_switch(vec![Action::OpenProject(dir.to_path_buf())])
        {
            return;
        }
        self.st.flush_history();
        self.st.save_now(); // the clip being left keeps its last edits
        if let Some(pre) = &self.opts.writable_prefix {
            self.st.read_only = !pid.starts_with(pre.as_str());
        }
        self.st.open(h);
        self.last_step = 1;
        self.go(1);
        if self.st.me().is_empty() {
            self.who_are_you();
        }
    }

    fn source_missing(&mut self, h: &projects::ProjectHandle) {
        let name = Path::new(&h.project.source.path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.dialogs.push(Dialog::message(
            "Source video missing",
            &format!("{name} was moved or deleted.\n\nRight-click the clip and choose Locate source video"),
        ));
    }

    pub fn who_are_you(&mut self) {
        if self.st.ev.is_none() {
            return;
        }
        self.dialogs.push(Dialog::who_are_you(&self.st));
    }

    // ------------------------------------------------------------ jobs on the open project
    fn busy(&mut self) -> bool {
        if let Some(j) = &self.job {
            if j.running() {
                let what = job_name(&j.kind);
                self.dialogs.push(Dialog::message("Busy", &format!("The {what} is still running. Try again when it finishes")));
                return true;
            }
        }
        false
    }

    fn cancel_job(&mut self) {
        if let Some(j) = self.job.as_mut() {
            j.cancel();
            // the tree is killed synchronously; let the process exit so a queued "open the other clip" doesn't ask again
            j.wait(std::time::Duration::from_secs(2));
        }
    }

    fn start_job(&mut self, kind: &str, spec: Value) -> bool {
        let waker = self.shell.gl.as_ref().map(|g| {
            let c = g.egui.clone();
            std::sync::Arc::new(move || c.request_repaint()) as crate::jobs::Waker
        });
        match Job::start(kind, spec, waker) {
            Ok(j) => {
                self.job = Some(j);
                true
            }
            Err(e) => {
                self.dialogs.push(Dialog::fail("Task failed", "The task could not be started", &e.to_string()));
                false
            }
        }
    }

    fn notify_page(&mut self, kind: &str, progress: Option<f64>, msg: String, result: Option<Value>) {
        let n = JobNotice { kind: kind.to_string(), progress, msg, result };
        let (st, shell) = (&mut self.st, &mut self.shell);
        match kind {
            "transcribe" => self.subs.on_job(&n, st, shell),
            "proxy" => self.layout.on_job(&n, st, shell),
            "smart" => self.trim.on_job(&n, st, shell),
            "render" => self.export.on_job(&n, st, shell),
            _ => {}
        }
    }

    pub fn transcribe(&mut self, whole: bool, only: Option<(f64, f64)>, confirmed: bool) {
        if self.st.h.is_none() || self.busy() {
            return;
        }
        if only.is_none() && !confirmed && self.st.tr.as_ref().map(|t| t.segments.iter().any(|s| s.edited)).unwrap_or(false) {
            self.dialogs.push(Dialog::confirm(
                "Transcribe again",
                "This replaces every caption, including the ones you fixed by hand. To redo one line, select it and use the microphone button instead.\n\nTranscribe everything again?",
                "Yes",
                false,
                vec![Action::TranscribeConfirmed { whole, only }],
            ));
            return;
        }
        self.st.save_now();
        let h = self.st.h.as_ref().unwrap();
        self.job_pid = Some(h.project.id.clone());
        let mut spec = json!({"project_dir": h.dir.to_string_lossy(), "whole": whole});
        if let Some((a, b)) = only {
            spec["only"] = json!([a, b]);
        }
        if self.start_job("transcribe", spec) {
            self.notify_page("transcribe", Some(0.0), if only.is_some() { "Listening again…".into() } else { "Starting…".into() }, None);
        }
    }

    fn transcribed(&mut self, r: &Value, line: bool) {
        if self.st.h.is_none() {
            return;
        }
        let sub = &mut self.st.ed.subtitles;
        sub.enabled = true;
        if !line {
            // old line-indexed choices refer to the previous transcript
            sub.hidden_lines.clear();
            sub.edits.clear();
            sub.censored.clear();
        }
        self.st.load_transcript_from_disk();
        let msg = if line {
            "Caption transcribed again".to_string()
        } else {
            format!("{} captions", r.get("segments").and_then(Value::as_i64).unwrap_or(0))
        };
        self.notify_page("transcribe", None, msg, Some(r.clone()));
        self.st.touch();
        self.st.notify_transcript();
    }

    /// Smart cut's sound pass for the open clip; the cut is applied when it finishes.
    pub fn smart_cut(&mut self) {
        if self.st.h.is_none() || self.st.ev.is_none() || self.busy() {
            return;
        }
        self.st.save_now();
        let h = self.st.h.as_ref().unwrap();
        self.job_pid = Some(h.project.id.clone());
        let spec = json!({"project_dir": h.dir.to_string_lossy()});
        if self.start_job("smart", spec) {
            self.notify_page("smart", Some(0.0), "Listening to the clip…".into(), None);
        }
    }

    fn smart_done(&mut self, r: Value) {
        self.st.reload_events();
        self.st.apply_quick_cut("smart", true);
        let n = self.st.smart_moments().len();
        let msg = format!("Smart cut · {n} moments, {:.1} s kept", self.st.kept_duration());
        self.notify_page("smart", None, msg, Some(r));
    }

    pub fn preview(&mut self) {
        if self.st.h.is_none() || self.busy() {
            return;
        }
        self.st.save_now();
        let h = self.st.h.as_ref().unwrap();
        self.job_pid = Some(h.project.id.clone());
        let out = h.dir.join("cache").join("preview.mp4");
        let spec = json!({"project_dir": h.dir.to_string_lossy(), "out": out.to_string_lossy()});
        self.layout.stop_players();
        if self.start_job("proxy", spec) {
            self.notify_page("proxy", Some(0.0), "Rendering preview…".into(), None);
        }
    }

    pub fn render(&mut self, out: String, options: Value) {
        if self.st.h.is_none() || self.busy() {
            return;
        }
        self.st.save_now();
        let h = self.st.h.as_ref().unwrap();
        self.job_pid = Some(h.project.id.clone());
        let spec = json!({"project_dir": h.dir.to_string_lossy(), "out": out, "options": options});
        if self.start_job("render", spec) {
            self.notify_page("render", Some(0.0), "Starting…".into(), None);
        }
    }

    fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else {
            self.shell.job = None;
            return;
        };
        let kind = job.kind.clone();
        let only = job.spec.get("only").is_some();
        let evs = job.poll();
        let eta = job.eta();
        let done = job.is_done();
        self.shell.job = if done { None } else { Some(JobInfo { kind: kind.clone(), p: job.last_p, msg: job.last_msg.clone(), eta }) };
        let same = self.job_pid.is_some() && self.st.pid() == self.job_pid.as_deref();
        for ev in evs {
            if !same {
                // the Export page follows its render even after another clip was opened
                if kind == "render" {
                    let (p, msg) = match ev {
                        JobEvent::Progress { p, msg } => (Some(p), msg),
                        JobEvent::Finished(_) => (None, "Saved".to_string()),
                        JobEvent::Failed(e) => (None, if e == "Cancelled" { e } else { "Render failed".to_string() }),
                    };
                    self.notify_page("render", p, msg, None);
                }
                continue;
            }
            match (kind.as_str(), ev) {
                ("transcribe", JobEvent::Progress { p, msg }) => self.notify_page("transcribe", Some(p), msg, None),
                ("transcribe", JobEvent::Finished(r)) => self.transcribed(&r, only),
                ("proxy", JobEvent::Progress { p, .. }) => self.notify_page("proxy", Some(p), "Rendering preview…".into(), None),
                ("smart", JobEvent::Progress { p, msg }) => self.notify_page("smart", Some(p), msg, None),
                ("smart", JobEvent::Finished(r)) => self.smart_done(r),
                ("proxy", JobEvent::Finished(r)) => self.notify_page("proxy", None, "Preview ready · 540p".into(), Some(r)),
                ("render", JobEvent::Progress { p, msg }) => {
                    let left = match eta {
                        Some(e) if e as i64 > 0 => format!(" · about {} s left", e as i64),
                        _ => String::new(),
                    };
                    self.notify_page("render", Some(p), format!("{msg} · {:.0}%{left}", p * 100.0), None);
                }
                ("render", JobEvent::Finished(r)) => {
                    let out = r.get("out").and_then(Value::as_str).unwrap_or("").to_string();
                    self.notify_page("render", None, format!("Saved {out}"), Some(r));
                }
                (k, JobEvent::Failed(e)) => {
                    let cancelled = e == "Cancelled";
                    let status = match (k, cancelled) {
                        (_, true) => "Cancelled",
                        ("render", false) => "Render failed",
                        _ => "",
                    };
                    self.notify_page(k, None, status.into(), None);
                    if !cancelled {
                        let (title, text) = match k {
                            "transcribe" => ("Transcription failed", "The captions could not be made"),
                            "proxy" => ("Preview failed", "The preview could not be rendered"),
                            "smart" => ("Smart cut failed", "The clip's sound could not be checked"),
                            _ => ("Export failed", "The video could not be exported"),
                        };
                        self.dialogs.push(Dialog::fail(title, text, &e));
                    }
                }
                _ => {}
            }
        }
        if done {
            self.job = None;
            self.shell.job = None;
        }
    }

    fn settings(&mut self) {
        let old = config::load().highlights;
        // is the open clip's cut still exactly what its quick cut produces? (judged with the old settings)
        let auto = self.st.ev.is_some() && self.st.h.is_some() && !self.st.cut_is_custom();
        self.dialogs.push(Dialog::settings(old, auto));
    }

    fn settings_saved(&mut self, old: config::HighlightsCfg, auto: bool) {
        if self.st.ev.is_none() || config::load().highlights == old {
            self.show_toast("Settings saved");
        } else if auto {
            let q = self.st.ed.quick_cut.clone();
            self.st.apply_quick_cut(&q, true);
            self.show_toast("Settings saved · the cut was rebuilt with the new timings");
        } else {
            // never overwrite hand trimming behind the user's back
            self.show_toast("Settings saved · your trimmed cut was kept. Pick a quick cut in Trim to use the new timings");
        }
    }

    fn running_kinds(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = vec![];
        for j in self.analysis_jobs.values().filter(|j| j.running()) {
            if !v.contains(&job_name(&j.kind)) {
                v.push(job_name(&j.kind));
            }
        }
        if let Some(j) = &self.job {
            if j.running() && !v.contains(&job_name(&j.kind)) {
                v.push(job_name(&j.kind));
            }
        }
        v
    }

    fn quit(&mut self, ctx: &egui::Context) {
        for j in self.analysis_jobs.values_mut() {
            j.cancel();
        }
        if let Some(j) = self.job.as_mut() {
            j.cancel();
        }
        self.st.flush_history();
        self.st.close();
        self.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // ------------------------------------------------------------ actions
    pub fn run_actions(&mut self, ctx: &egui::Context) {
        loop {
            let actions = std::mem::take(&mut self.shell.actions);
            if actions.is_empty() {
                break;
            }
            for a in actions {
                self.run_action(a, ctx);
            }
        }
    }

    fn run_action(&mut self, a: Action, ctx: &egui::Context) {
        match a {
            Action::Toast(m) => self.show_toast(m),
            Action::Go(i) => self.go(i),
            Action::WhoAreYou => self.who_are_you(),
            Action::Transcribe { whole, only } => self.transcribe(whole, only, false),
            Action::TranscribeConfirmed { whole, only } => self.transcribe(whole, only, true),
            Action::Preview => self.preview(),
            Action::SmartCut => self.smart_cut(),
            Action::Render { out, options } => self.render(out, options),
            Action::CancelJob => self.cancel_job(),
            Action::Dialog(d) => self.dialogs.push(d),
            Action::Settings => self.settings(),
            Action::CheckUpdates => {
                let s = crate::update_ui::spawn_check(ctx);
                self.update = Some(s.clone());
                self.dialogs.push(crate::update_ui::dialog(s));
            }
            Action::SkipUpdate(v) => {
                let mut cfg = config::load();
                cfg.updates.skipped = v;
                if !self.st.read_only {
                    let _ = cfg.save();
                }
            }
            Action::InstallUpdate(path) => match crate::update::start_install(&path) {
                Ok(()) => {
                    log::info!("installing update {}", path.display());
                    self.st.save_now();
                    self.quit(ctx);
                }
                Err(e) => self.dialogs.push(Dialog::fail("Update", "The update could not be installed", &format!("{e:#}"))),
            },
            Action::SettingsSaved { old, auto } => self.settings_saved(old, auto),
            Action::OpenProject(d) => self.open_project(&d, false),
            Action::AddVideos(f) => self.add_videos(f, false),
            Action::AddVideosConfirmed(f) => self.add_videos(f, true),
            Action::PickVideos => {
                let files = library::pick_videos("Add videos", None, true);
                if !files.is_empty() {
                    self.add_videos(files, false);
                }
            }
            Action::Reanalyse(d) => self.reanalyse(&d, false),
            Action::ReanalyseConfirmed(d) => self.reanalyse(&d, true),
            Action::CancelAnalysis(d) => self.cancel_analysis(&d),
            Action::RemoveProject(d) | Action::RemoveConfirmed(d) => self.remove_project(&d),
            Action::AnalyseAndOpen(d) => {
                if self.blocked("analysing") {
                    return;
                }
                if let Ok(h) = projects::open_dir(&d) {
                    self.open_when_done = Some(h.project.id.clone());
                    self.start_analysis(&h.project.id, &d);
                }
            }
            Action::ExportConfirmed(tag) => self.export.confirmed(&tag, &mut self.st, &mut self.shell),
            Action::Quit => self.quit(ctx),
        }
    }

    /// State notifications → top bar + every page.
    fn dispatch_state(&mut self) {
        let evs = self.st.take_events();
        for ev in &evs {
            match ev {
                StateEvent::Restored(m) | StateEvent::Message(m) => self.show_toast(m.clone()),
                StateEvent::ProjectChanged => {
                    let pid = self.st.pid().map(str::to_string);
                    self.library.set_current(pid.as_deref());
                }
                _ => {}
            }
            let (st, shell) = (&mut self.st, &mut self.shell);
            self.library.on_state(ev, st, shell);
            self.trim.on_state(ev, st, shell);
            self.layout.on_state(ev, st, shell);
            self.subs.on_state(ev, st, shell);
            self.export.on_state(ev, st, shell);
        }
    }

    // ------------------------------------------------------------ input
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if !self.dialogs.is_empty() {
            return;
        }
        let (undo, redo, next, prev, num) = ctx.input_mut(|i| {
            let m = egui::Modifiers::COMMAND;
            let ms = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
            let redo = i.consume_key(ms, Key::Z) || i.consume_key(m, Key::Y);
            let undo = i.consume_key(m, Key::Z);
            let next = i.consume_key(m, Key::PageDown);
            let prev = i.consume_key(m, Key::PageUp);
            let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5];
            let num = keys.iter().position(|k| i.consume_key(m, *k));
            (undo, redo, next, prev, num)
        });
        if undo {
            self.undo(ctx);
        }
        if redo {
            self.redo(ctx);
        }
        if next {
            self.step_key(1, false);
        }
        if prev {
            self.step_key(-1, false);
        }
        if let Some(k) = num {
            self.step_key(k as isize, true);
        }
    }

    fn drag_and_drop(&mut self, ctx: &egui::Context) {
        let (hovered, dropped) = ctx.input(|i| {
            (
                i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect::<Vec<_>>(),
                i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(),
            )
        });
        let n = library::video_files(&hovered).len();
        if n != self.library.drag_n {
            if n > 0 {
                self.show_toast(format!("Let go to add {n} video{} to Clips", if n != 1 { "s" } else { "" }));
            } else if self.library.drag_n > 0 && dropped.is_empty() {
                self.toast.clear();
            }
            self.library.drag_n = n;
        }
        if !dropped.is_empty() {
            self.library.drag_n = 0;
            let files = library::video_files(&dropped);
            let skipped = dropped.len() - files.len();
            if !files.is_empty() {
                self.add_videos(files, false);
            }
            if skipped > 0 {
                // after adding, so this is the message left in the top bar
                self.show_toast(format!(
                    "Skipped {skipped} file{} that {} not a video",
                    if skipped != 1 { "s" } else { "" },
                    if skipped != 1 { "are" } else { "is" }
                ));
            }
        }
    }

    // ------------------------------------------------------------ top bar
    fn top_bar(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, c::DEEP);
        ui.painter().line_segment([rect.left_bottom() + vec2(0.0, -0.5), rect.right_bottom() + vec2(0.0, -0.5)], Stroke::new(1.0, c::HAIR));
        let has = self.st.ev.is_some();
        let inner = Rect::from_min_max(rect.min + vec2(20.0, 0.0), rect.max - vec2(16.0, 0.0));
        let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
        bar.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // brand
        if let Some(t) = brand_texture(bar.ctx()) {
            let (r, _) = bar.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            bar.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
            bar.add_space(8.0);
        }
        ui::label(&mut bar, "ValoLights", Role::Brand);
        bar.add_space(32.0);
        // steps
        self.step_rects.clear();
        let mut go_to = None;
        for (k, name) in STEPS.iter().enumerate() {
            let enabled = k == 0 || has;
            let font = theme::ui_font(13.0, Weight::Medium);
            let w = ui::text_width(&bar, name, &font) + 24.0;
            let (r, resp) = bar.allocate_exact_size(vec2(w, 54.0), if enabled { Sense::click() } else { Sense::hover() });
            self.step_rects.push(r);
            let checked = k == self.page;
            let col = if !enabled {
                c::DISABLED
            } else if checked || resp.hovered() {
                c::TEXT
            } else {
                c::MUTED
            };
            bar.painter().text(r.center(), egui::Align2::CENTER_CENTER, *name, font, col);
            if checked {
                let u = Rect::from_min_max(pos2(r.left(), rect.bottom() - 2.0), pos2(r.right(), rect.bottom()));
                bar.painter().rect_filled(u, 0.0, c::ACCENT);
            }
            let tip = if enabled { format!("{name}  (Ctrl+{})", k + 1) } else { "Open a clip first".into() };
            let resp =
                if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip) } else { resp.on_hover_text(tip) };
            if resp.clicked() {
                go_to = Some(k);
            }
            if k == 0 {
                bar.add_space(4.0);
                ui::label(&mut bar, "/", Role::Muted);
                bar.add_space(4.0);
            }
        }
        bar.add_space(24.0);
        // right side, laid out from the right edge
        let rest = bar.available_rect_before_wrap();
        let mut right = bar.new_child(egui::UiBuilder::new().max_rect(rest).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let r = right.add(IconButton::new("settings", "Settings"));
        self.settings_rect = r.rect;
        if r.clicked() {
            self.shell.push(Action::Settings);
        }
        right.add_space(16.0);
        // a newer version found by the launch check
        if let Some(v) = self.update.as_ref().and_then(crate::update_ui::offered) {
            let r =
                right.add(Button::primary(format!("Update to {v}")).icon("download").tooltip("A newer version of ValoLights is available"));
            if r.clicked() {
                if let Some(s) = self.update.clone() {
                    self.dialogs.push(crate::update_ui::dialog(s));
                }
            }
            right.add_space(12.0);
        }
        let i = self.page;
        if 0 < i && i < STEPS.len() - 1 || (i == 0 && has) {
            let tip = if i > 0 { "Next step  (Ctrl+PgDown)" } else { "Back to the open clip" };
            if right.add(Button::new("Next").icon_right("chevron-right").tooltip(tip)).clicked() {
                self.next();
            }
            right.add_space(8.0);
        }
        if i > 0 && right.add(Button::ghost("Back").icon("chevron-left").tooltip("Previous step  (Ctrl+PgUp)")).clicked() {
            go_to = Some(i - 1);
        }
        right.add_space(12.0);
        if self.st.h.is_some() && i > 0 {
            let (ul, rl) = (self.st.undo_label(), self.st.redo_label());
            let rt = if rl.is_empty() { "Nothing to redo".to_string() } else { format!("Redo {rl}  (Ctrl+Y)") };
            let ut = if ul.is_empty() { "Nothing to undo".to_string() } else { format!("Undo {ul}  (Ctrl+Z)") };
            let rr = right.add(IconButton::new("redo-2", rt.clone()).enabled(self.st.can_redo()));
            let rr = if self.st.can_redo() { rr } else { rr.on_disabled_hover_text(rt) };
            self.redo_rect = rr.rect;
            if rr.clicked() {
                self.redo(right.ctx());
            }
            let ur = right.add(IconButton::new("undo-2", ut.clone()).enabled(self.st.can_undo()));
            let ur = if self.st.can_undo() { ur } else { ur.on_disabled_hover_text(ut) };
            self.undo_rect = ur.rect;
            if ur.clicked() {
                self.undo(right.ctx());
            }
            right.add_space(12.0);
            ui::label(&mut right, format!("{:.1} s kept of {:.1} s", self.st.kept_duration(), self.st.duration()), Role::Mono);
            right.add_space(16.0);
        } else {
            self.undo_rect = Rect::NOTHING;
            self.redo_rect = Rect::NOTHING;
        }
        // the toast fills what is left, right-aligned and elided
        let tr = right.available_rect_before_wrap();
        if !self.toast.is_empty() {
            let r = right.allocate_rect(tr, Sense::hover());
            ui::paint_elided_right(&right, tr, &self.toast, Role::Muted);
            r.on_hover_text(self.toast.clone());
        }
        if let Some(k) = go_to {
            self.go(k);
        }
    }

    /// One frame of the whole window.
    pub fn frame_ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // once, on the first frame: look for updates (not in QA / read-only modes or source checkouts)
        if !self.update_checked {
            self.update_checked = true;
            if !self.opts.read_only && !self.opts.no_first_run && config::load().updates.check_on_launch && !crate::update::is_dev_build() {
                self.update = Some(crate::update_ui::spawn_check(&ctx));
            }
        }
        // close request: confirm when jobs run
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let running = self.running_kinds();
            if running.is_empty() {
                self.quit(&ctx);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if !self.dialogs.iter().any(|d| d.title() == "Quit") {
                    self.dialogs.push(Dialog::confirm(
                        "Quit",
                        &format!("Still running: {}.\n\nQuit and stop it?", running.join(", ")),
                        "Quit",
                        false,
                        vec![Action::Quit],
                    ));
                }
            }
        }
        self.poll_analysis();
        self.poll_job();
        self.dispatch_state();
        self.shortcuts(&ctx);
        self.drag_and_drop(&ctx);
        if let Some(t) = self.toast_until {
            let now = Instant::now();
            if now >= t {
                self.toast.clear();
                self.toast_until = None;
            } else {
                ctx.request_repaint_after(t - now);
            }
        }
        let title = self.window_title();
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        egui::Panel::top("top-bar").exact_size(56.0).frame(egui::Frame::NONE).show(ui, |ui| self.top_bar(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let full = ui.max_rect();
            ui.painter().rect_filled(full, 0.0, c::BG);
            let page = self.page;
            let mut cx = PageCtx { st: &mut self.st, shell: &mut self.shell, frame };
            match page {
                CLIPS => self.library.ui(ui, &mut cx),
                TRIM => self.trim.ui(ui, &mut cx),
                LAYOUT => self.layout.ui(ui, &mut cx),
                SUBTITLES => self.subs.ui(ui, &mut cx),
                _ => self.export.ui(ui, &mut cx),
            }
            let _ = EXPORT;
        });
        // dialogs: the newest is on top and modal
        let n = self.dialog_counter + self.dialogs.len();
        if let Some(d) = self.dialogs.last_mut() {
            if let Some(actions) = d.show(&ctx, &mut self.st, n) {
                self.dialogs.pop();
                self.dialog_counter += 1;
                self.shell.actions.extend(actions);
            }
        }
        self.run_actions(&ctx);
        self.dispatch_state();
        if let Some(wait) = self.st.tick() {
            ctx.request_repaint_after(wait);
        }
        // undo grouping is visible in the buttons ("Undo last change"): keep them right while it settles
    }

    pub fn shutdown(&mut self) {
        self.trim.shutdown();
        self.layout.shutdown();
        self.subs.shutdown();
        self.export.shutdown();
    }
}

fn brand_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    static TEX: std::sync::Mutex<Option<Option<egui::TextureHandle>>> = std::sync::Mutex::new(None);
    let mut g = TEX.lock().unwrap();
    if g.is_none() {
        let t = image::open(paths::assets_dir().join("icon.png")).ok().map(|i| {
            let i = i.resize(80, 80, image::imageops::FilterType::Lanczos3).to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw());
            ctx.load_texture("brand", ci, egui::TextureOptions::LINEAR)
        });
        *g = Some(t);
    }
    g.clone().flatten()
}

// ---------------------------------------------------------------- eframe glue
pub struct ValoApp {
    pub win: MainWindow,
}

impl eframe::App for ValoApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.win.frame_ui(ui, frame);
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        self.win.shutdown();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        c::BG.to_normalized_gamma_f32()
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

pub fn gl_env(cc: &eframe::CreationContext) -> Option<GlEnv> {
    Some(GlEnv { gl: cc.gl.clone()?, get_proc_address: cc.get_proc_address.clone()?, egui: cc.egui_ctx.clone() })
}

pub fn native_options() -> eframe::NativeOptions {
    let mut vp = egui::ViewportBuilder::default()
        .with_title(format!("ValoLights {}", vl_core::VERSION))
        .with_inner_size([1500.0, 920.0])
        .with_min_inner_size([1100.0, 700.0])
        .with_drag_and_drop(true);
    if let Ok(i) = image::open(paths::assets_dir().join("icon.png")) {
        let i = i.resize(256, 256, image::imageops::FilterType::Lanczos3).to_rgba8();
        vp = vp.with_icon(egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() });
    }
    eframe::NativeOptions { viewport: vp, renderer: eframe::Renderer::Glow, ..Default::default() }
}

/// GUI entry.
pub fn run(opts: Options) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("app", false);
    if !paths::is_writable() {
        let _ = rfd::MessageDialog::new()
            .set_title("ValoLights")
            .set_description(format!(
                "Cannot write to {}.\n\nMove the ValoLights folder somewhere you can write to, like Desktop or Documents",
                paths::data_dir().display()
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
        return 1;
    }
    let res = eframe::run_native(
        "ValoLights",
        native_options(),
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let win = MainWindow::new(gl_env(cc), opts);
            Ok(Box::new(ValoApp { win }))
        }),
    );
    match res {
        Ok(()) => 0,
        Err(e) => {
            log::error!("GUI failed: {e}");
            1
        }
    }
}
