//! Colour looks as ffmpeg filter snippets, scaled by intensity (0..1).

/// (key, label) in display order.
pub const LOOKS: &[(&str, &str)] =
    &[("none", "None"), ("vivid", "Vivid"), ("warm", "Warm"), ("cool", "Cool"), ("moody", "Moody"), ("bw", "Black and white")];

fn f3(v: f64) -> String {
    format!("{v:.3}")
}

pub fn look_filter(name: &str, intensity: f64) -> String {
    let k = intensity.clamp(0.0, 1.0);
    if name == "none" || k == 0.0 {
        return String::new();
    }
    match name {
        "vivid" => format!("eq=saturation={}:contrast={}", f3(1.0 + 0.45 * k), f3(1.0 + 0.12 * k)),
        "warm" => format!(
            "colorbalance=rs={}:gs={}:bs={}:rm={}:bm={},eq=saturation={}",
            f3(0.12 * k),
            f3(0.02 * k),
            f3(-0.12 * k),
            f3(0.08 * k),
            f3(-0.08 * k),
            f3(1.0 + 0.1 * k)
        ),
        "cool" => format!(
            "colorbalance=rs={}:bs={}:rm={}:bm={},eq=saturation={}",
            f3(-0.10 * k),
            f3(0.12 * k),
            f3(-0.05 * k),
            f3(0.08 * k),
            f3(1.0 + 0.05 * k)
        ),
        "moody" => format!(
            "eq=contrast={}:saturation={}:brightness={},colorbalance=bs={}:bm={}",
            f3(1.0 + 0.18 * k),
            f3(1.0 - 0.30 * k),
            f3(-0.04 * k),
            f3(0.06 * k),
            f3(0.04 * k)
        ),
        "bw" => format!("hue=s={},eq=contrast={}", f3(1.0 - k), f3(1.0 + 0.1 * k)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn looks() {
        for (k, _) in LOOKS {
            let _ = look_filter(k, 0.5);
        }
        assert_eq!(look_filter("none", 1.0), "");
        assert_eq!(look_filter("vivid", 0.7), "eq=saturation=1.315:contrast=1.084");
    }
}
