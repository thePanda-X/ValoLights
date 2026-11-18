//! Modal dialogs: confirmations, errors with details, first run, "who are you", settings, vocabulary.
//!
//! A dialog is a value: pages push `Action::Dialog(Dialog::confirm(...))`; the main window shows the
//! newest one as a modal and runs the actions it returns when it closes (instead of a blocking modal loop).
#![allow(dead_code)]

use crate::pages::Action;
use crate::state::AppState;
use crate::theme::{self, c, Role, S2, S3, S4, S6};
use crate::widgets::ui::{self, BtnRole, Button, Toggle};
use egui::{vec2, Color32, Key, TextureHandle, Ui};
use std::collections::HashMap;
use vl_core::config;

/// Comma separated Riot names. The killfeed shows names without the #tag, so a pasted tag is dropped.
pub fn parse_aliases(s: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for a in s.split(',') {
        let a = a.split('#').next().unwrap_or("").trim();
        if !a.is_empty() && !out.iter().any(|o| o.to_lowercase() == a.to_lowercase()) {
            out.push(a.to_string());
        }
    }
    out
}

fn aliases_text(cfg: &config::Config) -> String {
    cfg.identity.aliases.join(", ")
}

pub use vl_core::speech_words::DEFAULT_VOCAB;

pub struct Confirm {
    pub title: String,
    pub text: String,
    pub yes: String,
    pub default_yes: bool,
    pub on_yes: Vec<Action>,
    pub on_no: Vec<Action>,
}

pub struct Message {
    pub title: String,
    pub text: String,
}

pub struct Fail {
    pub title: String,
    pub text: String,
    pub detail: String,
    pub show_details: bool,
}

pub struct WhoRow {
    pub id: String,
    pub display: String,
    pub crop: Option<std::path::PathBuf>,
    pub kills: usize,
    pub deaths: usize,
    pub on: bool,
}

pub struct WhoAreYou {
    pub rows: Vec<WhoRow>,
    pub current: usize,
    pub remember: bool,
    textures: HashMap<String, Option<TextureHandle>>,
}

pub struct Settings {
    pub aliases: String,
    pub pre: f64,
    pub post: f64,
    pub gap: f64,
    pub best: f64,
    pub team: bool,
    pub model: String,
    pub device: String,
    pub fps: f64,
    pub check_updates: bool,
    /// the highlight settings before editing, and whether the open cut was automatic
    pub old: config::HighlightsCfg,
    pub auto: bool,
}

pub struct FirstRun {
    pub aliases: String,
    pub gpu: bool,
}

pub struct Vocabulary {
    pub text: String,
}

pub enum Dialog {
    Confirm(Confirm),
    Message(Message),
    Fail(Fail),
    WhoAreYou(WhoAreYou),
    Settings(Settings),
    FirstRun(FirstRun),
    Vocabulary(Vocabulary),
    Update(crate::update_ui::UpdateDialog),
}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dialog({})", self.title())
    }
}

impl Dialog {
    /// Question box with a verb on the confirm button instead of a bare Yes.
    pub fn confirm(title: &str, text: &str, yes: &str, default_yes: bool, on_yes: Vec<Action>) -> Dialog {
        Dialog::Confirm(Confirm { title: title.into(), text: text.into(), yes: yes.into(), default_yes, on_yes, on_no: vec![] })
    }

    /// Plain information / warning box with OK.
    pub fn message(title: &str, text: &str) -> Dialog {
        Dialog::Message(Message { title: title.into(), text: text.into() })
    }

    /// Error box: a plain sentence up front, the technical text behind Show details.
    pub fn fail(title: &str, text: &str, detail: &str) -> Dialog {
        Dialog::Fail(Fail { title: title.into(), text: text.into(), detail: detail.trim().to_string(), show_details: false })
    }

