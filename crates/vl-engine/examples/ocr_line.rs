//! Dev tool: `ocr_line <image> [x y w h]...` prints read_line for each crop (whole image if none given).
use vl_core::image::BgrImage;
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let img = BgrImage::load(std::path::Path::new(&a[0]))?;
    let nums: Vec<i64> = a[1..].iter().map(|s| s.parse().unwrap()).collect();
    if nums.is_empty() {
        println!("{:?}", vl_engine::ocr::read_line(&img));
        println!("block {:?}", vl_engine::ocr::read_block(&img, 0.5));
    }
    for c in nums.chunks(4) {
        let t = std::time::Instant::now();
        let r = vl_engine::ocr::read_line(&img.crop(c[0], c[1], c[2], c[3]));
        println!("{:?} {:?} {:.1}ms", c, r, t.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(())
}
