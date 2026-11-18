//! Layout: pick the export shape first, then arrange every layer on the canvas; kill effects and looks.
//!
//! The page edits `st.ed.layout` (created from the format on first use), the kill feed
//! settings and the effects. Discrete changes (clicks, toggles, switches) are their own undo step
//! (`step`), slider drags group (`touch`). The arrange canvas is [`LayoutCanvas`]; "Render preview" asks the
//! main window for the 540p proxy job (`Action::Preview`) and plays the result in the page's own player.
use super::{Action, JobNotice, Page, PageCtx, Shell};
use crate::backend;
use crate::state::{AppState, StateEvent};
use crate::theme::{c, Role, S1, S2, S3, S4, S6};
use crate::widgets::crop_editor::CropEditor;
use crate::widgets::layout_canvas::{hex_color, LayoutCanvas};
use crate::widgets::player::{fmt_time, Load, Player};
use crate::widgets::ui::{self, side_panel, Button, IconButton, Slider, Toggle};
use egui::{pos2, vec2, Color32, Id, Key, Rect, Sense, Ui};
use image::RgbaImage;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use vl_core::models::{GameArea, LayoutSpec, Rect4};

pub const SHAPES: [(&str, &str); 4] = [("16:9", "16:9"), ("9:16", "9:16"), ("1:1", "1:1"), ("4:5", "4:5")];

pub fn shape_note(k: &str) -> &'static str {
    match k {
        "16:9" => "Landscape for YouTube and Discord",
        "9:16" => "Vertical for Shorts, TikTok and Reels",
        "1:1" => "Square for feeds",
        "4:5" => "Tall feed post for Instagram",
        _ => "",
    }
}

fn layer_hint_text(kind: &str) -> &'static str {
    match kind {
        "gameplay" => "Drag to move, corners to resize. Zoom and pan below choose which part of the game shows",
        "scorebar" => "Live crop of the in-game score and timer. The crop icon changes what it shows; its shape follows the crop",
        "killfeed" => "Your kills as they looked in the game feed, newest {newest}",
        "hud" => "Live crop of your health, abilities and ammo. The crop icon changes what it shows; its shape follows the crop",
        "minimap" => "Live crop of the minimap. The crop icon changes what it shows; its shape follows the crop",
        "webcam" => "Your facecam from the recording, as a live crop, anywhere in the video. The crop icon frames it",
        "subtitles" => "Captions sit at the bottom of this box",
        _ => "",
    }
}

pub fn keep_note(k: &str) -> &'static str {
    match k {
        "timed" => "Each kill leaves the feed after a few seconds, like in game",
        "round" => "Kills stay until the round ends, then the feed clears",
        _ => "Kills stay on screen until the clip ends",
    }
}

pub const NUDGE: f64 = 0.002;
pub const NUDGE_BIG: f64 = 0.01;
const MSG_TIME: Duration = Duration::from_millis(3500);
pub const TIPS: &str =
    "Click a layer to select it · drag to move · corners to resize · arrow keys nudge, Shift for bigger steps · Esc cancels a drag";
const CROP_TIPS: &str = "Drag the box to choose what the layer shows · corners resize it · Esc or the tick goes back to the layers";

/// No dialog is open over the page.
fn ctx_modal_free(ui: &Ui) -> bool {
    ui.ctx().memory(|m| m.top_modal_layer().is_none())
}
const SMOOTH_TEXT: &str = "A light motion blur that follows the movement makes the gameplay feel smoother. Only the gameplay is smoothed, the kill feed and captions stay sharp. Applied in the final export only. Faster blurs along the measured motion on the processor; Fast and HD subtle make AI in-between frames on the graphics card and blend them, which is smoother in big flicks but several times slower";

/// Tooltips of the super smooth presets.
fn smooth_tip(k: &str) -> String {
    match k {
        "faster" => "Optical-flow motion blur: each pixel is blurred along its own motion. The quickest (roughly 2 to 3 seconds per second of 1080p video) and works on any PC",
        "fast" => "AI in-between frames, 4 per frame, blended into motion blur. Needs a Vulkan graphics card; roughly 4 times slower than Faster",
        "subtle" => "AI in-between frames, 8 per frame: the smoothest blur. Needs a Vulkan graphics card; roughly 8 times slower than Faster",
        "custom" => "Pick the method, the number of samples and how much blur yourself",
        _ => "",
    }
    .into()
}

fn times(n: f64) -> String {
    format!("×{n:.2}")
}

/// Round half to even to an int.
fn round_even(v: f64) -> i64 {
    v.round_ties_even() as i64
}