    /// Pick which killfeed names are you (shows the name crops so unreadable names still work).
    pub fn who_are_you(st: &AppState) -> Dialog {
        let me = st.me();
        let mut rows = vec![];
        if let (Some(ev), Some(h)) = (&st.ev, &st.h) {
            let mut players = ev.players.clone();
            players.sort_by_key(|p| std::cmp::Reverse(p.seen));
            for p in players {
                let kills = ev.kills.iter().filter(|k| k.killer == p.id && k.killer != k.victim).count();
                let deaths = ev.kills.iter().filter(|k| k.victim == p.id).count();
                rows.push(WhoRow {
                    on: me.contains(&p.id),
                    crop: p.crop.as_ref().map(|c| h.dir.join(c)),
                    display: p.display.clone(),
                    id: p.id,
                    kills,
                    deaths,
                });
            }
        }
        Dialog::WhoAreYou(WhoAreYou { rows, current: 0, remember: true, textures: HashMap::new() })
    }

    pub fn settings(old: config::HighlightsCfg, auto: bool) -> Dialog {
        let cfg = config::load();
        Dialog::Settings(Settings {
            aliases: aliases_text(&cfg),
            pre: cfg.highlights.pre_roll,
            post: cfg.highlights.post_roll,
            gap: cfg.highlights.merge_gap,
            best: cfg.highlights.best_of_target,
            team: cfg.highlights.include_team_kills,
            model: cfg.subtitles.model.clone(),
            device: cfg.subtitles.device.clone(),
            fps: cfg.analysis.sample_fps,
            check_updates: cfg.updates.check_on_launch,
            old,
            auto,
        })
    }

    pub fn first_run() -> Dialog {
        Dialog::FirstRun(FirstRun { aliases: aliases_text(&config::load()), gpu: crate::sys::nvidia_driver_present() })
    }

    /// Edit data/vocabulary.txt: names and callouts the speech model should expect.
    pub fn vocabulary() -> Dialog {
        let p = vl_core::paths::data_dir().join("vocabulary.txt");
        if !p.exists() {
            let _ = std::fs::create_dir_all(vl_core::paths::data_dir());
            let _ = std::fs::write(&p, DEFAULT_VOCAB);
        }
        Dialog::Vocabulary(Vocabulary { text: std::fs::read_to_string(&p).unwrap_or_else(|_| DEFAULT_VOCAB.into()) })
    }

    pub fn title(&self) -> &str {
        match self {
            Dialog::Confirm(d) => &d.title,
            Dialog::Message(d) => &d.title,
            Dialog::Fail(d) => &d.title,
            Dialog::WhoAreYou(_) => "Who are you",
            Dialog::Settings(_) => "Settings",
            Dialog::FirstRun(_) => "Welcome to ValoLights",
            Dialog::Vocabulary(_) => "Words it should know",
            Dialog::Update(_) => "Updates",
        }
    }

    /// Draw the dialog as a modal. Returns Some(actions to run) once it closed.
    pub fn show(&mut self, ctx: &egui::Context, st: &mut AppState, n: usize) -> Option<Vec<Action>> {
        let mut out: Option<Vec<Action>> = None;
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let id = egui::Id::unique(("vl-dialog", n));
        egui::Modal::new(id)
            .area(egui::Modal::default_area(id).fade_in(false))
            .frame(theme::dialog_frame())
            .backdrop_color(Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                out = match self {
                    Dialog::Confirm(d) => confirm_ui(ui, d, esc),
                    Dialog::Message(d) => message_ui(ui, d, esc),
                    Dialog::Fail(d) => fail_ui(ui, d, esc),
                    Dialog::WhoAreYou(d) => who_ui(ui, d, st, esc),
                    Dialog::Settings(d) => settings_ui(ui, d, st, esc),
                    Dialog::FirstRun(d) => first_run_ui(ui, d, st),
                    Dialog::Vocabulary(d) => vocab_ui(ui, d, esc),
                    Dialog::Update(d) => crate::update_ui::dialog_ui(ui, d, esc),
                };
            });
        out
    }
}

// ------------------------------------------------------------------ pieces
fn heading(ui: &mut Ui, title: &str, width: f32) {
    ui.set_min_width(width);
    ui.set_max_width(width);
    ui::label(ui, title, Role::Title);
    ui.add_space(S2);
}

