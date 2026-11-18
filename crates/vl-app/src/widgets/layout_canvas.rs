//! Interactive layout editor: place and size every layer on the output canvas.
//!
//! Draws a real frame of the clip the way the renderer will compose it (blurred background, gameplay crop,
//! live HUD crops, your kill feed rows, a subtitle sample) and lets you drag layers and their corner handles.
//! The canvas edits the layout it is given in place; [`CanvasOut`] carries what happened
//! (`changed` on release / nudge, `selected`, `editStarted`).
use crate::backend;
use crate::theme::{self, c, Weight};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, Key, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui,
};
use image::RgbaImage;
use vl_core::models::{GameArea, LayerSpec, LayoutSpec, Rect4};

pub const HANDLE: f32 = 8.0;
/// px in widget space
pub const SNAP: f32 = 6.0;
/// a layer can hang off the canvas, but this much of it (fraction of the canvas) always stays on
pub const KEEP_ON: f64 = 0.05;
pub const MIN_W: f64 = 0.03;
pub const MIN_H: f64 = 0.02;

/// A drag in progress: mode ("move" or a corner "tl" / "tr" / "bl" / "br"), start position, original dst.
#[derive(Debug, Clone)]
pub struct Drag {
    pub mode: String,
    pub start: Pos2,
    pub orig: Rect4,
}

/// What happened on the canvas this frame.
#[derive(Debug, Default, Clone)]
pub struct CanvasOut {
    /// the layout was edited (on release when something moved)
    pub changed: bool,
    /// a layer was selected by clicking it
    pub selected: Option<String>,
    /// a drag that may change the layout is about to begin (closes the undo group)
    pub edit_started: bool,
}

struct Tex {
    tex: TextureHandle,
    w: f32,
    h: f32,
}

pub struct LayoutCanvas {
    pub sel: String,
    pub hover: String,
    pub drag: Option<Drag>,
    guides: Vec<(char, f32)>,
    pub feed_rows: i32,
    pub feed_newest: String,
    pub sub_text: String,
    /// most words a caption shows at once (0 = all)
    pub sub_max_words: usize,
    pub sub_color: Color32,
    pub sub_hl: Color32,
    still: Option<Tex>,
    still_blur: Option<Tex>,
    rows: Vec<Tex>,
    /// widget rect of the last frame
    pub rect: Rect,
    aspect: String,
    ga: Option<GameArea>,
}

impl Default for LayoutCanvas {
    fn default() -> Self {
        LayoutCanvas {
            sel: "gameplay".into(),
            hover: String::new(),
            drag: None,
            guides: vec![],
            feed_rows: backend::KILLFEED_ROWS,
            feed_newest: "bottom".into(),
            sub_text: "nice shot, that's the round".into(),
            sub_max_words: 0,
            sub_color: Color32::WHITE,
            sub_hl: c::ACCENT,
            still: None,
            still_blur: None,
            rows: vec![],
            rect: Rect::NOTHING,
            aspect: "16:9".into(),
            ga: None,
        }
    }
}

fn tex_options() -> TextureOptions {
    TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..TextureOptions::LINEAR }
}

fn to_tex(ctx: &egui::Context, name: &str, img: &RgbaImage) -> Tex {
    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    Tex { tex: ctx.load_texture(name, ci, tex_options()), w: img.width() as f32, h: img.height() as f32 }
}

/// `#RRGGBB` -> colour (white when unreadable).
pub fn hex_color(s: &str) -> Color32 {
    let h = s.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    match (p(0), p(2), p(4)) {
        (Some(r), Some(g), Some(b)) => Color32::from_rgb(r, g, b),
        _ => Color32::WHITE,
    }
}

fn dashed_rect(p: &Painter, r: Rect, stroke: Stroke) {
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    // dashes 4 px, gaps 2 px (times the pen width)
    let w = stroke.width.max(1.0);
    p.extend(Shape::dashed_line(&pts, stroke, 4.0 * w, 2.0 * w));
}

impl LayoutCanvas {
    // ------------------------------------------------------------ data
    /// A frame of the clip (the whole video frame) and a small version of its game area for the blurred
    /// background; None = no frame.
    pub fn set_still(&mut self, ctx: &egui::Context, img: Option<(&RgbaImage, &RgbaImage)>) {
        match img {
            Some((full, small)) => {
                self.still = Some(to_tex(ctx, "layout-still", full));
                self.still_blur = Some(to_tex(ctx, "layout-still-blur", small));
            }
            None => {
                self.still = None;
            }
        }
    }

