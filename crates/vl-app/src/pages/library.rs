//! Clips: add videos, see analysis progress, open projects.
use super::{Action, Page, PageCtx, Shell};
use crate::dialogs::Dialog;
use crate::icons;
use crate::state::AppState;
use crate::theme::{self, c, Role, Weight, S1, S2, S3, S4, S6, S8};
use crate::widgets::ui::{self, Button};
use egui::{pos2, vec2, Color32, Id, Key, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use vl_core::projects::{self, ProjectHandle};

pub const VIDEO_EXT: [&str; 6] = ["mp4", "mkv", "mov", "avi", "webm", "m4v"];
pub const CARD_W: f32 = 296.0;
pub const THUMB_H: f32 = 166.0; // 16:9 of the inner width
pub const DROP_TEXT: &str = "Drop a Valorant recording here, or click to choose one";

pub fn fmt_dur(s: f64) -> String {
    let s = s.max(0.0) as i64;
    let (m, s) = (s / 60, s % 60);
    let (h, m) = (m / 60, m % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "Added" stamp so several takes of the same recording can be told apart.
pub fn fmt_added(iso: &str, now: chrono::NaiveDateTime) -> String {
    let Ok(t) = chrono::NaiveDateTime::parse_from_str(iso.split('.').next().unwrap_or(iso), "%Y-%m-%dT%H:%M:%S") else {
        return String::new();
    };
    use chrono::Datelike;
    if t.date() == now.date() {
        return format!("today {}", t.format("%H:%M"));
    }
    if Some(t.date()) == now.date().pred_opt() {
        return format!("yesterday {}", t.format("%H:%M"));
    }
    if t.year() == now.year() {
        format!("{} {}", t.day(), t.format("%b %H:%M"))
    } else {
        format!("{} {}", t.day(), t.format("%b %Y"))
    }
}

pub fn video_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()).map(|e| VIDEO_EXT.contains(&e.to_lowercase().as_str())).unwrap_or(false))
        .cloned()
        .collect()
}

pub fn let_go(n: usize) -> String {
    format!("Let go to add {n} video{}", if n != 1 { "s" } else { "" })
}

/// Pick videos with the native file dialog.
pub fn pick_videos(title: &str, start: Option<&Path>, many: bool) -> Vec<PathBuf> {
    let mut d = rfd::FileDialog::new().set_title(title).add_filter("Videos", &VIDEO_EXT);
    if let Some(s) = start {
        d = d.set_directory(s);
    }
    if many {
        d.pick_files().unwrap_or_default()
    } else {
        d.pick_file().into_iter().collect()
    }
}

/// Thumbnail with pillarbox bars trimmed: the texture + the uv rect that covers 16:9.
fn load_thumb(ctx: &egui::Context, path: &Path, w: f32, h: f32) -> Option<(TextureHandle, Rect)> {
    let img = image::open(path).ok()?.to_rgb8();
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    if iw < 4 || ih < 4 {
        return None;
    }
    let dark = |x: i64| {
        let p = img.get_pixel(x as u32, (ih / 2) as u32);
        (p[0] as u32 + p[1] as u32 + p[2] as u32) < 30
    };
    let (mut x0, mut x1) = (0i64, iw - 1);
    while x0 < iw / 4 && dark(x0) {
        x0 += 1;
    }
    while x1 > iw * 3 / 4 && dark(x1) {
        x1 -= 1;
    }
    let cw = (x1 - x0 + 1) as f32;
    // cover: crop the trimmed image to the frame's aspect, centred
    let (fa, ia) = (w / h, cw / ih as f32);
    let (mut u0, mut u1, mut v0, mut v1) = (x0 as f32 / iw as f32, (x1 + 1) as f32 / iw as f32, 0.0, 1.0);
    if ia > fa {
        let keep = fa / ia;
        let mid = (u0 + u1) / 2.0;
        let half = (u1 - u0) * keep / 2.0;
        u0 = mid - half;
        u1 = mid + half;
    } else {
        let keep = ia / fa;
        v0 = 0.5 - keep / 2.0;
        v1 = 0.5 + keep / 2.0;
    }
    let ci = egui::ColorImage::from_rgb([iw as usize, ih as usize], img.as_raw());
    let tex = ctx.load_texture(format!("thumb-{}", path.display()), ci, egui::TextureOptions::LINEAR);
    Some((tex, Rect::from_min_max(pos2(u0, v0), pos2(u1, v1))))
}