/// Right-aligned [primary][Cancel] row (Windows button order). Enter = the default button.
fn buttons(ui: &mut Ui, ok: &str, ok_enabled: bool, cancel: Option<&str>, default_ok: bool) -> (bool, bool) {
    let (mut okc, mut cc) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if let Some(ct) = cancel {
            if ui.add(Button::new(ct).min_width(72.0)).clicked() {
                cc = true;
            }
        }
        if ui.add(Button::primary(ok).min_width(72.0).enabled(ok_enabled)).clicked() {
            okc = true;
        }
    });
    let typing_multiline = false;
    if !typing_multiline && ui.input(|i| i.key_pressed(Key::Enter)) && !ui.ctx().text_edit_focused() {
        if default_ok && ok_enabled {
            okc = true;
        } else if !default_ok && cancel.is_some() {
            cc = true;
        }
    }
    (okc, cc)
}

fn confirm_ui(ui: &mut Ui, d: &mut Confirm, esc: bool) -> Option<Vec<Action>> {
    heading(ui, &d.title, 440.0);
    ui::label_wrap(ui, &d.text, Role::Body);
    ui.add_space(S6);
    let (yes, no) = buttons(ui, &d.yes, true, Some("Cancel"), d.default_yes);
    if yes {
        return Some(std::mem::take(&mut d.on_yes));
    }
    if no || esc {
        return Some(std::mem::take(&mut d.on_no));
    }
    None
}

fn message_ui(ui: &mut Ui, d: &mut Message, esc: bool) -> Option<Vec<Action>> {
    heading(ui, &d.title, 420.0);
    ui::label_wrap(ui, &d.text, Role::Body);
    ui.add_space(S6);
    let (ok, _) = buttons(ui, "OK", true, None, true);
    (ok || esc).then(Vec::new)
}

fn fail_ui(ui: &mut Ui, d: &mut Fail, esc: bool) -> Option<Vec<Action>> {
    heading(ui, &d.title, 480.0);
    ui::label_wrap(ui, &d.text, Role::Body);
    if !d.detail.is_empty() {
        ui.add_space(S2);
        let last: String = d.detail.lines().last().unwrap_or("").chars().take(300).collect();
        ui::label_wrap(ui, &last, Role::Muted);
        ui.add_space(S3);
        let tip = if d.show_details { "Hide details" } else { "Show details" };
        if ui.add(Button::ghost(tip).icon(if d.show_details { "chevron-down" } else { "chevron-right" })).clicked() {
            d.show_details = !d.show_details;
        }
        if d.show_details {
            ui.add_space(S2);
            let n = d.detail.chars().count();
            let mut tail: String = d.detail.chars().skip(n.saturating_sub(4000)).collect();
            egui::Frame::new().fill(c::RAISED).corner_radius(8).inner_margin(8).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut tail)
                            .font(theme::mono_font(11.5))
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE)
                            .interactive(true),
                    );
                });
            });
        }
    }
    ui.add_space(S6);
    let (ok, _) = buttons(ui, "OK", true, None, true);
    (ok || esc).then(Vec::new)
}

fn crop_texture(ctx: &egui::Context, path: &std::path::Path) -> Option<TextureHandle> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
    Some(ctx.load_texture(format!("crop-{}", path.display()), ci, egui::TextureOptions::LINEAR))
}

