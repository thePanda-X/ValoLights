//! Dev tool: time super smooth presets on a part of a video.
//! `cargo run --release -p vl-render --example smooth_bench -- <src> <start> <dur> <out_dir> [preset...|decode]`
//! (presets: faster, fast, subtle, custom = the default custom settings; `decode` times decoding alone)
use std::time::Instant;
use vl_render::smooth;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (src, start, dur, dir) = (&a[0], a[1].parse::<f64>()?, a[2].parse::<f64>()?, std::path::PathBuf::from(&a[3]));
    std::fs::create_dir_all(&dir)?;
    let presets: Vec<String> =
        if a.len() > 4 { a[4..].to_vec() } else { vec!["faster".into(), "custom".into(), "fast".into(), "subtle".into()] };
    let info = vl_core::ffmpeg::probe(std::path::Path::new(src))?;
    let crop = (0, 0, info.width as i32, info.height as i32);
    let cancel = vl_core::jobs::Cancel(Default::default());
    if presets[0] == "decode" {
        let t = Instant::now();
        let opts = vl_core::ffmpeg::ReadOpts {
            fps: info.fps,
            crop: Some(crop),
            start,
            duration: Some(dur),
            hwaccel: true,
            matrix: Some("bt709".into()),
            ..Default::default()
        };
        let n = vl_core::ffmpeg::read_frames(std::path::Path::new(src), opts, None)?.count();
        println!("decode only: {n} frames in {:.2} s", t.elapsed().as_secs_f64());
        return Ok(());
    }
    for p in presets {
        let params = smooth::params(&p, &Default::default());
        let out = dir.join(format!("{p}.mp4"));
        let t = Instant::now();
        smooth::smooth_part(src, start, dur, crop, info.fps, 60.0, &params, &out, &|_| {}, &cancel)?;
        let s = t.elapsed().as_secs_f64();
        println!("{p:8} {:6.1} s  ({:.2} s per second of video)  {params:?}", s, s / dur);
    }
    Ok(())
}
