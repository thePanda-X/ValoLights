//! Embedded libmpv video player.
//!
//! mpv renders through its OpenGL render API into our own FBO + texture, which egui shows as an image
//! (`frame.register_native_glow_texture`). mpv's update callback wakes egui, so the UI repaints exactly when
//! there is a new video frame (or a property changed) and stays idle otherwise.
//!
//! ```ignore
//! let mut player = Player::new(&env.gl, "trim");                 // in the page constructor
//! player.load(&edl_url(src, &parts), Load { start: 0.0, pause: true, ..Default::default() });
//! player.show(ui, frame);                                          // every frame the page is visible
//! for e in player.take_events() { match e { PlayerEvent::Position(t) => .., PlayerEvent::Loaded(d) => .. } }
//! ```
#![allow(dead_code)]

use super::mpv::{self, Mpv};
use crate::icons;
use crate::theme::{c, Role};
use crate::widgets::ui::{self, IconButton, Slider};
use egui::{pos2, vec2, Rect, Sense, Ui};
use glow::HasContext;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// GL access for players: the glow context and the loader eframe gives at creation.
#[derive(Clone)]
pub struct GlEnv {
    pub gl: Arc<glow::Context>,
    pub get_proc_address: Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>,
    pub egui: egui::Context,
}

/// mpv EDL playlist of source ranges (length-prefixed path escaping).
pub fn edl_url(src: &str, parts: &[(f64, f64)]) -> String {
    let p = src.replace('\\', "/");
    let seg = format!("%{}%{}", p.len(), p);
    let items: Vec<String> = parts.iter().map(|(a, b)| format!("{seg},{a:.3},{:.3}", (b - a).max(0.05))).collect();
    format!("edl://{}", items.join(";"))
}

/// "mm:ss.ss"
pub fn fmt_time(t: f64) -> String {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    format!("{:02}:{:05.2}", (t / 60.0).floor() as i64, t % 60.0)
}

/// Options of [`Player::load`].
#[derive(Clone, Debug)]
pub struct Load {
    pub start: f64,
    pub subs: Option<String>,
    pub pause: bool,
    /// a single-input lavfi graph applied to the video (the export layout preview)
    pub compose: Option<String>,
    pub has_audio: bool,
    /// an audio file already on the edit timeline, mixed under the game audio the way the export mixes it
    pub music: Option<String>,
    pub game_volume: f64,
    pub duck: bool,
}

impl Default for Load {
    fn default() -> Self {
        Load { start: 0.0, subs: None, pause: true, compose: None, has_audio: true, music: None, game_volume: 1.0, duck: false }
    }
}

/// The lavfi-complex graph for `load`. Empty = no graph.
pub fn lavfi_graph(compose: Option<&str>, has_audio: bool, music: Option<&str>, game_volume: f64, duck: bool) -> String {
    let mut graph = String::new();
    if compose.is_some() || music.is_some() {
        graph = format!("[vid1]{}[vo]", compose.unwrap_or("null"));
        if music.is_some() {
            let mid = if has_audio { "aid2" } else { "aid1" };
            if !has_audio {
                graph += &format!(";[{mid}]anull[ao]");
            } else if duck {
                graph += &format!(
                    ";[aid1]volume={game_volume:.2},asplit=2[ga][gs];[{mid}][gs]sidechaincompress=threshold=0.08:ratio=5:attack=20:release=300[md];[ga][md]amix=inputs=2:normalize=0:duration=first[ao]"
                );
            } else {
                graph += &format!(";[aid1]volume={game_volume:.2}[ga];[ga][{mid}]amix=inputs=2:normalize=0:duration=first[ao]");
            }
        } else if has_audio {
            graph += ";[aid1]anull[ao]";
        }
    }
    graph
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// playback position (player timeline)
    Position(f64),
    /// duration of what was loaded, once known
    Loaded(f64),
}

struct Notify {
    flag: AtomicBool,
    egui: egui::Context,
}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    let n = unsafe { &*(ctx as *const Notify) };
    n.flag.store(true, Ordering::SeqCst);
    n.egui.request_repaint();
}

unsafe extern "C" fn get_proc(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let f = unsafe { &*(ctx as *const GlEnv) };
    let name = unsafe { CStr::from_ptr(name) };
    (f.get_proc_address)(name) as *mut c_void
}

