//! Packaging smoke test: `valolights --selftest` (writes data/logs/selftest.txt).
use std::time::Instant;
use vl_core::ffmpeg as ff;
use vl_core::image::BgrImage;
use vl_core::paths;

pub fn run() -> i32 {
    let mut lines: Vec<String> = vec![];
    let mut ok_all = true;
    let mut check = |name: &str, f: &dyn Fn() -> anyhow::Result<String>| {
        let t = Instant::now();
        match f() {
            Ok(res) => lines.push(format!("[ OK ] {name}: {res} ({:.1}s)", t.elapsed().as_secs_f64())),
            Err(e) => {
                ok_all = false;
                lines.push(format!("[FAIL] {name}: {e}\n{e:?}"));
            }
        }
    };

    check("data folder writable", &|| {
        paths::ensure_data_dirs();
        if paths::is_writable() {
            Ok(format!("{}", paths::data_dir().display()))
        } else {
            anyhow::bail!("cannot write to {}", paths::data_dir().display())
        }
    });
    check("ffmpeg", &|| {
        let out = ff::run(&[ff::ffmpeg_exe()?.as_str(), "-version"])?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string())
    });
    check("NVENC", &|| Ok(if ff::nvenc_works() { "True".into() } else { "False".into() }));
    check("GPU probe", &|| {
        Ok(if crate::sys::nvidia_driver_present() {
            "NVIDIA driver found (speech-to-text can use CUDA)".into()
        } else {
            "No NVIDIA driver; speech-to-text uses the CPU".into()
        })
    });
    check("OCR", &|| {
        for m in ["PP-OCRv6_rec_small.onnx", "PP-OCRv6_det_small.onnx"] {
            let p = paths::ocr_dir().join(m);
            if !p.exists() {
                anyhow::bail!("{} missing", p.display());
            }
        }
        let img = test_word("thePanda X")?;
        let (text, score) = crate::backend::ocr_read_line(&img)?;
        Ok(format!("('{text}', {score:.3})"))
    });
    check("libmpv", &|| {
        let v = crate::widgets::mpv::version()?;
        let m = crate::widgets::mpv::Mpv::new(&[("vo", "libmpv"), ("config", "no"), ("load-scripts", "no"), ("ytdl", "no")])?;
        drop(m);
        Ok(format!("client API {v}"))
    });
    check("speech engine", &crate::speech::probe);
    check("frame interpolation", &|| {
        let (ok, why) = crate::backend::rife_works();
        Ok(if ok { "ok".into() } else { format!("unavailable ({why}), super smooth disabled") })
    });

    let report = format!("{}\n\nRESULT: {}\n", lines.join("\n"), if ok_all { "PASS" } else { "FAIL" });
    let _ = std::fs::create_dir_all(paths::logs_dir());
    let _ = std::fs::write(paths::logs_dir().join("selftest.txt"), &report);
    println!("{report}");
    if ok_all {
        0
    } else {
        1
    }
}

/// White Inter text on a teal killfeed-like background (360x60).
pub fn test_word(text: &str) -> anyhow::Result<BgrImage> {
    use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
    let data = std::fs::read(paths::assets_dir().join("fonts").join("Inter-Variable.ttf"))?;
    let font = FontVec::try_from_vec(data)?;
    let (w, h) = (360usize, 60usize);
    let bg = [140u8, 160, 40]; // BGR of (40, 160, 140)
    let mut img = BgrImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set(x, y, bg);
        }
    }
    let sf = font.as_scaled(PxScale::from(34.0));
    // anchor "lm": left edge at x=10, the middle of ascender..descender at y=30
    let base = 30.0 + (sf.ascent() + sf.descent()) / 2.0;
    let mut x = 10.0f32;
    let mut prev = None;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            x += sf.kern(p, gid);
        }
        let g = gid.with_scale_and_position(PxScale::from(34.0), ab_glyph::point(x, base));
        if let Some(og) = font.outline_glyph(g) {
            let b = og.px_bounds();
            og.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let o = img.px(px as usize, py as usize);
                    let mix = |a: u8| (a as f32 + (255.0 - a as f32) * cov.min(1.0)).round() as u8;
                    img.set(px as usize, py as usize, [mix(o[0]), mix(o[1]), mix(o[2])]);
                }
            });
        }
        x += sf.h_advance(gid);
        prev = Some(gid);
    }
    Ok(img)
}
