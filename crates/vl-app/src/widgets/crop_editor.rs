//! Crop editor: the whole video frame with the box a layer shows (minimap, health and ammo, score bar,
//! webcam). Drag the box to move it, its corners to resize it; outside the box is dimmed.
//!
//! Works in fractions of the full video frame. The page converts to and from the layer's `src` (fractions of
//! the game area) and turns a release into one undo step.
use crate::theme::{self, c, Weight};
use egui::{pos2, vec2, Align2, Color32, CursorIcon, Key, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, TextureId, Ui};
use vl_core::models::Rect4;

const HANDLE: f32 = 9.0;
/// smallest crop, in video pixels
const MIN_PX: f64 = 16.0;

#[derive(Debug, Clone)]
struct Drag {
    /// "move" or a corner: "tl" / "tr" / "bl" / "br"
    mode: &'static str,
    start: Pos2,
    orig: Rect4,
}

/// What happened this frame.
#[derive(Debug, Default, Clone)]
pub struct CropOut {
    /// a drag ended and moved the box: the new crop (fractions of the frame)
    pub released: Option<Rect4>,
    /// a drag is running
    pub dragging: bool,
}

pub struct CropEditor {
    /// the layer being cropped
    pub kind: String,
    /// the box, fractions of the video frame
    pub live: Rect4,
    /// where the box may go (the game area for HUD crops, the whole frame for the webcam)
    pub bounds: Rect4,
    drag: Option<Drag>,
    /// video frame size in pixels
    frame_px: (f64, f64),
    /// widget rect of the last frame (checks)
    pub rect: Rect,
}

