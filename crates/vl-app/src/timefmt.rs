//! How times are written in the UI.
//!
//! - Lengths and countdowns ([`dur`]): "12.3 s" under a minute, then "mm:ss", then "hh:mm:ss".
//! - Positions on a timeline ([`clock`]): "mm:ss.ss", "hh:mm:ss.ss" from an hour (always with the fraction, so a
//!   running playhead readout doesn't change shape below a minute).

/// A length or time left: `decimals` digits under a minute ("12.3 s"), whole seconds after ("01:15",
/// "01:02:05"). Negative and non-finite values count as 0.
pub fn dur(d: f64, decimals: usize) -> String {
    let d = if d.is_finite() { d.max(0.0) } else { 0.0 };
    let scale = 10f64.powi(decimals as i32);
    // decided on the value as written: 59.97 at one decimal would read "60.0 s"
    if (d * scale).round() / scale < 60.0 {
        return format!("{d:.decimals$} s");
    }
    let total = d.round() as i64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// A position: "mm:ss" plus `decimals` digits of the seconds ("01:15.50"), with hours from an hour on
/// ("01:02:05.00"). Negative and non-finite values count as 0.
pub fn clock(t: f64, decimals: usize) -> String {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let scale = 10f64.powi(decimals as i32);
    // round once, then split: 59.996 must become "01:00.00", not "00:60.00"
    let units = (t * scale).round() as i64;
    let per_min = 60 * scale as i64;
    let (mins, rest) = (units / per_min, units % per_min);
    let (h, m) = (mins / 60, mins % 60);
    let secs = rest as f64 / scale;
    let width = if decimals > 0 { 3 + decimals } else { 2 };
    let s = format!("{secs:0width$.decimals$}");
    if h > 0 {
        format!("{h:02}:{m:02}:{s}")
    } else {
        format!("{m:02}:{s}")
    }
}

/// What was typed into a time field: "75.5", "75.5 s", "01:15.5" or "1:02:05.5" (a comma works as the
/// decimal point). None if it isn't a time.
pub fn parse(text: &str) -> Option<f64> {
    let t = text.trim().trim_end_matches('s').trim().replace(',', ".");
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (whole, secs) = parts.split_at(parts.len() - 1);
    let secs: f64 = secs[0].trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 || (!whole.is_empty() && secs >= 60.0) {
        return None;
    }
    let mut v = 0.0;
    for (i, p) in whole.iter().enumerate() {
        let n: u32 = p.trim().parse().ok()?;
        // minutes after hours stay under 60
        if i > 0 && n >= 60 {
            return None;
        }
        v = v * 60.0 + n as f64;
    }
    Some(v * 60.0 + secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_times() {
        assert_eq!(parse("75.5"), Some(75.5));
        assert_eq!(parse(" 75,5 s "), Some(75.5));
        assert_eq!(parse("01:15.5"), Some(75.5));
        assert_eq!(parse("1:02:05.5"), Some(3725.5));
        assert_eq!(parse("90:00"), Some(5400.0));
        assert_eq!(parse("1:75"), None);
        assert_eq!(parse("1:61:00"), None);
        assert_eq!(parse("abc"), None);
        assert_eq!(parse(""), None);
        // what `clock` writes reads back
        for v in [0.0, 12.34, 75.5, 3725.25] {
            assert_eq!(parse(&clock(v, 2)), Some(v));
        }
    }

    #[test]
    fn lengths_switch_to_minutes_and_hours() {
        assert_eq!(dur(12.34, 1), "12.3 s");
        assert_eq!(dur(5.0, 0), "5 s");
        assert_eq!(dur(59.4, 0), "59 s");
        assert_eq!(dur(59.97, 1), "01:00");
        assert_eq!(dur(60.0, 1), "01:00");
        assert_eq!(dur(75.9, 1), "01:16");
        assert_eq!(dur(3599.4, 0), "59:59");
        assert_eq!(dur(3600.0, 0), "01:00:00");
        assert_eq!(dur(3725.0, 0), "01:02:05");
        assert_eq!(dur(-3.0, 1), "0.0 s");
        assert_eq!(dur(f64::NAN, 0), "0 s");
    }

    #[test]
    fn positions_gain_hours() {
        assert_eq!(clock(0.25, 2), "00:00.25");
        assert_eq!(clock(75.5, 2), "01:15.50");
        assert_eq!(clock(59.996, 2), "01:00.00");
        assert_eq!(clock(3725.5, 2), "01:02:05.50");
        assert_eq!(clock(3.3, 1), "00:03.3");
        assert_eq!(clock(61.0, 0), "01:01");
        assert_eq!(clock(-1.0, 2), "00:00.00");
    }
}
