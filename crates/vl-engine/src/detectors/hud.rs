//! Bottom HUD readers: ammo counter, spectate/replay name card.
use super::{digit, sc64};
use crate::layout::Layout;
use crate::ocr;
use vl_core::image::BgrImage;

pub fn read_ammo(frame: &BgrImage, lay: &Layout) -> Option<i32> {
    let (txt, sc) = ocr::read_line(&lay.crop(frame, "ammo"));
    if sc64(sc) < 0.6 {
        return None;
    }
    // leading digits (re.match(r"\d+"))
    let digits: Vec<u32> = txt.chars().filter(|&c| c != ' ').map_while(digit).collect();
    if digits.is_empty() {
        return None;
    }
    let s: String = digits.iter().map(|d| char::from_digit(*d, 10).unwrap()).collect::<String>();
    let s = s.trim_start_matches('0');
    let s = if s.is_empty() { "0" } else { s };
    // magazine sizes top out at 100 (Odin); reserve digits are smaller and right of it
    let v: i64 = if s.len() > 18 { i64::MAX } else { s.parse().ok()? };
    if v > 100 {
        return s[..2].parse().ok();
    }
    Some(v as i32)
}

pub fn read_spectate_name(frame: &BgrImage, lay: &Layout) -> (String, f32) {
    ocr::read_line(&lay.crop(frame, "spectate"))
}
