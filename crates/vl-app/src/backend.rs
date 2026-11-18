//! Adapter to the analysis (`vl-engine`) and render (`vl-render`) crates.
//!
//! Everything the app needs from those crates goes through this module, so the GUI, the worker and the CLI
//! never name their functions directly.
#![allow(dead_code)]

use anyhow::{bail, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use vl_core::config::HighlightsCfg;
use vl_core::image::BgrImage;
use vl_core::models::{Events, GameArea, Kill, LayoutSpec, Part};
use vl_core::projects::ProjectHandle;

/// Progress callback (fraction 0..1, message), same as `vl_core::jobs::Progress`.
pub type Progress<'a> = &'a (dyn Fn(f64, &str) + Sync);

/// A scored highlight.
#[derive(Debug, Clone, PartialEq)]
pub struct HighlightSegment {
    pub start: f64,
    pub end: f64,
    pub score: f64,
    pub round: i32,
    pub label: String,
}

// ------------------------------------------------------------------ analysis (vl-engine)

/// Run the full analysis of a source video. Returns the events and the detected game area.
pub fn analyze(src: &Path, project_dir: &Path, progress: Progress) -> Result<(Events, GameArea)> {
    let (ev, ga, _info) = vl_engine::analyze::analyze(src, project_dir, progress, &vl_core::jobs::Cancel::default())?;
    Ok((ev, ga))
}

/// Recompute "me" and round results after the identity changed.
pub fn finalize_identity(ev: &mut Events, aliases: &[String]) -> Vec<String> {
    vl_engine::analyze::finalize_identity(ev, aliases)
}

/// Canonical comparable player name.
pub fn normalize_name(name: &str) -> String {
    vl_engine::names::normalize(name)
}

/// The generic "Me" name shown with hidden names.
pub fn is_me_text(name: &str) -> bool {
    vl_engine::names::is_me_text(name)
}

/// OCR one text line (BGR image). Returns (text, score).
pub fn ocr_read_line(img: &BgrImage) -> Result<(String, f32)> {
    Ok(vl_engine::ocr::read_line(img))
}

/// `valolights cli eval`: compare detections with the hand-checked labels. Returns the exit code.
pub fn run_eval(labels: &Path) -> Result<i32> {
    vl_engine::evaluate::run_eval(labels, None)
}

// ------------------------------------------------------------------ highlights (vl-render)

/// Kills by "me". `disabled` = kill ids left out of the cut, None = keep all.
pub fn my_kills(ev: &Events, me: &[String], include_team: bool, disabled: Option<&[String]>) -> Vec<Kill> {
    vl_render::highlights::my_kills(ev, me, include_team, disabled.unwrap_or(&[]))
}

/// The parts of a quick cut ("kills", "segments", "best", "rounds", "plays", "whole").
/// Returns None while highlights are not available (the cut is then treated as hand-made and never rebuilt).
pub fn quick_cut(name: &str, ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Option<Vec<Part>> {
    vl_render::highlights::quick_cut(name, ev, me, cfg, duration, disabled).ok()
}

// ------------------------------------------------------------------ Smart cut (vl-engine audio + vl-render smart)

/// A part the Smart cut keeps: start, end, score, reasons ("Triple kill", "Laughter"…).
pub use vl_render::smart::Moment as SmartMoment;

/// Signals newer than this are current (older projects run the sound pass again).
pub const SMART_VERSION: u32 = vl_engine::audio::VERSION;

/// The sound pass: laughter, shouting, cheering, loudness and speech along the clip.
pub fn analyze_audio(src: &Path, duration: f64, progress: &dyn Fn(f64)) -> Result<vl_core::models::AudioSignals> {
    vl_engine::audio::analyze_audio(src, duration, progress, &vl_core::jobs::Cancel(Default::default()))
}

/// Funny / hype phrases in transcript words ((start, text) pairs).
pub fn smart_words(words: &[(f64, String)]) -> Vec<(f64, String)> {
    vl_render::smart::transcript_words(words)
}

/// The moments the Smart cut keeps, in clip order, with their reasons.
pub fn smart_moments(ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Vec<SmartMoment> {
    vl_render::smart::chosen(ev, me, cfg, duration, disabled)
}

/// Scored highlight segments.
pub fn build_segments(ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Vec<HighlightSegment> {
    vl_render::highlights::build_segments(ev, me, cfg, duration, disabled)
        .into_iter()
        .map(|s| HighlightSegment { start: s.start, end: s.end, score: s.score, round: s.round, label: s.label() })
        .collect()
}

/// Transition overlap.
pub const TRANSITION_DUR: f64 = 0.35;

/// Length of the exported video: parts placed back to back, transitions overlap neighbours.
pub fn edit_duration(parts: &[Part], transition: &str) -> f64 {
    vl_render::editlist::EditList::new(parts, transition).duration
}

/// Default layout for an output aspect.
pub fn default_layout(aspect: &str, ga: &GameArea) -> LayoutSpec {
    vl_render::layoutspec::default_layout(aspect, ga)
}

/// Render a project to `out`. `options` = the Export page options.
pub fn render_project(h: &ProjectHandle, out: &Path, progress: Progress, proxy: bool, options: &Value) -> Result<PathBuf> {
    vl_render::render::render_project(h, out, progress, proxy, options, &vl_core::jobs::Cancel::default())
}

/// Is RIFE frame interpolation usable? (ok, reason)
pub fn rife_works() -> (bool, String) {
    vl_render::smooth::rife_works()
}

/// Super smooth presets (key, label) in display order.
pub const SMOOTH_PRESETS: [(&str, &str); 4] = vl_render::smooth::PRESETS;

/// Whether these settings make AI in-between frames (RIFE, needs a Vulkan GPU) rather than optical-flow blur.
pub fn smooth_uses_ai(fx: &vl_core::models::EffectSettings) -> bool {
    !vl_render::smooth::params(&fx.smooth_amount, &fx.smooth_custom).flow
}

// ------------------------------------------------------------------ subtitles preview (vl-render)

/// Source parts on the edit timeline.
pub use vl_render::editlist::EditList;

/// (lavfi graph, W, H, subtitle box in px) of the export layout at preview resolution
/// (`preview_graph(ga, effective_layout(ed, ga), look, intensity)`).
#[allow(clippy::type_complexity)]
pub fn preview_composition(ed: &vl_core::models::Edits, ga: &GameArea) -> (String, i64, i64, Option<(i64, i64, i64, i64)>) {
    let l = vl_render::layoutspec::effective_layout(ed, ga);
    let (graph, w, h) = vl_render::render::preview_graph(ga, &l, &ed.effects.look, ed.effects.look_intensity, 720);
    let bx = l.layer("subtitles").filter(|s| s.visible).map(|s| vl_render::layoutspec::px_rect(s.dst, w, h));
    (graph, w, h, bx)
}

/// Write the captions as .ass for the preview player.
#[allow(clippy::too_many_arguments)]
pub fn build_ass(
    tr: &vl_core::transcript::Transcript,
    st: &vl_core::models::SubtitleSettings,
    el: &EditList,
    w: i64,
    h: i64,
    highlight: &str,
    out: &Path,
    bx: Option<(i64, i64, i64, i64)>,
) -> Result<PathBuf> {
    vl_render::ass::build_ass(tr, st, el, w, h, highlight, out, bx)
}

/// The music as heard under a back-to-back preview of `parts`.
pub fn music_preview_wav(mus: &vl_core::models::MusicSettings, parts: &[Part], transition: &str, out_dir: &Path) -> Option<PathBuf> {
    vl_render::music::preview_wav(mus, parts, transition, out_dir).ok().flatten()
}

// ------------------------------------------------------------------ Export page (vl-render)

/// The output timeline of a cut: (parts kept, duration, transition overlap).
pub fn edit_list_info(parts: &[Part], transition: &str) -> (usize, f64, f64) {
    let el = vl_render::editlist::EditList::new(parts, transition);
    (el.parts.len(), el.duration, el.td)
}

/// How many of these source times are in the cut.
pub fn count_in_output(parts: &[Part], transition: &str, times: &[f64]) -> usize {
    let el = vl_render::editlist::EditList::new(parts, transition);
    times.iter().filter(|t| el.to_output(**t).is_some()).count()
}

/// Output shape of the edit.
pub fn output_aspect(ed: &vl_core::models::Edits, ga: Option<&GameArea>) -> String {
    match ga {
        Some(ga) => vl_render::layoutspec::effective_layout(ed, ga).aspect,
        None => ed.format.clone(),
    }
}

/// Pixel size of the output canvas.
pub fn canvas_size(aspect: &str, short_side: i64) -> (i64, i64) {
    vl_render::layoutspec::canvas_size(aspect, short_side)
}

/// Editor marker files: ".xml" Premiere, ".edl" Resolve (+ `_markers.edl`), ".json".
pub fn export_for_editors(ext: &str, h: &ProjectHandle, out: &Path) -> Result<PathBuf> {
    match ext {
        ".xml" => vl_render::exporters::export_premiere_xml(h, out),
        ".edl" => vl_render::exporters::export_resolve_edl(h, out),
        ".json" => vl_render::exporters::export_json(h, out),
        _ => bail!("unknown export {ext}"),
    }
}

// ------------------------------------------------------------------ Layout page (vl-render layoutspec / looks / overlays)

/// Layer kinds back to front.
pub const LAYER_ORDER: &[&str] = vl_render::layoutspec::LAYER_ORDER;
/// Kill feed rows shown by default.
pub const KILLFEED_ROWS: i32 = vl_render::layoutspec::KILLFEED_ROWS;
/// Colour looks (key, label) in display order.
pub const LOOKS: &[(&str, &str)] = vl_render::looks::LOOKS;

/// Display name of a layer kind.
pub fn layer_name(kind: &str) -> &'static str {
    vl_render::layoutspec::layer_name(kind)
}

/// Score bar / HUD / minimap / webcam: live crops with a locked aspect.
pub fn layer_is_overlay(kind: &str) -> bool {
    vl_render::layoutspec::is_overlay(kind)
}

/// Width / height in pixels of an overlay's starting HUD crop.
pub fn layer_src_aspect_px(kind: &str, ga: &GameArea) -> f64 {
    vl_render::layoutspec::src_aspect_px(kind, ga)
}

/// Width / height in pixels of a crop (fractions of the game area).
pub fn crop_aspect_px(src: vl_core::models::Rect4, ga: &GameArea) -> f64 {
    vl_render::layoutspec::rect_aspect_px(src, ga)
}

/// An overlay's starting crop (detected HUD region; the webcam's default box).
pub fn overlay_default_src(kind: &str, ga: &GameArea) -> vl_core::models::Rect4 {
    vl_render::layoutspec::overlay_src(kind, ga)
}

/// The webcam may be cropped anywhere in the video frame, the HUD layers only inside the game area.
pub fn crops_full_frame(kind: &str) -> bool {
    vl_render::layoutspec::crops_full_frame(kind)
}

/// Height of an overlay's box from its width and its crop's shape.
pub fn fit_overlay_height(layer: &mut vl_core::models::LayerSpec, ga: &GameArea, w_px: i64, h_px: i64) {
    vl_render::layoutspec::fit_overlay_height(layer, ga, w_px, h_px)
}

/// Add layers an older layout lacks (the webcam), hidden. True when something was added.
pub fn complete_layout(l: &mut LayoutSpec, ga: &GameArea) -> bool {
    vl_render::layoutspec::complete_layout(l, ga)
}

/// Source crop of the gameplay layer, fractions of the game area.
pub fn gameplay_src(layer: &vl_core::models::LayerSpec, ga: &GameArea, w_px: i64, h_px: i64) -> vl_core::models::Rect4 {
    vl_render::layoutspec::gameplay_src(layer, ga, w_px, h_px)
}

/// The edit's layout, or the default for its format.
pub fn effective_layout(ed: &vl_core::models::Edits, ga: &GameArea) -> LayoutSpec {
    vl_render::layoutspec::effective_layout(ed, ga)
}

/// Output time of each source time, None where it is cut out.
pub fn output_times(parts: &[Part], transition: &str, times: &[f64]) -> Vec<Option<f64>> {
    let el = vl_render::editlist::EditList::new(parts, transition);
    times.iter().map(|t| el.to_output(*t)).collect()
}

/// Draw a kill feed row PNG from our own data.
#[allow(clippy::too_many_arguments)]
pub fn draw_feed_row(
    out: &Path,
    killer: &str,
    victim: &str,
    killer_portrait: Option<&Path>,
    victim_portrait: Option<&Path>,
    headshot: bool,
    killer_side: &str,
    height: i64,
) -> Result<(i64, i64)> {
    vl_render::overlays::feed_row(out, killer, victim, killer_portrait, victim_portrait, headshot, killer_side, height)
}

// ------------------------------------------------------------------ Trim page (vl-render music / highlights, vl-engine rounds)

/// Quick cut keys and names, in menu order.
pub const QUICK_CUTS: &[(&str, &str)] = vl_render::highlights::QUICK_CUTS;

/// The analysis of a song (beats, bars, waveform...).
pub use vl_render::music::SongInfo;

/// Song analysis from the cache only (None when it was never analysed).
pub fn song_cached(path: &str) -> Option<SongInfo> {
    vl_render::music::cached_analysis(path)
}

/// Analyse a song (slow, cached on disk).
pub fn song_analyze(path: &str) -> Result<SongInfo> {
    vl_render::music::analyze(path)
}

/// Edit-time range the music plays in.
pub fn music_span(mus: &vl_core::models::MusicSettings, total: f64) -> (f64, f64) {
    vl_render::music::span(mus, total)
}

/// `music.preview_wav` keeping the error (the Trim page says when the preview music failed).
pub fn music_preview_wav_result(
    mus: &vl_core::models::MusicSettings,
    parts: &[Part],
    transition: &str,
    out_dir: &Path,
) -> Result<Option<PathBuf>> {
    vl_render::music::preview_wav(mus, parts, transition, out_dir)
}

/// Song time that puts the song's best part under the edit, its drop on `first_kill`.
pub fn music_best_part(info: &SongInfo, length: f64, first_kill: f64) -> f64 {
    vl_render::music::best_part(info, length, first_kill)
}

/// Beat times on the edit timeline.
pub fn music_edit_beats(info: &SongInfo, mus: &vl_core::models::MusicSettings, total: f64, bounded: bool) -> Vec<f64> {
    vl_render::music::edit_beats(info, mus, total, bounded)
}

/// Move every cut onto a beat: (new parts, cuts moved onto a beat, cuts).
pub fn music_sync_cuts(
    parts: &[Part],
    transition: &str,
    beats: &[f64],
    kill_times: &[f64],
    src_duration: f64,
) -> (Vec<Part>, usize, usize) {
    vl_render::music::sync_cuts(parts, transition, beats, kill_times, src_duration)
}

/// Index of the round a moment belongs to.
pub fn round_of(t: f64, rounds: &[vl_core::models::Round]) -> i32 {
    vl_engine::rounds::round_of(t, rounds)
}
