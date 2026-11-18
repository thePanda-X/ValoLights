//! Output layouts: canvas size per aspect, default layer placement and rect maths.
//!
//! Every layer has a destination rect on the canvas (fractions of W/H). Overlay layers (score bar, HUD,
//! minimap, webcam) show a live crop of the video: `src` in fractions of the game area (the webcam's may
//! reach outside it, anywhere in the frame). The crop starts on the detected HUD region and can be adjusted;
//! the layer's aspect is locked to its crop. The gameplay layer fills its destination either by cropping
//! (cover) or stretching.
use crate::numfmt::{floordiv, round_i};
use vl_core::models::{Edits, GameArea, LayerSpec, LayoutSpec, Rect4};
use vl_engine::layout::Layout;

pub const ASPECTS: &[(&str, (i64, i64))] = &[("16:9", (16, 9)), ("9:16", (9, 16)), ("1:1", (1, 1)), ("4:5", (4, 5))];
/// layer kind -> HUD ROI it shows
pub const OVERLAY_ROI: &[(&str, &str)] = &[("scorebar", "topbar"), ("hud", "hud_strip"), ("minimap", "minimap")];
pub const KILLFEED_ROWS: i32 = 4;
pub const LAYER_NAMES: &[(&str, &str)] = &[
    ("gameplay", "Gameplay"),
    ("scorebar", "Score bar"),
    ("killfeed", "Kill feed"),
    ("hud", "Health and ammo"),
    ("minimap", "Minimap"),
    ("webcam", "Webcam"),
    ("subtitles", "Subtitles"),
];
/// back to front
pub const LAYER_ORDER: &[&str] = &["gameplay", "minimap", "hud", "scorebar", "webcam", "killfeed", "subtitles"];
/// Webcam crop before the user frames it (fractions of the game area): a box in the lower left.
pub const WEBCAM_SRC: Rect4 = (0.02, 0.5, 0.24, 0.3);

pub fn aspect_ratio(aspect: &str) -> Option<(i64, i64)> {
    ASPECTS.iter().find(|a| a.0 == aspect).map(|a| a.1)
}

pub fn overlay_roi(kind: &str) -> Option<&'static str> {
    OVERLAY_ROI.iter().find(|o| o.0 == kind).map(|o| o.1)
}

/// A live crop of the video with a locked aspect (score bar, HUD, minimap, webcam).
pub fn is_overlay(kind: &str) -> bool {
    overlay_roi(kind).is_some() || kind == "webcam"
}

/// Crops that may reach outside the game area (into a recording's borders).
pub fn crops_full_frame(kind: &str) -> bool {
    kind == "webcam"
}

pub fn layer_name(kind: &str) -> &'static str {
    LAYER_NAMES.iter().find(|o| o.0 == kind).map(|o| o.1).unwrap_or("")
}

/// `max(2, int(round(v)) // 2 * 2)`
pub fn even(v: f64) -> i64 {
    2.max(floordiv(round_i(v), 2) * 2)
}

pub fn canvas_size(aspect: &str, short_side: i64) -> (i64, i64) {
    let (a, b) = aspect_ratio(aspect).unwrap_or((16, 9));
    let s = short_side as f64;
    if a >= b {
        (even(s * a as f64 / b as f64), even(s))
    } else {
        (even(s), even(s * b as f64 / a as f64))
    }
}

/// Starting crop of an overlay: its detected HUD region (the webcam: [`WEBCAM_SRC`]).
pub fn overlay_src(kind: &str, ga: &GameArea) -> Rect4 {
    if kind == "webcam" {
        return WEBCAM_SRC;
    }
    let mut lay = Layout::new(ga.w as i64, ga.h as i64);
    lay.scale = ga.hud_scale;
    let (x, y, w, h) = lay.rect(overlay_roi(kind).unwrap_or("topbar"));
    let (gw, gh) = (ga.w as f64, ga.h as f64);
    (x as f64 / gw, y as f64 / gh, w as f64 / gw, h as f64 / gh)
}

/// Width/height in pixels of an overlay's starting source region.
pub fn src_aspect_px(kind: &str, ga: &GameArea) -> f64 {
    rect_aspect_px(overlay_src(kind, ga), ga)
}

