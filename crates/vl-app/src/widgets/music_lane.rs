//! The song under the edit: the whole track as a waveform with the part that plays highlighted.
//!
//! Drag the highlight to choose which part of the song plays, drag its ends to trim where the music starts and
//! stops in the edit, click outside it to move it there, click inside it to hear the edit from that moment.
#![allow(dead_code)]

use crate::backend::SongInfo;
use crate::theme::{self, c, Weight};
use egui::{pos2, vec2, Align2, Color32, CursorIcon, Rect, Sense, Stroke, StrokeKind, Ui};
use std::sync::Arc;

pub const PAD: f32 = 8.0;
/// px around a window end that grabs it
pub const EDGE: f32 = 7.0;
pub const MIN_LEN: f64 = 1.0;
pub const HEIGHT: f32 = 56.0;

/// "m:ss.s"
pub fn fmt(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0).floor() as i64, t % 60.0)
}

#[derive(Clone, Debug, PartialEq)]
pub enum LaneEvent {
    /// song_in, start, end (None = to the end of the video); on release
    Placed(f64, f64, Option<f64>),
    /// song_in, start, end while dragging (for the labels)
    Dragging(f64, f64, f64),
    /// edit time
    Seek(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    None,
    Left,
    Right,
    Move,
    Outside,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    mode: Hit, // Move after 3 px; `click` = Move with moved == false
    click: bool,
    x0: f32,
    si0: f64,
    st0: f64,
    en0: f64,
}

pub struct MusicLane {
    pub info: Option<Arc<SongInfo>>,
    pub busy_text: String,
    pub song_in: f64,
    pub start: f64,
    /// effective edit-time end
    pub end: f64,
    /// end follows the video's end
    pub end_auto: bool,
    pub total: f64,
    /// song time
    pub playhead: Option<f64>,
    drag: Option<Drag>,
    width: f32,
    /// where it was drawn last frame (screen coordinates; for tests)
    pub rect: Rect,
    events: Vec<LaneEvent>,
}

impl Default for MusicLane {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicLane {
    pub fn new() -> Self {
        MusicLane {
            info: None,
            busy_text: String::new(),
            song_in: 0.0,
            start: 0.0,
            end: 0.0,
            end_auto: true,
            total: 0.0,
            playhead: None,
            drag: None,
            width: 600.0,
            rect: Rect::NOTHING,
            events: vec![],
        }
    }

    pub fn take_events(&mut self) -> Vec<LaneEvent> {
        std::mem::take(&mut self.events)
    }

    // ------------------------------------------------------------ state
    pub fn set_song(&mut self, info: Option<Arc<SongInfo>>, busy_text: &str) {
        self.info = info;
        self.busy_text = busy_text.to_string();
    }

    pub fn set_placement(&mut self, song_in: f64, start: f64, end: Option<f64>, total: f64) {
        if self.drag.is_some() {
            return;
        }
        self.total = total;
        self.start = start.max(0.0).min(total);
        self.end_auto = end.is_none();
        self.end = match end {
            None => total,
            Some(e) => e.max(self.start).min(total),
        };
        self.song_in = song_in.max(0.0);
    }

    pub fn set_playhead(&mut self, song_t: Option<f64>) {
        self.playhead = song_t;
    }

    pub fn dur(&self) -> f64 {
        self.info.as_ref().map(|i| i.duration).unwrap_or(0.0)
    }

