//! Lucide line icons (assets/icons, ISC licence) recoloured at runtime and rendered with resvg.
//!
//! `icons::texture(ctx, "play", color, 16.0)` returns a cached egui texture (rasterised at the screen's
//! pixel density); `icons::paint(painter, rect, "play", color)` draws one centred in a rect.
use egui::{Color32, Context, Rect, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::sync::Mutex;

type IconKey = (String, [u8; 4], u32);
static CACHE: Mutex<Option<HashMap<IconKey, Option<TextureHandle>>>> = Mutex::new(None);

fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// The SVG with `currentColor` replaced and the stroke thinned to 1.5.
fn svg_source(name: &str, color: Color32, stroke: f32) -> Option<String> {
    let p = vl_core::paths::assets_dir().join("icons").join(format!("{name}.svg"));
    let s = std::fs::read_to_string(p).ok()?;
    Some(s.replace("currentColor", &hex(color)).replace("stroke-width=\"2\"", &format!("stroke-width=\"{stroke}\"")))
}

/// Rasterise an icon to RGBA (premultiplied by resvg; converted to egui's ColorImage).
pub fn rasterize(name: &str, color: Color32, px: u32, stroke: f32) -> Option<egui::ColorImage> {
    let src = svg_source(name, color, stroke)?;
    let tree = resvg::usvg::Tree::from_str(&src, &resvg::usvg::Options::default()).ok()?;
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px)?;
    let sz = tree.size();
    let tr = resvg::tiny_skia::Transform::from_scale(px as f32 / sz.width(), px as f32 / sz.height());
    resvg::render(&tree, tr, &mut pm.as_mut());
    let mut img = egui::ColorImage::from_rgba_premultiplied([px as usize, px as usize], pm.data());
    // the alpha channel carries the shape; keep the colour exact and let alpha do the antialiasing
    let a = color.a() as f32 / 255.0;
    for p in img.pixels.iter_mut() {
        let al = (p.a() as f32 * a) as u8;
        *p = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), al);
    }
    Some(img)
}

/// Cached texture of an icon at `size` points.
pub fn texture(ctx: &Context, name: &str, color: Color32, size: f32) -> Option<TextureHandle> {
    let px = (size * ctx.pixels_per_point().max(1.0) * 2.0).round().max(8.0) as u32; // 2x for crisp edges
    let key = (name.to_string(), color.to_array(), px);
    let mut guard = CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(t) = cache.get(&key) {
        return t.clone();
    }
    let tex = rasterize(name, color, px, 1.5).map(|img| ctx.load_texture(format!("icon-{name}-{px}"), img, TextureOptions::LINEAR));
    cache.insert(key, tex.clone());
    tex
}

/// Paint an icon of `size` points centred in `rect`.
pub fn paint_sized(painter: &egui::Painter, rect: Rect, name: &str, color: Color32, size: f32) {
    if let Some(t) = texture(painter.ctx(), name, color, size) {
        let r = Rect::from_center_size(rect.center(), egui::vec2(size, size));
        painter.image(t.id(), r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
}

/// Paint an icon filling `rect` (square).
#[allow(dead_code)]
pub fn paint(painter: &egui::Painter, rect: Rect, name: &str, color: Color32) {
    paint_sized(painter, rect, name, color, rect.width().min(rect.height()));
}

/// An icon as a widget (no interaction).
pub fn show(ui: &mut egui::Ui, name: &str, color: Color32, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    paint_sized(ui.painter(), rect, name, color, size);
    resp
}
