//! HUD regions of interest in *HUD units* (fractions of the game-area height), anchored to an edge of the
//! game area. Valorant scales its HUD with the vertical resolution, so one definition works for every
//! resolution and for 16:9, 16:10 and (stretched or pillarboxed) 4:3.

/// (anchor, x, y, w, h). anchor: "tr" x measured leftwards from the right edge to the ROI's right side,
/// "tc" x = offset of the ROI centre from the horizontal centre, "tl"/"bl"/"br"/"bc" analogous.
pub const ROIS: &[(&str, &str, f64, f64, f64, f64)] = &[
    ("killfeed", "tr", 0.0, 0.075, 0.80, 0.36),
    ("topbar", "tc", 0.0, 0.0, 0.62, 0.085),
    ("timer", "tc", 0.0, 0.018, 0.105, 0.052),
    ("score_left", "tc", -0.127, 0.018, 0.062, 0.052),
    ("score_right", "tc", 0.127, 0.018, 0.062, 0.052),
    ("team_left", "tc", -0.205, 0.022, 0.07, 0.04),
    ("team_right", "tc", 0.205, 0.022, 0.07, 0.04),
    ("banner", "tc", 0.0, 0.10, 0.60, 0.17),
    ("killbanner", "bc", 0.0, 0.17, 0.16, 0.16),
    ("ammo", "bc", 0.315, 0.015, 0.075, 0.07),
    ("hp", "bc", -0.325, 0.015, 0.10, 0.07),
    ("hud_strip", "bc", 0.0, 0.0, 0.84, 0.13),
    ("spectate", "bl", 0.075, 0.195, 0.30, 0.06),
    ("minimap", "tl", 0.01, 0.045, 0.40, 0.38),
];

/// Maps HUD-unit ROIs onto pixel rectangles of a (cropped) game frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub width: i64,
    pub height: i64,
    /// HUD scale relative to default (auto-calibration hook)
    pub scale: f64,
    /// low-res / heavily compressed source (enlarged for analysis): use tolerant text masks
    pub soft: bool,
}

/// Round half to even.
pub fn round_half_even(v: f64) -> i64 {
    let r = v.round();
    if (v - v.trunc()).abs() == 0.5 {
        let f = v.floor();
        if (f as i64) % 2 == 0 {
            f as i64
        } else {
            (f + 1.0) as i64
        }
    } else {
        r as i64
    }
}

impl Layout {
    pub fn new(width: i64, height: i64) -> Self {
        Layout { width, height, scale: 1.0, soft: false }
    }

    /// (x, y, w, h) in pixels, clamped to the frame.
    pub fn rect(&self, name: &str) -> (i64, i64, i64, i64) {
        let &(_, anchor, x, y, w, h) = ROIS.iter().find(|r| r.0 == name).unwrap_or_else(|| panic!("unknown ROI {name}"));
        let hh = self.height as f64 * self.scale;
        let (pw, ph) = (w * hh, h * hh);
        let (px_off, py_off) = (x * hh, y * hh);
        let a = anchor.as_bytes();
        let x0 = match a[1] {
            b'r' => self.width as f64 - px_off - pw,
            b'l' => px_off,
            _ => self.width as f64 / 2.0 + px_off - pw / 2.0,
        };
        let y0 = if a[0] == b't' { py_off } else { self.height as f64 - py_off - ph };
        let x0 = round_half_even(x0).max(0);
        let y0 = round_half_even(y0).max(0);
        let x1 = round_half_even(x0 as f64 + pw).min(self.width);
        let y1 = round_half_even(y0 as f64 + ph).min(self.height);
        (x0, y0, x1 - x0, y1 - y0)
    }

    pub fn crop(&self, img: &vl_core::image::BgrImage, name: &str) -> vl_core::image::BgrImage {
        let (x, y, w, h) = self.rect(name);
        img.crop(x, y, w, h)
    }

    /// HUD units -> pixels.
    pub fn u(&self, v: f64) -> f64 {
        v * self.height as f64 * self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn killfeed_rect_1080p() {
        let l = Layout::new(1920, 1080);
        assert_eq!(l.rect("killfeed"), (1056, 81, 864, 389));
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(3.5), 4);
    }

    #[test]
    fn layout_anchors_scale_with_height() {
        let a = Layout::new(1280, 720).rect("killfeed");
        let b = Layout::new(1920, 1080).rect("killfeed");
        assert!((b.0 as f64 / 1.5 - a.0 as f64).abs() <= 2.0 && (b.3 as f64 / 1.5 - a.3 as f64).abs() <= 2.0);
        // 4:3 game area: kill feed stays anchored to the right edge
        let (x, _, w, _) = Layout::new(1440, 1080).rect("killfeed");
        assert_eq!(x + w, 1440);
    }
}