/// One clip card's cached content.
pub struct Card {
    pub h: ProjectHandle,
    pub meta: String,
    pub tip: String,
    pub added: String,
    pub source_ok: bool,
    thumb: Option<Option<(TextureHandle, Rect)>>,
    /// where it was drawn last frame (for keyboard navigation and the UI check)
    pub rect: Rect,
}

impl Card {
    fn new(h: ProjectHandle) -> Self {
        let mut c =
            Card { h, meta: String::new(), tip: String::new(), added: String::new(), source_ok: false, thumb: None, rect: Rect::NOTHING };
        c.refresh();
        c
    }

    /// Status line + tooltip.
    pub fn refresh(&mut self) {
        let p = &self.h.project;
        self.added = fmt_added(&p.created, chrono::Local::now().naive_local());
        self.source_ok = self.h.source_ok();
        let dur = fmt_dur(p.source.duration);
        let mut tip = vec![p.name.clone()];
        let text = if !self.source_ok {
            tip.push(format!("Not found: {}\nRight-click and choose Locate source video", p.source.path));
            format!("{dur} · source video missing")
        } else if p.status == "ready" {
            let me = &p.identity.me;
            let ev = self.h.events();
            let mine: Vec<_> = ev
                .as_ref()
                .map(|e| e.kills.iter().filter(|k| me.contains(&k.killer) && k.killer != k.victim).cloned().collect())
                .unwrap_or_default();
            let hs = mine.iter().filter(|k| k.headshot).count();
            let n = mine.len();
            if me.is_empty() {
                format!("{dur} · pick who you are")
            } else {
                format!("{dur} · {n} kill{} · {hs} HS", if n != 1 { "s" } else { "" })
            }
        } else if p.status == "error" {
            if !p.error.is_empty() {
                let e: Vec<char> = p.error.chars().collect();
                tip.push(e[e.len().saturating_sub(300)..].iter().collect());
            }
            tip.push("Click to try again".into());
            format!("{dur} · analysis failed")
        } else if p.status == "analyzing" {
            tip.push("The analysis was stopped before it finished. Click to analyse it again".into());
            format!("{dur} · analysis not finished")
        } else {
            format!("{dur} · not analysed yet")
        };
        self.meta = text;
        self.tip = tip.join("\n");
    }
}

pub struct LibraryPage {
    pub cards: Vec<Card>,
    /// analysing or queued clips: last progress shown
    pub live: HashMap<String, (f64, String)>,
    pub current_pid: Option<String>,
    /// files dragged over the window (0 = none)
    pub drag_n: usize,
    /// open context menu: (project id, screen position)
    menu: Option<(String, Pos2)>,
    focus_req: Option<usize>,
    pub add_rect: Rect,
}

impl LibraryPage {
    pub fn new(_shell: &Shell) -> Self {
        LibraryPage {
            cards: vec![],
            live: HashMap::new(),
            current_pid: None,
            drag_n: 0,
            menu: None,
            focus_req: None,
            add_rect: Rect::NOTHING,
        }
    }