    pub fn length(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    // ------------------------------------------------------------ geometry (local x)
    fn x(&self, t: f64) -> f32 {
        if self.dur() > 0.0 {
            PAD + (t / self.dur()) as f32 * (self.width - 2.0 * PAD)
        } else {
            PAD
        }
    }

    fn t(&self, x: f32) -> f64 {
        ((x - PAD) / (self.width - 2.0 * PAD).max(1.0)) as f64 * self.dur()
    }

    /// The playing part (local coordinates).
    pub fn win(&self) -> Rect {
        let (a, b) = (self.x(self.song_in), self.x(self.dur().min(self.song_in + self.length())));
        Rect::from_min_size(pos2(a, 4.0), vec2((b - a).max(2.0), HEIGHT - 8.0))
    }

    /// The playing part on screen (for tests driving the mouse).
    pub fn win_screen(&self) -> Rect {
        self.win().translate(self.rect.min.to_vec2())
    }

    fn hit(&self, x: f32) -> Hit {
        if self.info.is_none() {
            return Hit::None;
        }
        let w = self.win();
        if (x - w.left()).abs() <= EDGE {
            return Hit::Left;
        }
        if (x - w.right()).abs() <= EDGE {
            return Hit::Right;
        }
        if w.left() < x && x < w.right() {
            Hit::Move
        } else {
            Hit::Outside
        }
    }

    // ------------------------------------------------------------ mouse
    fn moved(&mut self, x: f32) {
        let Some(mut d) = self.drag else { return };
        let dt = self.t(x) - self.t(d.x0);
        if (x - d.x0).abs() > 3.0 && d.click {
            d.click = false;
            self.drag = Some(d);
        }
        if d.click {
            return; // a click inside until it moves
        }
        match d.mode {
            Hit::Move => {
                self.song_in = (d.si0 + dt).max(0.0).min((self.dur() - (d.en0 - d.st0)).max(0.0));
            }
            Hit::Left => {
                // the music starts later (or earlier) in the edit; what plays stays in place
                let dt = dt.max(-d.st0).max(-d.si0).min((d.en0 - d.st0) - MIN_LEN);
                self.song_in = d.si0 + dt;
                self.start = d.st0 + dt;
            }
            Hit::Right => {
                self.end = (d.en0 + dt).max(d.st0 + MIN_LEN).min(self.total).min(d.st0 + (self.dur() - d.si0));
                self.end_auto = false;
            }
            _ => {}
        }
        self.events.push(LaneEvent::Dragging(self.song_in, self.start, self.end));
    }

    fn press(&mut self, x: f32) {
        if self.info.is_none() {
            return;
        }
        let h = self.hit(x);
        if h == Hit::Outside {
            // place the part here (centred on the click)
            self.song_in = (self.t(x) - self.length() / 2.0).max(0.0).min((self.dur() - self.length()).max(0.0));
            self.emit();
            return;
        }
        self.drag = Some(Drag { mode: h, click: h == Hit::Move, x0: x, si0: self.song_in, st0: self.start, en0: self.end });
    }

    fn release(&mut self) {
        let Some(d) = self.drag.take() else { return };
        if d.click {
            // hear the edit from this moment of the song
            self.events.push(LaneEvent::Seek(self.start + (self.t(d.x0) - self.song_in)));
            return;
        }
        self.emit();
    }

    fn emit(&mut self) {
        let end = if self.end_auto && self.end >= self.total - 1e-3 { None } else { Some(self.end) };
        self.events.push(LaneEvent::Placed(self.song_in, self.start, end));
    }

    pub fn cancel_drag(&mut self) -> bool {
        let Some(d) = self.drag.take() else { return false };
        self.song_in = d.si0;
        self.start = d.st0;
        self.end = d.en0;
        self.events.push(LaneEvent::Dragging(d.si0, d.st0, d.en0));
        true
    }

    // ------------------------------------------------------------ ui
    pub fn show(&mut self, ui: &mut Ui) -> egui::Response {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, HEIGHT), Sense::click_and_drag());
        self.width = w;
        self.rect = rect;
        let o = rect.min.to_vec2();
        let (pressed, released, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.interact_pos()));
        let local = pos.map(|p| p - o);
        let over = resp.hovered() || resp.contains_pointer();
        if pressed && over {
            if let Some(p) = local {
                self.press(p.x);
            }
        } else if self.drag.is_some() {
            if let Some(p) = local {
                self.moved(p.x);
            }
        }
        if released && self.drag.is_some() {
            self.release();
        }
        if let (true, Some(p)) = (over, local) {
            let icon = match self.drag {
                Some(d) if d.mode == Hit::Move => CursorIcon::Grabbing,
                Some(_) => CursorIcon::ResizeHorizontal,
                None => match self.hit(p.x) {
                    Hit::Left | Hit::Right => CursorIcon::ResizeHorizontal,
                    Hit::Move => CursorIcon::Grab,
                    Hit::Outside => CursorIcon::PointingHand,
                    Hit::None => CursorIcon::Default,
                },
            };
            ui.ctx().set_cursor_icon(icon);
        }
        self.paint(ui, rect);
        resp
    }