/// Width/height in pixels of a crop given in fractions of the game area.
pub fn rect_aspect_px(src: Rect4, ga: &GameArea) -> f64 {
    let (_, _, sw, sh) = src;
    (sw * ga.w as f64) / (1e-6f64).max(sh * ga.h as f64)
}

/// Keep an overlay's destination aspect equal to its crop's (height follows width).
pub fn fit_overlay_height(layer: &mut LayerSpec, ga: &GameArea, w_px: i64, h_px: i64) {
    if !is_overlay(&layer.kind) {
        return;
    }
    let (x, y, w, _) = layer.dst;
    let h = (w * w_px as f64 / rect_aspect_px(layer.src, ga)) / h_px as f64;
    layer.dst = (x, y, w, h);
}

/// Source crop (fractions of the game area) for the gameplay layer.
pub fn gameplay_src(layer: &LayerSpec, ga: &GameArea, w_px: i64, h_px: i64) -> Rect4 {
    let (_, _, dw, dh) = layer.dst;
    let z = layer.zoom.max(1.0);
    let (sw, sh);
    if layer.fit == "stretch" {
        sw = 1.0 / z;
        sh = 1.0 / z;
    } else {
        let a = (dw * w_px as f64) / (1e-6f64).max(dh * h_px as f64); // destination aspect in px
        let g = ga.w as f64 / ga.h as f64;
        if a < g {
            sh = 1.0 / z;
            sw = (a / g) / z;
        } else {
            sw = 1.0 / z;
            sh = (g / a) / z;
        }
    }
    let (px, py) = layer.pan;
    let sx = (0.0f64).max(px - sw / 2.0).min(1.0 - sw);
    let sy = (0.0f64).max(py - sh / 2.0).min(1.0 - sh);
    (sx, sy, sw, sh)
}

pub fn default_layout(aspect: &str, ga: &GameArea) -> LayoutSpec {
    let (w_px, h_px) = canvas_size(aspect, 1080);
    let is43 = (ga.w as f64) / (ga.h as f64) < 1.6;
    let vertical = aspect == "9:16" || aspect == "4:5";
    let mut gameplay = LayerSpec::new("gameplay", (0.0, 0.0, 1.0, 1.0));
    gameplay.fit = if aspect == "16:9" && is43 { "stretch".into() } else { "cover".into() };
    let mut scorebar = LayerSpec::new("scorebar", (0.04, 0.03, 0.92, 0.1));
    scorebar.visible = aspect == "9:16";
    let mut hud = LayerSpec::new("hud", (0.0, 0.85, 1.0, 0.1));
    hud.visible = aspect == "9:16";
    let mut minimap = LayerSpec::new("minimap", (0.03, 0.14, 0.32, 0.2));
    minimap.visible = false;
    let cam_dst = match aspect {
        "16:9" => (0.02, 0.03, 0.22, 0.2),
        "1:1" => (0.03, 0.62, 0.3, 0.2),
        _ => (0.06, 0.38, 0.4, 0.2),
    };
    let mut webcam = LayerSpec::new("webcam", cam_dst);
    webcam.visible = false;
    let (killfeed, subtitles);
    if aspect == "16:9" {
        let mut k = LayerSpec::new("killfeed", (0.62, 0.42, 0.36, 0.2));
        k.visible = false;
        killfeed = k;
        subtitles = LayerSpec::new("subtitles", (0.15, 0.72, 0.7, 0.12));
    } else if vertical {
        killfeed = LayerSpec::new("killfeed", (0.28, 0.135, 0.68, 0.11));
        subtitles = LayerSpec::new("subtitles", (0.06, 0.6, 0.88, 0.1));
    } else {
        // 1:1
        killfeed = LayerSpec::new("killfeed", (0.48, 0.04, 0.49, 0.16));
        subtitles = LayerSpec::new("subtitles", (0.08, 0.74, 0.84, 0.12));
    }
    for l in [&mut scorebar, &mut hud, &mut minimap, &mut webcam] {
        l.src = overlay_src(&l.kind, ga);
        fit_overlay_height(l, ga, w_px, h_px);
    }
    // HUD hugs the bottom edge
    let (hx, _, hw, hh) = hud.dst;
    hud.dst = (hx, 1.0 - hh - 0.012, hw, hh);
    LayoutSpec {
        aspect: aspect.to_string(),
        background: "blur".into(),
        layers: vec![gameplay, minimap, hud, scorebar, webcam, killfeed, subtitles],
    }
}

