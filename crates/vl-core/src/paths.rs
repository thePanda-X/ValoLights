//! Portable, exe-relative paths.
//!
//! Shipped build: app root = the folder containing ValoLights.exe (it has `assets/` and `bin/` next to it).
//! Dev build (`target/...`): the first ancestor that has both `assets/` and `bin/` = the repository root,
//! so development builds use the repository's `data/` folder.
//! Everything the app writes goes under `data/` inside the app root.
use once_cell::sync::Lazy;
use std::path::{Path, PathBuf};

static ROOT: Lazy<PathBuf> = Lazy::new(|| {
    if let Ok(r) = std::env::var("VALOLIGHTS_ROOT") {
        return PathBuf::from(r);
    }
    let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()).unwrap_or_default();
    let mut dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    for anc in exe.ancestors().skip(1) {
        if anc.join("assets").is_dir() && anc.join("bin").is_dir() {
            dir = anc.to_path_buf();
            break;
        }
    }
    strip_unc(dir)
});

fn strip_unc(p: PathBuf) -> PathBuf {
    // canonicalize() on Windows gives \\?\C:\..., which ffmpeg and mpv don't accept
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

pub fn app_root() -> &'static Path {
    &ROOT
}
pub fn assets_dir() -> PathBuf {
    app_root().join("assets")
}
pub fn bin_dir() -> PathBuf {
    app_root().join("bin")
}
pub fn data_dir() -> PathBuf {
    app_root().join("data")
}
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}
pub fn projects_dir() -> PathBuf {
    data_dir().join("projects")
}
pub fn exports_dir() -> PathBuf {
    data_dir().join("exports")
}
pub fn music_dir() -> PathBuf {
    data_dir().join("music")
}
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}
pub fn cache_dir() -> PathBuf {
    data_dir().join("cache")
}
pub fn tmp_dir() -> PathBuf {
    cache_dir().join("tmp")
}
pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}
/// OCR models (PP-OCR onnx), shipped next to the exe under assets/ocr.
pub fn ocr_dir() -> PathBuf {
    assets_dir().join("ocr")
}

pub fn ensure_data_dirs() {
    for d in [data_dir(), models_dir(), projects_dir(), exports_dir(), music_dir(), logs_dir(), tmp_dir()] {
        let _ = std::fs::create_dir_all(d);
    }
}

pub fn is_writable() -> bool {
    let _ = std::fs::create_dir_all(data_dir());
    let probe = data_dir().join(".write_test");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Strip the `\\?\` prefix canonicalize() adds on Windows.
pub fn clean(p: PathBuf) -> PathBuf {
    strip_unc(p)
}

/// Remove temporary files and folders (job specs, render work folders) older than `max_age` from
/// data/cache/tmp: what interrupted renders and closed apps leave behind.
pub fn clean_tmp(max_age: std::time::Duration) {
    let Ok(rd) = std::fs::read_dir(tmp_dir()) else { return };
    let now = std::time::SystemTime::now();
    for e in rd.flatten() {
        let old =
            e.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).map(|age| age > max_age).unwrap_or(false);
        if old {
            let p = e.path();
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
}