    pub fn reload(&mut self) {
        let old: HashMap<String, Option<(TextureHandle, Rect)>> =
            self.cards.drain(..).filter_map(|c| c.thumb.map(|t| (c.h.project.id.clone(), t))).collect();
        let mut old = old;
        self.cards = projects::list_all()
            .into_iter()
            .map(|h| {
                let id = h.project.id.clone();
                let mut c = Card::new(h);
                c.thumb = old.remove(&id);
                c
            })
            .collect();
        let ids: Vec<String> = self.cards.iter().map(|c| c.h.project.id.clone()).collect();
        self.live.retain(|k, _| ids.contains(k));
        if let Some((pid, _)) = &self.menu {
            if !ids.contains(pid) {
                self.menu = None;
            }
        }
    }

    pub fn progress(&mut self, pid: &str, p: f64, msg: &str) {
        self.live.insert(pid.to_string(), (p, msg.to_string()));
    }

    pub fn refresh_card(&mut self, pid: &str) {
        self.live.remove(pid);
        if let Some(c) = self.cards.iter_mut().find(|c| c.h.project.id == pid) {
            if let Ok(h) = projects::open_dir(&c.h.dir) {
                c.h = h;
            }
            c.thumb = None; // the thumbnail may have been made meanwhile
            c.refresh();
        }
    }

    pub fn set_current(&mut self, pid: Option<&str>) {
        self.current_pid = pid.map(str::to_string);
    }

