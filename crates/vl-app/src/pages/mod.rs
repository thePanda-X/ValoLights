//! Pages and the contract between a page and the main window. See ARCHITECTURE.md.
//!
//! A page owns its widgets' state (and its own [`Player`](crate::widgets::player::Player) if it has one).
//! Each frame the main window calls [`Page::ui`] for the visible page with a [`PageCtx`]: the shared
//! [`AppState`] (open project, edits, undo), the [`Shell`] (requests to the main window: toasts, dialogs,
//! jobs, navigation) and the eframe `Frame` (needed by the video player). State notifications and job
//! progress are delivered to every page through [`Page::on_state`] / [`Page::on_job`].
#![allow(dead_code)]

pub mod export;
pub mod layout;
pub mod library;
pub mod subtitles;
pub mod trim;

use crate::dialogs::Dialog;
use crate::state::{AppState, StateEvent};
use crate::widgets::player::GlEnv;
use serde_json::Value;
use std::path::PathBuf;

/// The five steps, in navigation order (index = page index).
pub const STEPS: [&str; 5] = ["Clips", "Trim", "Layout", "Subtitles", "Export"];
pub const CLIPS: usize = 0;
pub const TRIM: usize = 1;
pub const LAYOUT: usize = 2;
pub const SUBTITLES: usize = 3;
pub const EXPORT: usize = 4;

/// Requests from pages (and dialogs) to the main window. Pushed with [`Shell::push`] and handled after the
/// frame's UI ran.
#[derive(Debug)]
pub enum Action {
    /// Short message in the top bar (same as `st.say`).
    Toast(String),
    /// Switch to a step (0 = Clips … 4 = Export); the main window applies the navigation rules.
    Go(usize),
    /// Open the "who are you" picker for the open clip (Trim's Change button).
    WhoAreYou,
    /// Start a transcription of the open clip (Subtitles). `only` = re-listen to one source range.
    Transcribe {
        whole: bool,
        only: Option<(f64, f64)>,
    },
    /// Render the 540p layout preview of the open clip (Layout).
    Preview,
    /// Run Smart cut's sound pass on the open clip, then apply the Smart cut (Trim).
    SmartCut,
    /// Export the open clip (Export page): output path + the page's options.
    Render {
        out: String,
        options: Value,
    },
    /// Stop the running transcription / preview / export.
    CancelJob,
    /// Show a modal dialog (confirmations, errors, vocabulary…).
    Dialog(Dialog),
    /// Open the settings dialog.
    Settings,
    // ---- Clips page
    OpenProject(PathBuf),
    AddVideos(Vec<PathBuf>),
    PickVideos,
    Reanalyse(PathBuf),
    CancelAnalysis(PathBuf),
    RemoveProject(PathBuf),
    // ---- internal follow-ups (the second half of a question the user answered)
    AddVideosConfirmed(Vec<PathBuf>),
    ReanalyseConfirmed(PathBuf),
    RemoveConfirmed(PathBuf),
    AnalyseAndOpen(PathBuf),
    TranscribeConfirmed {
        whole: bool,
        only: Option<(f64, f64)>,
    },
    SettingsSaved {
        old: vl_core::config::HighlightsCfg,
        auto: bool,
    },
    /// Look for a newer version now and show the result (Settings' Check now, the top bar's update button).
    CheckUpdates,
    /// Don't offer this version again on launch.
    SkipUpdate(String),
    /// Run the downloaded, verified installer and quit (it reopens the app).
    InstallUpdate(PathBuf),
    /// Export page: the user confirmed "Replace" ("render" or an editor file extension like ".edl").
    ExportConfirmed(String),
    Quit,
}

/// The currently running transcription / preview / export of the open clip.
#[derive(Debug, Clone, Default)]
pub struct JobInfo {
    /// "transcribe" | "proxy" | "render"
    pub kind: String,
    pub p: f64,
    pub msg: String,
    pub eta: Option<f64>,
}

/// App services available to pages. Pages never touch the main window directly.
pub struct Shell {
    pub actions: Vec<Action>,
    /// OpenGL access for video players (None without GL, e.g. in tests).
    pub gl: Option<GlEnv>,
    /// The running job of the open clip, if any (read-only for pages; updated every frame).
    pub job: Option<JobInfo>,
}

impl Shell {
    pub fn new(gl: Option<GlEnv>) -> Self {
        Shell { actions: vec![], gl, job: None }
    }
    pub fn push(&mut self, a: Action) {
        self.actions.push(a);
    }
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.actions.push(Action::Toast(msg.into()));
    }
    pub fn dialog(&mut self, d: Dialog) {
        self.actions.push(Action::Dialog(d));
    }
    /// Is a job of this kind running for the open clip?
    pub fn job_running(&self, kind: &str) -> bool {
        self.job.as_ref().map(|j| j.kind == kind).unwrap_or(false)
    }
}

/// Everything a page gets while drawing.
pub struct PageCtx<'a> {
    pub st: &'a mut AppState,
    pub shell: &'a mut Shell,
    pub frame: &'a mut eframe::Frame,
}

/// Progress of a job that belongs to a page.
/// Transcribe → Subtitles, proxy → Layout, render → Export.
#[derive(Debug, Clone, PartialEq)]
pub struct JobNotice {
    pub kind: String,
    /// Some(fraction) while running, None when it ended (then `msg` is the final status text).
    pub progress: Option<f64>,
    pub msg: String,
    /// the worker's result when it finished successfully (e.g. {"out": "...mp4"})
    pub result: Option<Value>,
}

pub trait Page {
    /// A state notification (project opened, edits / events / transcript changed, undo restored…).
    /// Pages refresh their widgets from `st` here.
    fn on_state(&mut self, _ev: &StateEvent, _st: &mut AppState, _shell: &mut Shell) {}
    /// Progress / end of this page's job (see [`JobNotice`]).
    fn on_job(&mut self, _n: &JobNotice, _st: &mut AppState, _shell: &mut Shell) {}
    /// The page became visible / hidden.
    fn on_show(&mut self, _st: &mut AppState, _shell: &mut Shell) {}
    fn on_hide(&mut self, _st: &mut AppState, _shell: &mut Shell) {}
    /// Draw the page into the central area.
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx);
    /// Stop playback (the clip is being closed or removed).
    fn stop_players(&mut self) {}
    /// Free players (window closing; GL is current).
    fn shutdown(&mut self) {}
}

/// Placeholder body for pages that are not built yet.
pub fn placeholder(ui: &mut egui::Ui, title: &str, text: &str) {
    use crate::theme::{Role, S6, S8};
    use crate::widgets::ui;
    egui::Frame::new().inner_margin(egui::Margin { left: S8 as i8, right: S8 as i8, top: (S6 + 8.0) as i8, bottom: S6 as i8 }).show(
        ui,
        |ui| {
            ui::label(ui, title, Role::Title);
            ui.add_space(4.0);
            ui::hint(ui, text);
        },
    );
}