/// `{v:g}`-style text for the values used here (seconds).
fn fmt_g(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// What the user did this frame, applied after drawing.
#[derive(Debug, Clone)]
enum Do {
    Shape(String),
    Reset,
    Background(String),
    Visible(String, bool),
    Select(String),
    /// start cropping a layer (Some) or finish (None)
    CropEdit(Option<String>),
    /// a layer's new crop, fractions of the game area
    Crop(String, Rect4),
    ResetCrop(String),
    Fit(String),
    Zoom(i64),
    Pan(bool, i64),
    KfKeep(String),
    KfNewest(String),
    KfAnim(String),
    KfSeconds(i64),
    KfRows(i64),
    PunchIn(bool),
    Flash(bool),
    Kzoom(i64),
    PerKill(String, i64),
    Transition(String),
    Smooth(bool),
    SmoothAmount(String),
    /// custom super smooth: "flow" | "rife"
    SmoothMethod(String),
    SmoothSamples(i64),
    /// percent of the frame interval
    SmoothShutter(i64),
    SmoothDetail(bool),
    Look(String),
    LookIntensity(i64),
    View(String),
    Preview,
    Cancel,
}

type StillMsg = (PathBuf, Option<(RgbaImage, RgbaImage)>);

pub struct LayoutPage {
    pub canvas: LayoutCanvas,
    pub player: Player,
    /// "arrange" | "preview"
    pub view: String,
    /// aspect -> the arrangement left behind when switching away
    pub shape_cache: HashMap<String, LayoutSpec>,
    rows_sig: Option<(String, Vec<String>)>,
    rows_dirty: bool,
    want_still: bool,
    still_rx: Option<Receiver<StillMsg>>,
    /// cached interpolator check (it takes a few seconds)
    pub smooth_ok: Option<(bool, String)>,
    smooth_rx: Option<Receiver<(bool, String)>>,
    /// the interpolator check (tests replace it)
    pub smooth_probe: fn() -> (bool, String),
    pub preview_path: String,
    /// edits the shown preview was rendered from
    preview_sig: String,
    pending_sig: String,
    /// a finished preview waiting to be loaded into the player (and whether to keep it paused)
    pending_load: Option<(String, bool)>,
    /// feedback line under the canvas
    pub msg: String,
    msg_until: Option<Instant>,
    /// top-right status text
    pub status: String,
    /// progress bar shown (a preview renders): Some(p)
    pub progress: Option<f64>,
    pub stop_enabled: bool,
    visible: bool,
    /// sliders drawn this frame (a focused slider keeps its arrow keys)
    slider_ids: Vec<Id>,
    /// widget rects of the last frame, for scripted checks ("shape:1:1", "layer:hud", "toggle:hud", ...)
    pub rects: HashMap<String, Rect>,
    /// cropping a layer: the centre shows the whole video frame with the layer's crop box
    pub crop: Option<CropEditor>,
}

impl LayoutPage {
    pub fn new(shell: &Shell) -> Self {
        LayoutPage {
            canvas: LayoutCanvas::default(),
            player: Player::new(shell.gl.as_ref(), "layout"),
            view: "arrange".into(),
            shape_cache: HashMap::new(),
            rows_sig: None,
            rows_dirty: true,
            want_still: false,
            still_rx: None,
            smooth_ok: None,
            smooth_rx: None,
            smooth_probe: backend::rife_works,
            preview_path: String::new(),
            preview_sig: String::new(),
            pending_sig: String::new(),
            pending_load: None,
            msg: String::new(),
            msg_until: None,
            status: String::new(),
            progress: None,
            stop_enabled: true,
            visible: false,
            slider_ids: vec![],
            rects: HashMap::new(),
            crop: None,
        }
    }

    // ------------------------------------------------------------ data
    /// The edit's layout, created from its format on first use. None without a game area.
    pub fn ensure_layout(st: &mut AppState) -> bool {
        let Some(ga) = st.h.as_ref().and_then(|h| h.project.game_area) else { return false };
        if st.ed.layout.is_none() {
            let l = backend::effective_layout(&st.ed, &ga);
            st.ed.format = l.aspect.clone();
            st.ed.layout = Some(l);
        } else if let Some(l) = st.ed.layout.as_mut() {
            // layouts saved before the webcam layer existed
            backend::complete_layout(l, &ga);
        }
        true
    }

    /// The video frame size in pixels.
    fn frame_px(st: &AppState) -> (f64, f64) {
        st.h.as_ref().map(|h| (h.project.source.width.max(1) as f64, h.project.source.height.max(1) as f64)).unwrap_or((1920.0, 1080.0))
    }

    /// A crop in fractions of the game area -> fractions of the whole frame.
    fn crop_to_frame(src: Rect4, ga: &GameArea, fp: (f64, f64)) -> Rect4 {
        let (x, y, w, h) = src;
        let (gw, gh) = (ga.w as f64, ga.h as f64);
        ((ga.x as f64 + x * gw) / fp.0, (ga.y as f64 + y * gh) / fp.1, w * gw / fp.0, h * gh / fp.1)
    }

    /// Fractions of the whole frame -> fractions of the game area (what a layer stores).
    fn crop_from_frame(r: Rect4, ga: &GameArea, fp: (f64, f64)) -> Rect4 {
        let (x, y, w, h) = r;
        let (gw, gh) = (ga.w as f64, ga.h as f64);
        ((x * fp.0 - ga.x as f64) / gw, (y * fp.1 - ga.y as f64) / gh, w * fp.0 / gw, h * fp.1 / gh)
    }

    /// Open the crop editor on an overlay layer (score bar, health and ammo, minimap, webcam).
    pub fn start_crop(&mut self, st: &AppState, kind: &str) {
        let (Some(ga), Some(l)) = (Self::ga(st), st.ed.layout.as_ref()) else { return };
        let Some(lyr) = l.layer(kind).filter(|x| backend::layer_is_overlay(&x.kind)) else { return };
        let fp = Self::frame_px(st);
        let bounds =
            if backend::crops_full_frame(kind) { (0.0, 0.0, 1.0, 1.0) } else { Self::crop_to_frame((0.0, 0.0, 1.0, 1.0), &ga, fp) };
        let (bx, by, bw, bh) = bounds;
        // older or odd crops: back inside where the box may go
        let (x, y, w, h) = Self::crop_to_frame(lyr.src, &ga, fp);
        let (w, h) = (w.min(bw), h.min(bh));
        let live = (x.max(bx).min(bx + bw - w), y.max(by).min(by + bh - h), w, h);
        self.canvas.select(kind);
        self.set_view("arrange");
        self.crop = Some(CropEditor::new(kind, live, bounds, fp));
    }

    /// Store a layer's crop; its box keeps its width and top-left corner, the height follows the crop.
    fn set_crop(&mut self, st: &mut AppState, kind: &str, src: Rect4) {
        let Some(ga) = Self::ga(st) else { return };
        let Some(aspect) = st.ed.layout.as_ref().filter(|l| l.layer(kind).map(|x| x.src != src).unwrap_or(false)).map(|l| l.aspect.clone())
        else {
            return;
        };
        let (cw, ch) = backend::canvas_size(&aspect, 1080);
        let k = kind.to_string();
        Self::step(st, |st| {
            if let Some(lyr) = st.ed.layout.as_mut().and_then(|l| l.layer_mut(&k)) {
                lyr.src = src;
                backend::fit_overlay_height(lyr, &ga, cw, ch);
            }
        });
    }

    /// Carry the overlay crops of `from` over to `to` (a shape switch or reset re-places boxes, but what
    /// each layer shows stays).
    fn keep_crops(from: &LayoutSpec, to: &mut LayoutSpec, ga: &GameArea) {
        let (cw, ch) = backend::canvas_size(&to.aspect, 1080);
        for lyr in to.layers.iter_mut().filter(|x| backend::layer_is_overlay(&x.kind)) {
            if let Some(old) = from.layer(&lyr.kind) {
                if old.src != lyr.src {
                    lyr.src = old.src;
                    backend::fit_overlay_height(lyr, ga, cw, ch);
                }
            }
        }
    }

    fn ga(st: &AppState) -> Option<GameArea> {
        st.h.as_ref().and_then(|h| h.project.game_area)
    }

    fn project_changed(&mut self, st: &mut AppState) {
        self.canvas.clear_still();
        self.canvas.clear_rows();
        self.canvas.drop_drag();
        self.crop = None;
        self.shape_cache.clear();
        self.rows_sig = None;
        self.preview_path.clear();
        self.preview_sig.clear();
        self.pending_sig.clear();
        self.pending_load = None;
        self.still_rx = None;
        self.player.stop();
        self.set_view("arrange");
        self.set_progress(None, "", st);
        self.msg.clear();
        self.msg_until = None;
        if st.h.is_none() {
            return;
        }
        Self::ensure_layout(st);
        self.want_still = true;
        self.rows_dirty = true;
    }

    /// A frame of the clip for the canvas: cache/still.png (made with ffmpeg once), decoded on a thread.
    fn load_still(&mut self, st: &AppState, ctx: &egui::Context) {
        self.want_still = false;
        let Some(h) = st.h.as_ref() else { return };
        let Some(ga) = h.project.game_area else { return };
        // the whole video frame: the webcam may sit outside the game area, and the crop editor shows it all
        let mut out = h.dir.join("cache").join("still_full.png");
        if st.read_only && !out.exists() {
            // QA / read-only mode never writes into a project
            out = std::env::temp_dir().join(format!("valolights_still_full_{}.png", h.project.id));
        }
        let kills = st.my_kills(false);
        let t = kills.first().map(|k| k.t + 0.6).unwrap_or(h.project.source.duration * 0.3);
        let src = h.source().to_string();
        if !(out.exists() || h.source_ok()) {
            return;
        }
        let dir = h.dir.clone();
        let (tx, rx) = channel();
        self.still_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let work = || -> anyhow::Result<(RgbaImage, RgbaImage)> {
                if !out.exists() {
                    if let Some(p) = out.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    let exe = vl_core::ffmpeg::ffmpeg_exe()?;
                    let ss = format!("{t:.2}");
                    let o = out.to_string_lossy().into_owned();
                    vl_core::ffmpeg::run(&[exe.as_str(), "-y", "-v", "error", "-ss", &ss, "-i", &src, "-frames:v", "1", &o])?;
                }
                let full = image::open(&out)?.to_rgba8();
                // the blurred background is made from the game area, like the render's
                let (gx, gy) =
                    ((ga.x.max(0) as u32).min(full.width().saturating_sub(1)), (ga.y.max(0) as u32).min(full.height().saturating_sub(1)));
                let game = image::imageops::crop_imm(
                    &full,
                    gx,
                    gy,
                    (ga.w.max(1) as u32).min(full.width() - gx),
                    (ga.h.max(1) as u32).min(full.height() - gy),
                )
                .to_image();
                let (sw, sh) = ((game.width() / 24).max(8), (game.height() / 24).max(8));
                let small = image::imageops::thumbnail(&game, sw, sh);
                Ok((full, small))
            };
            let r = work();
            if let Err(e) = &r {
                log::warn!("layout still: {e}");
            }
            let _ = tx.send((dir, r.ok()));
            ctx.request_repaint();
        });
    }

    /// Kill feed rows drawn on the canvas: your first kills that are still switched on.
    fn load_rows(&mut self, st: &AppState, ctx: &egui::Context) {
        self.rows_dirty = false;
        let Some(h) = st.h.as_ref() else { return };
        let kills: Vec<_> = st.my_kills(false).into_iter().take(6).collect();
        let sig = (h.project.id.clone(), kills.iter().map(|k| k.id.clone()).collect::<Vec<_>>());
        if self.rows_sig.as_ref() == Some(&sig) {
            return;
        }
        self.rows_sig = Some(sig);
        let names: HashMap<&str, &str> =
            st.ev.as_ref().map(|e| e.players.iter().map(|p| (p.id.as_str(), p.display.as_str())).collect()).unwrap_or_default();
        let mut rows = vec![];
        for k in &kills {
            let img = match k.row_image.as_ref().map(|r| h.dir.join(r)).filter(|p| p.exists()) {
                Some(p) => image::open(&p).ok(),
                None => {
                    let dir = if st.read_only {
                        std::env::temp_dir().join("valolights_rows_drawn").join(&h.project.id)
                    } else {
                        h.dir.join("cache").join("rows_drawn")
                    };
                    let p = dir.join(format!("{}.png", k.id));
                    if !p.exists() {
                        let _ = std::fs::create_dir_all(&dir);
                        let kp = k.killer_portrait.as_ref().map(|x| h.dir.join(x));
                        let vp = k.victim_portrait.as_ref().map(|x| h.dir.join(x));
                        if let Err(e) = backend::draw_feed_row(
                            &p,
                            names.get(k.killer.as_str()).copied().unwrap_or("?"),
                            names.get(k.victim.as_str()).copied().unwrap_or("?"),
                            kp.as_deref(),
                            vp.as_deref(),
                            k.headshot,
                            &k.killer_side,
                            48,
                        ) {
                            log::warn!("feed row {}: {e}", k.id);
                        }
                    }
                    image::open(&p).ok()
                }
            };
            if let Some(i) = img {
                rows.push(i.to_rgba8());
            }
        }
        self.canvas.set_rows(ctx, &rows);
    }

    pub fn layer_hint(st: &AppState, kind: &str) -> String {
        let lyr = st.ed.layout.as_ref().and_then(|l| l.layer(kind));
        if let Some(l) = lyr {
            if !l.visible {
                return format!("{} is hidden. Tick its box to show it and move it on the canvas", backend::layer_name(kind));
            }
        }
        let mut hint = layer_hint_text(kind).replace("{newest}", if st.ed.killfeed.newest == "bottom" { "below" } else { "on top" });
        if kind == "subtitles" && !st.ed.subtitles.enabled {
            hint += ". Subtitles are off, turn them on in Subtitles";
        }
        hint
    }

    /// Width and height of the gameplay crop (fractions of the game area) at the current zoom and box.
    pub fn pan_span(st: &AppState) -> (f64, f64) {
        let (Some(l), Some(ga)) = (st.ed.layout.as_ref(), Self::ga(st)) else { return (1.0, 1.0) };
        let Some(g) = l.layer("gameplay") else { return (1.0, 1.0) };
        let (w, h) = backend::canvas_size(&l.aspect, 1080);
        let (_, _, sw, sh) = backend::gameplay_src(g, &ga, w, h);
        (sw, sh)
    }

    /// Slider positions (0..100) of the pan: the sliders move the crop from one edge to the other; the stored
    /// pan is the crop centre.
    pub fn pan_slider(st: &AppState) -> (i64, i64) {
        let (sw, sh) = Self::pan_span(st);
        let pan = st.ed.layout.as_ref().and_then(|l| l.layer("gameplay")).map(|g| g.pan).unwrap_or((0.5, 0.5));
        let pos = |c: f64, span: f64| if span >= 0.999 { 0.5 } else { ((c - span / 2.0) / (1.0 - span)).clamp(0.0, 1.0) };
        (round_even(pos(pan.0, sw) * 100.0), round_even(pos(pan.1, sh) * 100.0))
    }

    pub fn pan_note(st: &AppState) -> &'static str {
        let (sw, sh) = Self::pan_span(st);
        let (can_x, can_y) = (sw < 0.999, sh < 0.999);
        if can_x && can_y {
            ""
        } else if !(can_x || can_y) {
            "The whole game already shows. Zoom in to move it around"
        } else if !can_x {
            "The full width of the game already shows. Zoom in to move left or right"
        } else {
            "The full height of the game already shows. Zoom in to move up or down"
        }
    }

    /// Your switched-on kills that are inside the cut, with their time in the edited video.
    pub fn kills_in_cut(st: &AppState) -> Vec<(vl_core::models::Kill, f64)> {
        let kills = st.my_kills(false);
        let parts = &st.ed.parts;
        if parts.is_empty() {
            return kills
                .into_iter()
                .map(|k| {
                    let t = k.t;
                    (k, t)
                })
                .collect();
        }
        let ts: Vec<f64> = kills.iter().map(|k| k.t).collect();
        let outs = backend::output_times(parts, &st.ed.effects.transition, &ts);
        kills.into_iter().zip(outs).filter_map(|(k, t)| t.map(|t| (k, t))).collect()
    }

    pub fn tr_note(st: &AppState) -> &'static str {
        if st.ed.parts.len() <= 1 {
            "Your cut is one part, so there is nothing to blend"
        } else {
            "Shown in Preview video. Each blend overlaps the parts on either side by 0.35 s"
        }
    }

    /// The super smooth status line (empty = hidden).
    pub fn smooth_note(&self, st: &AppState) -> String {
        if self.smooth_rx.is_some() {
            "Checking that your graphics card can make AI frames…".into()
        } else if let Some((false, why)) = &self.smooth_ok {
            let ai = backend::smooth_uses_ai(&st.ed.effects);
            format!(
                "AI frames can't run on this PC: {why}. {}",
                if st.ed.effects.super_smooth && ai { "The export will use Faster instead" } else { "Faster still works" }
            )
        } else {
            String::new()
        }
    }

    /// Load the still frame again (checks).
    pub fn reload_still(&mut self) {
        self.want_still = true;
    }

    pub fn smooth_checking(&self) -> bool {
        self.smooth_rx.is_some()
    }

    // ------------------------------------------------------------ feedback
    fn flash(&mut self, msg: impl Into<String>) {
        self.msg = msg.into();
        self.msg_until = Some(Instant::now() + MSG_TIME);
    }

    // ------------------------------------------------------------ edits
    fn save(st: &mut AppState) {
        if let Some(l) = &st.ed.layout {
            st.ed.format = l.aspect.clone();
        }
        st.touch();
    }

    /// A discrete change (click, toggle, switch): its own undo step, never merged with the one before.
    fn step(st: &mut AppState, f: impl FnOnce(&mut AppState)) {
        st.begin_step();
        f(st);
        Self::save(st);
        st.end_step();
    }

    fn shape(&mut self, st: &mut AppState, aspect: &str) {
        let Some(ga) = Self::ga(st) else { return };
        let Some(l) = st.ed.layout.clone() else { return };
        if l.aspect == aspect {
            return;
        }
        self.shape_cache.insert(l.aspect.clone(), l.clone());
        let kept = self.shape_cache.get(aspect).cloned();
        let had = kept.is_some();
        let mut next = match kept {
            Some(k) => k,
            None => {
                let mut d = backend::default_layout(aspect, &ga);
                d.background = l.background.clone();
                d
            }
        };
        backend::complete_layout(&mut next, &ga);
        Self::keep_crops(&l, &mut next, &ga);
        Self::step(st, |st| st.ed.layout = Some(next));
        self.crop = None;
        self.after_layout_replaced(st);
        self.flash(if had {
            format!("Back to your {aspect} arrangement · Ctrl+Z undoes the switch")
        } else {
            format!("Layers placed for {aspect} · Ctrl+Z undoes the switch")
        });
    }

    fn reset(&mut self, st: &mut AppState) {
        let Some(ga) = Self::ga(st) else { return };
        let Some(l) = st.ed.layout.clone() else { return };
        let mut fresh = backend::default_layout(&l.aspect, &ga);
        fresh.background = l.background.clone();
        Self::keep_crops(&l, &mut fresh, &ga);
        if serde_json::to_value(&fresh).ok() == serde_json::to_value(&l).ok() {
            self.flash("The layout is already at its starting arrangement");
            return;
        }
        Self::step(st, |st| st.ed.layout = Some(fresh));
        self.after_layout_replaced(st);
        self.flash(format!("Layout reset for {} · Ctrl+Z to undo", l.aspect));
    }

    /// After the layout was replaced (undo, reset): end any drag and keep a valid selection.
    fn after_layout_replaced(&mut self, st: &AppState) {
        self.canvas.drop_drag();
        if st.ed.layout.as_ref().map(|l| l.layer(&self.canvas.sel).is_none()).unwrap_or(false) {
            self.canvas.sel = "gameplay".into();
        }
    }

    fn visible_toggled(&mut self, st: &mut AppState, kind: &str, v: bool) {
        let cur = st.ed.layout.as_ref().and_then(|l| l.layer(kind)).map(|l| l.visible);
        if cur.is_none() || cur == Some(v) {
            return;
        }
        let k = kind.to_string();
        Self::step(st, |st| {
            if let Some(l) = st.ed.layout.as_mut().and_then(|l| l.layer_mut(&k)) {
                l.visible = v;
            }
        });
        if v {
            self.canvas.select(kind);
            // a webcam that was never framed: straight to the crop editor
            let unframed = Self::ga(st)
                .zip(st.ed.layout.as_ref().and_then(|l| l.layer(kind)))
                .map(|(ga, x)| x.src == backend::overlay_default_src(kind, &ga))
                .unwrap_or(false);
            if kind == "webcam" && unframed {
                self.start_crop(st, kind);
                self.flash("Drag the box over your webcam, then press the tick");
            }
        } else {
            if self.canvas.sel == kind {
                self.canvas.select("gameplay");
            }
            if self.crop.as_ref().map(|c| c.kind == kind).unwrap_or(false) {
                self.crop = None;
            }
        }
    }

    fn game(&mut self, st: &mut AppState, d: &Do) {
        let (sw, sh) = Self::pan_span(st);
        let Some(g) = st.ed.layout.as_mut().and_then(|l| l.layer_mut("gameplay")) else { return };
        match d {
            Do::Fit(k) => {
                if g.fit == *k {
                    return;
                }
                let k = k.clone();
                Self::step(st, |st| {
                    if let Some(g) = st.ed.layout.as_mut().and_then(|l| l.layer_mut("gameplay")) {
                        g.fit = k;
                    }
                });
                return;
            }
            Do::Zoom(v) => g.zoom = *v as f64 / 100.0,
            Do::Pan(x, v) => {
                let span = if *x { sw } else { sh };
                // slider runs the crop from one edge to the other
                let c = span / 2.0 + (*v as f64 / 100.0) * (1.0 - span);
                g.pan = if *x { (c, g.pan.1) } else { (g.pan.0, c) };
            }
            _ => return,
        }
        Self::save(st);
    }

    fn arrow(&mut self, st: &mut AppState, d: (f64, f64)) {
        if self.view != "arrange" || !Self::ensure_layout(st) {
            return;
        }
        let Some(l) = st.ed.layout.as_mut() else { return };
        match self.canvas.nudge(l, d.0, d.1) {
            Some(true) => Self::save(st),
            Some(false) => {}
            None => {
                if let Some(lyr) = l.layer(&self.canvas.sel) {
                    if !lyr.visible {
                        let name = backend::layer_name(&lyr.kind);
                        self.flash(format!("{name} is hidden, tick it in the layers to move it"));
                    }
                }
            }
        }
    }

    fn smooth_toggled(&mut self, st: &mut AppState, v: bool, ctx: &egui::Context) {
        if !v {
            Self::step(st, |st| st.ed.effects.super_smooth = false);
            return;
        }
        if !backend::smooth_uses_ai(&st.ed.effects) {
            // optical-flow blur runs on the processor: nothing to check
            Self::step(st, |st| st.ed.effects.super_smooth = true);
            self.flash("Super smooth is on for the final export");
            return;
        }
        if let Some((ok, why)) = self.smooth_ok.clone() {
            self.smooth_checked(st, ok, why, true);
            return;
        }
        self.start_smooth_probe(ctx);
    }

    /// Check (once, in the background) that the AI interpolator runs on this PC.
    fn start_smooth_probe(&mut self, ctx: &egui::Context) {
        if self.smooth_ok.is_some() || self.smooth_rx.is_some() {
            return;
        }
        // the check runs the interpolator once (a few seconds); keep the window responsive meanwhile
        let (tx, rx) = channel();
        self.smooth_rx = Some(rx);
        let probe = self.smooth_probe;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = probe();
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }

    /// `want_on` = the toggle is ticked.
    fn smooth_checked(&mut self, st: &mut AppState, ok: bool, why: String, want_on: bool) {
        self.smooth_ok = Some((ok, why));
        if want_on && !st.ed.effects.super_smooth {
            if ok {
                Self::step(st, |st| st.ed.effects.super_smooth = true);
                self.flash("Super smooth is on for the final export");
            } else {
                // AI frames can't run here, the optical-flow blur can
                Self::step(st, |st| {
                    st.ed.effects.super_smooth = true;
                    st.ed.effects.smooth_amount = "faster".into();
                });
                self.flash("Super smooth is on with Faster (no AI frames on this PC)");
            }
        }
    }

    fn apply(&mut self, st: &mut AppState, shell: &mut Shell, d: Do, ctx: &egui::Context) {
        match d {
            Do::Shape(a) => self.shape(st, &a),
            Do::Reset => self.reset(st),
            Do::Background(k) => {
                if st.ed.layout.as_ref().map(|l| l.background == k).unwrap_or(true) {
                    return;
                }
                Self::step(st, |st| {
                    if let Some(l) = st.ed.layout.as_mut() {
                        l.background = k;
                    }
                });
            }
            Do::Visible(k, v) => self.visible_toggled(st, &k, v),
            Do::Select(k) => {
                self.canvas.select(&k);
                // the crop editor follows the selection while it is open
                if self.crop.as_ref().map(|c| c.kind != k).unwrap_or(false) {
                    if backend::layer_is_overlay(&k) {
                        self.start_crop(st, &k);
                    } else {
                        self.crop = None;
                    }
                }
            }
            Do::CropEdit(Some(k)) => self.start_crop(st, &k),
            Do::CropEdit(None) => self.crop = None,
            Do::Crop(k, src) => {
                self.set_crop(st, &k, src);
                self.flash(format!("{} crop changed · Ctrl+Z to undo", backend::layer_name(&k)));
            }
            Do::ResetCrop(k) => {
                if let Some(ga) = Self::ga(st) {
                    self.set_crop(st, &k, backend::overlay_default_src(&k, &ga));
                    if let Some(c) = self.crop.as_mut().filter(|c| c.kind == k) {
                        // re-read on the next frame
                        c.live = (0.0, 0.0, 0.0, 0.0);
                    }
                    self.flash(format!("{} crop is back where it started · Ctrl+Z to undo", backend::layer_name(&k)));
                }
            }
            d @ (Do::Fit(_) | Do::Zoom(_) | Do::Pan(..)) => self.game(st, &d),
            Do::KfKeep(v) => {
                if st.ed.killfeed.keep != v {
                    Self::step(st, |st| st.ed.killfeed.keep = v);
                }
            }
            Do::KfNewest(v) => {
                if st.ed.killfeed.newest != v {
                    Self::step(st, |st| st.ed.killfeed.newest = v);
                }
            }
            Do::KfAnim(v) => {
                if st.ed.killfeed.animation != v {
                    Self::step(st, |st| st.ed.killfeed.animation = v);
                }
            }
            Do::KfSeconds(v) => {
                if st.ed.killfeed.seconds != v as f64 {
                    st.ed.killfeed.seconds = v as f64;
                    st.touch();
                }
            }
            Do::KfRows(v) => {
                if st.ed.killfeed.rows as i64 != v {
                    st.ed.killfeed.rows = v as i32;
                    st.touch();
                }
            }
            Do::PunchIn(v) => {
                if st.ed.effects.zoom != v {
                    Self::step(st, |st| st.ed.effects.zoom = v);
                }
            }
            Do::Flash(v) => {
                if st.ed.effects.flash_on_kill != v {
                    Self::step(st, |st| st.ed.effects.flash_on_kill = v);
                }
            }
            Do::Kzoom(v) => {
                st.ed.effects.zoom_amount = v as f64 / 100.0;
                st.touch();
            }
            Do::PerKill(kid, v) => {
                st.ed.effects.zoom_per_kill.insert(kid, v as f64 / 100.0);
                st.touch();
            }
            Do::Transition(v) => {
                if st.ed.effects.transition != v {
                    Self::step(st, |st| st.ed.effects.transition = v);
                }
            }
            Do::Smooth(v) => self.smooth_toggled(st, v, ctx),
            Do::SmoothAmount(v) => {
                if st.ed.effects.smooth_amount != v {
                    Self::step(st, |st| st.ed.effects.smooth_amount = v);
                    if backend::smooth_uses_ai(&st.ed.effects) {
                        self.start_smooth_probe(ctx);
                    }
                }
            }
            Do::SmoothMethod(v) => {
                if st.ed.effects.smooth_custom.method != v {
                    Self::step(st, |st| {
                        let c = &mut st.ed.effects.smooth_custom;
                        c.method = v;
                        // the AI path makes at most 16 frames per frame
                        c.samples = if c.method == "rife" { c.samples.clamp(2, 16) } else { c.samples.clamp(2, 32) };
                    });
                    if backend::smooth_uses_ai(&st.ed.effects) {
                        self.start_smooth_probe(ctx);
                    }
                }
            }
            Do::SmoothSamples(v) => {
                if st.ed.effects.smooth_custom.samples as i64 != v {
                    st.ed.effects.smooth_custom.samples = v as i32;
                    st.touch();
                }
            }
            Do::SmoothShutter(v) => {
                let v = v as f64 / 100.0;
                if st.ed.effects.smooth_custom.shutter != v {
                    st.ed.effects.smooth_custom.shutter = v;
                    st.touch();
                }
            }
            Do::SmoothDetail(v) => {
                if st.ed.effects.smooth_custom.high_detail != v {
                    Self::step(st, |st| st.ed.effects.smooth_custom.high_detail = v);
                }
            }
            Do::Look(v) => {
                if st.ed.effects.look != v {
                    Self::step(st, |st| st.ed.effects.look = v);
                }
            }
            Do::LookIntensity(v) => {
                let v = v as f64 / 100.0;
                if st.ed.effects.look_intensity != v {
                    st.ed.effects.look_intensity = v;
                    st.touch();
                }
            }
            Do::View(k) => self.set_view(&k),
            Do::Preview => {
                self.pending_sig = Self::edit_sig(st);
                shell.push(Action::Preview);
            }
            Do::Cancel => {
                self.stop_enabled = false;
                self.status = "Cancelling…".into();
                shell.push(Action::CancelJob);
            }
        }
    }

    // ------------------------------------------------------------ preview
    fn edit_sig(st: &AppState) -> String {
        let tr = st.tr.as_ref().and_then(|t| serde_json::to_string(t).ok()).unwrap_or_default();
        serde_json::to_string(&st.ed).unwrap_or_default() + &tr
    }

    pub fn set_view(&mut self, k: &str) {
        self.view = k.to_string();
        if k == "arrange" {
            self.player.pause();
        } else {
            self.crop = None;
        }
    }

    fn update_preview_note(&mut self, st: &AppState) {
        if self.progress.is_some() || self.preview_path.is_empty() {
            return;
        }
        let stale = Self::edit_sig(st) != self.preview_sig;
        self.status =
            if stale { "Preview is out of date · render again to see your changes".into() } else { "Preview ready · 540p".into() };
    }

    fn set_progress(&mut self, p: Option<f64>, msg: &str, st: &AppState) {
        match p {
            None => {
                self.progress = None;
                self.status = msg.to_string();
                if msg.is_empty() {
                    self.update_preview_note(st);
                }
            }
            Some(p) => {
                if self.progress.is_none() {
                    self.stop_enabled = true;
                }
                self.progress = Some(p);
                self.status = if p > 0.0 { format!("{} · {:.0}%", msg.trim_end_matches('…'), p * 100.0) } else { msg.to_string() };
            }
        }
    }

    fn play_preview(&mut self, path: &str, st: &AppState) {
        self.preview_path = path.to_string();
        self.preview_sig = self.pending_sig.clone();
        self.set_view("preview");
        // don't start sound on a page the user has left while the preview rendered
        self.pending_load = Some((path.to_string(), !self.visible));
        self.update_preview_note(st);
    }

    // ------------------------------------------------------------ drawing helpers
    /// A segmented control that records its cell rects and shows a tooltip per option.
    #[allow(clippy::too_many_arguments)]
    fn seg(
        &mut self,
        ui: &mut Ui,
        key: &str,
        opts: &[(&str, &str)],
        cur: &str,
        cols: usize,
        tip: Option<&dyn Fn(&str) -> String>,
    ) -> Option<String> {
        let options: Vec<(String, &str)> = opts.iter().map(|(k, t)| (k.to_string(), *t)).collect();
        let mut sel = cur.to_string();
        let r = ui::segmented(ui, key, &options, &mut sel, cols);
        let cols = if cols == 0 { opts.len().max(1) } else { cols };
        let cell_w = (r.rect.width() - 4.0 - (cols - 1) as f32 * 2.0) / cols as f32;
        for (i, (k, _)) in opts.iter().enumerate() {
            let (row, ci) = (i / cols, i % cols);
            let cell = Rect::from_min_size(
                pos2(r.rect.left() + 2.0 + ci as f32 * (cell_w + 2.0), r.rect.top() + 2.0 + row as f32 * 30.0),
                vec2(cell_w, 28.0),
            );
            self.rects.insert(format!("{key}:{k}"), cell);
            if let Some(tip) = tip {
                let t = tip(k);
                if !t.is_empty() {
                    ui.interact(cell, ui.scope_id().with((key, "tip", i)), Sense::hover()).on_hover_text(t);
                }
            }
        }
        r.changed().then_some(sel)
    }

    /// `name  [slider]  value`: 64 px name, slider, 40 px value; integer values.
    #[allow(clippy::too_many_arguments)]
    fn slider_row(
        &mut self,
        ui: &mut Ui,
        key: &str,
        name: &str,
        v: i64,
        lo: i64,
        hi: i64,
        value: &str,
        enabled: bool,
        tip: &str,
    ) -> Option<i64> {
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
        let on = ui.is_enabled();
        let font = Role::Secondary.font();
        ui.painter().text(
            pos2(row.left(), row.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            font,
            if on { Role::Secondary.color() } else { c::DISABLED },
        );
        ui.painter().text(
            pos2(row.right(), row.center().y),
            egui::Align2::RIGHT_CENTER,
            value,
            Role::Mono.font(),
            if on { Role::Mono.color() } else { c::DISABLED },
        );
        let srect = Rect::from_min_max(pos2(row.left() + 64.0 + S3, row.top()), pos2(row.right() - 40.0 - S3, row.bottom()));
        self.slider(ui, key, srect, v, lo, hi, enabled, tip)
    }

    #[allow(clippy::too_many_arguments)]
    fn slider(&mut self, ui: &mut Ui, key: &str, rect: Rect, v: i64, lo: i64, hi: i64, enabled: bool, tip: &str) -> Option<i64> {
        let mut f = v as f64;
        let mut resp = ui.put(rect, Slider::new(&mut f, lo as f64, hi as f64).step(1.0).width(rect.width()).enabled(enabled));
        self.slider_ids.push(resp.id);
        self.rects.insert(format!("slider:{key}"), rect);
        if resp.has_focus() {
            // Shift + arrows: a page step (10)
            let (shift, up, down) = ui.input(|i| {
                (
                    i.modifiers.shift,
                    i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown),
                )
            });
            if shift && (up || down) {
                f = (f + if up { 9.0 } else { -9.0 }).clamp(lo as f64, hi as f64);
                resp.mark_changed();
            }
        }
        if !tip.is_empty() {
            resp = resp.on_hover_text(tip).on_disabled_hover_text(tip);
        }
        let nv = round_even(f).clamp(lo, hi);
        (resp.changed() && nv != v).then_some(nv)
    }

    fn side(&mut self, ui: &mut Ui, st: &AppState, acts: &mut Vec<Do>) {
        let Some(l) = st.ed.layout.clone() else {
            side_panel(ui, 328.0, |p| {
                p.section(None, |ui| {
                    ui::label(ui, "Layout", Role::Title);
                    ui::hint(ui, "This clip has no game area yet. Analyse it first");
                });
            });
            return;
        };
        let ed = &st.ed;
        let sel = self.canvas.sel.clone();
        let ga = Self::ga(st);
        let kills = if st.ev.is_some() { Self::kills_in_cut(st) } else { vec![] };
        let names: HashMap<String, String> =
            st.ev.as_ref().map(|e| e.players.iter().map(|p| (p.id.clone(), p.display.clone())).collect()).unwrap_or_default();
        let smooth_note = self.smooth_note(st);
        let checking = self.smooth_checking();
        let hint = if self.crop.as_ref().map(|c| c.kind == sel).unwrap_or(false) {
            "Drag the box over what this layer should show, corners resize it. The layer keeps its width on the canvas and its height follows the crop. Press the tick or Esc when done".to_string()
        } else {
            Self::layer_hint(st, &sel)
        };
        let pan = Self::pan_slider(st);
        let pan_note = Self::pan_note(st);
        let (sw, sh) = Self::pan_span(st);
        side_panel(ui, 328.0, |p| {
            // 1 - shape
            p.section(None, |ui| {
                ui::label(ui, "Layout", Role::Title);
                ui::label(ui, "1  Pick the shape", Role::Section);
                let tip = |k: &str| format!("{}. Switching places the layers for this shape, Ctrl+Z goes back", shape_note(k));
                if let Some(k) = self.seg(ui, "shape", &SHAPES, &l.aspect, 0, Some(&tip)) {
                    acts.push(Do::Shape(k));
                }
                ui::hint(ui, shape_note(&l.aspect));
            });
            // 2 - layers
            p.section(Some("2  Arrange the layers"), |ui| {
                self.layer_list(ui, &l, ga.as_ref(), acts);
                ui::hint(ui, &hint);
                let r = ui.add(
                    Button::ghost("Reset this layout")
                        .min_width(ui.available_width())
                        .tooltip("Put every layer back where it starts for this shape"),
                );
                self.rects.insert("reset".into(), r.rect);
                if r.clicked() {
                    acts.push(Do::Reset);
                }
            });
            // gameplay controls
            let g = l.layer("gameplay").cloned().unwrap_or_else(|| vl_core::models::LayerSpec::new("gameplay", (0.0, 0.0, 1.0, 1.0)));
            p.section(Some("Gameplay"), |ui| {
                let tip = |k: &str| {
                    if k == "cover" {
                        "Fill the gameplay box and crop what doesn't fit".to_string()
                    } else {
                        "Squeeze the whole game into the gameplay box".to_string()
                    }
                };
                if let Some(k) = self.seg(ui, "fit", &[("cover", "Fill and crop"), ("stretch", "Stretch")], &g.fit, 0, Some(&tip)) {
                    acts.push(Do::Fit(k));
                }
                if let Some(v) = self.slider_row(ui, "zoom", "Zoom", round_even(g.zoom * 100.0), 100, 250, &times(g.zoom), true, "") {
                    acts.push(Do::Zoom(v));
                }
                if let Some(v) = self.slider_row(ui, "panx", "Left right", pan.0, 0, 100, "", sw < 0.999, "") {
                    acts.push(Do::Pan(true, v));
                }
                if let Some(v) = self.slider_row(ui, "pany", "Up down", pan.1, 0, 100, "", sh < 0.999, "") {
                    acts.push(Do::Pan(false, v));
                }
                if !pan_note.is_empty() {
                    ui::hint(ui, pan_note);
                }
            });
            p.section(Some("Behind the layers"), |ui| {
                if let Some(k) = self.seg(ui, "bg", &[("blur", "Blurred game"), ("black", "Black")], &l.background, 0, None) {
                    acts.push(Do::Background(k));
                }
            });
            // kill feed behaviour
            let kf = &ed.killfeed;
            let kf_on = l.layer("killfeed").map(|x| x.visible).unwrap_or(false);
            p.section(Some("Kill feed"), |ui| {
                if !kf_on {
                    ui::hint(ui, "The kill feed layer is hidden. Tick Kill feed in the layers to show your kills");
                }
                ui.add_enabled_ui(kf_on, |ui| {
                    ui::label(ui, "Keep each kill on screen for", Role::Muted);
                    if let Some(k) =
                        self.seg(ui, "keep", &[("timed", "Seconds"), ("round", "Round"), ("clip", "Whole clip")], &kf.keep, 0, None)
                    {
                        acts.push(Do::KfKeep(k));
                    }
                    if kf.keep == "timed" {
                        if let Some(v) = self.slider_row(
                            ui,
                            "secs",
                            "Seconds",
                            round_even(kf.seconds),
                            2,
                            12,
                            &format!("{} s", fmt_g(kf.seconds)),
                            true,
                            "",
                        ) {
                            acts.push(Do::KfSeconds(v));
                        }
                    }
                    ui::hint(ui, keep_note(&kf.keep));
                    if let Some(k) =
                        self.seg(ui, "newest", &[("bottom", "Newest at the bottom"), ("top", "Newest on top")], &kf.newest, 0, None)
                    {
                        acts.push(Do::KfNewest(k));
                    }
                    if let Some(v) = self.slider_row(ui, "rows", "Rows", kf.rows as i64, 1, 6, &kf.rows.to_string(), true, "") {
                        acts.push(Do::KfRows(v));
                    }
                    ui::label(ui, "How kills appear", Role::Muted);
                    if let Some(k) = self.seg(
                        ui,
                        "anim",
                        &[("slide", "Slide"), ("fade", "Fade"), ("pop", "Pop"), ("none", "None")],
                        &kf.animation,
                        0,
                        None,
                    ) {
                        acts.push(Do::KfAnim(k));
                    }
                });
            });
            // effects
            let fx = &ed.effects;
            p.section(Some("Kill effects"), |ui| {
                let mut z = fx.zoom;
                let r = ui.add(Toggle::new(&mut z).text("Punch in on every kill"));
                self.rects.insert("punch".into(), r.rect);
                if r.changed() {
                    acts.push(Do::PunchIn(z));
                }
                ui.add_enabled_ui(fx.zoom, |ui| {
                    if let Some(v) = self.slider_row(
                        ui,
                        "kzoom",
                        "Amount",
                        round_even(fx.zoom_amount * 100.0),
                        101,
                        130,
                        &times(fx.zoom_amount),
                        true,
                        "",
                    ) {
                        acts.push(Do::Kzoom(v));
                    }
                    ui::label(
                        ui,
                        if kills.is_empty() { "No kills of yours in the cut yet" } else { "Fine-tune single kills, ×1.00 skips one" },
                        Role::Muted,
                    );
                    if !kills.is_empty() {
                        let h = (kills.len().min(6) as f32 * 32.0 + 4.0).max(32.0);
                        egui::ScrollArea::vertical()
                            .id_salt("per-kill")
                            .max_height(h)
                            .min_scrolled_height(h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (k, t_out) in &kills {
                                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                                    let on = ui.is_enabled();
                                    let inner = row.shrink2(vec2(S2, 0.0));
                                    let ts = fmt_time(*t_out);
                                    let ts = &ts[..ts.len().saturating_sub(3)];
                                    let tr = Rect::from_min_size(pos2(inner.left(), row.top()), vec2(40.0, 32.0));
                                    ui.painter().text(
                                        pos2(tr.left(), tr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        ts,
                                        Role::Mono.font(),
                                        if on { c::MUTED } else { c::DISABLED },
                                    );
                                    ui.interact(tr, ui.scope_id().with(("pk-t", &k.id)), Sense::hover())
                                        .on_hover_text("Time in the edited video");
                                    let custom = fx.zoom_per_kill.get(&k.id).copied();
                                    let v = custom.unwrap_or(fx.zoom_amount);
                                    let vr = Rect::from_min_max(pos2(inner.right() - 40.0, row.top()), pos2(inner.right(), row.bottom()));
                                    let sr = Rect::from_min_max(
                                        pos2(vr.left() - S2 - 96.0, row.center().y - 10.0),
                                        pos2(vr.left() - S2, row.center().y + 10.0),
                                    );
                                    let nr = Rect::from_min_max(pos2(tr.right() + S2, row.top()), pos2(sr.left() - S2, row.bottom()));
                                    let name = names.get(&k.victim).cloned().unwrap_or_else(|| "?".into());
                                    let g = ui::elided(ui, &name, Role::Body.font(), if on { c::TEXT } else { c::DISABLED }, nr.width());
                                    ui.painter().galley(pos2(nr.left(), nr.center().y - g.size().y / 2.0), g, c::TEXT);
                                    let txt = if v <= 1.0 { "off".to_string() } else { times(v) };
                                    ui.painter().text(
                                        pos2(vr.right(), vr.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        txt,
                                        Role::Mono.font(),
                                        if on { c::MUTED } else { c::DISABLED },
                                    );
                                    ui.interact(vr, ui.scope_id().with(("pk-v", &k.id)), Sense::hover())
                                        .on_hover_text(if custom.is_some() { "Set for this kill" } else { "Follows the amount above" });
                                    if let Some(nv) =
                                        self.slider(ui, &format!("pk:{}", k.id), sr, round_even(v * 100.0), 100, 130, true, "")
                                    {
                                        acts.push(Do::PerKill(k.id.clone(), nv));
                                    }
                                }
                            });
                    }
                });
                let mut fl = fx.flash_on_kill;
                if ui.add(Toggle::new(&mut fl).text("Flash on kills")).changed() {
                    acts.push(Do::Flash(fl));
                }
            });
            p.section(Some("Between cuts"), |ui| {
                ui.add_enabled_ui(st.ed.parts.len() > 1, |ui| {
                    if let Some(k) = self.seg(
                        ui,
                        "tr",
                        &[("none", "None"), ("fade", "Fade"), ("flash", "Flash"), ("blur", "Blur")],
                        &fx.transition,
                        0,
                        None,
                    ) {
                        acts.push(Do::Transition(k));
                    }
                });
                ui::hint(ui, Self::tr_note(st));
            });
            p.section(Some("Super smooth"), |ui| {
                let mut on = checking || fx.super_smooth;
                let r = ui.add(Toggle::new(&mut on).text("Enable super smooth").enabled(!checking));
                self.rects.insert("smooth".into(), r.rect);
                if r.changed() {
                    acts.push(Do::Smooth(on));
                }
                if !smooth_note.is_empty() {
                    ui::label_wrap(ui, &smooth_note, Role::Secondary);
                }
                ui.add_enabled_ui(fx.super_smooth && !checking, |ui| {
                    if let Some(k) = self.seg(ui, "smooth_amount", &backend::SMOOTH_PRESETS, &fx.smooth_amount, 0, Some(&smooth_tip)) {
                        acts.push(Do::SmoothAmount(k));
                    }
                    if fx.smooth_amount == "custom" {
                        let c = &fx.smooth_custom;
                        let flow = c.method != "rife";
                        if let Some(k) = self.seg(ui, "smooth_method", &[("flow", "Optical flow"), ("rife", "AI frames")], if flow { "flow" } else { "rife" }, 0, None) {
                            acts.push(Do::SmoothMethod(k));
                        }
                        let (lo, hi) = if flow { (2, 32) } else { (2, 16) };
                        let n = (c.samples as i64).clamp(lo, hi);
                        let tip = if flow {
                            "Most blur samples per pixel: more makes smoother streaks in fast flicks and takes longer"
                        } else {
                            "In-between frames made per frame: more makes smoother blur and takes longer"
                        };
                        if let Some(v) = self.slider_row(ui, "smooth_samples", "Samples", n, lo, hi, &n.to_string(), true, tip) {
                            acts.push(Do::SmoothSamples(v));
                        }
                        let sh = round_even(c.shutter * 100.0).clamp(10, 100);
                        if let Some(v) = self.slider_row(ui, "smooth_shutter", "Blur", sh, 10, 100, &format!("{sh}%"), true, "How much of each frame's movement is blurred: 50% looks like a film camera, 100% smears the most") {
                            acts.push(Do::SmoothShutter(v));
                        }
                        if flow {
                            let mut hd = c.high_detail;
                            let r = ui.add(Toggle::new(&mut hd).text("High detail")).on_hover_text("Measures motion at twice the resolution and blends both neighbouring frames: cleaner edges around the gun and players, about 3 times slower");
                            self.rects.insert("smooth_detail".into(), r.rect);
                            if r.changed() {
                                acts.push(Do::SmoothDetail(hd));
                            }
                        }
                    }
                });
                ui::hint(ui, SMOOTH_TEXT);
            });
            p.section(Some("Colour look"), |ui| {
                let opts: Vec<(&str, &str)> =
                    backend::LOOKS.iter().map(|(k, v)| (*k, if *v == "Black and white" { "B&W" } else { *v })).collect();
                if let Some(k) = self.seg(ui, "look", &opts, &fx.look, 3, None) {
                    acts.push(Do::Look(k));
                }
                let on = fx.look != "none";
                let val = if on { format!("{:.0}%", fx.look_intensity * 100.0) } else { String::new() };
                if let Some(v) = self.slider_row(
                    ui,
                    "look",
                    "Strength",
                    round_even(fx.look_intensity * 100.0),
                    0,
                    100,
                    &val,
                    on,
                    if on { "" } else { "Pick a look to set its strength" },
                ) {
                    acts.push(Do::LookIntensity(v));
                }
            });
        });
    }

    fn layer_list(&mut self, ui: &mut Ui, l: &LayoutSpec, ga: Option<&GameArea>, acts: &mut Vec<Do>) {
        let kinds: Vec<&str> = backend::LAYER_ORDER.iter().rev().copied().filter(|k| l.layer(k).is_some()).collect();
        let w = ui.available_width();
        let (list, _) = ui.allocate_exact_size(vec2(w, kinds.len() as f32 * 38.0 + 4.0), Sense::hover());
        for (i, kind) in kinds.iter().enumerate() {
            let Some(lyr) = l.layer(kind) else { continue };
            let row = Rect::from_min_size(pos2(list.left() + 1.0, list.top() + 2.0 + i as f32 * 38.0 + 1.0), vec2(w - 2.0, 36.0));
            let resp = ui.interact(row, ui.scope_id().with(("layer-row", *kind)), Sense::click());
            self.rects.insert(format!("layer:{kind}"), row);
            let selected = *kind == self.canvas.sel;
            let bg = if selected {
                c::RAISED
            } else if resp.hovered() {
                c::CARD
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(row, 8.0, bg);
            let tr = Rect::from_min_size(pos2(row.left() + S2, row.center().y - 12.0), vec2(ui::SWITCH_W + 6.0, 24.0));
            let mut on = lyr.visible;
            let t = ui.put(tr, Toggle::new(&mut on).enabled(*kind != "gameplay"));
            self.rects.insert(format!("toggle:{kind}"), tr);
            let tip = if *kind == "gameplay" { "Gameplay is always shown" } else { "Show this layer" };
            let t = t.on_hover_text(tip).on_disabled_hover_text(tip);
            if t.changed() {
                acts.push(Do::Visible(kind.to_string(), on));
            }
            let nx = tr.right() + S3;
            let (font, col) = if lyr.visible { (Role::Body.font(), c::TEXT) } else { (Role::Muted.font(), c::MUTED) };
            ui.painter().text(pos2(nx, row.center().y), egui::Align2::LEFT_CENTER, backend::layer_name(kind), font, col);
            // live crops: adjust / reset the crop right on the row
            let mut right = row.right() - S1;
            if backend::layer_is_overlay(kind) {
                let cropping = self.crop.as_ref().map(|c| c.kind == *kind).unwrap_or(false);
                let custom = ga.map(|ga| lyr.src != backend::overlay_default_src(kind, ga)).unwrap_or(false);
                let what = if *kind == "webcam" {
                    "Reset crop: put the box back where it started"
                } else {
                    "Reset crop: back to the detected HUD position"
                };
                let rr = Rect::from_min_size(pos2(right - 32.0, row.center().y - 16.0), vec2(32.0, 32.0));
                let r = ui.put(rr, IconButton::new("rotate-ccw", what).size(15.0).enabled(custom));
                let r = if custom { r } else { r.on_hover_text("Reset crop: the crop is where it started") };
                self.rects.insert(format!("crop_reset:{kind}"), rr);
                if r.clicked() {
                    acts.push(Do::ResetCrop(kind.to_string()));
                }
                let cr = Rect::from_min_size(pos2(rr.left() - 32.0, rr.top()), vec2(32.0, 32.0));
                let tip =
                    if cropping { "Done adjusting the crop (Esc)" } else { "Adjust crop: choose which part of the video this layer shows" };
                let mut ib = IconButton::new(if cropping { "check" } else { "crop" }, tip).size(15.0).checked(cropping);
                if cropping {
                    ib = ib.color(c::ACCENT);
                }
                let r = ui.put(cr, ib);
                self.rects.insert(format!("crop:{kind}"), cr);
                if r.clicked() {
                    acts.push(Do::CropEdit(if cropping { None } else { Some(kind.to_string()) }));
                }
                right = cr.left() - S1;
            }
            if !lyr.visible {
                ui.painter().text(pos2(right - S2, row.center().y), egui::Align2::RIGHT_CENTER, "Hidden", Role::Muted.font(), c::MUTED);
            }
            if resp.clicked() {
                acts.push(Do::Select(kind.to_string()));
            }
        }
    }

    fn centre(&mut self, ui: &mut Ui, rect: Rect, cx: &mut PageCtx, acts: &mut Vec<Do>) {
        let inner = Rect::from_min_max(pos2(rect.left() + S6, rect.top() + S4 + 4.0), pos2(rect.right() - S6, rect.bottom() - S4));
        // top row: view switch · status · progress · Render preview / Cancel
        let top = Rect::from_min_size(inner.min, vec2(inner.width(), 32.0));
        let mut tu = ui.new_child(egui::UiBuilder::new().max_rect(top).layout(egui::Layout::left_to_right(egui::Align::Center)));
        tu.allocate_ui(vec2(200.0, 32.0), |ui| {
            ui.set_width(200.0);
            if let Some(k) = self.seg(ui, "view", &[("arrange", "Arrange"), ("preview", "Preview video")], &self.view.clone(), 0, None) {
                acts.push(Do::View(k));
            }
        });
        tu.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = S3;
            if self.progress.is_some() {
                let r = ui.add(Button::ghost("Cancel").icon("x").enabled(self.stop_enabled).tooltip("Stop rendering the preview"));
                self.rects.insert("cancel".into(), r.rect);
                if r.clicked() {
                    acts.push(Do::Cancel);
                }
                ui::progress_bar(ui, self.progress.unwrap_or(0.0) as f32, 160.0);
            } else {
                let r = ui
                    .add(Button::primary("Render preview").icon("film").tooltip("Render a quick 540p video of the edit with this layout"));
                self.rects.insert("render".into(), r.rect);
                if r.clicked() {
                    acts.push(Do::Preview);
                }
            }
            if !self.status.is_empty() {
                ui::label(ui, &self.status, Role::Muted)
                    .on_hover_text("Previews are quick 540p renders. Super smooth is only applied in the final export");
            }
        });
        // bottom row: tips · message
        let tips_h = 18.0;
        let bottom = Rect::from_min_max(pos2(inner.left(), inner.bottom() - tips_h), inner.max);
        let msg_w = if self.msg.is_empty() { 0.0 } else { ui::text_width(ui, &self.msg, &Role::Secondary.font()) };
        if !self.msg.is_empty() {
            ui::paint_elided_right(ui, bottom, &self.msg, Role::Secondary);
        }
        if self.view == "arrange" {
            let tr = Rect::from_min_max(bottom.min, pos2(bottom.right() - msg_w - S3, bottom.bottom()));
            let tips = if self.crop.is_some() { CROP_TIPS } else { TIPS };
            let g = ui::elided(ui, tips, Role::Muted.font(), c::MUTED, tr.width());
            ui.painter().galley(pos2(tr.left(), tr.center().y - g.size().y / 2.0), g, c::MUTED);
        }
        // stack
        let stack = Rect::from_min_max(pos2(inner.left(), top.bottom() + S3), pos2(inner.right(), bottom.top() - S3));
        self.rects.insert("stack".into(), stack);
        if self.view == "arrange" && self.crop.is_some() {
            let (ga, fp) = (Self::ga(cx.st), Self::frame_px(cx.st));
            let tex = self.canvas.still_texture();
            let Some(ed) = self.crop.as_mut() else { return };
            let kind = ed.kind.clone();
            // follow the stored crop (undo, reset) unless the box is being dragged
            if !ed.dragging() {
                if let (Some(ga), Some(src)) = (ga, cx.st.ed.layout.as_ref().and_then(|l| l.layer(&kind)).map(|x| x.src)) {
                    ed.live = Self::crop_to_frame(src, &ga, fp);
                }
            }
            let was_dragging = ed.dragging();
            let out = ed.show(ui, stack, tex, backend::layer_name(&kind));
            self.rects.insert("crop_box".into(), ed.to_screen(ed.live));
            if let (Some(r), Some(ga)) = (out.released, ga) {
                acts.push(Do::Crop(kind, Self::crop_from_frame(r, &ga, fp)));
            }
            // Esc with no drag running: back to arranging
            if !was_dragging && ui.input(|i| i.key_pressed(Key::Escape)) && ctx_modal_free(ui) {
                acts.push(Do::CropEdit(None));
            }
        } else if self.view == "arrange" {
            let ga = Self::ga(cx.st);
            let out = self.canvas.show(ui, stack, cx.st.ed.layout.as_mut(), ga.as_ref());
            if out.edit_started {
                cx.st.begin_step();
            }
            if let Some(k) = out.selected {
                acts.push(Do::Select(k));
            }
            if out.changed {
                Self::save(cx.st);
            }
        } else if !self.preview_path.is_empty() {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(stack).layout(egui::Layout::top_down(egui::Align::Min)));
            self.player.show(&mut child, cx.frame);
        } else {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(stack).layout(egui::Layout::top_down(egui::Align::Center)));
            child.add_space((stack.height() / 2.0 - 20.0).max(0.0));
            ui::label(&mut child, "No preview yet", Role::Secondary);
            ui::label(&mut child, "Render preview makes a quick 540p video of the edit with this layout", Role::Muted);
        }
    }
}