    /// No frame (project changed): placeholders until the next one arrives.
    pub fn clear_still(&mut self) {
        self.still = None;
    }

    pub fn clear_rows(&mut self) {
        self.rows.clear();
    }

    pub fn has_still(&self) -> bool {
        self.still.is_some()
    }

    /// The still frame's texture (the whole video frame), for the crop editor.
    pub fn still_texture(&self) -> Option<egui::TextureId> {
        self.still.as_ref().map(|t| t.tex.id())
    }

    /// Texture coordinates of a crop given in fractions of the game area (the still is the whole frame).
    fn uv(&self, r: Rect4) -> Rect {
        let (x, y, w, h) = r;
        match (&self.still, self.ga) {
            (Some(img), Some(ga)) => {
                let (iw, ih) = (img.w as f64, img.h as f64);
                let (gw, gh) = (ga.w as f64, ga.h as f64);
                Rect::from_min_size(
                    pos2(((ga.x as f64 + x * gw) / iw) as f32, ((ga.y as f64 + y * gh) / ih) as f32),
                    vec2((w * gw / iw) as f32, (h * gh / ih) as f32),
                )
            }
            _ => Rect::from_min_size(pos2(x as f32, y as f32), vec2(w as f32, h as f32)),
        }
    }

    pub fn set_rows(&mut self, ctx: &egui::Context, rows: &[RgbaImage]) {
        self.rows = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.width() > 0 && r.height() > 0)
            .map(|(i, r)| to_tex(ctx, &format!("layout-row-{i}"), r))
            .collect();
    }

    pub fn rows_len(&self) -> usize {
        self.rows.len()
    }

    pub fn select(&mut self, kind: &str) {
        self.sel = kind.to_string();
    }

    /// The layout was replaced under a drag (undo): drop the drag.
    pub fn drop_drag(&mut self) {
        self.drag = None;
        self.guides.clear();
    }

    // ------------------------------------------------------------ geometry
    fn canvas_px(&self) -> (f64, f64) {
        let (w, h) = backend::canvas_size(&self.aspect, 1080);
        (w as f64, h as f64)
    }

    /// The output frame inside the widget (16 px margin, aspect kept).
    pub fn frame_rect(&self) -> Rect {
        let (cw, ch) = self.canvas_px();
        let m = 16.0;
        let (ww, wh) = (self.rect.width() as f64, self.rect.height() as f64);
        let (aw, ah) = ((ww - 2.0 * m).max(1.0), (wh - 2.0 * m).max(1.0));
        let s = (aw / cw).min(ah / ch);
        let (fw, fh) = (cw * s, ch * s);
        Rect::from_min_size(
            pos2(self.rect.left() + ((ww - fw) / 2.0) as f32, self.rect.top() + ((wh - fh) / 2.0) as f32),
            vec2(fw as f32, fh as f32),
        )
    }

    pub fn dst_rect(&self, lyr: &LayerSpec) -> Rect {
        let f = self.frame_rect();
        let (x, y, w, h) = lyr.dst;
        let (fw, fh) = (f.width() as f64, f.height() as f64);
        Rect::from_min_size(pos2(f.left() + (x * fw) as f32, f.top() + (y * fh) as f32), vec2((w * fw) as f32, (h * fh) as f32))
    }

    fn corners(r: Rect) -> [(&'static str, Pos2); 4] {
        [("tl", r.left_top()), ("tr", r.right_top()), ("bl", r.left_bottom()), ("br", r.right_bottom())]
    }

    // ------------------------------------------------------------ frame
    /// Draw and handle input in `rect`. `l` is edited in place while dragging.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, l: Option<&mut LayoutSpec>, ga: Option<&GameArea>) -> CanvasOut {
        self.rect = rect;
        let mut out = CanvasOut::default();
        let resp = ui.interact(rect, ui.scope_id().with("layout-canvas"), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, c::BG);
        let (Some(l), Some(ga)) = (l, ga) else {
            return out;
        };
        self.aspect = l.aspect.clone();
        self.ga = Some(*ga);
        if l.layer(&self.sel).is_none() {
            self.sel = "gameplay".into();
        }
        self.interact(ui, &resp, l, ga, &mut out);
        self.paint(ui, l);
        out
    }

    fn interact(&mut self, ui: &Ui, resp: &egui::Response, l: &mut LayoutSpec, ga: &GameArea, out: &mut CanvasOut) {
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
        // press
        if pressed && self.drag.is_none() && resp.is_pointer_button_down_on() {
            if let Some(pos) = origin.or(latest) {
                self.press(pos, l, out);
            }
        }
        // Esc cancels a drag
        if esc && self.drag.is_some() {
            self.cancel_drag(l);
        }
        // move
        if self.drag.is_some() {
            if let Some(pos) = latest {
                self.drag_to(pos, l, ga);
            }
            ui.ctx().request_repaint();
        } else if resp.hovered() {
            if let Some(pos) = latest {
                let handle = self.handle_at(pos, l);
                let k = if !handle.is_empty() {
                    ui.ctx().set_cursor_icon(if handle == "tl" || handle == "br" {
                        CursorIcon::ResizeNwSe
                    } else {
                        CursorIcon::ResizeNeSw
                    });
                    self.sel.clone()
                } else {
                    let k = self.layer_at(pos, l);
                    if !k.is_empty() {
                        ui.ctx().set_cursor_icon(CursorIcon::Move);
                    }
                    k
                };
                self.hover = k;
            }
        } else {
            self.hover.clear();
        }
        // release (or the button went up without us seeing it)
        if self.drag.is_some() && (released || !down) {
            self.release(l, out);
        }
    }

    fn press(&mut self, pos: Pos2, l: &LayoutSpec, out: &mut CanvasOut) {
        let handle = self.handle_at(pos, l);
        if !handle.is_empty() {
            out.edit_started = true;
            let orig = l.layer(&self.sel).map(|x| x.dst).unwrap_or((0.0, 0.0, 1.0, 1.0));
            self.drag = Some(Drag { mode: handle, start: pos, orig });
            return;
        }
        let kind = self.layer_at(pos, l);
        if !kind.is_empty() {
            if kind != self.sel {
                self.sel = kind.clone();
                out.selected = Some(kind.clone());
            }
            out.edit_started = true;
            let orig = l.layer(&kind).map(|x| x.dst).unwrap_or((0.0, 0.0, 1.0, 1.0));
            self.drag = Some(Drag { mode: "move".into(), start: pos, orig });
        }
    }

    fn drag_to(&mut self, pos: Pos2, l: &mut LayoutSpec, ga: &GameArea) {
        let Some(d) = self.drag.clone() else { return };
        let (x, y, w, h) = d.orig;
        let f = self.frame_rect();
        let dx = (pos.x - d.start.x) as f64 / f.width() as f64;
        let dy = (pos.y - d.start.y) as f64 / f.height() as f64;
        self.guides.clear();
        let sel = self.sel.clone();
        let new = if d.mode == "move" {
            let (nx, ny) = self.snap_move(x + dx, y + dy, w, h);
            let (cx, cy) = Self::clamp_pos(nx, ny, w, h);
            (cx, cy, w, h)
        } else if backend::layer_is_overlay(&sel) {
            let a = l.layer(&sel).map(|x| backend::crop_aspect_px(x.src, ga)).unwrap_or(1.0);
            self.resize_locked(&d.mode, (x, y, w, h), dx, dy, a)
        } else {
            let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
            if d.mode.contains('l') {
                x0 = (x1 - MIN_W).min(self.snap_edge(x0 + dx, 'x'));
            }
            if d.mode.contains('r') {
                x1 = (x0 + MIN_W).max(self.snap_edge(x1 + dx, 'x'));
            }
            if d.mode.contains('t') {
                y0 = (y1 - MIN_H).min(self.snap_edge(y0 + dy, 'y'));
            }
            if d.mode.contains('b') {
                y1 = (y0 + MIN_H).max(self.snap_edge(y1 + dy, 'y'));
            }
            (x0, y0, x1 - x0, y1 - y0)
        };
        if let Some(lyr) = l.layer_mut(&sel) {
            lyr.dst = new;
        }
    }

    /// Corner resize of an overlay whose aspect is locked to its HUD crop: the pointer's movement is
    /// projected onto the box diagonal (so both sideways and up/down drags work, in proportion to the box),
    /// and the opposite corner stays where it was.
    /// `a` = the crop's width / height in pixels.
    fn resize_locked(&mut self, mode: &str, r: Rect4, dx: f64, dy: f64, a: f64) -> Rect4 {
        let (x, y, w, h) = r;
        let (cw, ch) = self.canvas_px();
        let f = self.frame_rect();
        let (fw, fh) = (f.width() as f64, f.height() as f64);
        let (wp, hp) = (w * fw, h * fh);
        let mx = dx * fw * if mode.contains('l') { -1.0 } else { 1.0 };
        let my = dy * fh * if mode.contains('t') { -1.0 } else { 1.0 };
        let k = (mx * wp + my * hp) / (1e-6f64).max(wp * wp + hp * hp);
        let mut nw = MIN_W.max(w * (1.0 + k));
        if mode.contains('r') {
            // snap the moving edge
            nw = self.snap_edge(x + nw, 'x') - x;
        } else {
            nw = (x + w) - self.snap_edge(x + w - nw, 'x');
        }
        nw = MIN_W.max(nw);
        let nh = nw * cw / a / ch;
        let nx = if mode.contains('l') { x + w - nw } else { x };
        let ny = if mode.contains('t') { y + h - nh } else { y };
        (nx, ny, nw, nh)
    }

    /// Keep a sliver of the layer on the canvas so it can always be seen and grabbed again.
    pub fn clamp_pos(x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
        let (kx, ky) = (w.min(KEEP_ON), h.min(KEEP_ON));
        (x.max(kx - w).min(1.0 - kx), y.max(ky - h).min(1.0 - ky))
    }

    fn release(&mut self, l: &LayoutSpec, out: &mut CanvasOut) {
        if let Some(d) = self.drag.take() {
            self.guides.clear();
            if let Some(lyr) = l.layer(&self.sel) {
                if lyr.dst != d.orig {
                    out.changed = true;
                }
            }
        }
    }

    /// Move the selected layer by a fraction of the canvas. Some(changed) when there was a visible layer to
    /// move, None when there is nothing visible to move.
    pub fn nudge(&mut self, l: &mut LayoutSpec, dx: f64, dy: f64) -> Option<bool> {
        if self.drag.is_some() {
            return None;
        }
        let lyr = l.layer_mut(&self.sel)?;
        if !lyr.visible {
            return None;
        }
        let (x, y, w, h) = lyr.dst;
        let (nx, ny) = Self::clamp_pos(x + dx, y + dy, w, h);
        if (nx, ny) != (x, y) {
            lyr.dst = (nx, ny, w, h);
            return Some(true);
        }
        Some(false)
    }

    /// Esc during a drag: put the layer back. True when a drag was cancelled.
    pub fn cancel_drag(&mut self, l: &mut LayoutSpec) -> bool {
        let Some(d) = self.drag.take() else { return false };
        if let Some(lyr) = l.layer_mut(&self.sel) {
            lyr.dst = d.orig;
        }
        self.guides.clear();
        true
    }

    pub fn layer_at(&self, pos: Pos2, l: &LayoutSpec) -> String {
        for lyr in l.layers.iter().rev().filter(|x| x.visible) {
            if lyr.kind != "gameplay" && self.dst_rect(lyr).contains(pos) {
                return lyr.kind.clone();
            }
        }
        if let Some(g) = l.layer("gameplay") {
            if g.visible && self.dst_rect(g).contains(pos) {
                return "gameplay".into();
            }
        }
        String::new()
    }

    /// Corner handle of the selected (visible) layer under the pointer, or "".
    pub fn handle_at(&self, pos: Pos2, l: &LayoutSpec) -> String {
        let Some(cur) = l.layer(&self.sel).filter(|x| x.visible) else { return String::new() };
        let (mut best, mut dist) = ("", 1e9f32);
        for (name, c) in Self::corners(self.dst_rect(cur)) {
            let (dx, dy) = ((pos.x - c.x).abs(), (pos.y - c.y).abs());
            // tiny layers: the nearest corner wins
            if dx <= HANDLE && dy <= HANDLE && dx + dy < dist {
                best = name;
                dist = dx + dy;
            }
        }
        best.to_string()
    }

    // ------------------------------------------------------------ snapping
    fn snap_move(&mut self, mut x: f64, mut y: f64, w: f64, h: f64) -> (f64, f64) {
        let f = self.frame_rect();
        let (tx, ty) = (SNAP as f64 / f.width() as f64, SNAP as f64 / f.height() as f64);
        for (cand, target) in [(x, 0.0), (x + w, 1.0), (x + w / 2.0, 0.5)] {
            if (cand - target).abs() < tx {
                x += target - cand;
                self.guides.push(('x', f.left() + (target * f.width() as f64) as f32));
                break;
            }
        }
        for (cand, target) in [(y, 0.0), (y + h, 1.0), (y + h / 2.0, 0.5)] {
            if (cand - target).abs() < ty {
                y += target - cand;
                self.guides.push(('y', f.top() + (target * f.height() as f64) as f32));
                break;
            }
        }
        (x, y)
    }

    fn snap_edge(&mut self, v: f64, axis: char) -> f64 {
        let f = self.frame_rect();
        let t = SNAP as f64 / if axis == 'x' { f.width() } else { f.height() } as f64;
        for target in [0.0, 0.5, 1.0] {
            if (v - target).abs() < t {
                let g =
                    if axis == 'x' { f.left() + (target * f.width() as f64) as f32 } else { f.top() + (target * f.height() as f64) as f32 };
                self.guides.push((axis, g));
                return target;
            }
        }
        v
    }

    // ------------------------------------------------------------ paint
    fn paint(&self, ui: &Ui, l: &LayoutSpec) {
        let f = self.frame_rect();
        let p = ui.painter_at(f);
        p.rect_filled(f, 0.0, Color32::BLACK);
        let (cw, ch) = backend::canvas_size(&l.aspect, 1080);
        if let (Some(_), true, Some(img)) = (&self.still, l.background == "blur", &self.still_blur) {
            // cover-fit the blurred still, darkened like the render
            let s = (f.width() / img.w).max(f.height() / img.h);
            let (w, h) = (img.w * s, img.h * s);
            p.image(
                img.tex.id(),
                Rect::from_center_size(f.center(), vec2(w, h)),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            p.rect_filled(f, 0.0, Color32::from_black_alpha(70));
        }
        for lyr in l.layers.iter().filter(|x| x.visible) {
            let r = self.dst_rect(lyr);
            let overlay = backend::layer_is_overlay(&lyr.kind);
            match (&self.still, lyr.kind.as_str()) {
                (None, k) if k == "gameplay" || overlay => self.paint_placeholder(ui, &p, r, backend::layer_name(k), k == "gameplay"),
                (Some(img), "gameplay") => {
                    if let Some(ga) = self.ga {
                        let src = backend::gameplay_src(lyr, &ga, cw, ch);
                        p.image(img.tex.id(), r, self.uv(src), Color32::WHITE);
                    }
                }
                (Some(img), _) if overlay => {
                    p.image(img.tex.id(), r, self.uv(lyr.src), Color32::WHITE);
                }
                (_, "killfeed") => self.paint_feed(&p, r),
                (_, "subtitles") => self.paint_subs(ui, &p, r, f),
                _ => {}
            }
        }
        // rounded frame corners
        p.rect_stroke(f.expand(4.0), 10.0, Stroke::new(8.0, c::BG), StrokeKind::Middle);
        // chrome: hover hairline, selection outline + handles, snap guides
        let cp = ui.painter_at(self.rect);
        for lyr in l.layers.iter().filter(|x| x.visible) {
            let r = self.dst_rect(lyr);
            if lyr.kind == self.sel {
                cp.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.5, c::ACCENT), StrokeKind::Middle);
                for (_, c) in Self::corners(r) {
                    let hr = Rect::from_center_size(c, vec2(HANDLE, HANDLE));
                    cp.rect_filled(hr, 0.0, c::TEXT);
                    cp.rect_stroke(hr, 0.0, Stroke::new(1.5, c::ACCENT), StrokeKind::Middle);
                }
                self.tag(ui, &cp, r, f, backend::layer_name(&lyr.kind));
            } else if lyr.kind == self.hover {
                dashed_rect(&cp, r.shrink(0.5), Stroke::new(1.0, Color32::from_rgba_unmultiplied(237, 237, 237, 90)));
            }
        }
        for (axis, v) in &self.guides {
            let pts = if *axis == 'x' { [pos2(*v, f.top()), pos2(*v, f.bottom())] } else { [pos2(f.left(), *v), pos2(f.right(), *v)] };
            cp.extend(Shape::dashed_line(&pts, Stroke::new(1.0, c::ACCENT), 4.0, 2.0));
        }
        cp.rect_stroke(f.expand(0.5), 6.0, Stroke::new(1.0, c::HAIR), StrokeKind::Middle);
    }

    /// No frame of the clip yet (still loading, or the source video is missing): labelled boxes instead.
    fn paint_placeholder(&self, ui: &Ui, p: &Painter, r: Rect, name: &str, back: bool) {
        p.rect_filled(r, 0.0, if back { c::CARD } else { c::RAISED });
        dashed_rect(p, r.shrink(0.5), Stroke::new(1.0, Color32::from_rgba_unmultiplied(237, 237, 237, 50)));
        if back {
            return; // the gameplay box is the whole frame; its name sits on the selection tag
        }
        let _ = ui;
        p.text(r.center(), Align2::CENTER_CENTER, name, theme::ui_font(11.0, Weight::Regular), c::MUTED);
    }

    fn paint_feed(&self, p: &Painter, r: Rect) {
        let gap = 0.12f32;
        let n = self.feed_rows.max(1) as usize;
        let row_h = r.height() / (n as f32 + gap * (n as f32 - 1.0));
        let mut rows: Vec<&Tex> = self.rows.iter().take(n).collect();
        if self.feed_newest == "top" {
            rows.reverse();
        }
        if rows.is_empty() {
            dashed_rect(p, r, Stroke::new(1.0, Color32::from_rgba_unmultiplied(237, 237, 237, 60)));
            return;
        }
        for (i, img) in rows.iter().enumerate() {
            let mut w = img.w * row_h / img.h.max(1.0);
            let mut h = row_h;
            if w > r.width() {
                h = row_h * r.width() / w;
                w = r.width();
            }
            let dst = Rect::from_min_size(pos2(r.right() - w, r.top() + i as f32 * row_h * (1.0 + gap)), vec2(w, h));
            p.image(img.tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }

    fn paint_subs(&self, ui: &Ui, p: &Painter, r: Rect, f: Rect) {
        let px = 8.0f32.max((f.height() * if f.width() < f.height() { 0.03 } else { 0.05 }).floor());
        let font = theme::ui_font(px, Weight::SemiBold);
        let mut words: Vec<&str> = self.sub_text.split_whitespace().collect();
        // "Words at a time" in Subtitles: the sample shows as much as a caption would
        if self.sub_max_words > 0 {
            words.truncate(self.sub_max_words);
        }
        let spoken = 1.min(words.len().saturating_sub(1));
        let mut job = LayoutJob::default();
        for (i, w) in words.iter().enumerate() {
            let col = if i == spoken { self.sub_hl } else { self.sub_color };
            job.append(w, 0.0, TextFormat::simple(font.clone(), col));
            if i + 1 < words.len() {
                job.append(" ", 0.0, TextFormat::simple(font.clone(), col));
            }
        }
        let g = ui.fonts_mut(|fo| fo.layout_job(job));
        let (tw, th) = (g.size().x, g.size().y);
        let x = r.center().x - tw / 2.0;
        let bx = Rect::from_min_max(pos2(x - 6.0, r.bottom() - th - 8.0), pos2(x + tw + 6.0, r.bottom()));
        p.rect_filled(bx, 0.0, Color32::from_black_alpha(150));
        p.galley(pos2(x, bx.top() + 4.0), g, Color32::WHITE);
    }

    fn tag(&self, ui: &Ui, p: &Painter, r: Rect, f: Rect, text: &str) {
        let font = theme::ui_font(11.0, Weight::Regular);
        let g = ui.fonts_mut(|fo| fo.layout_no_wrap(text.to_string(), font, c::ON_ACCENT));
        let tw = g.size().x + 12.0;
        let y = if r.top() - 20.0 > f.top() - 18.0 { r.top() - 20.0 } else { r.top() + 4.0 };
        let tag = Rect::from_min_size(pos2(r.left(), y.max(self.rect.top() + 2.0)), vec2(tw, 18.0));
        p.rect_filled(tag, 4.0, c::ACCENT);
        p.galley(tag.center() - g.size() / 2.0, g, c::ON_ACCENT);
    }
}
