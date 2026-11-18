//! Dev tool: `kf_dump <roi.png> <layout_w> <layout_h> [soft]` prints rows, parts and headshot scores.
use vl_engine::detectors::killfeed as kf;
use vl_engine::imgproc;
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let img = vl_core::image::BgrImage::load(std::path::Path::new(&a[0]))?;
    let mut lay = vl_engine::layout::Layout::new(a[1].parse()?, a[2].parse()?);
    lay.soft = a.get(3).is_some();
    for r in kf::segment_rows(&img, &lay) {
        let p = kf::parse_row(&img, &r, &lay);
        let hs = p.mods.map(|m| kf::headshot_match(Some(&imgproc::slice(&img, r.y0, r.y1, m.0 - 6, m.1 + 6)))).unwrap_or(-1.0);
        println!(
            "{} {} {} {} {} {} {} {} | k{:?} w{:?} m{:?} v{:?} kp{:?} vp{:?} hs={:.4}",
            r.y0,
            r.y1,
            r.x0,
            r.x1,
            r.split,
            r.killer_color,
            r.victim_color,
            r.gold,
            p.killer_name,
            p.weapon,
            p.mods,
            p.victim_name,
            p.killer_portrait,
            p.victim_portrait,
            hs
        );
    }
    Ok(())
}