fn who_ui(ui: &mut Ui, d: &mut WhoAreYou, st: &mut AppState, esc: bool) -> Option<Vec<Action>> {
    heading(ui, "Who are you in this clip?", 560.0);
    ui::label_wrap(
        ui,
        "Names as they appeared in the killfeed, most active first. Tick every name that is you, for example \u{201c}Me\u{201d} and your account name",
        Role::Secondary,
    );
    ui.add_space(S4);
    // keyboard: up/down move the current row, Space ticks it
    let n = d.rows.len();
    if n > 0 && !ui.ctx().text_edit_focused() {
        let (up, down, space) = ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::Space)));
        if up {
            d.current = d.current.saturating_sub(1);
        }
        if down {
            d.current = (d.current + 1).min(n - 1);
        }
        if space {
            d.rows[d.current].on = !d.rows[d.current].on;
        }
    }
    let list_h = (n as f32 * 41.0).clamp(41.0, 360.0);
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        let w = ui.available_width();
        for (i, row) in d.rows.iter_mut().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), egui::Sense::click());
            let bg = if i == d.current {
                c::RAISED
            } else if resp.hovered() {
                c::CARD
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, 8.0, bg);
            if resp.clicked() {
                row.on = !row.on; // the whole row toggles, not only the small switch
                d.current = i;
            }
            let mut x = rect.left() + S3;
            let t = ui.ctx().animate_bool_with_time_and_easing(
                egui::Id::unique(("who-toggle", &row.id)),
                row.on,
                0.14,
                egui::emath::easing::cubic_out,
            );
            let sw = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - ui::SWITCH_H / 2.0), vec2(ui::SWITCH_W, ui::SWITCH_H));
            ui::paint_switch(ui.painter(), sw, t, true, resp.hovered(), false);
            x += ui::SWITCH_W + S3;
            let img_r = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 12.0), vec2(144.0, 24.0));
            if let Some(p) = &row.crop {
                let tex = d.textures.entry(row.id.clone()).or_insert_with(|| crop_texture(ui.ctx(), p)).clone();
                if let Some(t) = tex {
                    let s = t.size_vec2();
                    let k = (144.0 / s.x).min(24.0 / s.y);
                    let r = egui::Rect::from_min_size(img_r.left_center() - vec2(0.0, s.y * k / 2.0), s * k);
                    ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
            x += 144.0 + S3;
            let kd = format!("{} K · {} D", row.kills, row.deaths);
            let kd_g = ui.fonts_mut(|f| f.layout_no_wrap(kd, Role::Mono.font(), c::MUTED));
            let kd_x = rect.right() - S3 - kd_g.size().x;
            ui.painter().galley(egui::pos2(kd_x, rect.center().y - kd_g.size().y / 2.0), kd_g, c::MUTED);
            let name_w = (kd_x - S3 - x).min(200.0);
            let g = ui::elided(ui, &row.display, Role::Body.font(), c::TEXT, name_w);
            ui.painter().galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, c::TEXT);
            resp.on_hover_text(&row.display);
        }
    });
    if d.rows.is_empty() {
        ui::label_wrap(ui, "No names could be read from the killfeed in this clip", Role::Muted);
    }
    ui.add_space(S4);
    ui.add(Toggle::new(&mut d.remember).text("Remember these names for future clips"));
    ui.add_space(S6);
    let ids: Vec<String> = d.rows.iter().filter(|r| r.on).map(|r| r.id.clone()).collect();
    let mut result = None;
    ui.horizontal(|ui| {
        let k = ids.len();
        let hint = if k > 0 { format!("{k} name{} ticked", if k != 1 { "s" } else { "" }) } else { "Tick at least one name".into() };
        ui::label(ui, hint, Role::Muted);
        let (ok, cancel) = buttons(ui, "Use these names", k > 0, Some("Cancel"), true);
        if cancel || esc {
            result = Some(vec![]);
        } else if ok && k > 0 {
            let me = st.me();
            let confirmed = st.h.as_ref().map(|h| h.project.identity.confirmed).unwrap_or(false);
            let same = ids.len() == me.len() && ids.iter().all(|i| me.contains(i));
            if !same || !confirmed {
                st.set_me(ids.clone()); // rebuilds the cut from your kills, so only when something changed
            }
            if d.remember && !st.read_only {
                if let Some(ev) = &st.ev {
                    let mut cfg = config::load();
                    for pid in &ids {
                        if let Some(p) = ev.player(pid) {
                            let disp = &p.display;
                            if !disp.is_empty()
                                && disp.to_lowercase() != "me"
                                && disp.chars().count() >= 3
                                && disp != "?"
                                && !cfg.identity.aliases.contains(disp)
                            {
                                cfg.identity.aliases.push(disp.clone());
                            }
                        }
                    }
                    if let Err(e) = cfg.save() {
                        log::error!("saving settings failed: {e}");
                    }
                }
            }
            result = Some(vec![]);
        }
    });
    result
}

