//! Custom-painted multi-track timeline of the source video.
//!
//! Tracks: ruler, rounds, film strip (kept parts lit, pink trim handles), kill portraits (crosshair on
//! headshots), ammo step line, playhead. Mouse: click/drag to seek, drag a part's ends to trim, Shift-drag on
//! empty strip to add a part, click a portrait to jump to the kill, wheel scrolls, Ctrl+wheel zooms.
//! What the user did comes out of [`Timeline::take_events`].
#![allow(dead_code)]

use crate::theme::{self, c, Weight};
use crate::widgets::ui::paint_crosshair;
use egui::epaint::RectShape;
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Event, FontId, MouseWheelUnit, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, Ui,
};

pub const RULER_H: f32 = 24.0;
pub const ROUND_H: f32 = 20.0;
pub const VIDEO_H: f32 = 56.0;
pub const KILL_H: f32 = 36.0;
pub const AMMO_H: f32 = 28.0;
pub const GAP: f32 = 8.0;
pub const LABEL_W: f32 = 72.0;
pub const EDGE_PX: f32 = 7.0;
/// canvas height
pub const CANVAS_H: f32 = RULER_H + ROUND_H + VIDEO_H + KILL_H + AMMO_H + GAP * 4.0 + 4.0;
const SB_H: f32 = 8.0;
/// canvas + 2 px + the (size-retaining) scrollbar
pub const HEIGHT: f32 = CANVAS_H + 2.0 + SB_H;

#[derive(Clone)]
pub struct TLKill {
    pub id: String,
    pub t: f64,
    pub label: String,
    pub headshot: bool,
    pub enabled: bool,
    /// portrait (project-relative path) and its texture once loaded by the owner
    pub pix_path: Option<String>,
    pub pix: Option<TextureHandle>,
    pub mine: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TLRound {
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub result: String,
    /// spike planted at (source time)
    pub planted: Option<f64>,
    /// round decided at (the post-round seconds after it are drawn dimmer)
    pub decided: Option<f64>,
    /// end-of-round banner badges ("Flawless", "Clutch", ...)
    pub badges: Vec<String>,
}

fn mmss(t: f64) -> String {
    format!("{}:{:04.1}", (t / 60.0).floor() as i64, t % 60.0)
}

/// What happened on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub enum TlEvent {
    Seek(f64),
    /// parts after a trim / new part, on release
    PartsEdited(Vec<(f64, f64)>),
    KillClicked(String),
    SelectionChanged(i32),
    /// source time of the edge being dragged (live)
    Trimming(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Seek,
    Left,
    Right,
    New,
}

/// The film strip: ffmpeg's one-row tile cut into textures (GPUs cap the texture width).
pub struct Strip {
    pub chunks: Vec<TextureHandle>,
    /// thumbnails per chunk
    pub per_chunk: usize,
    pub count: usize,
    /// one thumbnail in pixels
    pub cell: [usize; 2],
}

pub struct Timeline {
    pub duration: f64,
    pub parts: Vec<(f64, f64)>,
    pub kills: Vec<TLKill>,
    pub rounds: Vec<TLRound>,
    pub ammo: Vec<(f64, i32)>,
    pub thumbs: Option<Strip>,
    pub playhead: f64,
    pub pps: f64,
    pub offset: f64,
    pub selected: i32,
    /// shown on the film strip while it has no thumbnails
    pub strip_note: String,
    /// showing the whole video: stays that way when the window is resized
    fitted: bool,
    drag: Option<(Mode, usize, f64)>,
    /// parts when a trim drag started (Esc restores)
    before: Option<Vec<(f64, f64)>>,
    sel_before: i32,
    width: f32,
    /// where the canvas was drawn last frame (screen coordinates; for tests)
    pub rect: Rect,
    sb_drag: Option<(f32, f64)>,
    events: Vec<TlEvent>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

fn mono(px: f32) -> FontId {
    theme::mono_font(px)
}

impl Timeline {
    pub fn new() -> Self {
        Timeline {
            duration: 1.0,
            parts: vec![],
            kills: vec![],
            rounds: vec![],
            ammo: vec![],
            thumbs: None,
            playhead: 0.0,
            pps: 10.0,
            offset: 0.0,
            selected: -1,
            strip_note: String::new(),
            fitted: true,
            drag: None,
            before: None,
            sel_before: -1,
            width: 1000.0,
            rect: Rect::NOTHING,
            sb_drag: None,
            events: vec![],
        }
    }