impl CropEditor {
    pub fn new(kind: &str, live: Rect4, bounds: Rect4, frame_px: (f64, f64)) -> CropEditor {
        CropEditor { kind: kind.to_string(), live, bounds, drag: None, frame_px, rect: Rect::NOTHING }
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Esc during a drag: put the box back. True when there was a drag.
    pub fn cancel_drag(&mut self) -> bool {
        match self.drag.take() {
            Some(d) => {
                self.live = d.orig;
                true
            }
            None => false,
        }
    }

    /// The video frame inside the widget (aspect kept, 16 px margin).
    pub fn frame_rect(&self) -> Rect {
        let (fw, fh) = self.frame_px;
        let m = 16.0;
        let (aw, ah) = ((self.rect.width() as f64 - 2.0 * m).max(1.0), (self.rect.height() as f64 - 2.0 * m).max(1.0));
        let s = (aw / fw.max(1.0)).min(ah / fh.max(1.0));
        let (w, h) = ((fw * s) as f32, (fh * s) as f32);
        Rect::from_center_size(self.rect.center(), vec2(w, h))
    }

    /// A rect in frame fractions -> widget space.
    pub fn to_screen(&self, r: Rect4) -> Rect {
        let f = self.frame_rect();
        let (x, y, w, h) = r;
        Rect::from_min_size(
            pos2(f.left() + (x * f.width() as f64) as f32, f.top() + (y * f.height() as f64) as f32),
            vec2((w * f.width() as f64) as f32, (h * f.height() as f64) as f32),
        )
    }

    fn corners(r: Rect) -> [(&'static str, Pos2); 4] {
        [("tl", r.left_top()), ("tr", r.right_top()), ("bl", r.left_bottom()), ("br", r.right_bottom())]
    }

    fn handle_at(&self, pos: Pos2) -> Option<&'static str> {
        let r = self.to_screen(self.live);
        let mut best = None;
        let mut dist = f32::MAX;
        for (name, c) in Self::corners(r) {
            let (dx, dy) = ((pos.x - c.x).abs(), (pos.y - c.y).abs());
            if dx <= HANDLE && dy <= HANDLE && dx + dy < dist {
                best = Some(name);
                dist = dx + dy;
            }
        }
        best
    }

    /// Draw the frame and the box, handle the pointer. `tex` = the still frame (id, w, h); `label` names
    /// the layer on the box.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, tex: Option<TextureId>, label: &str) -> CropOut {
        self.rect = rect;
        let mut out = CropOut::default();
        let resp = ui.interact(rect, ui.scope_id().with(("crop-editor", &self.kind)), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, c::BG);
        let (pressed, released, down, latest, origin, esc) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.primary_down(),
                i.pointer.latest_pos(),
                i.pointer.press_origin(),
                i.key_pressed(Key::Escape),
            )
        });
        if pressed && self.drag.is_none() && resp.is_pointer_button_down_on() {
            if let Some(pos) = origin.or(latest) {
                if let Some(h) = self.handle_at(pos) {
                    self.drag = Some(Drag { mode: h, start: pos, orig: self.live });
                } else if self.to_screen(self.live).contains(pos) {
                    self.drag = Some(Drag { mode: "move", start: pos, orig: self.live });
                }
            }
        }
        if esc {
            self.cancel_drag();
        }
        if self.drag.is_some() {
            if let Some(pos) = latest {
                self.drag_to(pos);
            }
            ui.ctx().set_cursor_icon(if self.drag.as_ref().map(|d| d.mode == "move").unwrap_or(false) {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Crosshair
            });
            ui.ctx().request_repaint();
            if released || !down {
                let d = self.drag.take().unwrap();
                if d.orig != self.live {
                    out.released = Some(self.live);
                }
            }
        } else if resp.hovered() {
            if let Some(pos) = latest {
                if let Some(h) = self.handle_at(pos) {
                    ui.ctx().set_cursor_icon(if h == "tl" || h == "br" { CursorIcon::ResizeNwSe } else { CursorIcon::ResizeNeSw });
                } else if self.to_screen(self.live).contains(pos) {
                    ui.ctx().set_cursor_icon(CursorIcon::Grab);
                }
            }
        }
        out.dragging = self.drag.is_some();
        self.paint(ui, tex, label);
        out
    }

    fn drag_to(&mut self, pos: Pos2) {
        let Some(d) = self.drag.clone() else { return };
        let f = self.frame_rect();
        let dx = (pos.x - d.start.x) as f64 / f.width().max(1.0) as f64;
        let dy = (pos.y - d.start.y) as f64 / f.height().max(1.0) as f64;
        let (bx, by, bw, bh) = self.bounds;
        let (x, y, w, h) = d.orig;
        let (min_w, min_h) = (MIN_PX / self.frame_px.0.max(1.0), MIN_PX / self.frame_px.1.max(1.0));
        self.live = if d.mode == "move" {
            (clampf(x + dx, bx, bx + bw - w), clampf(y + dy, by, by + bh - h), w, h)
        } else {
            let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
            if d.mode.contains('l') {
                x0 = clampf(x0 + dx, bx, x1 - min_w);
            }
            if d.mode.contains('r') {
                x1 = clampf(x1 + dx, x0 + min_w, bx + bw).max(x0 + min_w);
            }
            if d.mode.contains('t') {
                y0 = clampf(y0 + dy, by, y1 - min_h);
            }
            if d.mode.contains('b') {
                y1 = clampf(y1 + dy, y0 + min_h, by + bh).max(y0 + min_h);
            }
            (x0, y0, x1 - x0, y1 - y0)
        };
    }

    fn paint(&self, ui: &Ui, tex: Option<TextureId>, label: &str) {
        let f = self.frame_rect();
        let p = ui.painter_at(self.rect);
        match tex {
            Some(t) => {
                p.image(t, f, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                p.rect_filled(f, 0.0, c::CARD);
                p.text(f.center(), Align2::CENTER_CENTER, "Loading a frame of the clip…", theme::ui_font(12.0, Weight::Regular), c::MUTED);
            }
        }
        // dim everything outside the box
        let b = self.to_screen(self.live);
        let dim = Color32::from_black_alpha(165);
        for r in [
            Rect::from_min_max(f.min, pos2(f.right(), b.top())),
            Rect::from_min_max(pos2(f.left(), b.bottom()), f.max),
            Rect::from_min_max(pos2(f.left(), b.top()), pos2(b.left(), b.bottom())),
            Rect::from_min_max(pos2(b.right(), b.top()), pos2(f.right(), b.bottom())),
        ] {
            if r.width() > 0.0 && r.height() > 0.0 {
                p.rect_filled(r.intersect(f), 0.0, dim);
            }
        }
        // the game area, when the box can't leave it and it isn't the whole frame
        if self.bounds != (0.0, 0.0, 1.0, 1.0) {
            let g = self.to_screen(self.bounds);
            let pts = [g.left_top(), g.right_top(), g.right_bottom(), g.left_bottom(), g.left_top()];
            p.extend(Shape::dashed_line(&pts, Stroke::new(1.0, Color32::from_rgba_unmultiplied(237, 237, 237, 90)), 4.0, 3.0));
        }
        // the box: outline, rule-of-thirds hairlines, corner handles
        p.rect_stroke(b, 0.0, Stroke::new(1.5, c::ACCENT), StrokeKind::Middle);
        let thin = Stroke::new(1.0, Color32::from_rgba_unmultiplied(237, 237, 237, 50));
        for k in 1..3 {
            let t = k as f32 / 3.0;
            p.line_segment([pos2(b.left() + b.width() * t, b.top()), pos2(b.left() + b.width() * t, b.bottom())], thin);
            p.line_segment([pos2(b.left(), b.top() + b.height() * t), pos2(b.right(), b.top() + b.height() * t)], thin);
        }
        for (_, c) in Self::corners(b) {
            let hr = Rect::from_center_size(c, vec2(HANDLE, HANDLE));
            p.rect_filled(hr, 0.0, c::TEXT);
            p.rect_stroke(hr, 0.0, Stroke::new(1.5, c::ACCENT), StrokeKind::Middle);
        }
        // tag: layer name and the crop size in video pixels
        let (_, _, w, h) = self.live;
        let text = format!("{label}  {}×{}", (w * self.frame_px.0).round(), (h * self.frame_px.1).round());
        let font = theme::ui_font(11.0, Weight::Regular);
        let g = ui.fonts_mut(|fo| fo.layout_no_wrap(text, font, c::ON_ACCENT));
        let tw = g.size().x + 12.0;
        let y = if b.top() - 22.0 > self.rect.top() { b.top() - 22.0 } else { b.top() + 4.0 };
        let tag = Rect::from_min_size(pos2(b.left().max(self.rect.left() + 2.0), y), vec2(tw, 18.0));
        p.rect_filled(tag, 4.0, c::ACCENT);
        p.galley(tag.center() - g.size() / 2.0, g, c::ON_ACCENT);
    }
}