fn form_row(ui: &mut Ui, text: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(200.0, 30.0), egui::Sense::hover());
        let g = ui::elided(ui, text, Role::Secondary.font(), c::SECONDARY, 200.0);
        ui.painter().galley(egui::pos2(r.left(), r.center().y - g.size().y / 2.0), g, c::SECONDARY);
        ui.add_space(S4 - 8.0);
        add(ui);
    });
    ui.add_space(S3);
}

fn settings_ui(ui: &mut Ui, d: &mut Settings, st: &mut AppState, esc: bool) -> Option<Vec<Action>> {
    heading(ui, "Settings", 560.0);
    ui.add_space(S4);
    let field_w = 560.0 - 200.0 - S4;
    ui::section(ui, "You");
    ui.add_space(S2);
    form_row(ui, "Your names", |ui| {
        ui::text_input(ui, &mut d.aliases, "Riot names, separated by commas", field_w);
    });
    ui::hint(ui, "Used for clips you add from now on. For other clips, use Change in Trim");
    ui.add_space(S6);

    ui::section(ui, "Highlights");
    ui.add_space(S2);
    form_row(ui, "Lead-in before a highlight", |ui| {
        ui::spin(ui, &mut d.pre, 0.5, 15.0, 1, " s", field_w);
    });
    form_row(ui, "Tail after a highlight", |ui| {
        ui::spin(ui, &mut d.post, 0.2, 10.0, 1, " s", field_w);
    });
    form_row(ui, "Join kills closer than", |ui| {
        ui::spin(ui, &mut d.gap, 1.0, 30.0, 1, " s", field_w);
    });
    form_row(ui, "Best moments length", |ui| {
        ui::spin(ui, &mut d.best, 10.0, 600.0, 1, " s", field_w);
    });
    ui.add(Toggle::new(&mut d.team).text("Include teammates' kills while spectating"));
    ui.add_space(S6);

    ui::section(ui, "Speech and analysis");
    ui.add_space(S2);
    form_row(ui, "Speech model", |ui| {
        ui::combo(
            ui,
            "set-model",
            &mut d.model,
            &[
                ("large-v3-turbo".to_string(), "Most accurate · 0.6 GB"),
                ("small".to_string(), "Faster · 0.2 GB"),
                ("base".to_string(), "Fastest · 0.06 GB"),
            ],
            field_w - 16.0,
        );
    });
    form_row(ui, "Speech runs on", |ui| {
        ui::combo(
            ui,
            "set-dev",
            &mut d.device,
            &[("auto".to_string(), "Automatic"), ("cuda".to_string(), "NVIDIA GPU"), ("cpu".to_string(), "CPU")],
            field_w - 16.0,
        );
    });
    form_row(ui, "Analysis samples", |ui| {
        ui::spin(ui, &mut d.fps, 2.0, 15.0, 1, " per second", field_w);
    });
    ui::hint(ui, "Analysis changes apply to newly added clips, or right-click a clip and analyse it again");
    ui.add_space(S6);

    ui::section(ui, "Updates");
    ui.add_space(S2);
    let mut check_now = false;
    ui.horizontal(|ui| {
        ui::label(ui, format!("ValoLights {}", env!("CARGO_PKG_VERSION")), Role::Mono);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(Button::new("Check now").icon("download").tooltip("Look for a newer version now")).clicked() {
                check_now = true;
            }
        });
    });
    ui.add_space(S2);
    ui.add(Toggle::new(&mut d.check_updates).text("Check for updates when ValoLights starts"));
    if check_now {
        return Some(vec![Action::CheckUpdates]);
    }
    ui.add_space(S6);
    ui::hline(ui);
    ui.add_space(S4);
    ui.horizontal(|ui| {
        let dpath = vl_core::paths::data_dir().to_string_lossy().into_owned();
        let shown = ui::elide_middle(ui, &format!("Data folder  {dpath}"), &Role::Mono.font(), 400.0);
        ui::label(ui, shown, Role::Mono).on_hover_text(&dpath);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(Button::ghost("Open").icon("folder-open").tooltip("Open the data folder")).clicked() {
                crate::sys::open_path(&vl_core::paths::data_dir());
            }
        });
    });
    ui.add_space(S6);
    let (ok, cancel) = buttons(ui, "Save", true, Some("Cancel"), true);
    if cancel || esc {
        return Some(vec![]);
    }
    if ok {
        let mut cfg = config::load();
        cfg.identity.aliases = parse_aliases(&d.aliases);
        cfg.highlights.pre_roll = d.pre;
        cfg.highlights.post_roll = d.post;
        cfg.highlights.merge_gap = d.gap;
        cfg.highlights.best_of_target = d.best;
        cfg.highlights.include_team_kills = d.team;
        cfg.subtitles.model = d.model.clone();
        cfg.subtitles.device = d.device.clone();
        cfg.analysis.sample_fps = d.fps;
        cfg.updates.check_on_launch = d.check_updates;
        if st.read_only {
            st.say("Read-only mode: settings were not saved");
            return Some(vec![]);
        }
        if let Err(e) = cfg.save() {
            return Some(vec![Action::Dialog(Dialog::fail("Settings", "The settings could not be saved", &e.to_string()))]);
        }
        return Some(vec![Action::SettingsSaved { old: d.old.clone(), auto: d.auto }]);
    }
    None
}