    pub fn take_events(&mut self) -> Vec<TlEvent> {
        std::mem::take(&mut self.events)
    }

    // -------------------------------------------------------------- geometry (canvas-local x)
    pub fn view_w(&self) -> f64 {
        (self.width as f64 - LABEL_W as f64).max(50.0)
    }

    pub fn x_of(&self, t: f64) -> f32 {
        (LABEL_W as f64 + (t - self.offset) * self.pps) as f32
    }

    pub fn t_of(&self, x: f32) -> f64 {
        (self.offset + (x as f64 - LABEL_W as f64) / self.pps).clamp(0.0, self.duration)
    }

    /// Track rows in canvas-local coordinates: ruler, rounds, video, kills, ammo.
    pub fn row(&self, name: &str) -> Rect {
        let mut y = 0.0;
        for (n, h) in [("ruler", RULER_H), ("rounds", ROUND_H), ("video", VIDEO_H), ("kills", KILL_H), ("ammo", AMMO_H)] {
            if n == name {
                return Rect::from_min_size(pos2(LABEL_W, y), vec2(self.width - LABEL_W, h));
            }
            y += h + GAP;
        }
        Rect::NOTHING
    }

    /// Screen position of source time `t` on a row's centre line (for tests driving the mouse).
    pub fn screen_pos(&self, t: f64, row: &str) -> Pos2 {
        let r = self.row(row);
        pos2(self.rect.left() + self.x_of(t), self.rect.top() + r.center().y)
    }

    // -------------------------------------------------------------- data
    pub fn set_data(&mut self, duration: f64, kills: Vec<TLKill>, rounds: Vec<TLRound>, ammo: Vec<(f64, i32)>) {
        let new_clip = (duration.max(0.1) - self.duration).abs() > 1e-6;
        self.duration = duration.max(0.1);
        self.kills = kills;
        self.rounds = rounds;
        self.ammo = ammo;
        if new_clip || self.fitted {
            self.fit();
        } else {
            // same clip (kill toggled, identity changed...): keep the zoom and scroll the user chose
            self.offset = self.offset.min(self.duration - self.view_w() / self.pps).max(0.0);
        }
    }

    /// A different clip is being shown: forget the zoom and the selection.
    pub fn reset(&mut self) {
        self.selected = -1;
        self.fitted = true;
    }

    pub fn set_parts(&mut self, parts: &[(f64, f64)]) {
        let mut new = parts.to_vec();
        new.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if new != self.parts {
            // keep the selection on the same part when it still exists (e.g. undo of something else)
            let old = if self.selected >= 0 { self.parts.get(self.selected as usize).copied() } else { None };
            self.selected = old.and_then(|o| new.iter().position(|p| *p == o)).map(|i| i as i32).unwrap_or(-1);
        }
        self.parts = new;
    }