struct Gpu {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    id: egui::TextureId,
    size: [i32; 2],
}

struct Inner {
    mpv: Mpv,
    rctx: *mut mpv::mpv_render_context,
    frame_ready: Arc<Notify>,
    wakeup: Arc<Notify>,
    env: Box<GlEnv>,
}

pub struct Player {
    name: String,
    env: Option<GlEnv>,
    inner: Option<Inner>,
    gpu: Option<Gpu>,
    pos: f64,
    dur: f64,
    pending_seek: Option<f64>,
    subs: Option<String>,
    subs_pending: bool,
    fresh: bool,
    playing: bool,
    volume: f64,
    info: String,
    events: Vec<PlayerEvent>,
    failed: bool,
    pub frames_rendered: u64,
    // mpv state, pushed by property observers (no blocking reads per frame)
    m_dur: Option<f64>,
    m_pos: Option<f64>,
    m_paused: bool,
    m_eof: bool,
    // seeks: never blocking; while a seek is running only the newest target waits, and fast scrubbing
    // shows keyframes, finished by one exact seek when the playhead rests
    seek_busy: bool,
    seek_next: Option<f64>,
    seek_exact_due: Option<f64>,
    last_seek: Option<std::time::Instant>,
}

const OBS_DUR: u64 = 1;
const OBS_POS: u64 = 2;
const OBS_PAUSE: u64 = 3;
const OBS_EOF: u64 = 4;
const REPLY_SEEK: u64 = 100;
/// seeks closer together than this count as scrubbing (keyframe seeks + one exact seek at rest)
const SCRUB_GAP: std::time::Duration = std::time::Duration::from_millis(180);

impl Player {
    /// `env` = None (no OpenGL): the player shows "Video player unavailable".
    pub fn new(env: Option<&GlEnv>, name: &str) -> Self {
        Player {
            name: name.into(),
            env: env.cloned(),
            inner: None,
            gpu: None,
            pos: 0.0,
            dur: 0.0,
            pending_seek: None,
            subs: None,
            subs_pending: false,
            fresh: false,
            playing: false,
            volume: 80.0,
            info: String::new(),
            events: vec![],
            failed: false,
            frames_rendered: 0,
            m_dur: None,
            m_pos: None,
            m_paused: true,
            m_eof: false,
            seek_busy: false,
            seek_next: None,
            seek_exact_due: None,
            last_seek: None,
        }
    }

    // ------------------------------------------------------------------ mpv
    fn ensure(&mut self) -> bool {
        if self.inner.is_some() {
            return true;
        }
        if self.failed {
            return false;
        }
        match self.create() {
            Ok(i) => {
                self.inner = Some(i);
                self.set("volume", &format!("{}", self.volume as i64));
                true
            }
            Err(e) => {
                log::error!("mpv init failed: {e:?}");
                self.info = format!("Video player unavailable: {e}");
                self.failed = true;
                false
            }
        }
    }