    pub fn card(&self, pid: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.h.project.id == pid)
    }

    fn count_text(&self) -> String {
        let n = self.cards.len();
        if self.drag_n > 0 {
            return let_go(self.drag_n);
        }
        if n > 0 {
            format!("{n} clip{} · drop more videos anywhere in this window", if n != 1 { "s" } else { "" })
        } else {
            "Your recordings are analysed on this PC and never uploaded".into()
        }
    }

    fn cols(width: f32) -> usize {
        (((width + S4) / (CARD_W + S4)).floor() as usize).max(1)
    }

    fn card_ui(&mut self, ui: &mut Ui, i: usize, shell: &mut Shell) {
        let id = Id::unique(("clip-card", &self.cards[i].h.project.id));
        let inner_w = CARD_W - 2.0 * S2 - 2.0;
        let card_h = 1.0 + S2 + THUMB_H + S3 + 18.0 + S1 + 16.0 + S2 + 4.0 + S3 + 2.0 + 1.0;
        let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, card_h), Sense::hover());
        let resp = ui.interact(rect, id, Sense::click());
        if self.focus_req == Some(i) {
            resp.request_focus();
            self.focus_req = None;
        }
        let card = &mut self.cards[i];
        card.rect = rect;
        let pid = card.h.project.id.clone();
        let live = self.live.get(&pid).cloned();
        let current = self.current_pid.as_deref() == Some(pid.as_str());
        let p = ui.painter();
        p.rect_filled(rect, 12.0, c::CARD);
        let border = if current {
            c::ACCENT
        } else if resp.has_focus() {
            c::SECONDARY
        } else if resp.hovered() {
            c::HAIR_STRONG
        } else {
            c::HAIR
        };
        p.rect_stroke(rect, 12.0, Stroke::new(1.0, border), StrokeKind::Inside);
        // thumbnail
        let tr = Rect::from_min_size(rect.min + vec2(1.0 + S2, 1.0 + S2), vec2(inner_w, THUMB_H));
        if card.thumb.is_none() {
            card.thumb = Some(load_thumb(ui.ctx(), &card.h.thumb_path(), inner_w, THUMB_H));
        }
        match card.thumb.as_ref().unwrap() {
            Some((tex, uv)) => {
                p.rect_filled(tr, 8.0, Color32::BLACK);
                egui::Image::from_texture((tex.id(), tex.size_vec2())).uv(*uv).corner_radius(8).paint_at(ui, tr);
            }
            None => {
                p.rect_filled(tr, 8.0, c::RAISED);
                icons::paint_sized(p, tr, "file-video", c::MUTED, 28.0);
            }
        }
        // text
        let tx = tr.left() + S1;
        let tw = inner_w - 2.0 * S1;
        let mut y = tr.bottom() + S3;
        let title = ui::elided(ui, &card.h.project.name, theme::ui_font(13.0, Weight::Medium), c::TEXT, tw);
        p.galley(pos2(tx, y), title, c::TEXT);
        y += 18.0 + S1;
        let mono = Role::Mono.font();
        let tip;
        if let Some((frac, msg)) = &live {
            let text = if msg.is_empty() { "Analysing".to_string() } else { msg.clone() };
            let g = ui::elided(ui, &text, mono, c::MUTED, tw);
            p.galley(pos2(tx, y), g, c::MUTED);
            tip = format!("{}\n{}", card.h.project.name, text);
            let bar = Rect::from_min_size(pos2(tr.left(), y + 16.0 + S2), vec2(inner_w, 4.0));
            ui::paint_progress(p, bar, *frac as f32);
        } else {
            let ag = ui.fonts_mut(|f| f.layout_no_wrap(card.added.clone(), mono.clone(), c::MUTED));
            let aw = ag.size().x;
            p.galley(pos2(tx + tw - aw, y), ag, c::MUTED);
            let g = ui::elided(ui, &card.meta, mono, c::MUTED, tw - aw - S2);
            p.galley(pos2(tx, y), g, c::MUTED);
            tip = card.tip.clone();
        }
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
        if !self.cards[i].added.is_empty() && live.is_none() && resp.hovered() {
            // the date's own tooltip ("Added …") shows in the card tooltip in egui
        }
        // interaction: release inside opens / right-click menu / keyboard
        if resp.clicked() {
            shell.push(Action::OpenProject(self.cards[i].h.dir.clone()));
        }
        if resp.secondary_clicked() {
            let pos = resp.interact_pointer_pos().unwrap_or(rect.center());
            self.menu = Some((pid.clone(), pos));
        }
        if resp.has_focus() {
            let (enter, menu_key) = ui.input(|i| {
                (
                    i.key_pressed(Key::Enter) || i.key_pressed(Key::Space),
                    i.key_pressed(Key::Menu) || (i.modifiers.shift && i.key_pressed(Key::F10)),
                )
            });
            if enter {
                shell.push(Action::OpenProject(self.cards[i].h.dir.clone()));
            }
            if menu_key {
                self.menu = Some((pid, rect.center()));
            }
        }
    }

    fn keyboard_nav(&mut self, ui: &Ui, cols: usize) {
        let focused = self.cards.iter().position(|c| ui.memory(|m| m.has_focus(Id::unique(("clip-card", &c.h.project.id)))));
        let Some(i) = focused else { return };
        let n = self.cards.len();
        let (l, r, u, d) = ui.input(|inp| {
            (
                inp.key_pressed(Key::ArrowLeft),
                inp.key_pressed(Key::ArrowRight),
                inp.key_pressed(Key::ArrowUp),
                inp.key_pressed(Key::ArrowDown),
            )
        });
        let target = if l && i > 0 {
            Some(i - 1)
        } else if r && i + 1 < n {
            Some(i + 1)
        } else if u && i >= cols {
            Some(i - cols)
        } else if d && i + cols < n {
            Some(i + cols)
        } else {
            None
        };
        if target.is_some() {
            self.focus_req = target;
        }
    }

    fn menu_ui(&mut self, ctx: &egui::Context, shell: &mut Shell) {
        let Some((pid, pos)) = self.menu.clone() else { return };
        let Some(card) = self.cards.iter().find(|c| c.h.project.id == pid) else {
            self.menu = None;
            return;
        };
        let busy = self.live.contains_key(&pid);
        let ready = card.h.project.status == "ready";
        let dir = card.h.dir.clone();
        let name = card.h.project.name.clone();
        let source_ok = card.source_ok;
        let mut close = false;
        let area = egui::Area::new(Id::unique("clip-menu")).fade_in(false).order(egui::Order::Foreground).fixed_pos(pos).constrain(true).show(ctx, |ui| {
            theme::menu_frame().show(ui, |ui| {
                ui.set_width(220.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                let item = |ui: &mut Ui, text: &str, enabled: bool| -> bool {
                    let (r, resp) = ui.allocate_exact_size(vec2(220.0, 30.0), if enabled { Sense::click() } else { Sense::hover() });
                    if enabled && resp.hovered() {
                        ui.painter().rect_filled(r, 6.0, c::RAISED_HI);
                    }
                    ui.painter().text(pos2(r.left() + 12.0, r.center().y), egui::Align2::LEFT_CENTER, text, Role::Body.font(), if enabled { c::TEXT } else { c::DISABLED });
                    enabled && resp.clicked()
                };
                if item(ui, "Open", !busy) {
                    shell.push(Action::OpenProject(dir.clone()));
                    close = true;
                }
                if item(ui, if ready { "Analyse again" } else { "Analyse" }, !busy && source_ok) {
                    shell.push(Action::Reanalyse(dir.clone()));
                    close = true;
                }
                if busy && item(ui, "Stop analysis", true) {
                    shell.push(Action::CancelAnalysis(dir.clone()));
                    close = true;
                }
                if item(ui, "Locate source video", true) {
                    close = true;
                    self.locate(&dir, shell);
                }
                if item(ui, "Show project folder", true) {
                    crate::sys::open_path(&dir);
                    close = true;
                }
                ui.add_space(4.0);
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(r.shrink2(vec2(8.0, 0.0)), 0.0, c::HAIR);
                ui.add_space(4.0);
                if item(ui, "Remove clip", true) {
                    shell.push(Action::Dialog(Dialog::confirm(
                        "Remove clip",
                        &format!("Remove \u{201c}{name}\u{201d} from ValoLights?\n\nYour edits for this clip are deleted. The original video file is not touched"),
                        "Remove",
                        false,
                        vec![Action::RemoveProject(dir.clone())],
                    )));
                    close = true;
                }
            });
        });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && !area.response.contains_pointer()
            && ctx.input(|i| i.pointer.press_origin()).map(|p| !area.response.rect.contains(p)).unwrap_or(false);
        if close || clicked_outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.menu = None;
        }
    }

    fn locate(&mut self, dir: &Path, shell: &mut Shell) {
        let Ok(mut h) = projects::open_dir(dir) else { return };
        let old = PathBuf::from(&h.project.source.path);
        let fname = old.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let start = old.parent().filter(|p| p.exists());
        let Some(f) = pick_videos(&format!("Locate {fname}"), start, false).into_iter().next() else { return };
        let fn2 = f.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        match projects::relocate(&mut h, &f) {
            Err(e) => shell.dialog(Dialog::message("Locate source video", &format!("Could not read {fn2}:\n{e}"))),
            Ok(false) => shell.dialog(Dialog::message(
                "Locate source video",
                &format!("{fn2} is not the same recording: its length does not match this clip"),
            )),
            Ok(true) => {
                self.refresh_card(&h.project.id);
                shell.toast(format!("Source video found for {}", h.project.name));
                if h.project.status != "ready" {
                    shell.push(Action::Reanalyse(h.dir.clone()));
                }
            }
        }
    }

    fn drop_zone(&mut self, ui: &mut Ui, shell: &mut Shell) {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 280.0f32.max(ui.available_height().min(420.0))), Sense::click());
        let hover = self.drag_n > 0;
        dashed_round_rect(ui.painter(), rect.shrink(0.5), 16.0, Stroke::new(1.0, if hover { c::ACCENT } else { c::HAIR_STRONG }));
        let cx = rect.center().x;
        let mut y = rect.center().y - 40.0;
        icons::paint_sized(ui.painter(), Rect::from_center_size(pos2(cx, y), vec2(32.0, 32.0)), "file-video", c::MUTED, 32.0);
        y += 16.0 + S2 + S2 + 10.0;
        let main = if hover { let_go(self.drag_n) } else { DROP_TEXT.to_string() };
        ui.painter().text(pos2(cx, y), egui::Align2::CENTER_TOP, main, Role::Body.font(), c::TEXT);
        y += 18.0 + S2;
        ui.painter().text(
            pos2(cx, y),
            egui::Align2::CENTER_TOP,
            "MP4, MKV, MOV, WEBM or AVI · it stays on this PC",
            Role::Muted.font(),
            c::MUTED,
        );
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            shell.push(Action::PickVideos);
        }
    }
}

