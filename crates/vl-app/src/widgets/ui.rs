//! Shared UI building blocks so every page uses the same spacing and controls.
//!
//! Everything here is plain egui: functions take `&mut Ui` and return a `Response`
//! (`.clicked()`, `.changed()`), builders implement `egui::Widget` (`ui.add(Button::primary("Export"))`).
#![allow(dead_code)]

use crate::icons;
use crate::theme::{self, c, Role, Weight, S2, S3, S5, S6};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontId, Galley, Id, Key, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, Widget,
};
use std::sync::Arc;

// ------------------------------------------------------------------ text helpers
/// One line of text, cut with "…" to `width` (the full text is not shown; add a tooltip if it matters).
pub fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_string(), TextFormat::simple(font, color));
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Elide in the middle ("C:\Users\…\data"), for paths.
pub fn elide_middle(ui: &Ui, text: &str, font: &FontId, width: f32) -> String {
    let w = |s: &str| ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x);
    if w(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len() / 2);
    while lo < hi {
        let k = (lo + hi).div_ceil(2);
        let s: String = chars[..k].iter().chain(['…'].iter()).chain(chars[chars.len() - k..].iter()).collect();
        if w(&s) <= width {
            lo = k
        } else {
            hi = k - 1
        }
    }
    chars[..lo].iter().chain(['…'].iter()).chain(chars[chars.len() - lo..].iter()).collect()
}

pub fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE).size().x)
}

/// A label in one of the text roles (title, section, muted, mono, figure, ...).
pub fn label(ui: &mut Ui, text: impl Into<String>, role: Role) -> Response {
    let color = if ui.is_enabled() { role.color() } else { c::DISABLED };
    ui.add(egui::Label::new(egui::RichText::new(text.into()).font(role.font()).color(color)).selectable(false))
}

/// A wrapping label.
pub fn label_wrap(ui: &mut Ui, text: impl Into<String>, role: Role) -> Response {
    let color = if ui.is_enabled() { role.color() } else { c::DISABLED };
    ui.add(egui::Label::new(egui::RichText::new(text.into()).font(role.font()).color(color)).wrap().selectable(false))
}

/// A label cut with "…" to the available width; the full text in the tooltip when cut.
pub fn label_elided(ui: &mut Ui, text: &str, role: Role) -> Response {
    let color = if ui.is_enabled() { role.color() } else { c::DISABLED };
    ui.add(egui::Label::new(egui::RichText::new(text).font(role.font()).color(color)).truncate().selectable(false))
}

/// Section label (12 px semibold, secondary).
pub fn section(ui: &mut Ui, text: &str) -> Response {
    label(ui, text, Role::Section)
}

/// Quiet explanatory text under a control (muted, wraps).
pub fn hint(ui: &mut Ui, text: &str) -> Response {
    label_wrap(ui, text, Role::Muted)
}

/// A 1 px hairline across the available width.
pub fn hline(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, c::HAIR);
}

/// Vertical space.
pub fn gap(ui: &mut Ui, h: f32) {
    ui.add_space(h);
}

// ------------------------------------------------------------------ buttons
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BtnRole {
    /// pink, for the one main action of a page
    Primary,
    /// raised graphite (default)
    #[default]
    Secondary,
    /// no background until hovered, secondary text
    Ghost,
}

/// Text button with a role and an optional lucide icon.
pub struct Button<'a> {
    text: String,
    role: BtnRole,
    icon: Option<&'a str>,
    icon_right: bool,
    enabled: bool,
    min_height: f32,
    min_width: f32,
    tooltip: Option<String>,
}