impl Page for LayoutPage {
    fn on_state(&mut self, ev: &StateEvent, st: &mut AppState, _shell: &mut Shell) {
        match ev {
            StateEvent::ProjectChanged => self.project_changed(st),
            StateEvent::EventsChanged => self.rows_dirty = true,
            StateEvent::EditsChanged | StateEvent::TranscriptChanged => self.update_preview_note(st),
            StateEvent::Restored(_) => {
                // the layout was replaced under any drag
                self.after_layout_replaced(st);
                self.rows_dirty = true;
                self.update_preview_note(st);
            }
            _ => {}
        }
    }

    fn on_job(&mut self, n: &JobNotice, st: &mut AppState, _shell: &mut Shell) {
        if n.kind != "proxy" {
            return;
        }
        self.set_progress(n.progress, &n.msg, st);
        if n.progress.is_none() {
            if let Some(out) = n.result.as_ref().and_then(|r| r.get("out")).and_then(|v| v.as_str()) {
                self.play_preview(out, st);
            }
        }
    }

    fn on_show(&mut self, st: &mut AppState, _shell: &mut Shell) {
        self.visible = true;
        if st.h.is_some() {
            self.rows_dirty = true;
            Self::ensure_layout(st);
        }
        self.update_preview_note(st);
    }