    fn paint(&self, ui: &Ui, rect: Rect) {
        let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let o = rect.min.to_vec2();
        let r = rect.shrink(0.5);
        p.rect_filled(r, 8.0, c::CARD);
        p.rect_stroke(r, 8.0, Stroke::new(1.0, c::HAIR), StrokeKind::Middle);
        let Some(info) = &self.info else {
            p.text(rect.center(), Align2::CENTER_CENTER, &self.busy_text, theme::ui_font(13.0, Weight::Regular), c::MUTED);
            return;
        };
        let win = self.win().translate(o);
        let wave = &info.wave;
        let mid = rect.top() + HEIGHT / 2.0;
        let amp = (HEIGHT - 18.0) / 2.0;
        if !wave.is_empty() {
            let n = wave.len();
            let (col_out, col_in) = (c::DISABLED, theme::accent_a(210));
            let inner = (self.width - 2.0 * PAD).max(1.0) as usize;
            let mut px = PAD as usize;
            while (px as f32) < self.width - PAD {
                let i = (n - 1).min(((px as f32 - PAD) / (self.width - 2.0 * PAD).max(1.0) * n as f32) as usize);
                let lo = (n - 1).min(i + 1usize.max(n / 1usize.max(inner / 2)));
                let v = if lo > i { wave[i..=lo].iter().cloned().fold(f64::MIN, f64::max) } else { wave[i] };
                let h = (v as f32 * amp).max(1.0);
                let x = rect.left() + px as f32;
                let col = if win.left() <= x && x <= win.right() { col_in } else { col_out };
                p.rect_filled(Rect::from_min_size(pos2(x, mid - h), vec2(1.4, 2.0 * h)), 0.0, col);
                px += 2;
            }
        }
        // bar lines inside the playing part
        let bar = Stroke::new(1.0, Color32::from_rgba_unmultiplied(237, 237, 237, 46));
        for b in &info.bars {
            let x = rect.left() + self.x(*b);
            if win.left() + 2.0 < x && x < win.right() - 2.0 {
                p.line_segment([pos2(x, win.top() + 3.0), pos2(x, win.top() + 9.0)], bar);
            }
        }
        // the playing part: outline + pink end handles
        p.rect_filled(win, 6.0, theme::accent_a(22));
        p.rect_stroke(win, 6.0, Stroke::new(1.4, c::ACCENT), StrokeKind::Middle);
        for x in [win.left(), win.right()] {
            p.rect_filled(Rect::from_min_size(pos2(x - 3.0, win.top() + win.height() / 2.0 - 11.0), vec2(6.0, 22.0)), 3.0, c::ACCENT);
        }
        if self.song_in + self.length() > self.dur() + 0.05 {
            // shorter than the edit: it loops
            p.text(
                pos2(win.right() - 6.0, win.top() + 8.0),
                Align2::RIGHT_CENTER,
                "loops",
                theme::ui_font(11.0, Weight::Regular),
                c::SECONDARY,
            );
        }
        if let Some(ph) = self.playhead {
            let x = rect.left() + self.x(ph);
            if win.left() <= x && x <= win.right() {
                p.line_segment([pos2(x, rect.top() + 3.0), pos2(x, rect.bottom() - 3.0)], Stroke::new(1.5, c::TEXT));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> MusicLane {
        let mut l = MusicLane::new();
        l.width = 616.0; // 600 px for the song
        l.info = Some(Arc::new(SongInfo { duration: 60.0, bpm: 120.0, ..Default::default() }));
        l.set_placement(0.0, 0.0, None, 20.0);
        l
    }

    #[test]
    fn move_trim_click() {
        let mut l = lane();
        // drag the part 100 px (= 10 s) to the right
        let x0 = l.win().center().x;
        l.press(x0);
        l.moved(x0 + 50.0);
        l.moved(x0 + 100.0);
        l.release();
        assert!((l.song_in - 10.0).abs() < 1e-5);
        assert!(matches!(l.take_events().last(), Some(LaneEvent::Placed(si, st, None)) if (si - 10.0).abs() < 1e-5 && *st == 0.0));
        // drag the left end 30 px: the music starts 3 s later, what plays stays in place
        let xl = l.win().left();
        l.press(xl);
        l.moved(xl + 30.0);
        l.release();
        assert!((l.start - 3.0).abs() < 1e-5 && (l.song_in - 13.0).abs() < 1e-5);
        // a click inside asks to hear the edit from there
        l.take_events();
        let xc = l.win().left() + 20.0;
        l.press(xc);
        l.release();
        assert!(matches!(l.take_events().last(), Some(LaneEvent::Seek(_))));
        // Esc cancels a drag
        let x1 = l.win().center().x;
        l.press(x1);
        l.moved(x1 + 80.0);
        assert!(l.cancel_drag());
        assert!((l.song_in - 13.0).abs() < 1e-5);
        assert_eq!(fmt(75.0), "1:15.0");
    }
}
