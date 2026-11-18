//! Command-line interface.
//!
//!     valolights cli analyze <video> [--alias NAME ...]
//!     valolights cli summary <project_dir>
//!     valolights cli render <project_dir> [--cut kills|segments|best|rounds|plays|smart|whole] [--format 16:9|9:16] [--proxy] [-o out.mp4]
//!     valolights cli eval [--labels tests/eval/labels]
//!     valolights cli update [--install]
//!     valolights cli release-keygen
//!     valolights cli release-manifest --installer <setup.exe> --version <x.y.z> --url <download url>
//!                                     [--notes <file.md>] [--platform windows-x86_64] [-o latest.json]
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vl_core::projects::{self, ProjectHandle};

use crate::backend;

const USAGE: &str = "usage: valolights cli {analyze,summary,render,eval,update,release-keygen,release-manifest} ...

  analyze <video> [--alias NAME ...]
  summary <project_dir>
  render <project_dir> [--cut kills|segments|best|rounds|plays|smart|whole] [--format 16:9|9:16|1:1|4:5|source] [--proxy] [-o OUT]
  eval [--labels tests/eval/labels]
  update [--install]                  look for a newer version (and install it)
  release-keygen                      new update-signing key pair (release maintainers)
  release-manifest --installer SETUP --version X.Y.Z --url URL [--notes FILE] [--platform P] [-o OUT]
                                      signed latest.json for a release (key in VALOLIGHTS_UPDATE_KEY)";

fn cmd_update(install: bool) -> Result<i32> {
    println!("ValoLights {} ({})", crate::update::current_version(), crate::update::platform_key());
    let Some(av) = crate::update::check()? else {
        println!("up to date");
        return Ok(0);
    };
    println!("{} is available: {}", av.version, av.release.url);
    if install {
        let dir = vl_core::paths::tmp_dir().join("update");
        let path = crate::update::download(&av, &dir, &|_| {})?;
        println!("downloaded and verified: {}", path.display());
        crate::update::start_install(&path)?;
        println!("installer started");
    }
    Ok(0)
}

fn fmt_t(t: f64) -> String {
    format!("{}:{:05.2}", (t / 60.0).floor() as i64, t.rem_euclid(60.0))
}

/// A float with at least one decimal (1.0 stays "1.0").
fn float_repr(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn pair_repr(v: &Option<(i32, i32)>) -> String {
    match v {
        Some((a, b)) => format!("({a}, {b})"),
        None => "None".into(),
    }
}

fn str_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "\\'"))
    }
}

