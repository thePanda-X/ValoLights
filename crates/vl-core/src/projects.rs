//! Project folders under data/projects/<id>/.
use crate::ffmpeg as ff;
use crate::models::{load_json, save_json, Edits, Events, Project, SourceInfo};
use crate::paths;
use crate::transcript::Transcript;
use std::path::{Path, PathBuf};

fn slug(s: &str) -> String {
    let re = regex::Regex::new(r"[^A-Za-z0-9_-]+").unwrap();
    let out: String = re.replace_all(s, "-").trim_matches('-').chars().take(40).collect();
    if out.is_empty() {
        "clip".into()
    } else {
        out
    }
}

#[derive(Debug, Clone)]
pub struct ProjectHandle {
    pub dir: PathBuf,
    pub project: Project,
}

impl ProjectHandle {
    pub fn events_path(&self) -> PathBuf {
        self.dir.join("events.json")
    }
    pub fn edits_path(&self) -> PathBuf {
        self.dir.join("edits.json")
    }
    pub fn transcript_path(&self) -> PathBuf {
        self.dir.join("transcript.json")
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.dir.join("cache")
    }
    pub fn save(&self) -> anyhow::Result<()> {
        save_json(&self.project, &self.dir.join("project.json"))
    }
    pub fn events(&self) -> Option<Events> {
        let p = self.events_path();
        if p.exists() {
            match load_json(&p) {
                Ok(e) => Some(e),
                Err(e) => {
                    log::warn!("events unreadable {}: {e}", p.display());
                    None
                }
            }
        } else {
            None
        }
    }
    pub fn save_events(&self, ev: &Events) -> anyhow::Result<()> {
        save_json(ev, &self.events_path())
    }
    pub fn edits(&self) -> Edits {
        let mut ed: Edits = load_json(&self.edits_path()).unwrap_or_default();
        ed.effects.migrate();
        ed
    }
    pub fn save_edits(&self, ed: &Edits) -> anyhow::Result<()> {
        save_json(ed, &self.edits_path())
    }
    pub fn transcript(&self) -> Option<Transcript> {
        load_json(&self.transcript_path()).ok()
    }
    pub fn save_transcript(&self, tr: &Transcript) -> anyhow::Result<()> {
        save_json(tr, &self.transcript_path())
    }
    pub fn source(&self) -> &str {
        &self.project.source.path
    }
    pub fn source_ok(&self) -> bool {
        std::fs::metadata(&self.project.source.path).map(|m| m.len() == self.project.source.size).unwrap_or(false)
    }
    pub fn thumb_path(&self) -> PathBuf {
        self.dir.join("cache").join("thumb.jpg")
    }
    pub fn id(&self) -> &str {
        &self.project.id
    }
}

pub fn create(src: &Path) -> anyhow::Result<ProjectHandle> {
    let src = paths::clean(src.canonicalize()?);
    let info = ff::probe(&src)?;
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("clip").to_string();
    let pid =
        format!("{}_{}_{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), slug(&stem), &uuid::Uuid::new_v4().simple().to_string()[..4]);
    let d = paths::projects_dir().join(&pid);
    std::fs::create_dir_all(d.join("cache"))?;
    let proj = Project {
        schema_version: crate::models::SCHEMA,
        id: pid,
        name: stem,
        created: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        source: SourceInfo {
            path: src.to_string_lossy().into_owned(),
            size: info.size,
            duration: info.duration,
            width: info.width,
            height: info.height,
            fps: info.fps,
            vfr: info.vfr,
            has_audio: info.has_audio,
        },
        game_area: None,
        identity: Default::default(),
        status: "new".into(),
        error: String::new(),
        analysis_version: String::new(),
    };
    let h = ProjectHandle { dir: d, project: proj };
    h.save()?;
    let t = (info.duration * 0.3).min(60.0);
    let _ = ff::run(&[
        ff::ffmpeg_exe()?.as_str(),
        "-y",
        "-v",
        "error",
        "-ss",
        &format!("{t:.2}"),
        "-i",
        &h.project.source.path,
        "-frames:v",
        "1",
        "-vf",
        "scale=480:-2",
        &h.thumb_path().to_string_lossy(),
    ]);
    Ok(h)
}

pub fn open_dir(d: &Path) -> anyhow::Result<ProjectHandle> {
    Ok(ProjectHandle { dir: d.to_path_buf(), project: load_json(&d.join("project.json"))? })
}

pub fn list_all() -> Vec<ProjectHandle> {
    let root = paths::projects_dir();
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("project.json").exists()).collect(),
        Err(_) => return vec![],
    };
    dirs.sort();
    dirs.reverse();
    dirs.iter().filter_map(|d| open_dir(d).ok()).collect()
}

pub fn delete(h: &ProjectHandle) {
    let _ = std::fs::remove_dir_all(&h.dir);
}

/// Point a project at a moved source file (must be the same video: duration within 0.5 s).
pub fn relocate(h: &mut ProjectHandle, new_path: &Path) -> anyhow::Result<bool> {
    let info = ff::probe(new_path)?;
    if (info.duration - h.project.source.duration).abs() > 0.5 {
        return Ok(false);
    }
    h.project.source.path = paths::clean(new_path.canonicalize()?).to_string_lossy().into_owned();
    h.project.source.size = info.size;
    h.save()?;
    Ok(true)
}
