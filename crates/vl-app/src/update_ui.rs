//! The update check, download and dialog (see `update` for the release side).
//!
//! A check or download runs on a thread and writes a shared [`Status`]; the top bar and the update dialog
//! read it every frame. On launch the check is silent (the top bar shows "Update to x.y.z" when there is
//! one); "Check now" in Settings opens the dialog right away and shows the result there.
use crate::dialogs::Dialog;
use crate::pages::Action;
use crate::theme::{Role, S2, S4, S6};
use crate::update::{self, Available};
use crate::widgets::ui::{self, Button};
use egui::{Key, Ui};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Checking,
    UpToDate,
    Available(Available),
    Downloading(Available, f64),
    /// downloaded and verified: ready to install
    Ready(Available, PathBuf),
    Failed(String),
}

pub type Shared = Arc<Mutex<Status>>;

fn set(s: &Shared, v: Status, ctx: &egui::Context) {
    *s.lock().unwrap() = v;
    ctx.request_repaint();
}

/// Check for a newer version in the background.
pub fn spawn_check(ctx: &egui::Context) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(Status::Checking));
    let (s, ctx) = (shared.clone(), ctx.clone());
    std::thread::spawn(move || {
        let v = match update::check() {
            Ok(Some(av)) => Status::Available(av),
            Ok(None) => Status::UpToDate,
            Err(e) => {
                log::warn!("update check failed: {e:#}");
                Status::Failed(format!("{e:#}"))
            }
        };
        set(&s, v, &ctx);
    });
    shared
}

/// Download (and verify) the update in the background.
fn spawn_download(shared: &Shared, av: Available, ctx: &egui::Context) {
    set(shared, Status::Downloading(av.clone(), 0.0), ctx);
    let (s, ctx) = (shared.clone(), ctx.clone());
    std::thread::spawn(move || {
        let dir = vl_core::paths::tmp_dir().join("update");
        let progress = {
            let (s, ctx, av) = (s.clone(), ctx.clone(), av.clone());
            move |p: f64| {
                let mut g = s.lock().unwrap();
                if let Status::Downloading(_, old) = &*g {
                    // repaint at most every percent
                    if (p - old).abs() >= 0.01 || p >= 1.0 {
                        *g = Status::Downloading(av.clone(), p);
                        ctx.request_repaint();
                    }
                }
            }
        };
        let v = match update::download(&av, &dir, &progress) {
            Ok(path) => Status::Ready(av, path),
            Err(e) => {
                log::warn!("update download failed: {e:#}");
                Status::Failed(format!("The update could not be downloaded: {e:#}"))
            }
        };
        set(&s, v, &ctx);
    });
}

/// The version the top bar offers, if the launch check found one that isn't skipped.
pub fn offered(shared: &Shared) -> Option<semver::Version> {
    match &*shared.lock().unwrap() {
        Status::Available(av) | Status::Downloading(av, _) | Status::Ready(av, _) => {
            let skipped = vl_core::config::load().updates.skipped;
            (skipped != av.version.to_string()).then(|| av.version.clone())
        }
        _ => None,
    }
}

/// The update dialog's state.
pub struct UpdateDialog {
    pub shared: Shared,
}

impl UpdateDialog {
    pub fn new(shared: Shared) -> UpdateDialog {
        UpdateDialog { shared }
    }
}

/// Draw the update dialog. Some(actions) when it closes.
pub fn dialog_ui(ui: &mut Ui, d: &mut UpdateDialog, esc: bool) -> Option<Vec<Action>> {
    let status = d.shared.lock().unwrap().clone();
    let width = 480.0;
    ui.set_min_width(width);
    ui.set_max_width(width);
    let cur = update::current_version();
    match status {
        Status::Checking => {
            ui::label(ui, "Checking for updates", Role::Title);
            ui.add_space(S4);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui::label(ui, "Asking GitHub for the newest version…", Role::Secondary);
            });
            ui.add_space(S6);
            close_row(ui, "Close", esc)
        }
        Status::UpToDate => {
            ui::label(ui, "You're up to date", Role::Title);
            ui.add_space(S2);
            ui::label_wrap(ui, format!("ValoLights {cur} is the newest version"), Role::Body);
            ui.add_space(S6);
            close_row(ui, "OK", esc)
        }
        Status::Failed(e) => {
            ui::label(ui, "Couldn't check for updates", Role::Title);
            ui.add_space(S2);
            ui::label_wrap(ui, "Check your internet connection and try again later", Role::Body);
            ui.add_space(S2);
            ui::label_wrap(ui, &e, Role::Muted);
            ui.add_space(S6);
            close_row(ui, "OK", esc)
        }
        Status::Available(av) => {
            ui::label(ui, format!("ValoLights {} is available", av.version), Role::Title);
            ui.add_space(S2);
            let when = if av.published.is_empty() { String::new() } else { format!(" · released {}", av.published) };
            ui::label(ui, format!("You have {cur}{when}"), Role::Secondary);
            notes(ui, &av.notes);
            ui.add_space(S6);
            let mut out = None;
            ui.horizontal(|ui| {
                if ui.add(Button::ghost("Skip this version").tooltip("Don't remind me about this version")).clicked() {
                    out = Some(vec![Action::SkipUpdate(av.version.to_string())]);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(Button::new("Later").min_width(72.0)).clicked() || esc {
                        out = Some(vec![]);
                    }
                    if av.installable {
                        let r = ui.add(
                            Button::primary("Update now")
                                .icon("download")
                                .min_width(72.0)
                                .tooltip("Download, then close ValoLights, update and reopen it. Your projects stay"),
                        );
                        if r.clicked() {
                            spawn_download(&d.shared, av.clone(), ui.ctx());
                        }
                    } else if ui.add(Button::primary("Open download page").min_width(72.0)).clicked() {
                        crate::sys::open_url(update::RELEASES_URL);
                        out = Some(vec![]);
                    }
                });
            });
            out
        }
        Status::Downloading(av, p) => {
            ui::label(ui, format!("Downloading ValoLights {}", av.version), Role::Title);
            ui.add_space(S4);
            ui::progress_bar(ui, p as f32, width);
            ui.add_space(S2);
            let mb = av.release.size as f64 / 1_048_576.0;
            ui::label(ui, format!("{:.0}% of {mb:.0} MB · ValoLights closes and reopens when it's installed", p * 100.0), Role::Muted);
            ui.add_space(S6);
            None
        }
        Status::Ready(_, path) => Some(vec![Action::InstallUpdate(path)]),
    }
}

fn notes(ui: &mut Ui, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    ui.add_space(S4);
    ui::label(ui, "What's new", Role::Section);
    ui.add_space(S2);
    egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
        for line in text.lines() {
            let l = line.trim_end();
            if l.is_empty() {
                ui.add_space(S2);
            } else if let Some(h) = l.strip_prefix("### ").or_else(|| l.strip_prefix("## ")) {
                ui::label(ui, h, Role::Section);
            } else if let Some(b) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                ui::label_wrap(ui, format!("•  {b}"), Role::Body);
            } else {
                ui::label_wrap(ui, l, Role::Body);
            }
        }
    });
}

fn close_row(ui: &mut Ui, text: &str, esc: bool) -> Option<Vec<Action>> {
    let mut out = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add(Button::primary(text).min_width(72.0)).clicked() || esc || ui.input(|i| i.key_pressed(Key::Enter)) {
            out = Some(vec![]);
        }
    });
    out
}

/// The dialog for a status (the main window opens it).
pub fn dialog(shared: Shared) -> Dialog {
    Dialog::Update(UpdateDialog::new(shared))
}