    /// Put a part being trimmed back the way it was (Esc). False when nothing was being trimmed.
    pub fn cancel_drag(&mut self) -> bool {
        if self.drag.is_none() || self.before.is_none() {
            return false;
        }
        self.parts = self.before.take().unwrap_or_default();
        self.selected = self.sel_before;
        self.drag = None;
        true
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn set_playhead(&mut self, t: f64, follow: bool) {
        self.playhead = t;
        if follow {
            let vis = self.view_w() / self.pps;
            if t < self.offset || t > self.offset + vis * 0.95 {
                self.offset = (t - vis * 0.1).min(self.duration - vis).max(0.0);
            }
        }
    }

    pub fn fit(&mut self) {
        self.pps = self.view_w() / self.duration;
        self.offset = 0.0;
        self.fitted = true;
    }

    pub fn zoom_to(&mut self, a: f64, b: f64) {
        let span = (b - a).max(1.0);
        let lo = self.view_w() / self.duration;
        self.pps = (self.view_w() / span).min(400.0).max(lo);
        self.offset = a.min(self.duration - self.view_w() / self.pps).max(0.0);
        self.fitted = self.pps <= lo + 1e-9;
    }

    pub fn set_zoom(&mut self, pps: f64, anchor: Option<(f64, f32)>) {
        let lo = self.view_w() / self.duration;
        let (anchor_t, anchor_x) = anchor.unwrap_or_else(|| {
            // buttons zoom around the playhead when it is on screen, else around the middle of the view
            let vis = self.view_w() / self.pps;
            let at =
                if self.offset <= self.playhead && self.playhead <= self.offset + vis { self.playhead } else { self.offset + vis / 2.0 };
            (at, (LABEL_W as f64 + (at - self.offset) * self.pps) as f32)
        });
        self.pps = pps.min(400.0).max(lo);
        self.fitted = self.pps <= lo + 1e-9;
        self.offset = anchor_t - (anchor_x as f64 - LABEL_W as f64) / self.pps;
        self.offset = self.offset.min(self.duration - self.view_w() / self.pps).max(0.0);
    }

    pub fn scroll_by(&mut self, dt: f64) {
        self.offset = (self.offset + dt).min(self.duration - self.view_w() / self.pps).max(0.0);
    }

    fn resized(&mut self, w: f32) {
        self.width = w;
        if self.fitted {
            self.fit();
            return;
        }
        let lo = self.view_w() / self.duration;
        if self.pps <= lo {
            self.pps = lo;
            self.fitted = true;
        }
        self.offset = self.offset.min(self.duration - self.view_w() / self.pps).max(0.0);
    }

    // -------------------------------------------------------------- mouse
    fn hit_part(&self, x: f32) -> Option<(Mode, usize)> {
        for (i, (a, b)) in self.parts.iter().enumerate() {
            let (xa, xb) = (self.x_of(*a), self.x_of(*b));
            if (x - xa).abs() <= EDGE_PX {
                return Some((Mode::Left, i));
            }
            if (x - xb).abs() <= EDGE_PX {
                return Some((Mode::Right, i));
            }
        }
        for (i, (a, b)) in self.parts.iter().enumerate() {
            if self.x_of(*a) < x && x < self.x_of(*b) {
                return Some((Mode::Seek, i));
            }
        }
        None
    }

    fn kill_at(&self, x: f32) -> Option<&TLKill> {
        let near = self.kills.iter().min_by(|a, b| (self.x_of(a.t) - x).abs().total_cmp(&(self.x_of(b.t) - x).abs()))?;
        ((self.x_of(near.t) - x).abs() <= KILL_H / 2.0).then_some(near)
    }

    fn press(&mut self, p: Pos2, shift: bool) {
        let x = p.x;
        if x < LABEL_W {
            return;
        }
        if self.row("kills").contains(p) {
            if let Some(k) = self.kill_at(x) {
                let id = k.id.clone();
                self.events.push(TlEvent::KillClicked(id)); // the owner seeks (same lead-in as the kill list)
                return;
            }
        }
        if self.row("video").contains(p) {
            let hit = self.hit_part(x);
            if let Some((m @ (Mode::Left | Mode::Right), i)) = hit {
                self.selected = i as i32;
                self.before = Some(self.parts.clone());
                self.sel_before = self.selected;
                self.drag = Some((m, i, 0.0));
                self.events.push(TlEvent::SelectionChanged(self.selected));
                return;
            }
            if let Some((Mode::Seek, i)) = hit {
                self.selected = i as i32;
                self.events.push(TlEvent::SelectionChanged(self.selected));
                self.events.push(TlEvent::Seek(self.t_of(x)));
                self.drag = Some((Mode::Seek, i, 0.0));
                return;
            }
            if shift {
                // shift-drag on empty video track creates a new part
                let t = self.t_of(x);
                self.before = Some(self.parts.clone());
                self.sel_before = self.selected;
                self.parts.push((t, t));
                self.parts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let i = self.parts.iter().position(|q| *q == (t, t)).unwrap_or(0);
                self.selected = i as i32;
                self.drag = Some((Mode::New, i, t));
                self.events.push(TlEvent::SelectionChanged(i as i32));
                return;
            }
        }
        self.selected = -1;
        self.events.push(TlEvent::SelectionChanged(-1));
        self.events.push(TlEvent::Seek(self.t_of(x)));
        self.drag = Some((Mode::Seek, usize::MAX, 0.0));
    }

    fn moved(&mut self, x: f32) {
        let Some((mode, i, anchor)) = self.drag else { return };
        let t = self.t_of(x);
        if mode == Mode::Seek {
            self.events.push(TlEvent::Seek(t));
            return;
        }
        let Some(&(a, b)) = self.parts.get(i) else { return };
        let lo = if i > 0 { self.parts[i - 1].1 } else { 0.0 };
        let hi = if i + 1 < self.parts.len() { self.parts[i + 1].0 } else { self.duration };
        match mode {
            Mode::Left => {
                self.parts[i] = (t.min(b - 0.2).max(lo), b);
                self.events.push(TlEvent::Trimming(self.parts[i].0));
            }
            Mode::Right => {
                self.parts[i] = (a, t.max(a + 0.2).min(hi));
                self.events.push(TlEvent::Trimming(self.parts[i].1));
            }
            Mode::New => {
                // a new part grows from where the drag started and stops at its neighbours (never overlaps)
                let t = t.min(hi).max(lo);
                self.parts[i] = (anchor.min(t), anchor.max(t));
                self.events.push(TlEvent::Trimming(t));
            }
            Mode::Seek => {}
        }
    }

    fn release(&mut self) {
        if let Some((Mode::Left | Mode::Right | Mode::New, _, _)) = self.drag {
            let sel = if self.selected >= 0 { self.parts.get(self.selected as usize).copied() } else { None };
            self.parts.retain(|(a, b)| b - a >= 0.2);
            self.selected = sel.and_then(|s| self.parts.iter().position(|p| *p == s)).map(|i| i as i32).unwrap_or(-1);
            if Some(&self.parts) != self.before.as_ref() {
                self.events.push(TlEvent::PartsEdited(self.parts.clone()));
            } else {
                self.events.push(TlEvent::SelectionChanged(self.selected));
            }
        }
        self.drag = None;
        self.before = None;
    }

    fn wheel(&mut self, dy: f32, ctrl: bool, x: f32) {
        if dy == 0.0 {
            return;
        }
        if ctrl {
            let t = self.t_of(x);
            let factor = if dy > 0.0 { 1.25 } else { 0.8 };
            self.set_zoom(self.pps * factor, Some((t, x)));
        } else {
            self.scroll_by(-(dy as f64) * 60.0 / self.pps * 2.0);
        }
    }

    // -------------------------------------------------------------- ui
    /// Draw the timeline (canvas + scrollbar) across the available width and handle the mouse.
    pub fn show(&mut self, ui: &mut Ui) {
        let w = ui.available_width().max(LABEL_W + 60.0);
        let (outer, _) = ui.allocate_exact_size(vec2(w, HEIGHT), Sense::hover());
        let rect = Rect::from_min_size(outer.min, vec2(w, CANVAS_H));
        if (w - self.width).abs() > 0.5 {
            self.resized(w);
        }
        self.rect = rect;
        let id = ui.scope_id().with("timeline-canvas");
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let origin = rect.min.to_vec2();
        let (pressed, released, pos, shift, ctrl, wheels) = ui.input(|i| {
            let wheels: Vec<f32> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel { unit, delta, .. } => {
                        let d = if delta.y != 0.0 { delta.y } else { delta.x };
                        Some(match unit {
                            MouseWheelUnit::Point => d / 50.0,
                            MouseWheelUnit::Line => d,
                            MouseWheelUnit::Page => d * 10.0,
                        })
                    }
                    _ => None,
                })
                .collect();
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
                i.modifiers.shift,
                i.modifiers.ctrl || i.modifiers.command,
                wheels,
            )
        });
        let local = pos.map(|p| p - origin);
        let over = resp.hovered() || resp.contains_pointer();
        if pressed && over {
            if let Some(p) = local {
                self.press(p, shift);
            }
        } else if self.drag.is_some() {
            if let Some(p) = local {
                self.moved(p.x);
            }
        }
        if released && self.drag.is_some() {
            self.release();
        }
        if over {
            for d in wheels {
                self.wheel(d, ctrl, local.map(|p| p.x).unwrap_or(LABEL_W));
            }
        }
        // cursor + tooltip
        if let (true, Some(p)) = (over, local) {
            if self.drag.is_some() {
                if matches!(self.drag, Some((Mode::Left | Mode::Right | Mode::New, _, _))) {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
            } else if self.row("video").contains(p) {
                if matches!(self.hit_part(p.x), Some((Mode::Left | Mode::Right, _))) {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
            } else if self.row("rounds").contains(p) {
                // the plant / round-end markers explain themselves
                let near = |t: f64| (self.x_of(t) - p.x).abs() <= 7.0;
                let tip = self.rounds.iter().find_map(|rd| {
                    if let Some(pt) = rd.planted.filter(|&t| near(t)) {
                        Some(format!("Spike planted · {}", mmss(pt)))
                    } else {
                        rd.decided.filter(|&t| near(t)).map(|d| {
                            let res = match rd.result.as_str() {
                                "win" => " · won",
                                "loss" => " · lost",
                                _ => "",
                            };
                            let badges = if rd.badges.is_empty() { String::new() } else { format!(" · {}", rd.badges.join(", ")) };
                            format!("Round over · {}{res}{badges}", mmss(d))
                        })
                    }
                });
                if let Some(tip) = tip {
                    resp.clone().on_hover_text_at_pointer(tip);
                }
            } else if self.row("kills").contains(p) {
                if let Some(k) = self.kill_at(p.x) {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    let (mm, ss) = ((k.t / 60.0).floor(), k.t % 60.0);
                    let tip = format!(
                        "{} · {}:{:04.1}{}{}",
                        k.label,
                        mm as i64,
                        ss,
                        if k.headshot { " · headshot" } else { "" },
                        if k.enabled { "" } else { " · dropped" }
                    );
                    resp.clone().on_hover_text_at_pointer(tip);
                }
            }
        }
        self.paint(ui, rect);
        self.scrollbar(ui, Rect::from_min_size(pos2(outer.left() + LABEL_W, rect.bottom() + 2.0), vec2(w - LABEL_W, SB_H)));
    }

    fn scrollbar(&mut self, ui: &mut Ui, r: Rect) {
        let vis = self.view_w() / self.pps;
        let max = self.duration - vis;
        if max * 100.0 < 1.0 {
            return; // hidden, the space stays (the page above doesn't jump)
        }
        let frac_vis = (vis / self.duration).clamp(0.0, 1.0) as f32;
        let hw = (r.width() * frac_vis).max(32.0).min(r.width());
        let hx = r.left() + (r.width() - hw) * (self.offset / max).clamp(0.0, 1.0) as f32;
        let handle = Rect::from_min_size(pos2(hx, r.top()), vec2(hw, r.height()));
        let resp = ui.interact(r, ui.scope_id().with("timeline-sb"), Sense::click_and_drag());
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                if handle.contains(p) {
                    self.sb_drag = Some((p.x, self.offset));
                } else {
                    // page towards the click
                    let step = vis * if p.x < hx { -1.0 } else { 1.0 };
                    self.scroll_by(step);
                }
            }
        }
        if let (Some((x0, off0)), Some(p)) = (self.sb_drag, resp.interact_pointer_pos()) {
            if resp.dragged() {
                let dt = (p.x - x0) as f64 / (r.width() - hw).max(1.0) as f64 * max;
                self.offset = (off0 + dt).clamp(0.0, max);
                self.fitted = false;
            }
        }
        if resp.drag_stopped() {
            self.sb_drag = None;
        }
        let hover = resp.hovered() || self.sb_drag.is_some();
        ui.painter().rect_filled(handle, 4.0, if hover { c::SCROLL_HOVER } else { c::RAISED_HI });
    }

