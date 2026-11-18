//! Score + round timer from the top bar.
use super::{digit, killfeed::color_masks, sc64};
use crate::layout::Layout;
use crate::ocr;
use vl_core::image::BgrImage;

#[derive(Debug, Clone, PartialEq)]
pub struct TopbarReading {
    pub left: Option<i32>,
    pub right: Option<i32>,
    /// seconds remaining, None when hidden (spike planted / transitions)
    pub timer: Option<i32>,
    /// "teal" / "red" / ""
    pub left_color: &'static str,
}

fn score(img: &BgrImage) -> Option<i32> {
    let (txt, sc) = ocr::read_line(img);
    if sc64(sc) < 0.6 {
        return None;
    }
    let t: String = txt.chars().filter(|&c| c != ' ').collect();
    if t.is_empty() || !t.chars().all(|c| digit(c).is_some()) {
        return None;
    }
    let v = t.chars().try_fold(0i64, |a, c| a.checked_mul(10)?.checked_add(digit(c)? as i64))?;
    (0..=30).contains(&v).then_some(v as i32)
}

/// `(\d)[:.](\d{2})` search.
fn timer(img: &BgrImage) -> Option<i32> {
    let (txt, sc) = ocr::read_line(img);
    if sc64(sc) < 0.6 {
        return None;
    }
    let c: Vec<char> = txt.chars().filter(|&c| c != ' ').collect();
    for i in 0..c.len().saturating_sub(3) {
        if let (Some(m), true, Some(a), Some(b)) = (digit(c[i]), c[i + 1] == ':' || c[i + 1] == '.', digit(c[i + 2]), digit(c[i + 3])) {
            return Some((m * 60 + a * 10 + b) as i32);
        }
    }
    None
}

fn team_color(img: &BgrImage) -> &'static str {
    let (teal, red) = color_masks(img, false);
    let (t, r) = (teal.frac(), red.frac());
    // below the threshold, or NaN
    if !matches!(t.max(r).partial_cmp(&0.05), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)) {
        return "";
    }
    if t > r {
        "teal"
    } else {
        "red"
    }
}

/// Share of red pixels in the timer area: the red spike icon replaces the round timer while the spike is
/// planted (see `rounds::SPIKE_SHARE`). Cheap enough for every scanned frame.
pub fn spike_share(frame: &BgrImage, lay: &Layout) -> f32 {
    let roi = lay.crop(frame, "timer");
    let n = roi.w * roi.h;
    if n == 0 {
        return 0.0;
    }
    let red = roi
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|p| {
            let (h, s, v) = crate::imgproc::hsv_px(p[0], p[1], p[2]);
            (h <= 8 || h >= 170) && s >= 120 && v >= 70
        })
        .count();
    red as f32 / n as f32
}

pub fn read(frame: &BgrImage, lay: &Layout) -> TopbarReading {
    TopbarReading {
        left: score(&lay.crop(frame, "score_left")),
        right: score(&lay.crop(frame, "score_right")),
        timer: timer(&lay.crop(frame, "timer")),
        left_color: team_color(&lay.crop(frame, "team_left")),
    }
}