/// A dashed rounded rectangle.
fn dashed_round_rect(p: &egui::Painter, r: Rect, rad: f32, stroke: Stroke) {
    let mut pts = vec![];
    let corners = [
        (r.right_top() + vec2(-rad, rad), -90.0f32),
        (r.right_bottom() + vec2(-rad, -rad), 0.0),
        (r.left_bottom() + vec2(rad, -rad), 90.0),
        (r.left_top() + vec2(rad, rad), 180.0),
    ];
    for (cpt, a0) in corners {
        for k in 0..=8 {
            let a = (a0 + k as f32 * 90.0 / 8.0).to_radians();
            pts.push(cpt + vec2(a.cos(), a.sin()) * rad);
        }
    }
    pts.push(pts[0]);
    p.extend(egui::Shape::dashed_line(&pts, stroke, 4.0, 3.0));
}

impl Page for LibraryPage {
    fn ui(&mut self, ui: &mut Ui, cx: &mut PageCtx) {
        let shell = &mut *cx.shell;
        if ui.input(|i| i.modifiers.command && i.key_pressed(Key::O)) && !ui.ctx().text_edit_focused() {
            shell.push(Action::PickVideos);
        }
        egui::Frame::new().inner_margin(egui::Margin { left: S8 as i8, right: S8 as i8, top: (S6 + S2) as i8, bottom: S6 as i8 }).show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui::label(ui, "Clips", Role::Title);
                        ui.add_space(S1);
                        ui::label(ui, self.count_text(), Role::Muted);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        let r = ui.add(Button::primary("Add video").icon("upload").min_height(36.0).tooltip("Add video  (Ctrl+O)"));
                        self.add_rect = r.rect;
                        if r.clicked() {
                            shell.push(Action::PickVideos);
                        }
                    });
                });
                ui.add_space(S6);
                if self.cards.is_empty() {
                    self.drop_zone(ui, shell);
                    return;
                }
                let cols = Self::cols(ui.available_width());
                self.keyboard_nav(ui, cols);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(S4, S4);
                    let n = self.cards.len();
                    for row in 0..n.div_ceil(cols) {
                        ui.horizontal(|ui| {
                            for i in row * cols..((row + 1) * cols).min(n) {
                                self.card_ui(ui, i, shell);
                            }
                        });
                    }
                    ui.add_space(S4);
                });
            },
        );
        self.menu_ui(ui.ctx(), shell);
    }

    fn on_state(&mut self, _ev: &crate::state::StateEvent, _st: &mut AppState, _shell: &mut Shell) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_dur(58.4), "0:58");
        assert_eq!(fmt_dur(3725.0), "1:02:05");
        let now = chrono::NaiveDateTime::parse_from_str("2026-11-17T18:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();
        assert_eq!(fmt_added("2026-11-17T10:08:46", now), "today 10:08");
        assert_eq!(fmt_added("2026-11-16T09:05:00", now), "yesterday 09:05");
        assert_eq!(fmt_added("2026-03-02T09:05:00", now), "2 Mar 09:05");
        assert_eq!(fmt_added("2025-03-02T09:05:00", now), "2 Mar 2025");
        assert_eq!(video_files(&[PathBuf::from("a.MP4"), PathBuf::from("b.txt")]), vec![PathBuf::from("a.MP4")]);
    }
}
