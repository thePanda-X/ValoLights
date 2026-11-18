//! Dev tool (read-only): what Smart cut would keep for a project. Runs the sound pass, prints the moments.
//! `cargo run --release -p vl-render --example smart_preview -- <project_dir>`
fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("project dir"));
    let h = vl_core::projects::open_dir(&dir)?;
    let mut ev = h.events().ok_or_else(|| anyhow::anyhow!("no events"))?;
    let dur = h.project.source.duration;
    let t = std::time::Instant::now();
    let cancel = vl_core::jobs::Cancel(Default::default());
    let mut a = vl_engine::audio::analyze_audio(std::path::Path::new(h.source()), dur, &|_| {}, &cancel)?;
    let words: Vec<(f64, String)> = h
        .transcript()
        .map(|tr| tr.segments.iter().flat_map(|s| s.words.iter().map(|w| (w.start, w.text.clone()))).collect())
        .unwrap_or_default();
    a.words = vl_render::smart::transcript_words(&words);
    println!("sound pass {:.1} s · {} transcript words, {} matched", t.elapsed().as_secs_f64(), words.len(), a.words.len());
    let rx = vl_render::smart::reactions(&a);
    println!(
        "{} reactions: {}",
        rx.len(),
        rx.iter().take(40).map(|r| format!("{:.0}s {} {:.2}", r.t0, r.what, r.peak)).collect::<Vec<_>>().join(" | ")
    );
    ev.audio = Some(a);
    let cfg = vl_core::config::load().highlights;
    let me = h.project.identity.me.clone();
    for m in vl_render::smart::chosen(&ev, &me, &cfg, dur, &[]) {
        println!("  keep {:7.1}-{:7.1}  score {:5.2}  {}", m.start, m.end, m.score, m.reasons.join(", "));
    }
    let best = vl_render::highlights::quick_cut("best", &ev, &me, &cfg, dur, &[])?;
    println!("Best moments for comparison: {:?}", best.iter().map(|p| format!("{:.0}-{:.0}", p.src_in, p.src_out)).collect::<Vec<_>>());
    Ok(())
}
