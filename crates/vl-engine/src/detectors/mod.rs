//! HUD detectors: kill feed rows, top bar (score / timer), bottom HUD (ammo / spectate card).
pub mod hud;
pub mod killfeed;
pub mod topbar;

/// Value of an ASCII or full-width decimal digit (what OCR output may contain).
pub(crate) fn digit(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        '\u{FF10}'..='\u{FF19}' => Some(c as u32 - 0xFF10),
        _ => None,
    }
}

/// OCR score as stored (rounded to 5 decimals).
pub(crate) fn sc64(s: f32) -> f64 {
    (s as f64 * 1e5).round() / 1e5
}
