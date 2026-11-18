//! Caption timeline under the subtitle preview (edit time, i.e. the time you see in the player).
//!
//!
//! Captions are blocks; the playhead can be clicked or dragged anywhere to scrub. Click a block to select it,
//! drag its body to move it, drag its edges to retime it. Moves snap to the playhead and neighbouring captions
//! and never cross a cut, so a caption can't end up half in footage that was removed.
//! Ctrl+wheel zooms around the pointer, the wheel pans when zoomed in.
use crate::theme::{self, c, Weight};
use crate::widgets::ui;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui};

pub const RULER: f32 = 20.0;
pub const LANE: f32 = 40.0;
pub const HEIGHT: f32 = RULER + LANE + 12.0;
const EDGE: f32 = 6.0;
const SNAP_PX: f32 = 6.0;
const MIN_LEN: f64 = 0.2;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackItem {
    /// segment index in the transcript
    pub index: usize,
    /// edit-time start / end
    pub a: f64,
    pub b: f64,
    pub text: String,
    pub hidden: bool,
    /// movement limits (the kept part it lives in)
    pub lo: f64,
    pub hi: f64,
}

/// What the user did on the track this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackEvent {
    /// seekRequested(edit time)
    Seek(f64),
    /// selected(segment index)
    Selected(usize),
    /// retimed(segment index, new edit-time start, end) – on release
    Retimed(usize, f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Move,
    Start,
    End,
}

#[derive(Debug, Clone)]
enum Drag {
    Scrub,
    Item { mode: Mode, index: usize, x0: f32, a0: f64, b0: f64, moved: bool },
}

pub struct CaptionTrack {
    pub duration: f64,
    /// edit times where one kept part ends and the next begins
    pub cuts: Vec<f64>,
    pub items: Vec<TrackItem>,
    /// selected segment index (-1 = none)
    pub sel: isize,
    /// caption under the playhead (-1 = none)
    pub now: isize,
    pub playhead: f64,
    pub view0: f64,
    pub view1: f64,
    drag: Option<Drag>,
    snap_x: Option<f32>,
    /// where it was drawn last frame (screen points)
    pub rect: Rect,
}

impl Default for CaptionTrack {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptionTrack {
    pub fn new() -> Self {
        CaptionTrack {
            duration: 1.0,
            cuts: vec![],
            items: vec![],
            sel: -1,
            now: -1,
            playhead: 0.0,
            view0: 0.0,
            view1: 1.0,
            drag: None,
            snap_x: None,
            rect: Rect::NOTHING,
        }
    }

    // ------------------------------------------------------------ data
    pub fn set_data(&mut self, duration: f64, cuts: Vec<f64>, items: Vec<TrackItem>) {
        let refit = (duration - self.duration).abs() > 1e-3;
        self.duration = duration.max(0.1);
        self.cuts = cuts;
        if self.drag.is_none() {
            self.items = items;
        }
        if refit || self.view1 <= self.view0 {
            self.view0 = 0.0;
            self.view1 = self.duration;
        }
    }

    pub fn set_playhead(&mut self, t: f64) {
        self.playhead = t;
        self.now = self.items.iter().find(|it| it.a <= t && t < it.b && !it.hidden).map(|it| it.index as isize).unwrap_or(-1);
        // keep the playhead visible when zoomed in
        let span = self.view1 - self.view0;
        if span < self.duration && !(self.view0 <= t && t <= self.view1) {
            self.view0 = (t - span * 0.2).min(self.duration - span).max(0.0);
            self.view1 = self.view0 + span;
        }
    }

    pub fn select(&mut self, index: isize) {
        self.sel = index;
        if let Some(a) = self.item(index).map(|it| it.a) {
            let span = self.view1 - self.view0;
            if span < self.duration && !(self.view0 <= a && a <= self.view1) {
                self.view0 = (a - span * 0.2).min(self.duration - span).max(0.0);
                self.view1 = self.view0 + span;
            }
        }
    }

    pub fn item(&self, index: isize) -> Option<&TrackItem> {
        self.items.iter().find(|it| it.index as isize == index)
    }

    // ------------------------------------------------------------ geometry (screen points, using `self.rect`)
    pub fn x_of(&self, t: f64) -> f32 {
        let r = self.rect;
        r.left() + 8.0 + ((t - self.view0) / (self.view1 - self.view0).max(1e-6)) as f32 * (r.width() - 16.0)
    }

