use vl_core::{models::*, projects, transcript::Transcript};
fn main() {
    let all = projects::list_all();
    println!("projects: {}", all.len());
    for h in &all {
        let ev = h.events();
        let ed: anyhow::Result<Edits> = load_json(&h.edits_path());
        let tr: Option<anyhow::Result<Transcript>> = h.transcript_path().exists().then(|| load_json(&h.transcript_path()));
        println!(
            "{:60} events {:?} edits {} transcript {}",
            h.project.id,
            ev.as_ref().map(|e| (e.kills.len(), e.players.len())),
            match &ed {
                Ok(e) => format!("ok parts={}", e.parts.len()),
                Err(e) => format!("ERR {e}"),
            },
            match &tr {
                None => "-".into(),
                Some(Ok(t)) => format!("ok {}", t.segments.len()),
                Some(Err(e)) => format!("ERR {e}"),
            }
        );
    }
    println!("root {}", vl_core::paths::app_root().display());
}
