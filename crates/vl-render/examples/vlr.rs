//! Developer tool for checking render output (graphs, subtitles, exports) by hand.
//!
//!   vlr render <project_dir> <out.mp4> [--proxy] [--options '{json}']
//!   vlr analyze <song> [<cache_dir>]          (prints the cache path and the analysis summary)
//!   vlr cachepath <song>
//!   vlr preview-graph <project_dir>
//!   vlr export <project_dir> <out_dir>
//!   vlr overlays <out_dir>
use std::path::{Path, PathBuf};
use vl_core::jobs::Cancel;
use vl_core::projects;
use vl_render::*;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let cmd = a.get(1).map(|s| s.as_str()).unwrap_or("");
    match cmd {
        "render" => {
            let h = projects::open_dir(Path::new(&a[2]))?;
            let out = PathBuf::from(&a[3]);
            let proxy = a.iter().any(|x| x == "--proxy");
            let opts = a
                .iter()
                .position(|x| x == "--options")
                .map(|i| serde_json::from_str(&a[i + 1]))
                .transpose()?
                .unwrap_or(serde_json::json!({}));
            let t0 = std::time::Instant::now();
            let last = std::sync::Mutex::new(-1i64);
            let prog = |p: f64, m: &str| {
                let k = (p * 20.0) as i64;
                let mut l = last.lock().unwrap();
                if k != *l {
                    *l = k;
                    eprintln!("{:5.1}% {m}", p * 100.0);
                }
            };
            let cancel = Cancel::default();
            if let Some(i) = a.iter().position(|x| x == "--cancel-after") {
                let secs: f64 = a[i + 1].parse()?;
                let c = cancel.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                    c.set();
                });
            }
            let r = render::render_project(&h, &out, &prog, proxy, &opts, &cancel)?;
            eprintln!("done {} in {:.1}s", r.display(), t0.elapsed().as_secs_f64());
        }
        "analyze" => {
            let song = &a[2];
            let info = match a.get(3) {
                Some(d) => music::analyze_cached_in(song, Path::new(d))?,
                None => music::analyze(song)?,
            };
            println!("cache {}", music::cache_path(song)?.display());
            println!(
                "duration {} bpm {} beats {} bars {} energy {} wave {}",
                info.duration,
                info.bpm,
                info.beats.len(),
                info.bars.len(),
                info.energy.len(),
                info.wave.len()
            );
            let el_len = 20.0;
            println!("best_part(20, 3.5) = {}", music::best_part(&info, el_len, 3.5));
        }
        "preview-wav" => {
            let h = projects::open_dir(Path::new(&a[2]))?;
            let ed = h.edits();
            let r = music::preview_wav(&ed.music, &ed.parts, &ed.effects, Path::new(&a[3]))?;
            println!("{:?}", r);
        }
        "advances" => {
            use ab_glyph::{Font, ScaleFont};
            let f = overlays::font(&a[2], a[3].parse()?)?;
            let sf = f.font.as_scaled(f.scale());
            let v: Vec<String> = a[4].chars().map(|c| format!("{c}:{:.3}", sf.h_advance(sf.glyph_id(c)))).collect();
            println!("{} total {} asc {} desc {}", v.join(" "), f.text_length(&a[4]), f.ascent(), f.descent());
        }
        "cachepath" => println!("{}", music::cache_path(&a[2])?.display()),
        "preview-graph" => {
            let h = projects::open_dir(Path::new(&a[2]))?;
            let ed = h.edits();
            let ga = h.project.game_area.unwrap();
            let l = layoutspec::effective_layout(&ed, &ga);
            let (g, w, hh) = render::preview_graph(&ga, &l, &ed.effects.look, ed.effects.look_intensity, 720);
            println!("{w} {hh}\n{g}");
        }
        "export" => {
            let h = projects::open_dir(Path::new(&a[2]))?;
            let d = PathBuf::from(&a[3]);
            std::fs::create_dir_all(&d)?;
            exporters::export_resolve_edl(&h, &d.join("t.edl"))?;
            exporters::export_premiere_xml(&h, &d.join("t.xml"))?;
            exporters::export_json(&h, &d.join("t.json"))?;
        }
        "overlays" => {
            let d = PathBuf::from(&a[2]);
            std::fs::create_dir_all(&d)?;
            let p = a.get(3).map(PathBuf::from);
            overlays::feed_row(&d.join("row.png"), "thePanda X", "Hiichamallow", p.as_deref(), p.as_deref(), true, "teal", 48)?;
            overlays::kill_card(&d.join("card.png"), "thePanda X", "Hiichamallow", p.as_deref(), true, "#F4A6C4", 64, "2K")?;
            overlays::vertical_header(&d.join("header.png"), 1080, 220, "Round 17", "#F4A6C4")?;
        }
        _ => eprintln!("usage: see the source"),
    }
    Ok(())
}
