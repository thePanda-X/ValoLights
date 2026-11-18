//! User configuration in data/config.toml.
use crate::paths;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct IdentityCfg {
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AnalysisCfg {
    pub sample_fps: f64,
    pub hud_fps: f64,
    pub refine: bool,
}
impl Default for AnalysisCfg {
    fn default() -> Self {
        AnalysisCfg { sample_fps: 6.0, hud_fps: 1.0, refine: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HighlightsCfg {
    pub pre_roll: f64,
    pub post_roll: f64,
    pub merge_gap: f64,
    pub kill_pre: f64,
    pub kill_post: f64,
    pub include_team_kills: bool,
    pub w_headshot: f64,
    pub ace_bonus: f64,
    pub clutch_bonus: f64,
    pub round_win_bonus: f64,
    pub best_of_target: f64,
}
impl Default for HighlightsCfg {
    fn default() -> Self {
        HighlightsCfg {
            pre_roll: 3.5,
            post_roll: 1.5,
            merge_gap: 8.0,
            kill_pre: 1.6,
            kill_post: 0.9,
            include_team_kills: false,
            w_headshot: 0.3,
            ace_bonus: 2.0,
            clutch_bonus: 1.5,
            round_win_bonus: 0.5,
            best_of_target: 60.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RenderCfg {
    /// auto | nvenc | x264
    pub encoder: String,
    /// high | medium | small
    pub quality: String,
    pub resolution: i32,
    pub fps: i32,
}
impl Default for RenderCfg {
    fn default() -> Self {
        RenderCfg { encoder: "auto".into(), quality: "high".into(), resolution: 1080, fps: 60 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SubtitlesCfg {
    pub model: String,
    /// auto | cuda | cpu
    pub device: String,
}
impl Default for SubtitlesCfg {
    fn default() -> Self {
        SubtitlesCfg { model: "large-v3-turbo".into(), device: "auto".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    pub identity: IdentityCfg,
    pub analysis: AnalysisCfg,
    pub highlights: HighlightsCfg,
    pub render: RenderCfg,
    pub subtitles: SubtitlesCfg,
    pub updates: UpdatesCfg,
    pub first_run_done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdatesCfg {
    /// look for a newer version when the app starts
    pub check_on_launch: bool,
    /// a version the user chose to skip (not offered on launch)
    pub skipped: String,
}
impl Default for UpdatesCfg {
    fn default() -> Self {
        UpdatesCfg { check_on_launch: true, skipped: String::new() }
    }
}

static CFG: Lazy<RwLock<Option<Config>>> = Lazy::new(|| RwLock::new(None));

/// The current configuration (cached; `reload()` re-reads the file).
pub fn load() -> Config {
    if let Some(c) = CFG.read().unwrap().as_ref() {
        return c.clone();
    }
    let p = paths::config_path();
    let cfg = match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            log::warn!("config unreadable, using defaults: {e}");
            Config::default()
        }),
        Err(_) => Config::default(),
    };
    *CFG.write().unwrap() = Some(cfg.clone());
    cfg
}

pub fn reload() -> Config {
    *CFG.write().unwrap() = None;
    load()
}

impl Config {
    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(paths::data_dir())?;
        std::fs::write(paths::config_path(), toml::to_string(self)?)?;
        *CFG.write().unwrap() = Some(self.clone());
        Ok(())
    }
}