impl<'a> Button<'a> {
    pub fn new(text: impl Into<String>) -> Self {
        Button {
            text: text.into(),
            role: BtnRole::Secondary,
            icon: None,
            icon_right: false,
            enabled: true,
            min_height: 30.0,
            min_width: 0.0,
            tooltip: None,
        }
    }
    pub fn primary(text: impl Into<String>) -> Self {
        Self::new(text).role(BtnRole::Primary)
    }
    pub fn ghost(text: impl Into<String>) -> Self {
        Self::new(text).role(BtnRole::Ghost)
    }
    pub fn role(mut self, r: BtnRole) -> Self {
        self.role = r;
        self
    }
    pub fn icon(mut self, name: &'a str) -> Self {
        self.icon = Some(name);
        self
    }
    /// Icon after the text ("Next >").
    pub fn icon_right(mut self, name: &'a str) -> Self {
        self.icon = Some(name);
        self.icon_right = true;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn min_height(mut self, h: f32) -> Self {
        self.min_height = h;
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
    pub fn tooltip(mut self, t: impl Into<String>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let enabled = self.enabled && ui.is_enabled();
        let font =
            if self.role == BtnRole::Primary { theme::ui_font(13.0, Weight::SemiBold) } else { theme::ui_font(13.0, Weight::Regular) };
        let pad = if self.role == BtnRole::Primary { 16.0 } else { 12.0 };
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(self.text.clone(), font.clone(), Color32::PLACEHOLDER));
        let icon_w = if self.icon.is_some() { 16.0 + if self.text.is_empty() { 0.0 } else { 6.0 } } else { 0.0 };
        let w = (galley.size().x + icon_w + 2.0 * pad).max(self.min_width);
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let (rect, mut resp) = ui.allocate_exact_size(vec2(w, self.min_height), sense);
        if ui.is_rect_visible(rect) {
            let hovered = enabled && resp.hovered();
            let pressed = enabled && resp.is_pointer_button_down_on();
            let (bg, fg, border) = match (self.role, enabled) {
                (BtnRole::Primary, false) => (c::RAISED, c::DISABLED, None),
                (BtnRole::Primary, true) => (
                    if pressed {
                        c::ACCENT_PRESSED
                    } else if hovered {
                        c::ACCENT_HOVER
                    } else {
                        c::ACCENT
                    },
                    c::ON_ACCENT,
                    None,
                ),
                (BtnRole::Secondary, false) => (c::CARD, c::DISABLED, Some(c::HAIR)),
                (BtnRole::Secondary, true) => (
                    if pressed {
                        c::CARD
                    } else if hovered {
                        c::RAISED_HI
                    } else {
                        c::RAISED
                    },
                    c::TEXT,
                    Some(c::HAIR),
                ),
                (BtnRole::Ghost, false) => (Color32::TRANSPARENT, c::DISABLED, None),
                (BtnRole::Ghost, true) => (
                    if pressed {
                        c::RAISED_HI
                    } else if hovered {
                        c::RAISED
                    } else {
                        Color32::TRANSPARENT
                    },
                    if hovered { c::TEXT } else { c::SECONDARY },
                    None,
                ),
            };
            let p = ui.painter();
            p.rect_filled(rect, 8.0, bg);
            if let Some(b) = border {
                p.rect_stroke(rect, 8.0, Stroke::new(1.0, b), StrokeKind::Inside);
            }
            if resp.has_focus() {
                p.rect_stroke(rect.expand(1.5), 9.5, Stroke::new(1.0, c::SECONDARY), StrokeKind::Outside);
            }
            let content_w = galley.size().x + icon_w;
            let mut x = rect.center().x - content_w / 2.0;
            let icon_col = match self.role {
                BtnRole::Primary if enabled => c::ON_ACCENT,
                BtnRole::Ghost if enabled && !hovered => c::SECONDARY,
                _ if !enabled => c::DISABLED,
                _ => c::TEXT,
            };
            if let (Some(name), false) = (self.icon, self.icon_right) {
                icons::paint_sized(p, Rect::from_min_size(pos2(x, rect.center().y - 8.0), vec2(16.0, 16.0)), name, icon_col, 16.0);
                x += icon_w;
            }
            p.galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley.clone(), fg);
            if let (Some(name), true) = (self.icon, self.icon_right) {
                let ix = x + galley.size().x + 6.0;
                icons::paint_sized(p, Rect::from_min_size(pos2(ix, rect.center().y - 8.0), vec2(16.0, 16.0)), name, icon_col, 16.0);
            }
        }
        if enabled {
            resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        if let Some(t) = self.tooltip {
            resp = resp.on_hover_text(t);
        }
        resp
    }
}

/// Shorthand: `button(ui, "Save", BtnRole::Primary)`.
pub fn button(ui: &mut Ui, text: &str, role: BtnRole) -> Response {
    ui.add(Button::new(text).role(role))
}

/// 32x32 icon-only button, optional checked state.
pub struct IconButton<'a> {
    icon: &'a str,
    tip: String,
    size: f32,
    checked: bool,
    enabled: bool,
    color: Option<Color32>,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: &'a str, tip: impl Into<String>) -> Self {
        IconButton { icon, tip: tip.into(), size: 16.0, checked: false, enabled: true, color: None }
    }
    pub fn size(mut self, s: f32) -> Self {
        self.size = s;
        self
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
}

