//! Dev tool: `resize_check <dir>` compares imgproc::resize with cv2 outputs written by a generator script
//! (`man.json` + `<n>_in.png` / `<n>_out.png`).
use vl_engine::imgproc::{resize, Interp};
fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let man: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("man.json"))?)?;
    let mut stats: std::collections::BTreeMap<String, (usize, usize, usize)> = Default::default();
    for c in man.as_array().unwrap() {
        let n = c["n"].as_u64().unwrap();
        let interp = c["interp"].as_str().unwrap();
        let ip = match interp {
            "linear" => Interp::Linear,
            "area" => Interp::Area,
            "cubic" => Interp::Cubic,
            _ => Interp::Nearest,
        };
        let img = vl_core::image::BgrImage::load(&dir.join(format!("{n}_in.png")))?;
        let want = vl_core::image::BgrImage::load(&dir.join(format!("{n}_out.png")))?;
        let got = resize(&img, c["dw"].as_u64().unwrap() as usize, c["dh"].as_u64().unwrap() as usize, ip);
        let bad = got.data.iter().zip(&want.data).filter(|(a, b)| a != b).count();
        let e = stats.entry(interp.to_string()).or_default();
        e.0 += 1;
        e.1 += (bad > 0) as usize;
        e.2 += bad;
    }
    for (k, (n, cases, px)) in stats {
        println!("{k:8} cases {n:4}  inexact cases {cases:4}  wrong px {px}");
    }
    Ok(())
}