/// Add layers an older layout doesn't have (the webcam), hidden, in their place in the stacking order.
/// True when something was added.
pub fn complete_layout(l: &mut LayoutSpec, ga: &GameArea) -> bool {
    let missing: Vec<&str> = LAYER_ORDER.iter().copied().filter(|k| l.layer(k).is_none()).collect();
    if missing.is_empty() {
        return false;
    }
    let d = default_layout(if aspect_ratio(&l.aspect).is_some() { &l.aspect } else { "16:9" }, ga);
    let rank = |kind: &str| LAYER_ORDER.iter().position(|o| *o == kind).unwrap_or(usize::MAX);
    for k in missing {
        let Some(mut lyr) = d.layer(k).cloned() else { continue };
        lyr.visible = false;
        let at = l.layers.iter().position(|x| rank(&x.kind) > rank(k)).unwrap_or(l.layers.len());
        l.layers.insert(at, lyr);
    }
    true
}

/// The edit's layout, or the default for its format (old projects stored only a format).
pub fn effective_layout(ed: &Edits, ga: &GameArea) -> LayoutSpec {
    if let Some(l) = &ed.layout {
        let mut l = l.clone();
        complete_layout(&mut l, ga);
        return l;
    }
    let fmt = if aspect_ratio(&ed.format).is_some() { ed.format.as_str() } else { "16:9" };
    default_layout(fmt, ga)
}

/// Pixel rect; positions may be 0 or negative (off-canvas), sizes are even and at least 2.
pub fn px_rect(r: Rect4, w_px: i64, h_px: i64) -> (i64, i64, i64, i64) {
    let (x, y, w, h) = r;
    (floordiv(round_i(x * w_px as f64), 2) * 2, floordiv(round_i(y * h_px as f64), 2) * 2, even(w * w_px as f64), even(h * h_px as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_and_rects() {
        assert_eq!(canvas_size("16:9", 1080), (1920, 1080));
        assert_eq!(canvas_size("9:16", 1080), (1080, 1920));
        assert_eq!(canvas_size("4:5", 540), (540, 674));
        assert_eq!(px_rect((-0.0016, 0.5, 0.5, 0.0), 1920, 1080), (-4, 540, 960, 2));
        let ga = GameArea { x: 0, y: 0, w: 1440, h: 1080, hud_scale: 1.0 };
        let l = default_layout("9:16", &ga);
        let hud = l.layer("hud").unwrap();
        // matches the reference layout for this game area
        assert!((hud.dst.1 - 0.901175303197354).abs() < 1e-12);
        assert!((hud.src.0 - 0.18472222222222223).abs() < 1e-12);
        let sb = l.layer("scorebar").unwrap();
        assert!((sb.dst.3 - 0.07105970149253732).abs() < 1e-12);
        // the webcam starts hidden, just under the kill feed
        let kinds: Vec<&str> = l.layers.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(kinds, LAYER_ORDER);
        assert!(!l.layer("webcam").unwrap().visible);
    }

    #[test]
    fn older_layouts_get_the_webcam_layer() {
        let ga = GameArea { x: 0, y: 0, w: 1920, h: 1080, hud_scale: 1.0 };
        let mut l = default_layout("16:9", &ga);
        l.layers.retain(|x| x.kind != "webcam");
        assert!(complete_layout(&mut l, &ga));
        let kinds: Vec<&str> = l.layers.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(kinds, LAYER_ORDER);
        assert!(!complete_layout(&mut l, &ga));
    }

    #[test]
    fn overlay_height_follows_its_crop() {
        let ga = GameArea { x: 0, y: 0, w: 1920, h: 1080, hud_scale: 1.0 };
        let mut m = LayerSpec::new("minimap", (0.0, 0.0, 0.2, 0.1));
        m.src = (0.0, 0.0, 0.1, 0.2); // 192 x 216 px
        fit_overlay_height(&mut m, &ga, 1920, 1080);
        assert!((m.dst.3 - 0.2 * 1920.0 / (192.0 / 216.0) / 1080.0).abs() < 1e-9);
    }
}