impl Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let enabled = self.enabled && ui.is_enabled();
        let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), if enabled { Sense::click() } else { Sense::hover() });
        if ui.is_rect_visible(rect) {
            let hovered = enabled && resp.hovered();
            let pressed = enabled && resp.is_pointer_button_down_on();
            let bg = if pressed {
                c::RAISED_HI
            } else if hovered || self.checked {
                c::RAISED
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, 8.0, bg);
            if resp.has_focus() {
                ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0, c::SECONDARY), StrokeKind::Inside);
            }
            let col = if !enabled {
                c::DISABLED
            } else if hovered || self.checked {
                self.color.unwrap_or(c::TEXT)
            } else {
                self.color.unwrap_or(c::SECONDARY)
            };
            icons::paint_sized(ui.painter(), rect, self.icon, col, self.size);
        }
        let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        if self.tip.is_empty() {
            resp
        } else {
            resp.on_hover_text(self.tip).on_disabled_hover_text("")
        }
    }
}

pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> Response {
    ui.add(IconButton::new(icon, tip))
}

// ------------------------------------------------------------------ toggle
pub const SWITCH_W: f32 = 30.0;
pub const SWITCH_H: f32 = 18.0;

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    let (a, b) = (a.to_srgba_unmultiplied(), b.to_srgba_unmultiplied());
    Color32::from_rgba_unmultiplied(l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2]), l(a[3], b[3]))
}

/// A slide toggle in `r` (t = 0 off … 1 on). Shared by [`Toggle`] and list rows.
pub fn paint_switch(p: &egui::Painter, r: Rect, t: f32, enabled: bool, hover: bool, focus: bool) {
    let (mut off_track, mut on_track) =
        (if hover { c::SWITCH_OFF_HOVER } else { c::RAISED_HI }, if hover { c::ACCENT_HOVER } else { c::ACCENT });
    let (mut off_knob, mut on_knob) = (if hover { c::TEXT } else { c::SECONDARY }, c::ON_ACCENT);
    if !enabled {
        off_track = c::RAISED;
        off_knob = c::DISABLED;
        on_knob = c::RAISED;
        on_track = theme::accent_a(90);
    }
    let rad = r.height() / 2.0;
    if focus {
        p.rect_stroke(r.expand(2.5), rad + 2.5, Stroke::new(1.2, c::SECONDARY), StrokeKind::Middle);
    }
    let track = r.shrink(0.5);
    p.rect_filled(track, rad, mix(off_track, on_track, t));
    if t < 1.0 {
        p.rect_stroke(
            track,
            rad,
            Stroke::new(1.0, mix(Color32::from_rgba_unmultiplied(237, 237, 237, 36), Color32::TRANSPARENT, t)),
            StrokeKind::Middle,
        );
    }
    let d = r.height() - 6.0;
    let x = r.left() + 3.0 + t * (r.width() - 6.0 - d);
    p.circle_filled(pos2(x + d / 2.0, r.top() + 3.0 + d / 2.0), d / 2.0, mix(off_knob, on_knob, t));
}

/// A slide toggle with an optional label on its right. Every checkbox in the app is one of these.
/// The response is `changed()` when clicked (or Space while focused).
pub struct Toggle<'a> {
    on: &'a mut bool,
    text: String,
    enabled: bool,
}

impl<'a> Toggle<'a> {
    pub fn new(on: &'a mut bool) -> Self {
        Toggle { on, text: String::new(), enabled: true }
    }
    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = t.into();
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
}

const GAP: f32 = 10.0;

