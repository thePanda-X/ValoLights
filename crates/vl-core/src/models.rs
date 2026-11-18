//! Versioned JSON schemas for project files (project.json, events.json, edits.json, transcript.json).
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const SCHEMA: u32 = 1;

fn schema() -> u32 {
    SCHEMA
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceInfo {
    pub path: String,
    pub size: u64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    #[serde(default)]
    pub vfr: bool,
    #[serde(default = "yes")]
    pub has_audio: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GameArea {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(default = "one")]
    pub hud_scale: f64,
}
fn one() -> f64 {
    1.0
}

impl GameArea {
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.w, self.h)
    }
    pub fn aspect_label(&self) -> String {
        let r = self.w as f64 / self.h as f64;
        for (label, v) in [("16:9", 16.0 / 9.0), ("16:10", 1.6), ("4:3", 4.0 / 3.0), ("5:4", 1.25), ("21:9", 21.0 / 9.0)] {
            if (r - v).abs() < 0.04 {
                return label.to_string();
            }
        }
        format!("{r:.2}:1")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Player {
    pub id: String,
    pub names: Vec<String>,
    pub display: String,
    pub crop: Option<String>,
    pub seen: u32,
    /// "me_text" | "gold" | "alias" | "user" | ""
    pub me_hint: String,
    /// "A" | "B" | ""
    pub team: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Kill {
    pub id: String,
    pub t: f64,
    #[serde(default = "one")]
    pub t_conf: f64,
    #[serde(default = "minus_one")]
    pub round: i32,
    pub killer: String,
    pub victim: String,
    /// "teal" | "red": colour relative to the watched player
    #[serde(default = "teal")]
    pub killer_side: String,
    #[serde(default)]
    pub headshot: bool,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default)]
    pub gold: bool,
    #[serde(default = "one")]
    pub confidence: f64,
    #[serde(default)]
    pub victim_portrait: Option<String>,
    #[serde(default)]
    pub killer_portrait: Option<String>,
    #[serde(default)]
    pub row_image: Option<String>,
    #[serde(default)]
    pub manual: bool,
}
fn minus_one() -> i32 {
    -1
}
fn teal() -> String {
    "teal".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Round {
    pub index: i32,
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub score_before: Option<(i32, i32)>,
    #[serde(default)]
    pub score_after: Option<(i32, i32)>,
    /// "win" | "loss" | ""
    #[serde(default)]
    pub result: String,
    /// end-of-round banner badges ("FLAWLESS", "CLUTCH", "ACE", "THRIFTY", ...) and "winner:<colour>"
    #[serde(default)]
    pub banners: Vec<String>,
    /// when the spike was planted (source time), if it was
    #[serde(default)]
    pub planted: Option<f64>,
    /// when the round was decided (source time): the post-round countdown starts here. `end` stays the
    /// round boundary (the score change), which can be a few seconds later.
    #[serde(default)]
    pub decided: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Banner {
    pub t: f64,
    pub text: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Events {
    #[serde(rename = "schema", alias = "schema_version")]
    pub schema_version: u32,
    pub engine_version: String,
    pub players: Vec<Player>,
    pub kills: Vec<Kill>,
    pub rounds: Vec<Round>,
    pub banners: Vec<Banner>,
    pub killbanner_flashes: Vec<f64>,
    pub ammo: Vec<(f64, i32)>,
    pub timer: Vec<(f64, i32)>,
    pub scores: Vec<(f64, i32, i32)>,
    pub spectating: Vec<(f64, String)>,
    /// sound events along the clip, from the Smart cut pass (None until it ran)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioSignals>,
}

/// What the clip sounds like over time (Smart cut): one value per `step` seconds (YAMNet frames of 0.96 s
/// every 0.48 s), each 0..=100. Frame `i` covers `[i * step, i * step + 2 * step]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioSignals {
    pub version: u32,
    pub step: f64,
    /// laughter, giggles, chuckles (strongest of the AudioSet laughter classes)
    pub laugh: Vec<u8>,
    /// shouting, yelling, whooping, screaming
    pub shout: Vec<u8>,
    /// cheering, applause
    pub cheer: Vec<u8>,
    /// loudness, -60..0 dBFS mapped to 0..100
    pub loud: Vec<u8>,
    /// someone talking (a loud moment with speech is a reaction, a loud one without is gunfire)
    pub speech: Vec<u8>,
    /// words from the voice-chat transcript that mark a funny or hype moment: (time, word)
    pub words: Vec<(f64, String)>,
}

impl Default for Events {
    fn default() -> Self {
        Events {
            schema_version: SCHEMA,
            engine_version: String::new(),
            players: vec![],
            kills: vec![],
            rounds: vec![],
            banners: vec![],
            killbanner_flashes: vec![],
            ammo: vec![],
            timer: vec![],
            scores: vec![],
            spectating: vec![],
            audio: None,
        }
    }
}

impl Events {
    pub fn player(&self, pid: &str) -> Option<&Player> {
        self.players.iter().find(|p| p.id == pid)
    }
    pub fn player_mut(&mut self, pid: &str) -> Option<&mut Player> {
        self.players.iter_mut().find(|p| p.id == pid)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Identity {
    pub me: Vec<String>,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    #[serde(rename = "schema", alias = "schema_version", default = "schema")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub created: String,
    pub source: SourceInfo,
    #[serde(default)]
    pub game_area: Option<GameArea>,
    #[serde(default)]
    pub identity: Identity,
    /// "new" | "analyzing" | "ready" | "error"
    #[serde(default = "status_new")]
    pub status: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub analysis_version: String,
}
fn status_new() -> String {
    "new".into()
}

// ---------------------------------------------------------------- edits
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub src_in: f64,
    pub src_out: f64,
}

impl Part {
    pub fn new(src_in: f64, src_out: f64) -> Self {
        Part { src_in, src_out }
    }
    pub fn dur(&self) -> f64 {
        (self.src_out - self.src_in).max(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SubStyle {
    /// "clean" | "pop" | "boxed" | "custom"
    pub preset: String,
    pub font: String,
    pub size: i32,
    pub color: String,
    pub outline: i32,
    pub position: f64,
}

impl Default for SubStyle {
    fn default() -> Self {
        SubStyle { preset: "boxed".into(), font: "Inter".into(), size: 56, color: "#FFFFFF".into(), outline: 3, position: 0.78 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SubtitleSettings {
    pub enabled: bool,
    pub style: SubStyle,
    pub show_names: bool,
    pub speakers: BTreeMap<String, String>,
    pub hidden_lines: Vec<i32>,
    pub censored: Vec<(i32, i32)>,
    pub censor_audio: bool,
    pub edits: BTreeMap<String, String>,
    /// most words on screen at a time (the caption is shown in chunks); 0 = the whole caption
    pub max_words: i32,
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        SubtitleSettings {
            enabled: false,
            style: SubStyle::default(),
            show_names: true,
            speakers: BTreeMap::new(),
            hidden_lines: vec![],
            censored: vec![],
            censor_audio: true,
            edits: BTreeMap::new(),
            max_words: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VerticalSettings {
    pub crop_center: f64,
    pub scoreboard: bool,
    pub hud_strip: bool,
    pub kill_card: bool,
}

impl Default for VerticalSettings {
    fn default() -> Self {
        VerticalSettings { crop_center: 0.5, scoreboard: true, hud_strip: true, kill_card: true }
    }
}

pub type Rect4 = (f64, f64, f64, f64);

/// One element of the output frame. Rects are fractions: dst of the canvas, src of the game area.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerSpec {
    /// "gameplay" | "scorebar" | "killfeed" | "hud" | "minimap" | "subtitles"
    pub kind: String,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default = "full")]
    pub dst: Rect4,
    #[serde(default = "full")]
    pub src: Rect4,
    /// gameplay only: "cover" | "stretch"
    #[serde(default = "cover")]
    pub fit: String,
    #[serde(default = "one")]
    pub zoom: f64,
    #[serde(default = "centre")]
    pub pan: (f64, f64),
}
fn full() -> Rect4 {
    (0.0, 0.0, 1.0, 1.0)
}
fn cover() -> String {
    "cover".into()
}
fn centre() -> (f64, f64) {
    (0.5, 0.5)
}

impl LayerSpec {
    pub fn new(kind: &str, dst: Rect4) -> Self {
        LayerSpec { kind: kind.into(), visible: true, dst, src: full(), fit: cover(), zoom: 1.0, pan: centre() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayoutSpec {
    /// "16:9" | "9:16" | "1:1" | "4:5"
    pub aspect: String,
    /// "blur" | "black"
    pub background: String,
    pub layers: Vec<LayerSpec>,
}

impl Default for LayoutSpec {
    fn default() -> Self {
        LayoutSpec { aspect: "16:9".into(), background: "blur".into(), layers: vec![] }
    }
}

impl LayoutSpec {
    pub fn layer(&self, kind: &str) -> Option<&LayerSpec> {
        self.layers.iter().find(|l| l.kind == kind)
    }
    pub fn layer_mut(&mut self, kind: &str) -> Option<&mut LayerSpec> {
        self.layers.iter_mut().find(|l| l.kind == kind)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KillfeedSettings {
    /// "timed" | "round" | "clip"
    pub keep: String,
    pub seconds: f64,
    /// "bottom" | "top"
    pub newest: String,
    pub rows: i32,
    /// "slide" | "fade" | "pop" | "none"
    pub animation: String,
}

impl Default for KillfeedSettings {
    fn default() -> Self {
        KillfeedSettings { keep: "timed".into(), seconds: 4.5, newest: "bottom".into(), rows: 4, animation: "slide".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EffectSettings {
    /// "none" | "vivid" | "warm" | "cool" | "moody" | "bw"
    pub look: String,
    pub look_intensity: f64,
    pub zoom: bool,
    pub zoom_amount: f64,
    pub zoom_per_kill: BTreeMap<String, f64>,
    pub flash_on_kill: bool,
    /// "none" | "fade" | "flash" | "blur"
    pub transition: String,
    pub super_smooth: bool,
    /// "faster" (optical-flow motion blur) | "fast" (AI frames, 4 per frame) | "subtle" (HD: AI frames, 8 per
    /// frame) | "custom" (`smooth_custom`). Older projects may say "strong" (migrated to custom on load).
    pub smooth_amount: String,
    pub smooth_custom: SmoothCustom,
}

/// Hand-picked super smooth settings (`smooth_amount = "custom"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SmoothCustom {
    /// "flow" (optical-flow motion blur) | "rife" (AI frame interpolation)
    pub method: String,
    /// flow: most blur samples per pixel; rife: in-between frames per output frame
    pub samples: i32,
    /// fraction of the frame interval blurred (0.5 = a 180-degree shutter)
    pub shutter: f64,
    /// flow only: motion measured at half instead of quarter resolution, and both neighbouring frames sampled
    pub high_detail: bool,
}

impl Default for SmoothCustom {
    fn default() -> Self {
        SmoothCustom { method: "flow".into(), samples: 12, shutter: 0.5, high_detail: false }
    }
}

impl EffectSettings {
    /// Bring older settings up to date ("strong" became a custom preset).
    pub fn migrate(&mut self) {
        if self.smooth_amount == "strong" {
            self.smooth_amount = "custom".into();
            self.smooth_custom = SmoothCustom { method: "rife".into(), samples: 8, shutter: 0.75, high_detail: false };
        }
    }
}

impl Default for EffectSettings {
    fn default() -> Self {
        EffectSettings {
            look: "none".into(),
            look_intensity: 0.7,
            zoom: false,
            zoom_amount: 1.08,
            zoom_per_kill: BTreeMap::new(),
            flash_on_kill: false,
            transition: "none".into(),
            super_smooth: false,
            smooth_amount: "faster".into(),
            smooth_custom: SmoothCustom::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MusicSettings {
    pub path: String,
    pub enabled: bool,
    pub volume: f64,
    pub game_volume: f64,
    pub duck: bool,
    /// where in the song playback begins
    pub song_in: f64,
    /// where in the exported video the music begins
    pub start: f64,
    /// where in the exported video it stops (None = the end of the video)
    pub end: Option<f64>,
    /// placed by "Sync to the beat"
    pub synced: bool,
}

impl Default for MusicSettings {
    fn default() -> Self {
        MusicSettings {
            path: String::new(),
            enabled: false,
            volume: 0.35,
            game_volume: 1.0,
            duck: true,
            song_in: 0.0,
            start: 0.0,
            end: None,
            synced: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Edits {
    #[serde(rename = "schema", alias = "schema_version")]
    pub schema_version: u32,
    pub parts: Vec<Part>,
    pub quick_cut: String,
    pub kills_disabled: Vec<String>,
    /// "16:9" | "9:16" | "1:1" | "4:5" | "source"
    pub format: String,
    /// None = default layout for `format`
    pub layout: Option<LayoutSpec>,
    pub killfeed: KillfeedSettings,
    pub highlight_color: String,
    pub effects: EffectSettings,
    pub vertical: VerticalSettings,
    pub subtitles: SubtitleSettings,
    pub music: MusicSettings,
}

impl Default for Edits {
    fn default() -> Self {
        Edits {
            schema_version: SCHEMA,
            parts: vec![],
            quick_cut: "kills".into(),
            kills_disabled: vec![],
            format: "16:9".into(),
            layout: None,
            killfeed: KillfeedSettings::default(),
            highlight_color: "#F4A6C4".into(),
            effects: EffectSettings::default(),
            vertical: VerticalSettings::default(),
            subtitles: SubtitleSettings::default(),
            music: MusicSettings::default(),
        }
    }
}

// ---------------------------------------------------------------- json io
pub fn save_json<T: Serialize>(v: &T, path: &Path) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("json")));
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    std::fs::write(&tmp, s)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let s = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_roundtrip_json_names() {
        let ed = Edits::default();
        let s = serde_json::to_string(&ed).unwrap();
        assert!(s.contains("\"schema\":1"));
        let back: Edits = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ed);
        // a minimal file
        let py: Edits = serde_json::from_str(
            r#"{"schema": 1, "parts": [{"src_in": 1.0, "src_out": 2.5}],
            "subtitles": {"edits": {"3": "hi"}, "censored": [[1, 2]]}, "music": {"end": null}}"#,
        )
        .unwrap();
        assert_eq!(py.parts[0].dur(), 1.5);
        assert_eq!(py.subtitles.edits.get("3").unwrap(), "hi");
    }

    #[test]
    fn old_strong_smoothing_becomes_custom() {
        let mut fx: EffectSettings = serde_json::from_str(r#"{"super_smooth": true, "smooth_amount": "strong"}"#).unwrap();
        fx.migrate();
        assert_eq!(fx.smooth_amount, "custom");
        assert_eq!((fx.smooth_custom.method.as_str(), fx.smooth_custom.samples, fx.smooth_custom.shutter), ("rife", 8, 0.75));
        // other presets are left alone; new projects start on the optical-flow preset
        let mut fx: EffectSettings = serde_json::from_str(r#"{"smooth_amount": "subtle"}"#).unwrap();
        fx.migrate();
        assert_eq!(fx.smooth_amount, "subtle");
        assert_eq!(EffectSettings::default().smooth_amount, "faster");
    }
}
