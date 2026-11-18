//! Design tokens, fonts and the egui style.
//!
//! Direction: a late-night VOD review bay. Neutral graphite surfaces, hairlines instead of boxes,
//! one light-pink accent kept for the primary action, the current selection, kept footage on the
//! timeline and the headshot crosshair (the signature detail).
#![allow(dead_code)]

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle};
use std::sync::Arc;

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}
const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    // premultiplied, like Color32::from_rgba_unmultiplied but const
    Color32::from_rgba_premultiplied(
        ((r as u16 * a as u16 + 127) / 255) as u8,
        ((g as u16 * a as u16 + 127) / 255) as u8,
        ((b as u16 * a as u16 + 127) / 255) as u8,
        a,
    )
}

/// Colour tokens.
pub mod c {
    use super::*;
    // graphite ramp
    pub const DEEP: Color32 = hex(0x0f0f10); // top bar, side panels
    pub const BG: Color32 = hex(0x141415); // page
    pub const CARD: Color32 = hex(0x19191b);
    pub const RAISED: Color32 = hex(0x232325); // inputs, active rows, secondary buttons
    pub const RAISED_HI: Color32 = hex(0x2b2b2e); // hover on raised
    pub const HAIR: Color32 = rgba(237, 237, 237, 20); // rgba(237,237,237,0.08)
    pub const HAIR_STRONG: Color32 = rgba(237, 237, 237, 36); // 0.14
    pub const TEXT: Color32 = hex(0xededed);
    pub const SECONDARY: Color32 = hex(0xa1a1a6);
    pub const MUTED: Color32 = hex(0x85858b);
    pub const DISABLED: Color32 = hex(0x55555a);
    // accent
    pub const ACCENT: Color32 = hex(0xf4a6c4);
    pub const ACCENT_HOVER: Color32 = hex(0xf7bcd3);
    pub const ACCENT_PRESSED: Color32 = hex(0xe894b4);
    pub const ON_ACCENT: Color32 = hex(0x2a1520);
    pub const ACCENT_SOFT: Color32 = rgba(244, 166, 196, 31); // 0.12
                                                              // semantic
    pub const WIN: Color32 = hex(0x4cc38a);
    pub const LOSS: Color32 = hex(0xe5484d);
    // other values used by the stylesheet
    pub const SEG_CHECKED: Color32 = hex(0x38383c);
    pub const SLIDER_FILL: Color32 = hex(0x6e6e74);
    pub const SCROLL_HOVER: Color32 = hex(0x3a3a3e);
    pub const SWITCH_OFF_HOVER: Color32 = hex(0x36363a);
    pub const VIDEO_BG: Color32 = hex(0x0b0b0c);
    pub const LINE: Color32 = hex(0x2a2a2d);
}

/// Accent alpha helper: `accent(90)` = the accent at that alpha.
pub fn accent_a(a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(244, 166, 196, a)
}

pub const SWATCHES: [&str; 6] = ["#F4A6C4", "#EDEDED", "#FFD23F", "#3DD6F5", "#B8F35C", "#FF7A45"];

/// Parse "#rrggbb".
pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(hex(v))
}

/// Spacing scale (4 px grid): S1..S8.
pub const S1: f32 = 4.0;
pub const S2: f32 = 8.0;
pub const S3: f32 = 12.0;
pub const S4: f32 = 16.0;
pub const S5: f32 = 20.0;
pub const S6: f32 = 24.0;
pub const S8: f32 = 32.0;

// ------------------------------------------------------------------ fonts
/// Font families. Inter (variable) is registered at three weights; numbers use a mono font.
pub const INTER: &str = "inter";
pub const INTER_MEDIUM: &str = "inter-500";
pub const INTER_SEMIBOLD: &str = "inter-600";
pub const MONO: &str = "mono";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
}

pub fn family(w: Weight) -> FontFamily {
    match w {
        Weight::Regular => FontFamily::Proportional,
        Weight::Medium => FontFamily::Name(INTER_MEDIUM.into()),
        Weight::SemiBold => FontFamily::Name(INTER_SEMIBOLD.into()),
    }
}