impl Widget for Toggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let enabled = self.enabled && ui.is_enabled();
        let font = theme::ui_font(13.0, Weight::Regular);
        let text_w = if self.text.is_empty() { 0.0 } else { GAP + text_width(ui, &self.text, &font) };
        let want = vec2(SWITCH_W + 6.0 + text_w, (SWITCH_H + 6.0).max(22.0));
        let w = want.x.min(ui.available_width().max(SWITCH_W + 6.0));
        let (rect, mut resp) = ui.allocate_exact_size(vec2(w, want.y), if enabled { Sense::click() } else { Sense::hover() });
        let toggled = enabled && (resp.clicked() || (resp.has_focus() && ui.input(|i| i.key_pressed(Key::Space))));
        if toggled {
            *self.on = !*self.on;
            resp.mark_changed();
        }
        // the knob slides (OutCubic, 140 ms) – snaps when the toggle first appears
        let t = ui.ctx().animate_bool_with_time_and_easing(resp.id, *self.on, 0.14, egui::emath::easing::cubic_out);
        if ui.is_rect_visible(rect) {
            let r = Rect::from_min_size(pos2(rect.left() + 3.0, rect.center().y - SWITCH_H / 2.0), vec2(SWITCH_W, SWITCH_H));
            paint_switch(ui.painter(), r, t, enabled, enabled && resp.hovered(), resp.has_focus());
            if !self.text.is_empty() {
                let tx = r.right() + GAP;
                let g = elided(ui, &self.text, font, if enabled { c::TEXT } else { c::DISABLED }, rect.right() - tx);
                ui.painter().galley(pos2(tx, rect.center().y - g.size().y / 2.0), g, c::TEXT);
            }
        }
        if enabled {
            resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        resp
    }
}

// ------------------------------------------------------------------ slider
/// Horizontal slider for scrolling side panels: the mouse wheel only changes the value while the slider
/// has keyboard focus (click it first), so scrolling a panel past it never changes a value by accident.
pub struct Slider<'a> {
    value: &'a mut f64,
    lo: f64,
    hi: f64,
    step: f64,
    width: Option<f32>,
    enabled: bool,
}

impl<'a> Slider<'a> {
    pub fn new(value: &'a mut f64, lo: f64, hi: f64) -> Self {
        Slider { value, lo, hi, step: (hi - lo) / 100.0, width: None, enabled: true }
    }
    /// Keyboard / wheel step (default 1 % of the range).
    pub fn step(mut self, s: f64) -> Self {
        self.step = s;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
}

impl Widget for Slider<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let enabled = self.enabled && ui.is_enabled();
        let w = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, mut resp) = ui.allocate_exact_size(vec2(w, 20.0), if enabled { Sense::click_and_drag() } else { Sense::hover() });
        let span = (self.hi - self.lo).max(1e-9);
        let old = *self.value;
        let x0 = rect.left() + 6.0;
        let x1 = rect.right() - 6.0;
        if enabled {
            if resp.is_pointer_button_down_on() || resp.dragged() {
                resp.request_focus();
                if let Some(p) = resp.interact_pointer_pos() {
                    let f = ((p.x - x0) / (x1 - x0).max(1.0)).clamp(0.0, 1.0) as f64;
                    *self.value = self.lo + f * span;
                }
            }
            if resp.has_focus() {
                let (left, right, wheel) = ui.input(|i| {
                    (
                        i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown),
                        i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp),
                        if resp.hovered() { i.smooth_scroll_delta.y } else { 0.0 },
                    )
                });
                if left {
                    *self.value -= self.step;
                }
                if right {
                    *self.value += self.step;
                }
                if wheel != 0.0 {
                    *self.value += self.step * wheel.signum() as f64;
                    ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
                }
            }
            *self.value = self.value.clamp(self.lo, self.hi);
            if (*self.value - old).abs() > 0.0 {
                resp.mark_changed();
            }
        }
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            let cy = rect.center().y;
            let f = ((*self.value - self.lo) / span).clamp(0.0, 1.0) as f32;
            let groove = Rect::from_min_max(pos2(x0, cy - 2.0), pos2(x1, cy + 2.0));
            p.rect_filled(groove, 2.0, c::RAISED_HI);
            let hx = x0 + f * (x1 - x0);
            if enabled {
                p.rect_filled(Rect::from_min_max(groove.min, pos2(hx, groove.max.y)), 2.0, c::SLIDER_FILL);
            }
            p.circle_filled(pos2(hx, cy), 6.0, if enabled { c::TEXT } else { c::DISABLED });
            if resp.has_focus() {
                p.circle_stroke(pos2(hx, cy), 8.5, Stroke::new(1.0, c::SECONDARY));
            }
        }
        resp
    }
}