/// `v.clamp(lo, hi)` that never panics: when hi < lo, lo wins.
fn clampf(v: f64, lo: f64, hi: f64) -> f64 {
    v.min(hi).max(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> CropEditor {
        let mut e = CropEditor::new("minimap", (0.1, 0.1, 0.2, 0.2), (0.125, 0.0, 0.75, 1.0), (1920.0, 1080.0));
        e.rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1952.0, 1112.0)); // frame = 1920x1080 at (16, 16)
        e
    }

    #[test]
    fn move_stays_inside_the_bounds() {
        let mut e = editor();
        e.drag = Some(Drag { mode: "move", start: pos2(100.0, 100.0), orig: e.live });
        e.drag_to(pos2(-500.0, 100.0));
        assert!((e.live.0 - 0.125).abs() < 1e-9, "{:?}", e.live);
        e.drag_to(pos2(5000.0, 5000.0));
        assert!((e.live.0 + e.live.2 - 0.875).abs() < 1e-9 && (e.live.1 + e.live.3 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn corner_resize_keeps_the_opposite_corner_and_a_minimum() {
        let mut e = editor();
        let orig = e.live;
        e.drag = Some(Drag { mode: "br", start: pos2(0.0, 0.0), orig });
        e.drag_to(pos2(192.0, 108.0)); // +0.1 of the frame each way
        assert!((e.live.2 - 0.3).abs() < 1e-6 && (e.live.3 - 0.3).abs() < 1e-6 && e.live.0 == orig.0);
        e.drag_to(pos2(-3000.0, -3000.0));
        assert!((e.live.2 * 1920.0 - MIN_PX).abs() < 1e-6 && (e.live.3 * 1080.0 - MIN_PX).abs() < 1e-6);
    }
}