fn first_run_ui(ui: &mut Ui, d: &mut FirstRun, st: &mut AppState) -> Option<Vec<Action>> {
    heading(ui, "Which names are you?", 520.0);
    ui::label_wrap(
        ui,
        "ValoLights finds your kills by the name in the killfeed. Enter your Riot names, separated by commas if you have several accounts",
        Role::Secondary,
    );
    ui.add_space(S4);
    ui::text_input(ui, &mut d.aliases, "MainAccount, MySmurf", 520.0);
    ui.add_space(S2);
    ui::label_wrap(ui, "With hide names on you appear as \u{201c}Me\u{201d}, which is recognised automatically", Role::Muted);
    ui.add_space(S6);
    ui::hline(ui);
    ui.add_space(S4);
    ui::label_wrap(
        ui,
        if d.gpu {
            "NVIDIA GPU found, speech-to-text runs on the GPU"
        } else {
            "No usable NVIDIA GPU, everything still works on the CPU, only slower"
        },
        Role::Mono,
    );
    ui.add_space(S6);
    let (ok, _) = buttons(ui, "Continue", true, None, true);
    if ok {
        if !st.read_only {
            let mut cfg = config::load();
            cfg.identity.aliases = parse_aliases(&d.aliases);
            cfg.first_run_done = true;
            if let Err(e) = cfg.save() {
                log::error!("saving settings failed: {e}");
            }
        }
        return Some(vec![]);
    }
    None
}

fn vocab_ui(ui: &mut Ui, d: &mut Vocabulary, esc: bool) -> Option<Vec<Action>> {
    heading(ui, "Words it should know", 560.0);
    ui::label_wrap(
        ui,
        "Friends' names, nicknames, callouts and slang. Comma separated or one per line. Player names from the killfeed are added automatically. Applies to the next transcription",
        Role::Secondary,
    );
    ui.add_space(S3);
    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
        ui::text_area(ui, &mut d.text, 14, 560.0);
    });
    ui.add_space(S3);
    let (ok, cancel) = buttons(ui, "Save", true, Some("Cancel"), true);
    if cancel || esc {
        return Some(vec![]);
    }
    if ok {
        let p = vl_core::paths::data_dir().join("vocabulary.txt");
        if let Err(e) = std::fs::write(&p, &d.text) {
            return Some(vec![Action::Dialog(Dialog::fail("Words it should know", "The word list could not be saved", &e.to_string()))]);
        }
        return Some(vec![]);
    }
    None
}

#[allow(unused)]
fn _role(_: BtnRole) {}

#[cfg(test)]
mod tests {
    #[test]
    fn aliases() {
        assert_eq!(super::parse_aliases("Main#EUW, smurf ,MAIN, , x#1"), vec!["Main", "smurf", "x"]);
    }
}