// ------------------------------------------------------------------ segmented control
/// A row of mutually exclusive options on a raised track. `columns` = 0 → one row.
/// Returns a response that is `changed()` when the user picked another option.
pub fn segmented<K: Clone + PartialEq>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    options: &[(K, &str)],
    current: &mut K,
    columns: usize,
) -> Response {
    let cols = if columns == 0 { options.len().max(1) } else { columns };
    let rows = options.len().div_ceil(cols);
    let w = ui.available_width();
    let h = rows as f32 * 28.0 + (rows.saturating_sub(1)) as f32 * 2.0 + 4.0;
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let enabled = ui.is_enabled();
    ui.painter().rect_filled(rect, 8.0, c::RAISED);
    let cell_w = (rect.width() - 4.0 - (cols - 1) as f32 * 2.0) / cols as f32;
    let base = ui.scope_id().with(id_salt);
    for (i, (k, text)) in options.iter().enumerate() {
        let (r, cix) = (i / cols, i % cols);
        let cell = Rect::from_min_size(
            pos2(rect.left() + 2.0 + cix as f32 * (cell_w + 2.0), rect.top() + 2.0 + r as f32 * 30.0),
            vec2(cell_w, 28.0),
        );
        let cr = ui.interact(cell, base.with(i), if enabled { Sense::click() } else { Sense::hover() });
        let checked = *current == *k;
        if checked {
            ui.painter().rect_filled(cell, 6.0, c::SEG_CHECKED);
        }
        if cr.has_focus() {
            ui.painter().rect_stroke(cell, 6.0, Stroke::new(1.0, c::SECONDARY), StrokeKind::Inside);
        }
        let col = if !enabled {
            c::DISABLED
        } else if checked || cr.hovered() {
            c::TEXT
        } else {
            c::SECONDARY
        };
        let g = elided(ui, text, theme::ui_font(13.0, Weight::Regular), col, cell.width() - 8.0);
        ui.painter().galley(cell.center() - g.size() / 2.0, g, col);
        if enabled && (cr.clicked() || (cr.has_focus() && ui.input(|i| i.key_pressed(Key::Space) || i.key_pressed(Key::Enter)))) && !checked
        {
            *current = k.clone();
            resp.mark_changed();
        }
        if enabled {
            cr.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }
    resp
}

// ------------------------------------------------------------------ inputs
/// Single-line text field in the app style (raised, 30 px high, 8 px radius).
pub fn text_input(ui: &mut Ui, text: &mut String, placeholder: &str, width: f32) -> Response {
    let te = egui::TextEdit::singleline(text)
        .hint_text(egui::RichText::new(placeholder).color(c::MUTED))
        .desired_width(width)
        .min_size(vec2(0.0, 30.0))
        .margin(egui::Margin::symmetric(8, 7))
        .font(theme::ui_font(13.0, Weight::Regular))
        .text_color(c::TEXT)
        .background_color(c::RAISED)
        .align(egui::Align2::LEFT_CENTER);
    input_frame(ui, |ui| ui.add(te))
}

/// Multi-line text area in the app style.
pub fn text_area(ui: &mut Ui, text: &mut String, rows: usize, width: f32) -> Response {
    let te = egui::TextEdit::multiline(text)
        .desired_rows(rows)
        .desired_width(width)
        .margin(egui::Margin::same(8))
        .font(theme::mono_font(12.5))
        .text_color(c::TEXT)
        .background_color(c::RAISED);
    input_frame(ui, |ui| ui.add(te))
}

/// Inputs: hair border, slightly stronger when focused.
fn input_frame(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    ui.scope(|ui| {
        let v = &mut ui.visuals_mut().widgets;
        for w in [&mut v.inactive, &mut v.hovered, &mut v.active] {
            w.corner_radius = CornerRadius::same(8);
            w.bg_stroke = Stroke::new(1.0, c::HAIR);
        }
        ui.visuals_mut().selection.stroke = Stroke::new(1.0, c::HAIR_STRONG);
        add(ui)
    })
    .inner
}

/// Number field with a suffix. Drag or type.
pub fn spin(ui: &mut Ui, value: &mut f64, lo: f64, hi: f64, decimals: usize, suffix: &str, width: f32) -> Response {
    let dv = egui::DragValue::new(value).range(lo..=hi).fixed_decimals(decimals).suffix(suffix).speed((hi - lo) / 300.0);
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 30.0;
        ui.style_mut().override_font_id = Some(theme::ui_font(13.0, Weight::Regular));
        ui.add_sized(vec2(width, 30.0), dv)
    })
    .inner
}