    fn create(&mut self) -> anyhow::Result<Inner> {
        let env = self.env.clone().ok_or_else(|| anyhow::anyhow!("no OpenGL context"))?;
        // No Lua at all: mpv's built-in scripts (ytdl hook, console, stats, auto profiles...) run LuaJIT, whose
        // error unwinding on Windows can take the whole app down while files load. The app needs none of them.
        let mut opts: Vec<(&str, &str)> = vec![
            ("vo", "libmpv"),
            ("hwdec", "auto-safe"),
            ("keep-open", "yes"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("idle", "yes"),
            ("sub-auto", "no"),
            ("terminal", "no"),
        ];
        for k in [
            "ytdl",
            "load-scripts",
            "load-stats-overlay",
            "load-console",
            "load-select",
            "load-auto-profiles",
            "load-commands",
            "load-positioning",
            "load-context-menu",
            "load-osd-console",
            "config",
        ] {
            opts.push((k, "no"));
        }
        let m = Mpv::new(&opts)?;
        let env = Box::new(env);
        let frame_ready = Arc::new(Notify { flag: AtomicBool::new(true), egui: env.egui.clone() });
        let wakeup = Arc::new(Notify { flag: AtomicBool::new(true), egui: env.egui.clone() });
        let mut init =
            mpv::mpv_opengl_init_params { get_proc_address: Some(get_proc), get_proc_address_ctx: &*env as *const GlEnv as *mut c_void };
        let api_type = c"opengl";
        let mut params = [
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_API_TYPE, data: api_type.as_ptr() as *mut c_void },
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        let mut rctx: *mut mpv::mpv_render_context = std::ptr::null_mut();
        let r = unsafe { (m.api.render_context_create)(&mut rctx, m.h, params.as_mut_ptr()) };
        if r < 0 || rctx.is_null() {
            let s = unsafe { CStr::from_ptr((m.api.error_string)(r)) }.to_string_lossy().into_owned();
            anyhow::bail!("mpv render context: {s}");
        }
        unsafe {
            (m.api.render_context_set_update_callback)(rctx, Some(on_update), Arc::as_ptr(&frame_ready) as *mut c_void);
            (m.api.set_wakeup_callback)(m.h, Some(on_update), Arc::as_ptr(&wakeup) as *mut c_void);
        }
        m.observe(OBS_DUR, "duration", false);
        m.observe(OBS_POS, "time-pos", false);
        m.observe(OBS_PAUSE, "pause", true);
        m.observe(OBS_EOF, "eof-reached", true);
        Ok(Inner { mpv: m, rctx, frame_ready, wakeup, env })
    }

    fn set(&self, prop: &str, v: &str) {
        if let Some(i) = &self.inner {
            if let Err(e) = i.mpv.set_property(prop, v) {
                log::debug!("mpv set {prop}={v}: {e}");
            }
        }
    }

    fn cmd(&self, args: &[&str]) -> anyhow::Result<()> {
        match &self.inner {
            Some(i) => i.mpv.command(args),
            None => anyhow::bail!("no player"),
        }
    }

    /// Load a file / EDL url. See [`Load`].
    pub fn load(&mut self, url: &str, o: Load) {
        if !self.ensure() {
            return;
        }
        let graph = lavfi_graph(o.compose.as_deref(), o.has_audio, o.music.as_deref(), o.game_volume, o.duck);
        // a composed preview runs its filters on the CPU: decode on the GPU and copy the frames back
        // (plain nvdec can't feed CPU filters, so mpv fell back to software decoding)
        self.set("hwdec", if o.compose.is_some() { "auto-copy-safe" } else { "auto-safe" });
        self.seek_busy = false;
        self.seek_next = None;
        self.seek_exact_due = None;
        self.m_dur = None;
        let res = (|| -> anyhow::Result<()> {
            self.set_list("audio-files", o.music.as_deref())?;
            self.inner.as_ref().unwrap().mpv.set_property("lavfi-complex", &graph)?;
            self.subs = o.subs.clone();
            self.dur = 0.0;
            // start position and subtitles travel with the file, so they apply to this load and never
            // to the file being replaced (a pending seek could land on the old file while it unloads)
            let ok = self.inner.as_ref().unwrap().mpv.set_property("start", &format!("{:.3}", o.start.max(0.0))).is_ok()
                && self.set_list("sub-files", o.subs.as_deref()).is_ok();
            if ok {
                self.pending_seek = None;
                self.subs_pending = false;
            } else {
                self.pending_seek = Some(o.start);
                self.subs_pending = true;
            }
            self.cmd(&["loadfile", url, "replace"])?;
            self.set("pause", if o.pause { "yes" } else { "no" });
            self.pos = o.start.max(0.0);
            self.fresh = true;
            Ok(())
        })();
        if let Err(e) = res {
            self.info = e.to_string();
        }
    }

    /// Set a file-list option to one file or none (plain-string change-list commands).
    fn set_list(&self, name: &str, item: Option<&str>) -> anyhow::Result<()> {
        self.cmd(&["change-list", name, "clr", ""])?;
        if let Some(it) = item {
            self.cmd(&["change-list", name, "append", it])?;
        }
        Ok(())
    }

    pub fn set_subs(&mut self, path: Option<&str>) {
        self.subs = path.map(str::to_string);
        if self.inner.is_none() {
            return;
        }
        let _ = self.set_list("sub-files", path); // also for the next load of this file
        let _ = self.cmd(&["sub-remove"]);
        if let Some(p) = path {
            if let Err(e) = self.cmd(&["sub-add", p, "select"]) {
                log::warn!("sub-add failed: {e}");
            }
        }
    }

    /// Save the current frame as displayed (composed video + subtitles).
    pub fn snapshot(&self, path: &str) -> bool {
        self.cmd(&["screenshot-to-file", path, "subtitles"]).is_ok()
    }

    pub fn seek(&mut self, t: f64) {
        if self.inner.is_none() {
            return;
        }
        let t = t.max(0.0);
        let now = std::time::Instant::now();
        // fast, far jumps (dragging the playhead) show keyframes until the playhead rests; small steps (frame
        // stepping with held arrow keys) stay exact, mpv decodes those forward from where it is
        let scrubbing = self.last_seek.map(|l| now.duration_since(l) < SCRUB_GAP).unwrap_or(false) && (t - self.pos).abs() > 1.0;
        self.last_seek = Some(now);
        if self.seek_busy {
            self.seek_next = Some(t); // only the newest target matters
            self.seek_exact_due = Some(t);
            return;
        }
        self.send_seek(t, !scrubbing);
    }

    fn send_seek(&mut self, t: f64, exact: bool) {
        let Some(i) = &self.inner else { return };
        let mode = if exact { "absolute+exact" } else { "absolute+keyframes" };
        match i.mpv.command_async(REPLY_SEEK, &["seek", &format!("{t:.3}"), mode]) {
            Ok(()) => {
                self.seek_busy = true;
                self.seek_exact_due = if exact { None } else { Some(t) };
            }
            Err(_) => self.pending_seek = Some(t),
        }
    }

    /// Seek bookkeeping after mpv finished one (or when the playhead came to rest).
    fn pump_seeks(&mut self) {
        if self.seek_busy {
            return;
        }
        let resting = self.last_seek.map(|l| l.elapsed() >= SCRUB_GAP).unwrap_or(true);
        if let Some(t) = self.seek_next.take() {
            self.send_seek(t, resting);
        } else if resting {
            if let Some(t) = self.seek_exact_due.take() {
                self.send_seek(t, true);
            }
        }
    }

    fn paused(&self) -> bool {
        self.m_paused
    }

    pub fn toggle(&mut self) {
        if self.inner.is_none() {
            return;
        }
        let paused = self.paused();
        if paused && self.at_end() {
            self.seek(0.0); // play again from the start instead of doing nothing at the last frame
        }
        self.set("pause", if paused { "no" } else { "yes" });
    }

    fn at_end(&self) -> bool {
        if self.m_eof {
            return true;
        }
        self.dur > 0.0 && self.pos >= self.dur - 0.05
    }

    /// Length of what is loaded (0 until known).
    pub fn duration(&self) -> f64 {
        self.dur
    }

    pub fn pause(&mut self) {
        self.set("pause", "yes");
    }

    pub fn play(&mut self) {
        self.set("pause", "no");
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn stop(&mut self) {
        self.seek_busy = false;
        self.seek_next = None;
        self.seek_exact_due = None;
        let _ = self.cmd(&["stop"]);
        self.pos = 0.0;
        self.dur = 0.0;
    }

    pub fn pos(&self) -> f64 {
        self.pos
    }

    pub fn volume(&self) -> f64 {
        self.volume
    }

    pub fn set_volume(&mut self, v: f64) {
        self.volume = v.clamp(0.0, 100.0);
        self.set("volume", &format!("{}", self.volume.round() as i64));
    }

    /// Diagnostics (perf mode): an mpv property as text.
    pub fn prop(&self, name: &str) -> Option<String> {
        self.inner.as_ref().and_then(|i| i.mpv.get_string(name))
    }

    /// Error / info text shown in the controls bar.
    pub fn info(&self) -> &str {
        &self.info
    }

    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Read mpv's state. Called from `show`; cheap.
    fn poll(&mut self) {
        let Some(i) = &self.inner else { return };
        let egui_ctx = i.env.egui.clone();
        if i.wakeup.flag.swap(false, Ordering::SeqCst) {
            for e in i.mpv.drain_events() {
                match e {
                    mpv::Ev::Prop(OBS_DUR, v) => self.m_dur = v,
                    mpv::Ev::Prop(OBS_POS, v) => self.m_pos = v,
                    mpv::Ev::Prop(OBS_PAUSE, v) => self.m_paused = v.map(|v| v != 0.0).unwrap_or(true),
                    mpv::Ev::Prop(OBS_EOF, v) => self.m_eof = v.map(|v| v != 0.0).unwrap_or(false),
                    mpv::Ev::Reply(REPLY_SEEK, _) => self.seek_busy = false,
                    _ => {}
                }
            }
        }
        self.pump_seeks();
        if self.seek_exact_due.is_some() || self.seek_next.is_some() {
            egui_ctx.request_repaint_after(SCRUB_GAP); // finish the scrub with an exact frame
        }
        let d = self.m_dur;
        let p = self.m_pos;
        let paused = self.m_paused;
        if let Some(d) = d {
            if d > 0.0 && (d - self.dur).abs() > 1e-3 {
                self.dur = d;
                self.events.push(PlayerEvent::Loaded(d));
                if let Some(t) = self.pending_seek.take() {
                    self.seek(t);
                }
                if self.subs_pending {
                    self.subs_pending = false;
                    if let Some(s) = self.subs.clone() {
                        self.set_subs(Some(&s));
                    }
                }
            }
        }
        self.playing = !paused;
        if let Some(p) = p {
            if self.fresh || (p - self.pos).abs() > 1e-4 {
                self.fresh = false;
                self.pos = p;
                self.events.push(PlayerEvent::Position(p));
            }
        }
        if self.playing {
            // position updates even when no new frame arrives (audio-only stretches)
            egui_ctx.request_repaint_after(std::time::Duration::from_millis(40));
        }
    }

    /// Render the newest frame into our texture when mpv has one (or the size changed).
    fn render(&mut self, frame: &mut eframe::Frame, size_px: [i32; 2]) {
        let Some(i) = &self.inner else { return };
        let gl = i.env.gl.clone();
        let mut force = false;
        unsafe {
            if self.gpu.as_ref().map(|g| g.size != size_px).unwrap_or(true) {
                force = true;
                match &mut self.gpu {
                    Some(g) => {
                        gl.bind_texture(glow::TEXTURE_2D, Some(g.tex));
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            glow::RGBA8 as i32,
                            size_px[0],
                            size_px[1],
                            0,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(None),
                        );
                        gl.bind_texture(glow::TEXTURE_2D, None);
                        g.size = size_px;
                    }
                    None => {
                        let (Ok(tex), Ok(fbo)) = (gl.create_texture(), gl.create_framebuffer()) else { return };
                        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            glow::RGBA8 as i32,
                            size_px[0],
                            size_px[1],
                            0,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(None),
                        );
                        gl.bind_texture(glow::TEXTURE_2D, None);
                        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
                        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                        let id = frame.register_native_glow_texture(tex);
                        self.gpu = Some(Gpu { tex, fbo, id, size: size_px });
                    }
                }
            }
        }
        // mpv's own FRAME bit decides. Requiring our callback flag as well raced: render_context_update could
        // report (and consume) the frame before the callback set the flag, so a paused player kept its old
        // picture after a reload or a subtitle change.
        i.frame_ready.flag.store(false, Ordering::SeqCst);
        let flags = unsafe { (i.mpv.api.render_context_update)(i.rctx) };
        let new_frame = (flags & mpv::MPV_RENDER_UPDATE_FRAME) != 0;
        if !(new_frame || force) {
            return;
        }
        let g = self.gpu.as_ref().unwrap();
        let mut fbo = mpv::mpv_opengl_fbo { fbo: g.fbo.0.get() as i32, w: size_px[0], h: size_px[1], internal_format: 0 };
        let mut flip: i32 = 0;
        // never wait for the frame's display time inside the UI frame: egui already syncs to the display,
        // and blocking here made every UI frame cost a whole refresh while a video was playing
        let mut block: i32 = 0;
        let mut params = [
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_OPENGL_FBO, data: &mut fbo as *mut _ as *mut c_void },
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_FLIP_Y, data: &mut flip as *mut i32 as *mut c_void },
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME, data: &mut block as *mut i32 as *mut c_void },
            mpv::mpv_render_param { type_: mpv::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        unsafe {
            (i.mpv.api.render_context_render)(i.rctx, params.as_mut_ptr());
            (i.mpv.api.render_context_report_swap)(i.rctx);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        self.frames_rendered += 1;
    }

    /// The video surface and the controls bar, filling the available rect.
    pub fn show(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) -> egui::Response {
        let avail = ui.available_rect_before_wrap();
        let bar_h = 32.0;
        let video = Rect::from_min_max(avail.min, pos2(avail.max.x, (avail.max.y - bar_h - 12.0).max(avail.min.y + 180.0)));
        let resp = ui.allocate_rect(video, Sense::click());
        self.poll();
        ui.painter().rect_filled(video, 12.0, c::VIDEO_BG);
        if self.inner.is_some() && self.dur > 0.0 {
            let ppp = ui.ctx().pixels_per_point();
            let size_px = [(video.width() * ppp).round().max(16.0) as i32, (video.height() * ppp).round().max(16.0) as i32];
            self.render(frame, size_px);
            if let Some(g) = &self.gpu {
                // mpv renders top row first into our FBO (egui samples it the same way)
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(g.id, video, uv, egui::Color32::WHITE);
            }
        }
        if resp.clicked() {
            self.toggle();
        }
        ui.add_space(12.0);
        self.controls(ui);
        resp
    }

    fn controls(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            ui.spacing_mut().item_spacing.x = 8.0;
            let icon = if self.playing { "pause" } else { "play" };
            if ui.add(IconButton::new(icon, "Play / pause (Space)").size(18.0)).clicked() {
                self.toggle();
            }
            ui.add_space(4.0);
            ui::label(ui, fmt_time(self.pos), Role::Timecode);
            if self.dur > 0.0 {
                ui::label(ui, format!("/ {}", fmt_time(self.dur)), Role::Mono);
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut v = self.volume;
                if ui.add(Slider::new(&mut v, 0.0, 100.0).step(5.0).width(96.0)).changed() {
                    self.set_volume(v);
                }
                icons::show(ui, "volume-2", c::MUTED, 16.0);
                let w = ui.available_width();
                if !self.info.is_empty() {
                    let r = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover()).0;
                    let g = ui::elided(ui, &self.info, Role::Muted.font(), c::MUTED, r.width());
                    ui.painter().galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g, c::MUTED);
                }
            });
        });
    }

    /// Free mpv (call with the GL context current: from a page's ui or `on_exit`).
    pub fn shutdown(&mut self) {
        if let Some(i) = self.inner.take() {
            unsafe {
                (i.mpv.api.render_context_set_update_callback)(i.rctx, None, std::ptr::null_mut());
                (i.mpv.api.set_wakeup_callback)(i.mpv.h, None, std::ptr::null_mut());
                (i.mpv.api.render_context_free)(i.rctx);
            }
            if let Some(g) = self.gpu.take() {
                unsafe {
                    i.env.gl.delete_framebuffer(g.fbo);
                }
            }
            drop(i);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edl_and_time() {
        let u = edl_url(r"C:\v\a b.mp4", &[(1.0, 2.5), (3.0, 3.01)]);
        assert_eq!(u, "edl://%12%C:/v/a b.mp4,1.000,1.500;%12%C:/v/a b.mp4,3.000,0.050");
        assert_eq!(fmt_time(75.5), "01:15.50");
        assert_eq!(fmt_time(-3.0), "00:00.00");
    }

    #[test]
    fn graphs_match_reference() {
        assert_eq!(lavfi_graph(None, true, None, 1.0, false), "");
        assert_eq!(lavfi_graph(Some("scale=640:-2"), true, None, 1.0, false), "[vid1]scale=640:-2[vo];[aid1]anull[ao]");
        assert_eq!(lavfi_graph(None, false, Some("m.wav"), 1.0, false), "[vid1]null[vo];[aid1]anull[ao]");
        assert_eq!(
            lavfi_graph(None, true, Some("m.wav"), 0.8, false),
            "[vid1]null[vo];[aid1]volume=0.80[ga];[ga][aid2]amix=inputs=2:normalize=0:duration=first[ao]"
        );
        assert!(
            lavfi_graph(None, true, Some("m.wav"), 1.0, true).contains("sidechaincompress=threshold=0.08:ratio=5:attack=20:release=300")
        );
    }
}