pub fn main(args: &[String]) -> i32 {
    vl_core::logging::setup("cli", true);
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            if e.to_string().starts_with("usage") || e.to_string().contains("argument") {
                eprintln!("{USAGE}");
            }
            2
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let Some(cmd) = args.first() else {
        bail!("the following arguments are required: cmd");
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "analyze" => {
            let mut video = None;
            let mut alias = vec![];
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--alias" => {
                        alias.push(rest.get(i + 1).ok_or_else(|| anyhow!("argument --alias: expected one argument"))?.clone());
                        i += 1;
                    }
                    s if video.is_none() => video = Some(s.to_string()),
                    s => bail!("unrecognized arguments: {s}"),
                }
                i += 1;
            }
            cmd_analyze(&video.ok_or_else(|| anyhow!("the following arguments are required: video"))?, &alias)
        }
        "summary" => {
            let p = rest.first().ok_or_else(|| anyhow!("the following arguments are required: project"))?;
            summary(&projects::open_dir(Path::new(p))?)
        }
        "render" => {
            let mut project = None;
            let mut cut = "segments".to_string();
            let mut format = None;
            let mut proxy = false;
            let mut output = None;
            let mut i = 0;
            while i < rest.len() {
                let val = |i: usize| rest.get(i + 1).cloned().ok_or_else(|| anyhow!("argument {}: expected one argument", rest[i]));
                match rest[i].as_str() {
                    "--cut" => {
                        cut = val(i)?;
                        i += 1
                    }
                    "--format" => {
                        format = Some(val(i)?);
                        i += 1
                    }
                    "-o" | "--output" => {
                        output = Some(val(i)?);
                        i += 1
                    }
                    "--proxy" => proxy = true,
                    s if project.is_none() => project = Some(s.to_string()),
                    s => bail!("unrecognized arguments: {s}"),
                }
                i += 1;
            }
            let project = project.ok_or_else(|| anyhow!("the following arguments are required: project"))?;
            cmd_render(&project, &cut, format.as_deref(), proxy, output.as_deref())
        }
        "eval" => {
            let mut labels = "tests/eval/labels".to_string();
            if rest.first().map(String::as_str) == Some("--labels") {
                labels = rest.get(1).cloned().ok_or_else(|| anyhow!("argument --labels: expected one argument"))?;
            }
            backend::run_eval(Path::new(&labels))
        }
        "update" => cmd_update(rest.iter().any(|a| a == "--install")),
        "release-keygen" => {
            let (private, public) = crate::update::keygen()?;
            println!("public key (put it in crates/vl-app/src/update.rs PUBLIC_KEY):\n{public}\n");
            println!("private key (GitHub secret UPDATE_SIGNING_KEY; never commit it):\n{private}");
            Ok(0)
        }
        "release-manifest" => {
            let get = |k: &str| rest.iter().position(|a| a == k).and_then(|i| rest.get(i + 1)).cloned();
            let installer = get("--installer").ok_or_else(|| anyhow!("--installer is required"))?;
            let version = get("--version").ok_or_else(|| anyhow!("--version is required"))?;
            let url = get("--url").ok_or_else(|| anyhow!("--url is required"))?;
            let notes = match get("--notes") {
                Some(p) => std::fs::read_to_string(p)?,
                None => String::new(),
            };
            let platform = get("--platform").unwrap_or_else(crate::update::platform_key);
            let out = get("-o").or_else(|| get("--output")).unwrap_or_else(|| "latest.json".into());
            let key =
                std::env::var("VALOLIGHTS_UPDATE_KEY").map_err(|_| anyhow!("set VALOLIGHTS_UPDATE_KEY to the private signing key"))?;
            let published = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let m = crate::update::make_manifest(Path::new(&installer), &version, &platform, &url, notes.trim(), &published, &key)?;
            std::fs::write(&out, serde_json::to_string_pretty(&m)? + "\n")?;
            println!("{out}: {} {} {}", m.version, platform, m.platforms[&platform].sha256);
            Ok(0)
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(0)
        }
        c => bail!("argument cmd: invalid choice: '{c}'"),
    }
}

fn printer() -> impl Fn(f64, &str) + Sync {
    let last = Mutex::new(0.0f64);
    move |p: f64, m: &str| {
        let mut l = last.lock().unwrap();
        if p - *l >= 0.05 || p >= 1.0 {
            *l = p;
            println!("[{:5.1}%] {m}", p * 100.0);
        }
    }
}

fn cmd_analyze(video: &str, alias: &[String]) -> Result<i32> {
    let mut h = projects::create(Path::new(video))?;
    let prog = printer();
    let src = PathBuf::from(h.source());
    let (mut ev, ga) = backend::analyze(&src, &h.dir, &prog)?;
    if !alias.is_empty() {
        // --alias names can't be added to the in-memory config before analysing (the config cache is
        // read-only from here, so the names are applied to the finished events instead.
        let mut aliases = vl_core::config::load().identity.aliases;
        for a in alias {
            if !aliases.contains(a) {
                aliases.push(a.clone());
            }
        }
        let wanted: Vec<String> = aliases.iter().map(|a| backend::normalize_name(a)).collect();
        for p in ev.players.iter_mut() {
            if p.me_hint.is_empty() && p.names.iter().chain([&p.display]).any(|n| wanted.contains(&backend::normalize_name(n))) {
                p.me_hint = "alias".into();
            }
        }
        backend::finalize_identity(&mut ev, &aliases);
    }
    h.save_events(&ev)?;
    h.project.game_area = Some(ga);
    h.project.identity.me = ev.players.iter().filter(|p| !p.me_hint.is_empty()).map(|p| p.id.clone()).collect();
    h.project.identity.confirmed = !h.project.identity.me.is_empty();
    h.project.status = "ready".into();
    h.project.analysis_version = ev.engine_version.clone();
    h.save()?;
    println!("{}", h.dir.display());
    summary(&h)
}