/// Drop-down choice: `combo(ui, "model", &mut cfg.model, &[("small", "Faster")], 280.0)`.
pub fn combo<K: Clone + PartialEq>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    current: &mut K,
    options: &[(K, &str)],
    width: f32,
) -> Response {
    let shown = options.iter().find(|(k, _)| k == current).map(|(_, t)| t.to_string()).unwrap_or_default();
    let mut changed = false;
    let mut r = ui
        .scope(|ui| {
            ui.spacing_mut().interact_size.y = 30.0;
            ui.spacing_mut().button_padding = vec2(8.0, 6.0);
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(egui::RichText::new(shown).color(c::TEXT))
                .width(width)
                .height(300.0)
                .show_ui(ui, |ui| {
                    for (k, t) in options {
                        if ui.selectable_label(k == current, *t).clicked() && k != current {
                            *current = k.clone();
                            changed = true;
                        }
                    }
                })
                .response
        })
        .inner;
    if changed {
        r.mark_changed();
    }
    r
}

/// A swatch button (24x24, 2 px text-coloured ring when checked).
pub fn swatch(ui: &mut Ui, color: Color32, checked: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    ui.painter().rect_filled(rect, 6.0, color);
    if checked {
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(2.0, c::TEXT), StrokeKind::Inside);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip)
}

/// 4 px accent progress bar. `frac` 0..1.
pub fn progress_bar(ui: &mut Ui, frac: f32, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    paint_progress(ui.painter(), rect, frac);
    resp
}

pub fn paint_progress(p: &egui::Painter, rect: Rect, frac: f32) {
    p.rect_filled(rect, 2.0, c::RAISED);
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        p.rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * f, rect.height())), 2.0, c::ACCENT);
    }
}

// ------------------------------------------------------------------ side panel
/// Side panel column: deep background, 20 px padding, groups 24 px apart,
/// section label 8 px above its items.
///
/// ```ignore
/// side_panel(ui, 320.0, |p| {
///     p.section(Some("Quick cut"), |ui| { ... });
///     p.section(None, |ui| { ... });
/// });
/// ```
pub fn side_panel<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut PanelBuilder) -> R) -> R {
    let h = ui.available_height();
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, c::DEEP);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(S5)).layout(egui::Layout::top_down(egui::Align::Min)));
    child.spacing_mut().item_spacing.y = 0.0;
    let mut out = None;
    egui::ScrollArea::vertical().id_salt(ui.scope_id().with("panel-scroll")).auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_width(width - 2.0 * S5);
        let mut b = PanelBuilder { ui, first: true };
        out = Some(add(&mut b));
    });
    out.expect("panel contents")
}

pub struct PanelBuilder<'u> {
    pub ui: &'u mut Ui,
    first: bool,
}

impl PanelBuilder<'_> {
    /// A group: 24 px below the previous one, an optional section label 8 px above its items.
    pub fn section<R>(&mut self, title: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
        if !self.first {
            self.ui.add_space(S6);
        }
        self.first = false;
        if let Some(t) = title {
            section(self.ui, t);
            self.ui.add_space(S2);
        }
        self.ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing.y = S2;
                add(ui)
            })
            .inner
    }
}

// ------------------------------------------------------------------ choice list
/// One row of a [`choice_list`]: a title on the left, quiet mono meta on the right, optional muted sub line.
pub struct ChoiceRow<K> {
    pub key: K,
    pub title: String,
    pub meta: String,
    pub sub: String,
}

impl<K> ChoiceRow<K> {
    pub fn new(key: K, title: impl Into<String>, meta: impl Into<String>) -> Self {
        ChoiceRow { key, title: title.into(), meta: meta.into(), sub: String::new() }
    }
    pub fn sub(mut self, s: impl Into<String>) -> Self {
        self.sub = s.into();
        self
    }
}