pub fn ui_font(size: f32, w: Weight) -> FontId {
    FontId::new(size, family(w))
}

pub fn mono_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Text roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Body,
    Brand,
    Title,
    Section,
    Secondary,
    Muted,
    Figure,
    Mono,
    Timecode,
    Pill,
}

impl Role {
    pub fn font(self) -> FontId {
        match self {
            Role::Body | Role::Secondary => ui_font(13.0, Weight::Regular),
            Role::Brand => ui_font(15.0, Weight::SemiBold),
            Role::Title => ui_font(20.0, Weight::Medium),
            Role::Section => ui_font(12.0, Weight::SemiBold),
            Role::Muted => ui_font(12.0, Weight::Regular),
            Role::Figure => FontId::new(22.0, FontFamily::Name("mono-500".into())),
            Role::Mono | Role::Pill => mono_font(12.0),
            Role::Timecode => FontId::new(15.0, FontFamily::Name("mono-500".into())),
        }
    }
    pub fn color(self) -> Color32 {
        match self {
            Role::Body | Role::Brand | Role::Title | Role::Figure | Role::Timecode => c::TEXT,
            Role::Section | Role::Secondary | Role::Pill => c::SECONDARY,
            Role::Muted | Role::Mono => c::MUTED,
        }
    }
}

fn read_font(paths: &[std::path::PathBuf]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Inter from assets/fonts; mono = Cascadia Mono or Consolas from Windows, else egui's bundled Hack.
pub fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let fonts_dir = vl_core::paths::assets_dir().join("fonts");
    let win = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let fallback_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let fallback_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let wght =
        |w: f32| egui::epaint::text::FontTweak { coords: egui::epaint::text::VariationCoords::new([(b"wght", w)]), ..Default::default() };
    if let Some(inter) = read_font(&[fonts_dir.join("Inter-Variable.ttf")]) {
        let blob: Arc<Vec<u8>> = Arc::new(inter);
        for (name, w) in [(INTER, 400.0), (INTER_MEDIUM, 500.0), (INTER_SEMIBOLD, 600.0)] {
            defs.font_data.insert(name.into(), Arc::new(FontData::from_blob(blob.clone(), 0).tweak(wght(w))));
        }
    }
    let has_inter = defs.font_data.contains_key(INTER);
    let fam = |primary: &str| -> Vec<String> {
        let mut v = if has_inter { vec![primary.to_string()] } else { vec![] };
        v.extend(fallback_prop.iter().cloned());
        v
    };
    defs.families.insert(FontFamily::Proportional, fam(INTER));
    defs.families.insert(FontFamily::Name(INTER_MEDIUM.into()), fam(INTER_MEDIUM));
    defs.families.insert(FontFamily::Name(INTER_SEMIBOLD.into()), fam(INTER_SEMIBOLD));

    // mono: Cascadia Mono (variable on Windows 11) or Consolas
    let mono_files = [win.join("CascadiaMono.ttf"), win.join("consola.ttf")];
    let mut mono_fam = fallback_mono.clone();
    let mut mono_med = fallback_mono.clone();
    if let Some(m) = read_font(&mono_files) {
        let blob: Arc<Vec<u8>> = Arc::new(m);
        defs.font_data.insert(MONO.into(), Arc::new(FontData::from_blob(blob.clone(), 0)));
        defs.font_data.insert("mono-500".into(), Arc::new(FontData::from_blob(blob, 0).tweak(wght(500.0))));
        mono_fam.insert(0, MONO.into());
        mono_med.insert(0, "mono-500".into());
    }
    defs.families.insert(FontFamily::Monospace, mono_fam);
    defs.families.insert(FontFamily::Name("mono-500".into()), mono_med);
    defs
}