    fn thumbs(&self, p: &egui::Painter, o: egui::Vec2, vr: Rect, a: f64, b: f64) {
        let Some(s) = &self.thumbs else { return };
        if s.count == 0 || s.chunks.is_empty() {
            return;
        }
        let (tw, th) = (s.cell[0] as f32, s.cell[1] as f32);
        let per = self.duration / s.count as f64;
        let cell = vr.height() * tw / th; // on-screen width of one thumbnail
        let left = LABEL_W.max(self.x_of(a));
        let end = self.x_of(b);
        if end <= left {
            return;
        }
        let clip = Rect::from_min_max(pos2(left, vr.top()), pos2(end, vr.bottom())).translate(o);
        let p = p.with_clip_rect(clip.intersect(p.clip_rect()));
        let strip_l = self.x_of(0.0);
        let strip_r = self.x_of(self.duration);
        let mut x = LABEL_W + ((left - LABEL_W) / cell).floor() * cell;
        while x < end {
            let t = self.t_of(x + cell / 2.0);
            let k = ((t / per) as i64).clamp(0, s.count as i64 - 1) as usize;
            let (ci, kk) = (k / s.per_chunk, k % s.per_chunk);
            if let Some(tex) = s.chunks.get(ci) {
                let n_in = tex.size()[0] as f32 / tw;
                let uv = Rect::from_min_max(pos2(kk as f32 / n_in, 0.0), pos2((kk + 1) as f32 / n_in, 1.0));
                let r = Rect::from_min_size(pos2(x, vr.top()), vec2(cell, vr.height())).translate(o);
                // the strip's rounded ends
                let mut cr = CornerRadius::ZERO;
                if x <= strip_l + 0.5 {
                    cr.nw = 6;
                    cr.sw = 6;
                }
                if x + cell >= strip_r - 0.5 {
                    cr.ne = 6;
                    cr.se = 6;
                }
                p.add(RectShape::filled(r, cr, Color32::WHITE).with_texture(tex.id(), uv));
            }
            x += cell;
        }
    }