/// Rows with selection = raised row. Returns the key of the clicked row.
pub fn choice_list<K: Clone + PartialEq>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    rows: &[ChoiceRow<K>],
    selected: Option<&K>,
    row_h: f32,
) -> Option<K> {
    let mut chosen = None;
    let base = ui.scope_id().with(id_salt);
    let w = ui.available_width();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        for (i, row) in rows.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
            let _ = base.with(i);
            let sel = selected == Some(&row.key);
            let bg = if sel {
                c::RAISED
            } else if resp.hovered() {
                c::CARD
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, 8.0, bg);
            if resp.has_focus() {
                ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0, c::HAIR_STRONG), StrokeKind::Inside);
            }
            let inner = rect.shrink2(vec2(S3, 0.0));
            let meta_g = ui.fonts_mut(|f| f.layout_no_wrap(row.meta.clone(), Role::Mono.font(), c::MUTED));
            let meta_w = meta_g.size().x;
            ui.painter().galley(pos2(inner.right() - meta_w, inner.center().y - meta_g.size().y / 2.0), meta_g, c::MUTED);
            let tw = inner.width() - meta_w - S2;
            let tg = elided(ui, &row.title, Role::Body.font(), c::TEXT, tw);
            if row.sub.is_empty() {
                ui.painter().galley(pos2(inner.left(), inner.center().y - tg.size().y / 2.0), tg, c::TEXT);
            } else {
                let sg = elided(ui, &row.sub, Role::Muted.font(), c::MUTED, tw);
                let total = tg.size().y + sg.size().y;
                let y = inner.center().y - total / 2.0;
                let th = tg.size().y;
                ui.painter().galley(pos2(inner.left(), y), tg, c::TEXT);
                ui.painter().galley(pos2(inner.left(), y + th), sg, c::MUTED);
            }
            if resp.clicked() || (resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space))) {
                chosen = Some(row.key.clone());
            }
        }
    });
    chosen
}

// ------------------------------------------------------------------ figures, marks
/// A big mono number with a muted caption below.
pub fn figure(ui: &mut Ui, value: &str, caption: &str) -> Response {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        label(ui, value, Role::Figure);
        label(ui, caption, Role::Muted);
    })
    .response
}

/// The headshot mark: a small crosshair (signature detail).
pub fn paint_crosshair(p: &egui::Painter, center: Pos2, r: f32, color: Color32) {
    let st = Stroke::new((r * 0.28).max(1.2), color);
    p.circle_stroke(center, r * 0.62, st);
    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let a = center + vec2(dx * r * 0.35, dy * r * 0.35);
        let b = center + vec2(dx * r, dy * r);
        p.line_segment([a, b], st);
    }
}

/// Small accent crosshair used wherever a headshot is shown.
pub fn headshot_mark(ui: &mut Ui, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_crosshair(ui.painter(), rect.center(), size / 2.0 - 1.0, c::ACCENT);
    resp
}

/// A pill (mono text on a raised rounded background).
pub fn pill(ui: &mut Ui, text: &str) -> Response {
    let g = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), Role::Pill.font(), c::SECONDARY));
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(16.0, 8.0), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, c::RAISED);
    ui.painter().galley(rect.min + vec2(8.0, 4.0), g, c::SECONDARY);
    resp
}

/// Text placed right-aligned and elided inside `rect` (top-bar toast).
pub fn paint_elided_right(ui: &Ui, rect: Rect, text: &str, role: Role) {
    if text.is_empty() {
        return;
    }
    let g = elided(ui, text, role.font(), role.color(), rect.width());
    let pos = pos2(rect.right() - g.size().x, rect.center().y - g.size().y / 2.0);
    ui.painter().galley(pos, g, role.color());
}

/// Paint text centred at a point.
pub fn paint_text(ui: &Ui, pos: Pos2, align: Align2, text: &str, role: Role) -> Rect {
    ui.painter().text(pos, align, text, role.font(), role.color())
}

/// A focusable rect: draws nothing, returns the response (for custom widgets).
pub fn interact_rect(ui: &Ui, rect: Rect, id: Id) -> Response {
    ui.interact(rect, id, Sense::click())
}