// ------------------------------------------------------------------ style
/// Install fonts and the style (the QSS translated to egui Visuals).
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, ui_font(12.0, Weight::Regular)),
            (TextStyle::Body, ui_font(13.0, Weight::Regular)),
            (TextStyle::Button, ui_font(13.0, Weight::Regular)),
            (TextStyle::Heading, ui_font(20.0, Weight::Medium)),
            (TextStyle::Monospace, mono_font(12.0)),
        ]
        .into();
        let sp = &mut style.spacing;
        sp.item_spacing = egui::vec2(8.0, 8.0);
        sp.button_padding = egui::vec2(12.0, 6.0);
        sp.interact_size = egui::vec2(30.0, 30.0);
        sp.window_margin = Margin::same(24);
        sp.menu_margin = Margin::same(4);
        sp.combo_height = 300.0;
        sp.text_edit_width = 240.0;
        sp.tooltip_width = 420.0;
        sp.scroll = egui::style::ScrollStyle::solid();
        sp.scroll.bar_width = 8.0;
        sp.scroll.handle_min_length = 32.0;
        sp.scroll.bar_inner_margin = 0.0;
        sp.scroll.bar_outer_margin = 0.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = c::BG;
        v.window_fill = c::RAISED; // tooltips, menus and combo popups (dialogs use dialog_frame)
        v.window_stroke = Stroke::new(1.0, c::HAIR);
        v.window_corner_radius = CornerRadius::same(12);
        v.window_shadow = Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(140) };
        v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(120) };
        v.menu_corner_radius = CornerRadius::same(8);
        v.faint_bg_color = c::CARD;
        v.extreme_bg_color = c::RAISED; // text edits
        v.text_edit_bg_color = Some(c::RAISED);
        v.code_bg_color = c::RAISED;
        v.hyperlink_color = c::ACCENT;
        v.warn_fg_color = hex(0xffd23f);
        v.error_fg_color = c::LOSS;
        v.selection.bg_fill = c::ACCENT_SOFT;
        v.selection.stroke = Stroke::new(1.0, c::ACCENT);
        v.slider_trailing_fill = true;
        v.button_frame = true;
        v.striped = false;
        v.text_cursor.stroke = Stroke::new(1.5, c::TEXT);

        let r8 = CornerRadius::same(8);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = c::BG;
        w.noninteractive.weak_bg_fill = c::BG;
        w.noninteractive.bg_stroke = Stroke::new(1.0, c::HAIR);
        w.noninteractive.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.noninteractive.corner_radius = r8;
        w.inactive.bg_fill = c::RAISED;
        w.inactive.weak_bg_fill = c::RAISED;
        w.inactive.bg_stroke = Stroke::new(1.0, c::HAIR);
        w.inactive.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.inactive.corner_radius = r8;
        w.inactive.expansion = 0.0;
        w.hovered.bg_fill = c::RAISED_HI;
        w.hovered.weak_bg_fill = c::RAISED_HI;
        w.hovered.bg_stroke = Stroke::new(1.0, c::HAIR_STRONG);
        w.hovered.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.hovered.corner_radius = r8;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = c::CARD;
        w.active.weak_bg_fill = c::CARD;
        w.active.bg_stroke = Stroke::new(1.0, c::HAIR_STRONG);
        w.active.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.active.corner_radius = r8;
        w.active.expansion = 0.0;
        w.open.bg_fill = c::RAISED;
        w.open.weak_bg_fill = c::RAISED;
        w.open.bg_stroke = Stroke::new(1.0, c::HAIR_STRONG);
        w.open.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.open.corner_radius = r8;

        style.interaction.tooltip_delay = 0.5;
        style.interaction.selectable_labels = false;
        style.animation_time = 0.14;
    });
}

/// Frame of a side panel: deep background, 20 px padding.
pub fn side_panel_frame() -> egui::Frame {
    egui::Frame::new().fill(c::DEEP).inner_margin(Margin::same(S5 as i8))
}

/// Popup / context menu frame.
pub fn menu_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(c::RAISED)
        .stroke(Stroke::new(1.0, c::HAIR))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(4))
        .shadow(Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(120) })
}

/// Dialog frame.
pub fn dialog_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(c::BG)
        .stroke(Stroke::new(1.0, c::HAIR_STRONG))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(S6 as i8))
        .shadow(Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(160) })
}