    fn paint(&self, ui: &Ui, rect: Rect) {
        let o = rect.min.to_vec2();
        let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        p.rect_filled(rect, 0.0, c::BG);
        let f = theme::ui_font(12.0, Weight::Regular);
        let mf = mono(11.0);
        let hair = c::HAIR;
        // track labels
        for (name, title) in [("rounds", "Rounds"), ("video", "Video"), ("kills", "Kills"), ("ammo", "Ammo")] {
            let r = self.row(name);
            p.text(pos2(rect.left(), rect.top() + r.center().y), Align2::LEFT_CENTER, title, f.clone(), c::MUTED);
        }
        let p = p.with_clip_rect(Rect::from_min_max(pos2(rect.left() + LABEL_W, rect.top()), rect.max).intersect(p.clip_rect()));
        let t0 = self.offset;
        let t1 = self.offset + (self.width as f64 - LABEL_W as f64) / self.pps;
        // ruler
        let span = t1 - t0;
        let step =
            [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0].into_iter().find(|s| span / s <= 12.0).unwrap_or(900.0);
        let rr = self.row("ruler").translate(o);
        let mut t = (t0 / step).trunc() * step;
        while t <= t1 + step {
            let x = rect.left() + self.x_of(t);
            p.line_segment([pos2(x, rr.bottom() - 4.0), pos2(x, rr.bottom())], Stroke::new(1.0, hair));
            let (mm, ss) = ((t / 60.0).floor(), t - (t / 60.0).floor() * 60.0);
            let s = if step < 1.0 { format!("{}:{:04.1}", mm as i64, ss) } else { format!("{}:{:02}", mm as i64, ss as i64) };
            p.text(pos2(x + 4.0, rr.top() + 14.0), Align2::LEFT_BOTTOM, s, mf.clone(), c::MUTED);
            t += step;
        }
        p.line_segment([pos2(rect.left() + LABEL_W, rr.bottom()), pos2(rect.right(), rr.bottom())], Stroke::new(1.0, hair));
        // rounds: neutral bands, the result is a quiet coloured word
        let rr = self.row("rounds").translate(o);
        for (i, rd) in self.rounds.iter().enumerate() {
            let (a, b) = (rect.left() + self.x_of(rd.start), rect.left() + self.x_of(rd.end));
            if b < rect.left() + LABEL_W || a > rect.right() {
                continue;
            }
            let band = Rect::from_min_size(pos2(a, rr.top()), vec2((b - a - 2.0).max(0.0), rr.height()));
            p.rect_filled(band, 4.0, if i % 2 == 0 { c::RAISED } else { c::CARD });
            let name = rd.label.split(" · ").next().unwrap_or("");
            let bp = p.with_clip_rect(band.intersect(p.clip_rect()));
            let g = bp.layout_no_wrap(name.to_string(), f.clone(), c::SECONDARY);
            let nw = g.size().x;
            bp.galley(pos2(band.left() + 8.0, band.center().y - g.size().y / 2.0), g, c::SECONDARY);
            // after the round was decided: the post-round seconds, dimmer
            if let Some(d) = rd.decided {
                let x = rect.left() + self.x_of(d);
                if x < band.right() {
                    let tail = Rect::from_min_max(pos2(x.max(band.left()), band.top()), band.max);
                    bp.rect_filled(tail, 0.0, Color32::from_rgba_unmultiplied(15, 15, 16, 150));
                    bp.line_segment([pos2(x, band.top() + 4.0), pos2(x, band.bottom() - 4.0)], Stroke::new(1.5, c::SECONDARY));
                }
            }
            let mut tx = band.left() + 8.0 + nw + 8.0;
            if !rd.result.is_empty() && band.width() > 90.0 {
                let (word, col) = if rd.result == "win" { ("Won", c::WIN) } else { ("Lost", c::LOSS) };
                let g = bp.layout_no_wrap(word.to_string(), f.clone(), col);
                let w = g.size().x;
                bp.galley(pos2(tx, band.center().y - g.size().y / 2.0), g, col);
                tx += w + 8.0;
            }
            if !rd.badges.is_empty() && band.width() > 170.0 {
                bp.text(pos2(tx, band.center().y), Align2::LEFT_CENTER, rd.badges.join(" · "), f.clone(), c::SECONDARY);
            }
            // spike planted: a small red diamond (the spike icon's colour)
            if let Some(pt) = rd.planted {
                let x = rect.left() + self.x_of(pt);
                if x >= band.left() && x <= band.right() {
                    let (cy, r) = (band.center().y, 5.5);
                    let pts = vec![pos2(x, cy - r), pos2(x + r, cy), pos2(x, cy + r), pos2(x - r, cy)];
                    bp.add(egui::Shape::convex_polygon(pts, c::LOSS, Stroke::new(1.0, Color32::from_rgb(255, 170, 170))));
                }
            }
        }
        // film strip: everything dimmed, kept parts lit
        let vr_local = self.row("video");
        let vr = vr_local.translate(o);
        let strip =
            Rect::from_min_size(pos2(rect.left() + LABEL_W, vr.top()), vec2((self.x_of(self.duration) - LABEL_W).max(0.0), vr.height()));
        {
            let sp = p.with_clip_rect(strip.intersect(p.clip_rect()));
            sp.rect_filled(strip, 6.0, c::CARD);
            if self.thumbs.is_none() && !self.strip_note.is_empty() {
                sp.text(pos2(vr.left() + 12.0, vr.center().y), Align2::LEFT_CENTER, &self.strip_note, f.clone(), c::MUTED);
            }
            self.thumbs(&sp, o, vr_local, t0.max(0.0), self.duration.min(t1 + 1.0));
            sp.rect_filled(strip, 6.0, Color32::from_rgba_unmultiplied(15, 15, 16, 190));
            for (a, b) in &self.parts {
                if rect.left() + self.x_of(*b) < rect.left() + LABEL_W || rect.left() + self.x_of(*a) > rect.right() {
                    continue;
                }
                self.thumbs(&sp, o, vr_local, *a, *b);
            }
        }
        let acc = c::ACCENT;
        for (i, (a, b)) in self.parts.iter().enumerate() {
            let (xa, xb) = (rect.left() + self.x_of(*a), rect.left() + self.x_of(*b));
            if xb < rect.left() + LABEL_W - 8.0 || xa > rect.right() + 8.0 {
                continue;
            }
            let r = Rect::from_min_max(pos2(xa, vr.top()), pos2(xb, vr.bottom()));
            let sel = i as i32 == self.selected;
            if sel {
                // the selected part (what Delete removes) reads as selected, not just a hair thicker
                p.rect_filled(r, 6.0, theme::accent_a(46));
            }
            p.rect_stroke(r.shrink(0.75), 6.0, Stroke::new(if sel { 2.5 } else { 1.5 }, acc), StrokeKind::Middle);
            let hh = vr.height() * 0.42;
            for hx in [xa, xb] {
                p.rect_filled(Rect::from_min_size(pos2(hx - 3.0, vr.center().y - hh / 2.0), vec2(6.0, hh)), 3.0, acc);
            }
            if xb - xa > 46.0 {
                let tag = format!("{:.1}s", b - a);
                let g = p.layout_no_wrap(tag, mf.clone(), acc);
                let tr = Rect::from_min_size(pos2(xa + 6.0, vr.top() + 6.0), vec2(g.size().x + 10.0, 16.0));
                p.rect_filled(tr, 4.0, Color32::from_rgba_unmultiplied(15, 15, 16, 220));
                p.galley(tr.center() - g.size() / 2.0, g, acc);
            }
        }
        // kills: portraits framed by a hairline, a crosshair marks headshots
        let kr = self.row("kills").translate(o);
        let s = kr.height() - 8.0;
        for k in &self.kills {
            let x = rect.left() + self.x_of(k.t);
            if x < rect.left() + LABEL_W - 20.0 || x > rect.right() + 20.0 {
                continue;
            }
            p.line_segment(
                [pos2(x, vr.bottom() + 1.0), pos2(x, kr.top() + 4.0)],
                Stroke::new(1.0, if k.enabled { c::MUTED } else { hair }),
            );
            let r = Rect::from_min_size(pos2(x - s / 2.0, kr.top() + 4.0), vec2(s, s));
            match &k.pix {
                Some(tex) => {
                    p.add(
                        RectShape::filled(r, 6.0, Color32::WHITE)
                            .with_texture(tex.id(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                    );
                }
                None => {
                    p.rect_filled(r, 6.0, c::RAISED);
                }
            }
            if !k.enabled {
                p.rect_filled(r, 6.0, Color32::from_rgba_unmultiplied(15, 15, 16, 180));
            }
            p.rect_stroke(r.shrink(0.5), 6.0, Stroke::new(1.0, if k.enabled { c::TEXT } else { hair }), StrokeKind::Middle);
            if k.headshot && k.enabled {
                let cc = pos2(r.right() - 1.0, r.top() + 1.0);
                p.circle_filled(cc, 7.0, c::BG);
                paint_crosshair(&p, cc, 5.5, c::ACCENT);
            }
        }
        // ammo: a quiet step line
        let ar = self.row("ammo").translate(o);
        if !self.ammo.is_empty() {
            let mx = self.ammo.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1) as f32;
            let st = Stroke::new(1.0, c::MUTED);
            let mut prev: Option<(f64, Pos2)> = None;
            for (t, v) in &self.ammo {
                if *t < t0 - 5.0 || *t > t1 + 5.0 {
                    prev = None;
                    continue;
                }
                let pt = pos2(rect.left() + self.x_of(*t), ar.bottom() - 2.0 - (ar.height() - 6.0) * *v as f32 / mx);
                if let Some((pt0, pp)) = prev {
                    if t - pt0 < 6.0 {
                        p.line_segment([pp, pos2(pt.x, pp.y)], st);
                        p.line_segment([pos2(pt.x, pp.y), pt], st);
                    }
                }
                prev = Some((*t, pt));
            }
        }
        // playhead
        let x = rect.left() + self.x_of(self.playhead);
        let top = rect.top() + self.row("ruler").top();
        p.line_segment([pos2(x, top + 6.0), pos2(x, rect.bottom())], Stroke::new(1.0, c::TEXT));
        p.rect_filled(Rect::from_min_size(pos2(x - 4.0, top + 2.0), vec2(8.0, 8.0)), 2.0, c::TEXT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl() -> Timeline {
        let mut t = Timeline::new();
        t.width = 1072.0; // view 1000 px
        t.set_data(100.0, vec![], vec![], vec![]);
        t
    }

    #[test]
    fn fit_zoom_scroll() {
        let mut t = tl();
        assert!((t.pps - 10.0).abs() < 1e-9 && t.offset == 0.0);
        t.set_playhead(40.0, false);
        t.set_zoom(40.0, None); // around the playhead
        assert!((t.pps - 40.0).abs() < 1e-9);
        assert!(t.offset <= 40.0 && 40.0 <= t.offset + t.view_w() / t.pps);
        t.set_zoom(1.0, None);
        assert!(t.fitted && (t.pps - 10.0).abs() < 1e-9);
        t.zoom_to(30.0, 45.0);
        assert!((t.offset - 30.0).abs() < 1e-9 && !t.fitted);
        t.resized(2072.0); // grows: zoom kept
        assert!(!t.fitted);
    }

    #[test]
    fn trim_new_part_and_cancel() {
        let mut t = tl();
        t.set_parts(&[(10.0, 20.0), (40.0, 50.0)]);
        let y = t.row("video").center().y;
        // drag the right handle of the first part past the next part: stops at its neighbour
        t.press(pos2(t.x_of(20.0), y), false);
        t.moved(t.x_of(45.0));
        assert_eq!(t.parts[0], (10.0, 40.0));
        assert!(t.cancel_drag());
        assert_eq!(t.parts[0], (10.0, 20.0));
        t.release();
        assert!(!t.take_events().iter().any(|e| matches!(e, TlEvent::PartsEdited(_))));
        // shift-drag a new part from 25 s towards 45 s: stops at 40
        t.press(pos2(t.x_of(25.0), y), true);
        t.moved(t.x_of(45.0));
        t.release();
        let ev = t.take_events();
        let edited = ev.iter().find_map(|e| if let TlEvent::PartsEdited(p) = e { Some(p.clone()) } else { None }).unwrap();
        assert_eq!(edited, vec![(10.0, 20.0), (25.0, 40.0), (40.0, 50.0)]);
        assert_eq!(t.selected, 1);
    }
}