fn summary(h: &ProjectHandle) -> Result<i32> {
    let ev = h.events().ok_or_else(|| anyhow!("{} has no events.json (not analysed)", h.dir.display()))?;
    let me = &h.project.identity.me;
    let name = |id: &str| ev.player(id).map(|p| p.display.clone()).unwrap_or_else(|| "?".into());
    println!("\nPlayers ({}):", ev.players.len());
    let mut players = ev.players.clone();
    players.sort_by_key(|p| std::cmp::Reverse(p.seen));
    for p in &players {
        let team = if p.team.is_empty() { "-" } else { &p.team };
        let mark = if me.contains(&p.id) { format!("<- ME ({})", p.me_hint) } else { String::new() };
        println!("  {:4} {:24} seen={:3} team={} {}", p.id, str_repr(&p.display), p.seen, team, mark);
    }
    println!("\nRounds ({}):", ev.rounds.len());
    for (i, r) in ev.rounds.iter().enumerate() {
        println!(
            "  #{:2} {}-{} score {}->{} {}",
            i,
            fmt_t(r.start),
            fmt_t(r.end),
            pair_repr(&r.score_before),
            pair_repr(&r.score_after),
            r.result
        );
    }
    println!("\nKills ({}):", ev.kills.len());
    for k in &ev.kills {
        let mark = if me.contains(&k.killer) {
            "*"
        } else if me.contains(&k.victim) {
            "x"
        } else {
            " "
        };
        println!(
            " {} {} r{:<2} {:>18} -> {:<18} {} {} conf={}",
            mark,
            fmt_t(k.t),
            k.round,
            name(&k.killer),
            name(&k.victim),
            if k.headshot { "HS" } else { "  " },
            k.killer_side,
            float_repr(k.confidence)
        );
    }
    let cfg = vl_core::config::load().highlights;
    let mut segs = backend::build_segments(&ev, me, &cfg, h.project.source.duration, &[]);
    println!("\nHighlights ({}):", segs.len());
    segs.sort_by(|a, b| b.score.total_cmp(&a.score));
    for s in segs {
        println!("  {:5.2} {}-{} r{} {}", s.score, fmt_t(s.start), fmt_t(s.end), s.round, s.label);
    }
    Ok(0)
}

fn cmd_render(project: &str, cut: &str, format: Option<&str>, proxy: bool, output: Option<&str>) -> Result<i32> {
    let h = projects::open_dir(Path::new(project))?;
    let mut ed = h.edits();
    let ev = h.events().ok_or_else(|| anyhow!("the clip has not been analysed"))?;
    let cfg = vl_core::config::load().highlights;
    ed.parts = backend::quick_cut(cut, &ev, &h.project.identity.me, &cfg, h.project.source.duration, &[])
        .ok_or_else(|| anyhow!("quick cut '{cut}' is not available in this build"))?;
    ed.quick_cut = cut.to_string();
    if let Some(f) = format {
        let cur = ed.layout.as_ref().map(|l| l.aspect.clone()).unwrap_or_else(|| ed.format.clone());
        if f != cur {
            ed.format = f.to_string();
            ed.layout = match (f, &h.project.game_area) {
                ("source", _) | (_, None) => None,
                (_, Some(ga)) => Some(backend::default_layout(f, ga)),
            };
        }
    }
    h.save_edits(&ed)?;
    let out = output.map(PathBuf::from).unwrap_or_else(|| h.dir.join(format!("render_{cut}.mp4")));
    let last = Mutex::new(0.0f64);
    let prog = move |p: f64, m: &str| {
        let mut l = last.lock().unwrap();
        if p - *l >= 0.1 || p >= 1.0 {
            *l = p;
            println!("[{:5.1}%] {m}", p * 100.0);
        }
    };
    backend::render_project(&h, &out, &prog, proxy, &serde_json::json!({}))?;
    println!("{}", out.display());
    Ok(0)
}