    fn on_hide(&mut self, st: &mut AppState, _shell: &mut Shell) {
        self.visible = false;
        if let Some(l) = st.ed.layout.as_mut() {
            self.canvas.cancel_drag(l);
        }
        self.player.pause();
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx) {
        self.visible = true;
        let ctx = ui.ctx().clone();
        if cx.st.h.is_some() {
            Self::ensure_layout(cx.st);
        }
        // background work: the still frame, kill feed rows, the interpolator check
        if self.want_still {
            self.load_still(cx.st, &ctx);
        }
        if let Some(rx) = &self.still_rx {
            if let Ok((dir, img)) = rx.try_recv() {
                self.still_rx = None;
                if cx.st.h.as_ref().map(|h| h.dir == dir).unwrap_or(false) {
                    if let Some((full, small)) = img {
                        self.canvas.set_still(&ctx, Some((&full, &small)));
                    }
                }
            }
        }
        if self.rows_dirty {
            self.load_rows(cx.st, &ctx);
        }
        if let Some(rx) = &self.smooth_rx {
            if let Ok((ok, why)) = rx.try_recv() {
                self.smooth_rx = None;
                // the toggle stayed ticked while checking
                self.smooth_checked(cx.st, ok, why, true);
            }
        }
        if let Some((path, pause)) = self.pending_load.take() {
            self.player.load(&path, Load { start: 0.0, pause, ..Default::default() });
        }
        if let Some(t) = self.msg_until {
            let now = Instant::now();
            if now >= t {
                self.msg.clear();
                self.msg_until = None;
            } else {
                ctx.request_repaint_after(t - now);
            }
        }
        // the canvas follows the edits
        self.canvas.feed_rows = cx.st.ed.killfeed.rows;
        self.canvas.feed_newest = cx.st.ed.killfeed.newest.clone();
        self.canvas.sub_color = hex_color(&cx.st.ed.subtitles.style.color);
        self.canvas.sub_hl = hex_color(&cx.st.ed.highlight_color);
        self.canvas.sub_max_words = cx.st.ed.subtitles.max_words.max(0) as usize;

        self.slider_ids.clear();
        let mut acts = vec![];
        let full = ui.max_rect();
        let left = Rect::from_min_max(full.min, pos2(full.left() + 328.0, full.bottom()));
        {
            let mut su = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::top_down(egui::Align::Min)));
            self.side(&mut su, cx.st, &mut acts);
        }
        ui.painter().vline(left.right() - 0.5, left.y_range(), egui::Stroke::new(1.0, c::HAIR));
        let right = Rect::from_min_max(pos2(left.right(), full.top()), full.max);
        self.centre(ui, right, cx, &mut acts);

        // window-wide arrow keys while this page shows: nudge the selected layer (a focused slider keeps them)
        let modal_free = ctx.memory(|m| m.top_modal_layer().is_none());
        if modal_free && !ctx.text_edit_focused() {
            let (d, big) = ui.input(|i| {
                let big = i.modifiers.shift;
                let s = if big { NUDGE_BIG } else { NUDGE };
                let d = if i.key_pressed(Key::ArrowLeft) {
                    Some((-s, 0.0))
                } else if i.key_pressed(Key::ArrowRight) {
                    Some((s, 0.0))
                } else if i.key_pressed(Key::ArrowUp) {
                    Some((0.0, -s))
                } else if i.key_pressed(Key::ArrowDown) {
                    Some((0.0, s))
                } else {
                    None
                };
                (d, big)
            });
            let _ = big;
            if let Some(d) = d {
                // arrows never move egui's keyboard focus on this page
                ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                let focused = ctx.memory(|m| m.focused());
                if !focused.map(|f| self.slider_ids.contains(&f)).unwrap_or(false) && self.crop.is_none() {
                    self.arrow(cx.st, d);
                }
            }
        }
        for a in acts {
            self.apply(cx.st, cx.shell, a, &ctx);
        }
    }

    fn stop_players(&mut self) {
        self.player.stop();
    }

    fn shutdown(&mut self) {
        self.player.shutdown();
    }
}