    pub fn t_of(&self, x: f32) -> f64 {
        let r = self.rect;
        let t = self.view0 + ((x - r.left() - 8.0) / (r.width() - 16.0).max(1.0)) as f64 * (self.view1 - self.view0);
        t.clamp(0.0, self.duration)
    }

    pub fn lane(&self) -> Rect {
        let r = self.rect;
        Rect::from_min_size(pos2(r.left() + 8.0, r.top() + RULER + 4.0), vec2(r.width() - 16.0, LANE))
    }

    pub fn item_rect(&self, it: &TrackItem) -> Rect {
        let ln = self.lane();
        let (xa, xb) = (self.x_of(it.a), self.x_of(it.b));
        Rect::from_min_size(pos2(xa, ln.top() + 2.0), vec2((xb - xa).max(3.0), ln.height() - 4.0))
    }

    fn hit(&self, pos: Pos2) -> (Option<Mode>, Option<usize>) {
        if !self.lane().contains(pos) {
            return (None, None);
        }
        // selected caption first, so its edges win over neighbours
        let mut order: Vec<&TrackItem> = self.items.iter().collect();
        order.sort_by_key(|it| it.index as isize != self.sel);
        for it in order {
            let r = self.item_rect(it);
            let sel = it.index as isize == self.sel;
            if r.expand2(vec2(EDGE, 0.0)).contains(pos) {
                if sel && (pos.x - r.left()).abs() <= EDGE {
                    return (Some(Mode::Start), Some(it.index));
                }
                if sel && (pos.x - r.right()).abs() <= EDGE {
                    return (Some(Mode::End), Some(it.index));
                }
                if r.contains(pos) {
                    return (Some(Mode::Move), Some(it.index));
                }
            }
        }
        (None, None)
    }

    /// Snap a start (or, with length, the block's end too) to the playhead and neighbour edges.
    fn snap(&mut self, t: f64, index: usize, length: Option<f64>) -> f64 {
        let mut targets = vec![self.playhead];
        for o in self.items.iter().filter(|o| o.index != index) {
            targets.push(o.a);
            targets.push(o.b);
        }
        let tol = SNAP_PX as f64 / (self.rect.width() as f64 - 16.0).max(1.0) * (self.view1 - self.view0);
        let offs: &[f64] = match length {
            Some(l) if l != 0.0 => &[0.0, l],
            _ => &[0.0],
        };
        for &off in offs {
            for &tg in &targets {
                if (t + off - tg).abs() <= tol {
                    self.snap_x = Some(self.x_of(tg));
                    return tg - off;
                }
            }
        }
        t
    }

    // ------------------------------------------------------------ input + paint
    /// Draw the track across the available width and handle the mouse. Returns what happened.
    pub fn show(&mut self, ui: &mut Ui) -> Vec<TrackEvent> {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), HEIGHT), Sense::click_and_drag());
        self.rect = rect;
        let mut out = vec![];
        let (pressed, released, down, pos, zoom, scroll) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.primary_down(),
                i.pointer.interact_pos().or(i.pointer.hover_pos()),
                i.zoom_delta(),
                i.smooth_scroll_delta.y,
            )
        });
        let inside = pos.map(|p| rect.contains(p)).unwrap_or(false);
        // press
        if pressed && inside && resp.hovered() {
            let p = pos.unwrap();
            match self.hit(p) {
                (Some(mut mode), Some(index)) => {
                    if index as isize != self.sel {
                        self.sel = index as isize;
                        out.push(TrackEvent::Selected(index));
                        mode = Mode::Move;
                    }
                    let it = self.item(index as isize).cloned().unwrap();
                    self.drag = Some(Drag::Item { mode, index, x0: p.x, a0: it.a, b0: it.b, moved: false });
                }
                _ => {
                    self.drag = Some(Drag::Scrub);
                    out.push(TrackEvent::Seek(self.t_of(p.x)));
                }
            }
        } else if let (Some(d), Some(p), true) = (self.drag.clone(), pos, down) {
            // move
            match d {
                Drag::Scrub => {
                    let t = self.t_of(p.x);
                    if (t - self.playhead).abs() > 1e-9 {
                        out.push(TrackEvent::Seek(t));
                    }
                }
                Drag::Item { mode, index, x0, a0, b0, moved } => {
                    if (p.x - x0).abs() >= 3.0 || moved {
                        self.drag = Some(Drag::Item { mode, index, x0, a0, b0, moved: true });
                        self.snap_x = None;
                        let dt = self.t_of(p.x) - self.t_of(x0);
                        if let Some(k) = self.items.iter().position(|it| it.index == index) {
                            let (lo, hi, a, b) = (self.items[k].lo, self.items[k].hi, self.items[k].a, self.items[k].b);
                            match mode {
                                Mode::Move => {
                                    let length = b0 - a0;
                                    let na = self.snap(a0 + dt, index, Some(length));
                                    let na = na.max(lo).min(hi - length);
                                    self.items[k].a = na;
                                    self.items[k].b = na + length;
                                }
                                Mode::Start => {
                                    let na = self.snap(a0 + dt, index, None);
                                    self.items[k].a = na.max(lo).min(b - MIN_LEN);
                                }
                                Mode::End => {
                                    let nb = self.snap(b0 + dt, index, None);
                                    self.items[k].b = nb.min(hi).max(a + MIN_LEN);
                                }
                            }
                        }
                    }
                }
            }
        }
        if released || (!down && self.drag.is_some() && !pressed) {
            // release
            let d = self.drag.take();
            self.snap_x = None;
            match d {
                Some(Drag::Item { index, moved: true, .. }) => {
                    if let Some(it) = self.items.iter().find(|it| it.index == index) {
                        out.push(TrackEvent::Retimed(index, it.a, it.b));
                    }
                }
                // settle on the release point (mpv may coalesce a burst of seeks while scrubbing)
                Some(Drag::Scrub) if !pressed => {
                    if let Some(p) = pos {
                        out.push(TrackEvent::Seek(self.t_of(p.x)));
                    }
                }
                _ => {}
            }
        }
        // hover cursor
        if resp.hovered() {
            let icon = match &self.drag {
                Some(Drag::Item { mode: Mode::Start | Mode::End, .. }) => CursorIcon::ResizeHorizontal,
                Some(Drag::Item { .. }) => CursorIcon::Grabbing,
                Some(Drag::Scrub) => CursorIcon::Text,
                None => match pos.map(|p| self.hit(p).0) {
                    Some(Some(Mode::Start | Mode::End)) => CursorIcon::ResizeHorizontal,
                    Some(Some(Mode::Move)) => CursorIcon::Grab,
                    _ => CursorIcon::Text,
                },
            };
            ui.ctx().set_cursor_icon(icon);
            // wheel: Ctrl zooms around the pointer, plain wheel pans
            if zoom != 1.0 {
                if let Some(p) = pos {
                    let t = self.t_of(p.x);
                    let k = if zoom > 1.0 { 0.8 } else { 1.25 };
                    let span = ((self.view1 - self.view0) * k).max(2.0).min(self.duration);
                    let frac = (t - self.view0) / (self.view1 - self.view0).max(1e-6);
                    self.view0 = (t - frac * span).min(self.duration - span).max(0.0);
                    self.view1 = self.view0 + span;
                }
            } else if scroll != 0.0 {
                ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
                let span = self.view1 - self.view0;
                let shift = -(scroll as f64) / 40.0 * span * 0.1;
                self.view0 = (self.view0 + shift).min(self.duration - span).max(0.0);
                self.view1 = self.view0 + span;
            }
        }
        if !out.is_empty() || self.drag.is_some() {
            ui.ctx().request_repaint();
        }
        if ui.is_rect_visible(rect) {
            self.paint(ui, rect);
        }
        out
    }

    fn paint(&self, ui: &Ui, rect: Rect) {
        let p = ui.painter_at(rect.expand(1.0));
        p.rect_filled(rect, 0.0, c::BG);
        let mono = theme::mono_font(11.0);
        // ruler
        let span = self.view1 - self.view0;
        let step = [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0].into_iter().find(|s| span / s <= 14.0).unwrap_or(300.0);
        let mut t = (self.view0 / step).trunc() * step;
        while t <= self.view1 + step {
            let x = self.x_of(t);
            p.line_segment([pos2(x, rect.top() + RULER - 4.0), pos2(x, rect.top() + RULER)], Stroke::new(1.0, c::HAIR));
            let (m, s) = ((t / 60.0).floor(), t.rem_euclid(60.0));
            let label = if step < 1.0 { format!("{}:{:04.1}", m as i64, s) } else { format!("{}:{:02}", m as i64, s.trunc() as i64) };
            p.text(pos2(x + 3.0, rect.top() + 3.0), Align2::LEFT_TOP, label, mono.clone(), c::MUTED);
            t += step;
        }
        let ln = self.lane();
        let (x0, x1) = (self.x_of(0.0), self.x_of(self.duration));
        p.rect_filled(Rect::from_min_max(pos2(x0, ln.top()), pos2(x1, ln.bottom())), 6.0, c::CARD);
        // cuts between kept parts
        for &cut in &self.cuts {
            let x = self.x_of(cut);
            p.extend(Shape::dashed_line(&[pos2(x, ln.top()), pos2(x, ln.bottom())], Stroke::new(1.0, c::MUTED), 4.0, 2.0));
        }
        // captions
        let font = theme::ui_font(12.0, Weight::Regular);
        for it in &self.items {
            let r = self.item_rect(it);
            if r.right() < rect.left() || r.left() > rect.right() {
                continue;
            }
            let (sel, now) = (it.index as isize == self.sel, it.index as isize == self.now);
            let mut fill = if sel || now { c::RAISED_HI } else { c::RAISED };
            if it.hidden {
                fill = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 110);
            }
            p.rect_filled(r, 5.0, fill);
            let inner = r.shrink(0.5);
            if sel {
                p.rect_stroke(inner, 5.0, Stroke::new(1.5, c::ACCENT), StrokeKind::Middle);
            } else if it.hidden {
                p.extend(Shape::dashed_line(&rounded_rect_points(inner, 5.0), Stroke::new(1.0, c::MUTED), 4.0, 2.0));
            } else {
                p.rect_stroke(inner, 5.0, Stroke::new(1.0, c::HAIR), StrokeKind::Middle);
            }
            if sel {
                // edge grips
                for gx in [r.left(), r.right()] {
                    p.rect_filled(Rect::from_min_size(pos2(gx - 2.0, r.center().y - 8.0), vec2(4.0, 16.0)), 2.0, c::ACCENT);
                }
            }
            if r.width() > 24.0 {
                let col = if it.hidden { c::MUTED } else { c::TEXT };
                let g = ui::elided(ui, &it.text, font.clone(), col, r.width() - 14.0);
                let clip = Rect::from_min_max(pos2(r.left() + 8.0, r.top()), pos2(r.right() - 6.0, r.bottom()));
                ui.painter_at(clip.intersect(rect)).galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, col);
            }
        }
        // snap guide
        if let Some(sx) = self.snap_x {
            p.extend(Shape::dashed_line(&[pos2(sx, rect.top() + RULER), pos2(sx, ln.bottom())], Stroke::new(1.0, c::ACCENT), 4.0, 2.0));
        }
        // playhead
        let x = self.x_of(self.playhead);
        p.line_segment([pos2(x, rect.top() + 4.0), pos2(x, ln.bottom() + 4.0)], Stroke::new(1.0, c::TEXT));
        p.rect_filled(Rect::from_min_size(pos2(x - 4.0, rect.top() + 2.0), vec2(8.0, 8.0)), 2.0, c::TEXT);
    }
}

/// Closed outline of a rounded rect (for dashed strokes).
fn rounded_rect_points(r: Rect, rad: f32) -> Vec<Pos2> {
    let rad = rad.min(r.width() / 2.0).min(r.height() / 2.0);
    let mut pts = vec![];
    let corners = [
        (pos2(r.right() - rad, r.top() + rad), -90.0f32),
        (pos2(r.right() - rad, r.bottom() - rad), 0.0),
        (pos2(r.left() + rad, r.bottom() - rad), 90.0),
        (pos2(r.left() + rad, r.top() + rad), 180.0),
    ];
    for (cen, start) in corners {
        for k in 0..=4 {
            let a = (start + k as f32 * 22.5).to_radians();
            pts.push(pos2(cen.x + rad * a.cos(), cen.y + rad * a.sin()));
        }
    }
    pts.push(pts[0]);
    pts
}
