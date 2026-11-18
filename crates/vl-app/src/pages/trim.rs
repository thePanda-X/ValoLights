//! Trim: the core timeline editor.
//!
//! Side panel: who you play as, stats, quick cuts, highlights, the kill list (keep/drop toggles, portraits,
//! context menu, add a missed kill) and the audio controls. Centre: "Edit preview" (only the kept parts, with
//! the music mixed in) or "Full video", the toolbar (split / delete / add part / zoom), the timeline and the
//! music lane. Keyboard shortcuts work anywhere on the page while it is shown.
use super::{Action, JobNotice, Page, PageCtx, Shell};
use crate::backend::{self, EditList, SongInfo, QUICK_CUTS};
use crate::state::{AppState, StateEvent};
use crate::theme::{self, c, Role, Weight, S1, S2, S3, S4, S6};
use crate::widgets::music_lane::{self, LaneEvent, MusicLane};
use crate::widgets::player::{edl_url, fmt_time, Load, Player, PlayerEvent};
use crate::widgets::timeline::{self, Strip, TLKill, TLRound, Timeline, TlEvent};
use crate::widgets::ui::{self, Button, ChoiceRow, IconButton, Slider, Toggle};
use egui::epaint::RectShape;
use egui::{pos2, vec2, Align2, Color32, Id, Key, Modifiers, Rect, Sense, TextureHandle, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vl_core::models::{Kill, MusicSettings, Part, Player as EvPlayer};

pub const THUMBS: usize = 160;
/// jumping to a kill lands this long before it, so you see it happen
pub const KILL_LEAD: f64 = 1.5;
/// a split leaves at least this much on each side
pub const MIN_SPLIT: f64 = 0.2;
/// "Add part" keeps this long either side of the playhead
pub const ADD_HALF: f64 = 2.0;
const SIDE_W: f32 = 328.0;
const MSG_TIME: Duration = Duration::from_secs(4);
const MUSIC_DEBOUNCE: Duration = Duration::from_millis(350);
const HINT: &str = "Space play · ←/→ one frame · Shift+←/→ 1 s · S split · Del delete part · A add part · Drag the pink handles to trim · Shift-drag on the film strip to add a part";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// only the kept parts, back to back
    Edit,
    /// the whole recording
    Source,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Edit => "edit",
            Mode::Source => "source",
        }
    }
}

/// Results of the page's background work.
enum Bg {
    Thumbs { path: PathBuf, strip: Option<(Vec<egui::ColorImage>, usize, usize, [usize; 2])> },
    Song { key: String, res: Result<SongInfo, String> },
    Wav { gen: u64, url: String, start: f64, subs: Option<String>, playing: bool, res: Result<Option<PathBuf>, String> },
}

/// One row of the kill list.
#[derive(Clone, Debug)]
pub struct KillRow {
    pub id: String,
    pub t: f64,
    pub victim: String,
    pub portrait: Option<String>,
    pub headshot: bool,
    pub manual: bool,
}

/// A highlight row: (start, end, title, meta, sub).
#[derive(Clone, Debug)]
pub struct SegRow {
    pub start: f64,
    pub end: f64,
    pub title: String,
    pub meta: String,
    pub sub: String,
}

/// The "Add a missing kill" question.
struct AddKill {
    t: f64,
    text: String,
    focus: bool,
}

/// Figures shown in the side panel, refreshed when the edits / events change.
#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub me: String,
    pub kills_kept: usize,
    pub headshots: usize,
    pub length: f64,
    pub quick: Vec<(String, String, f64)>,
    pub custom: bool,
    pub segs: Vec<SegRow>,
}

pub struct TrimPage {
    pub player: Player,
    pub timeline: Timeline,
    pub lane: MusicLane,
    pub mode: Mode,
    /// the preview timeline (no transitions) while in Edit preview
    pub el: Option<EditList>,
    /// the export timeline (with transitions), for the music lane
    pub el_out: Option<EditList>,
    pub song: Option<Arc<SongInfo>>,
    song_path: String,
    music_gen: u64,
    music_sig: Option<(String, String)>,
    music_due: Option<Instant>,
    /// the music lane is shown (a song is chosen and on)
    pub music_visible: bool,
    /// Smart cut's sound pass is running: (progress, message)
    pub smart_job: Option<(f64, String)>,
    /// placement label above the lane
    pub mplace: String,
    pix_cache: HashMap<String, Option<TextureHandle>>,
    /// last position asked for (arrow keys step from here) and when
    seek_target: Option<(f64, Instant)>,
    /// edge being dragged on the timeline
    trim_t: Option<f64>,
    /// what the preview subtitles were built from
    pub subs_sig: Option<String>,
    /// feedback next to the hints; `msg_until` None = sticky
    pub msg: String,
    msg_until: Option<Instant>,
    pending_say: Vec<(String, bool)>,
    pub kills: Vec<KillRow>,
    /// the kill selected in the list
    pub cur_kill: Option<String>,
    scroll_to_kill: bool,
    kill_menu: Option<(String, egui::Pos2)>,
    add_kill: Option<AddKill>,
    pub stats: Stats,
    /// "Edit 00:03.50   Source 00:35.17" and the round pill
    pub src_t_label: String,
    pub round_label: String,
    pub player_note: String,
    pub shown: bool,
    loaded_pid: Option<String>,
    tx: Sender<Bg>,
    rx: Receiver<Bg>,
    egui_ctx: Option<egui::Context>,
    /// tests: the file "Choose" picks instead of opening the file dialog
    pub pick_override: Option<String>,
    /// tests: the answer to "Who did you kill" instead of asking
    pub victim_override: Option<String>,
    /// rects drawn last frame (tests drive the mouse with them)
    pub rects: HashMap<&'static str, Rect>,
    pub kill_rects: Vec<(String, Rect, Rect)>,
    pub quick_rects: Vec<(String, Rect)>,
    /// tests: the music file the last edit preview load mixed in, and the handle positions seen while trimming
    pub loaded_music: Option<String>,
    pub trimming_log: Vec<f64>,
}

fn ft1(t: f64) -> String {
    let s = fmt_time(t);
    s[..s.len() - 1].to_string()
}

fn names(st: &AppState) -> HashMap<String, String> {
    st.ev.as_ref().map(|e| e.players.iter().map(|p| (p.id.clone(), p.display.clone())).collect()).unwrap_or_default()
}

fn spans(st: &AppState) -> Vec<(f64, f64)> {
    st.ed.parts.iter().map(|p| (p.src_in, p.src_out)).collect()
}

fn to_parts(spans: &[(f64, f64)]) -> Vec<Part> {
    spans.iter().map(|(a, b)| Part::new(*a, *b)).collect()
}

fn music_sig(st: &AppState) -> (String, String) {
    (serde_json::to_string(&st.ed.music).unwrap_or_default(), st.ed.effects.transition.clone())
}

fn quick_name(key: &str) -> String {
    QUICK_CUTS.iter().find(|(k, _)| *k == key).map(|(_, n)| n.to_string()).unwrap_or_else(|| key.to_string())
}

/// Decode the ffmpeg film strip and cut it into textures no wider than 8192 px.
fn load_strip(path: &Path, n: usize) -> Option<(Vec<egui::ColorImage>, usize, usize, [usize; 2])> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let cell_w = w / n.max(1);
    if cell_w == 0 || h == 0 {
        return None;
    }
    let per = (8192 / cell_w).max(1);
    let mut chunks = vec![];
    let mut c0 = 0;
    while c0 < n {
        let k = per.min(n - c0);
        let sub = image::imageops::crop_imm(&img, (c0 * cell_w) as u32, 0, (k * cell_w) as u32, h as u32).to_image();
        chunks.push(egui::ColorImage::from_rgba_unmultiplied([k * cell_w, h], sub.as_raw()));
        c0 += k;
    }
    Some((chunks, per, n, [cell_w, h]))
}

impl TrimPage {
    pub fn new(shell: &Shell) -> Self {
        let (tx, rx) = channel();
        TrimPage {
            player: Player::new(shell.gl.as_ref(), "trim"),
            timeline: Timeline::new(),
            lane: MusicLane::new(),
            smart_job: None,
            mode: Mode::Edit,
            el: None,
            el_out: None,
            song: None,
            song_path: String::new(),
            music_gen: 0,
            music_sig: None,
            music_due: None,
            music_visible: false,
            mplace: String::new(),
            pix_cache: HashMap::new(),
            seek_target: None,
            trim_t: None,
            subs_sig: None,
            msg: String::new(),
            msg_until: None,
            pending_say: vec![],
            kills: vec![],
            cur_kill: None,
            scroll_to_kill: false,
            kill_menu: None,
            add_kill: None,
            stats: Stats::default(),
            src_t_label: String::new(),
            round_label: String::new(),
            player_note: String::new(),
            shown: false,
            loaded_pid: None,
            tx,
            rx,
            egui_ctx: shell.gl.as_ref().map(|g| g.egui.clone()),
            pick_override: None,
            victim_override: None,
            rects: HashMap::new(),
            kill_rects: vec![],
            quick_rects: vec![],
            loaded_music: None,
            trimming_log: vec![],
        }
    }

    fn repaint(&self) {
        if let Some(c) = &self.egui_ctx {
            c.request_repaint();
        }
    }

    /// Feedback next to the hints; sticky messages describe a state and stay until something changes.
    pub fn say(&mut self, msg: impl Into<String>, sticky: bool) {
        self.msg = msg.into();
        self.msg_until = if sticky { None } else { Some(Instant::now() + MSG_TIME) };
        self.repaint();
    }

    /// Say after the state notifications this action causes.
    fn say_later(&mut self, msg: impl Into<String>) {
        self.pending_say.push((msg.into(), false));
        self.repaint();
    }

    fn frame_dur(st: &AppState) -> f64 {
        let fps = st.h.as_ref().map(|h| h.project.source.fps).unwrap_or(0.0);
        if fps > 1.0 {
            1.0 / fps
        } else {
            1.0 / 30.0
        }
    }

    /// Where previews and the film strip are written: the project's cache (a temp folder in read-only mode).
    fn cache_dir(st: &AppState) -> Option<PathBuf> {
        let h = st.h.as_ref()?;
        Some(if st.read_only { vl_core::paths::tmp_dir().join("trim").join(&h.project.id) } else { h.dir.join("cache") })
    }

    // ------------------------------------------------------------ project
    pub fn load_project(&mut self, st: &mut AppState) {
        self.pix_cache.clear();
        self.timeline.reset();
        self.seek_target = None;
        self.subs_sig = None;
        self.msg.clear();
        self.pending_say.clear();
        self.cur_kill = None;
        self.kill_menu = None;
        self.add_kill = None;
        self.mode = Mode::Edit;
        self.loaded_pid = st.pid().map(str::to_string);
        let Some(h) = st.h.clone() else {
            self.player.stop();
            self.el = None;
            return;
        };
        self.timeline.thumbs = None;
        self.timeline.strip_note = "Building the film strip…".into();
        let own = h.dir.join("cache").join(format!("thumbs_{THUMBS}.png"));
        let out = if own.exists() {
            own
        } else {
            Self::cache_dir(st).unwrap_or_else(|| h.dir.join("cache")).join(format!("thumbs_{THUMBS}.png"))
        };
        self.make_thumbs(h.source().to_string(), h.project.source.duration, out);
        self.load_events(st);
        self.reload_player(st, false);
    }

    fn make_thumbs(&self, src: String, duration: f64, out: PathBuf) {
        let tx = self.tx.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let made = (|| -> anyhow::Result<()> {
                if !out.exists() {
                    if let Some(d) = out.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    let exe = vl_core::ffmpeg::ffmpeg_exe()?;
                    let vf = format!("fps={:.6},scale=-2:68,tile={THUMBS}x1", THUMBS as f64 / duration.max(1.0));
                    let mut args: Vec<String> = vec![exe, "-y".into(), "-v".into(), "error".into()];
                    args.extend(vl_core::ffmpeg::hw_decode_args().iter().map(|s| s.to_string()));
                    let o = out.to_string_lossy().into_owned();
                    // keyframes only: a film strip needs one picture every few seconds, not every frame decoded
                    // (25x faster on a 12-minute clip; each tile shows the nearest keyframe)
                    args.extend(["-skip_frame".into(), "nokey".into()]);
                    args.extend(["-i".into(), src, "-an".into(), "-vf".into(), vf, "-frames:v".into(), "1".into(), o]);
                    vl_core::ffmpeg::run(&args)?;
                }
                Ok(())
            })();
            let strip = if made.is_ok() { load_strip(&out, THUMBS) } else { None };
            if let Err(e) = made {
                log::warn!("film strip: {e}");
            }
            let _ = tx.send(Bg::Thumbs { path: out, strip });
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    pub fn load_events(&mut self, st: &mut AppState) {
        if st.ev.is_none() || st.h.is_none() {
            return;
        }
        let rounds: Vec<TLRound> = st
            .ev
            .as_ref()
            .map(|ev| {
                ev.rounds
                    .iter()
                    .enumerate()
                    .map(|(i, r)| TLRound {
                        start: r.start,
                        end: r.end,
                        label: format!("R{}", r.score_before.map(|(a, b)| a + b + 1).unwrap_or(i as i32 + 1)),
                        result: r.result.clone(),
                        planted: r.planted,
                        decided: r.decided,
                        badges: r
                            .banners
                            .iter()
                            .filter(|b| !b.contains(':'))
                            .map(|b| {
                                let l = b.to_lowercase();
                                let mut c = l.chars();
                                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                            })
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ammo = st.ev.as_ref().map(|e| e.ammo.clone()).unwrap_or_default();
        let kills = self.tl_kills(st);
        self.timeline.set_data(st.duration(), kills, rounds, ammo);
        self.fill_kill_list(st);
        self.edits_changed(st);
    }

    fn tl_kills(&self, st: &AppState) -> Vec<TLKill> {
        let names = names(st);
        let dis = &st.ed.kills_disabled;
        st.my_kills(true)
            .into_iter()
            .map(|k| TLKill {
                id: k.id.clone(),
                t: k.t,
                label: names.get(&k.victim).cloned().unwrap_or_else(|| "?".into()),
                headshot: k.headshot,
                enabled: !dis.contains(&k.id),
                pix: k.victim_portrait.as_ref().and_then(|p| self.pix_cache.get(p).cloned().flatten()),
                pix_path: k.victim_portrait.clone(),
                mine: true,
            })
            .collect()
    }

    fn fill_kill_list(&mut self, st: &AppState) {
        let names = names(st);
        self.kills = st
            .my_kills(true)
            .into_iter()
            .map(|k| KillRow {
                victim: names.get(&k.victim).cloned().unwrap_or_else(|| "?".into()),
                id: k.id,
                t: k.t,
                portrait: k.victim_portrait,
                headshot: k.headshot,
                manual: k.manual,
            })
            .collect();
        if let Some(cur) = &self.cur_kill {
            if !self.kills.iter().any(|k| &k.id == cur) {
                self.cur_kill = None;
            }
        }
    }

    fn refresh_stats(&mut self, st: &AppState) {
        let names = names(st);
        let me = st.me();
        let mine = st.my_kills(false);
        let kept: Vec<&Kill> = mine.iter().filter(|k| st.ed.parts.iter().any(|p| p.src_in <= k.t && k.t <= p.src_out)).collect();
        let rounds = st.ev.as_ref().map(|e| e.rounds.clone()).unwrap_or_default();
        let mut segs = st.segments();
        segs.sort_by(|a, b| b.score.total_cmp(&a.score));
        let smart_ready = st.smart_ready();
        let custom = st.ev.is_some() && st.cut_is_custom();
        // the Smart cut lists its own moments, with why each one is in
        let smart_rows: Option<Vec<SegRow>> = (st.ed.quick_cut == "smart" && smart_ready && !custom).then(|| {
            let mut ms = st.smart_moments();
            ms.sort_by(|a, b| b.score.total_cmp(&a.score));
            ms.iter()
                .map(|m| {
                    let ft = fmt_time(m.start);
                    SegRow {
                        start: m.start,
                        end: m.end,
                        title: m.reasons.join(" · "),
                        meta: ft[..ft.len() - 3].to_string(),
                        sub: format!("{:.0} s · score {:.1}", m.end - m.start, m.score),
                    }
                })
                .collect()
        });
        self.stats = Stats {
            me: if me.is_empty() {
                "Not set".into()
            } else {
                me.iter().map(|m| names.get(m).cloned().unwrap_or_else(|| "?".into())).collect::<Vec<_>>().join(" / ")
            },
            kills_kept: kept.len(),
            headshots: kept.iter().filter(|k| k.headshot).count(),
            length: st.kept_duration(),
            quick: QUICK_CUTS
                .iter()
                // the Smart cut has no length until it has listened to the clip
                .map(|(k, n)| (k.to_string(), n.to_string(), if *k == "smart" && !smart_ready { -1.0 } else { st.quick_cut_duration(k) }))
                .collect(),
            custom,
            segs: if let Some(rows) = smart_rows {
                rows
            } else {
                segs.iter()
                    .map(|s| {
                        let rn = if s.round >= 0 && (s.round as usize) < rounds.len() {
                            match rounds[s.round as usize].score_before {
                                Some((a, b)) => format!("Round {} · ", a + b + 1),
                                None => format!("Round {} · ", s.round + 1),
                            }
                        } else {
                            String::new()
                        };
                        let ft = fmt_time(s.start);
                        SegRow {
                            start: s.start,
                            end: s.end,
                            title: s.label.clone(),
                            meta: ft[..ft.len() - 3].to_string(),
                            sub: format!("{rn}score {:.1}", s.score),
                        }
                    })
                    .collect()
            },
        };
    }

    // ------------------------------------------------------------ edits
    pub fn edits_changed(&mut self, st: &mut AppState) {
        if st.h.is_none() || self.loaded_pid.as_deref() != st.pid() {
            return; // the project is being opened: load_project follows
        }
        self.timeline.set_parts(&spans(st));
        self.refresh_stats(st);
        self.music_refresh(st);
        // the kill marks follow the kept/dropped state (undo restores it too)
        for k in self.timeline.kills.iter_mut() {
            k.enabled = !st.ed.kills_disabled.contains(&k.id);
        }
        let new_el = EditList::new(&st.ed.parts, "none");
        let same = |a: &EditList| a.placed.iter().map(|p| (p.src_in, p.src_out)).eq(new_el.placed.iter().map(|p| (p.src_in, p.src_out)));
        if self.mode == Mode::Edit && !self.el.as_ref().map(same).unwrap_or(false) {
            self.reload_player(st, true);
        } else if self.mode == Mode::Edit {
            let sig = music_sig(st);
            if self.music_sig.is_some() && Some(&sig) != self.music_sig.as_ref() {
                self.music_due = Some(Instant::now() + MUSIC_DEBOUNCE); // hear the new placement / volume
            }
            self.music_sig = Some(sig);
        }
        if st.ed.parts.is_empty() && st.ev.is_some() {
            self.say("Nothing is kept yet · choose a quick cut, or Shift-drag on the film strip to add a part", true);
        } else if self.msg.starts_with("Nothing is kept") && self.msg_until.is_none() {
            self.msg.clear();
        }
        self.repaint();
    }

    /// Edit time for a source moment; when it was cut out, the start of the next kept part.
    fn out_near(el: &EditList, src: f64) -> f64 {
        if let Some(o) = el.to_output(src) {
            return o;
        }
        el.placed.iter().find(|pl| pl.src_in >= src).map(|pl| pl.out_start).unwrap_or((el.duration - 0.05).max(0.0))
    }

    pub fn reload_player(&mut self, st: &mut AppState, keep_source_pos: bool) {
        let Some(h) = st.h.clone() else { return };
        let src_pos = keep_source_pos.then_some(self.timeline.playhead);
        if !h.source_ok() {
            self.player.stop();
            self.player_note = "Source video not found. Right-click the clip in Clips to locate it".into();
            return;
        }
        self.player_note.clear();
        let subs = self.preview_subs(st);
        let has_audio = h.project.source.has_audio;
        if self.mode == Mode::Edit && !st.ed.parts.is_empty() {
            let el = EditList::new(&st.ed.parts, "none");
            let url = edl_url(h.source(), &spans(st));
            let start = src_pos.map(|s| Self::out_near(&el, s)).unwrap_or(0.0);
            self.el = Some(el);
            self.load_edit(st, &url, start, subs);
        } else {
            self.el = None;
            self.player.load(h.source(), Load { start: src_pos.unwrap_or(0.0), subs, has_audio, ..Default::default() });
        }
    }

    fn preview_subs(&mut self, st: &AppState) -> Option<String> {
        self.subs_sig = Some(self.subs_key(st));
        let h = st.h.as_ref()?;
        let tr = st.tr.as_ref()?;
        if !st.ed.subtitles.enabled {
            return None;
        }
        let el = if self.mode == Mode::Edit && !st.ed.parts.is_empty() {
            EditList::new(&st.ed.parts, "none")
        } else {
            EditList::new(&[Part::new(0.0, st.duration())], "none")
        };
        let (w, hh) = h.project.game_area.as_ref().map(|g| (g.w as i64, g.h as i64)).unwrap_or((1920, 1080));
        let out = Self::cache_dir(st)?.join(format!("preview_{}.ass", self.mode.name()));
        match backend::build_ass(tr, &st.ed.subtitles, &el, w, hh, &st.ed.highlight_color, &out, None) {
            Ok(p) => Some(p.to_string_lossy().into_owned()),
            Err(e) => {
                log::warn!("preview subtitles: {e}");
                None
            }
        }
    }

    pub fn subs_key(&self, st: &AppState) -> String {
        format!(
            "{}|{:?}|{}|{}|{}",
            self.mode.name(),
            spans(st),
            serde_json::to_string(&st.ed.subtitles).unwrap_or_default(),
            st.ed.highlight_color,
            st.tr.as_ref().and_then(|t| serde_json::to_string(t).ok()).unwrap_or_default()
        )
    }

    pub fn refresh_subs(&mut self, st: &AppState) {
        let s = self.preview_subs(st);
        self.player.set_subs(s.as_deref());
    }

    pub fn set_mode(&mut self, mode: Mode, st: &mut AppState) {
        if mode == self.mode {
            return; // clicking the chosen mode again must not reload the video
        }
        self.mode = mode;
        self.reload_player(st, true);
    }

    // ------------------------------------------------------------ player <-> timeline
    pub fn on_player_pos(&mut self, t: f64, st: &AppState) {
        if self.music_visible {
            let s = self.song_at(t, st);
            self.lane.set_playhead(s);
        }
        let edit = self.mode == Mode::Edit && self.el.is_some();
        let src_t = if edit { self.el.as_ref().map(|e| e.to_source(t)).unwrap_or(t) } else { t };
        if self.trim_t.is_none() {
            // while a handle is dragged the playhead stays on the handle
            self.timeline.set_playhead(src_t, true);
        }
        self.src_t_label =
            if edit { format!("Edit {}   Source {}", fmt_time(t), fmt_time(src_t)) } else { format!("Source {}", fmt_time(src_t)) };
        let mut rnd = String::new();
        if let Some(ev) = &st.ev {
            for (i, r) in ev.rounds.iter().enumerate() {
                if r.start <= src_t && src_t < r.end {
                    rnd = match r.score_before {
                        Some((a, b)) => format!("Round {} · {a}–{b}", a + b + 1),
                        None => format!("Round {}", i + 1),
                    };
                    break;
                }
            }
        }
        self.round_label = rnd;
    }

    pub fn seek_source(&mut self, src_t: f64, follow: bool, st: &mut AppState) {
        let src_t = src_t.min(st.duration()).max(0.0);
        self.timeline.set_playhead(src_t, follow);
        self.seek_target = None;
        if self.mode == Mode::Edit && self.el.is_some() {
            match self.el.as_ref().and_then(|e| e.to_output(src_t)) {
                None => {
                    // outside the kept parts: show the full video at that point (the load starts there)
                    self.set_mode(Mode::Source, st);
                    self.say("Showing the full video · that moment isn't in your edit", false);
                }
                Some(o) => self.player.seek(o),
            }
        } else {
            self.player.seek(src_t);
        }
    }

    pub fn toggle_play(&mut self) {
        self.player.toggle();
    }

    pub fn nudge(&mut self, dt: f64) {
        // step from where the last step went, not from the (slightly late) reported position, so holding
        // an arrow key keeps moving instead of repeating the same frame
        let now = Instant::now();
        let base = match self.seek_target {
            Some((t, at)) if now.duration_since(at) < Duration::from_millis(500) => t,
            _ => self.player.pos(),
        };
        let mut t = (base + dt).max(0.0);
        let dur = self.player.duration();
        if dur > 0.0 {
            t = t.min((dur - 0.001).max(0.0));
        }
        self.seek_target = Some((t, now));
        self.player.seek(t);
    }

    pub fn jump(&mut self, t: Option<f64>) {
        self.seek_target = None;
        let t = t.unwrap_or_else(|| (self.player.duration() - 0.05).max(0.0));
        self.player.seek(t);
    }

    pub fn escape(&mut self) {
        if self.lane.cancel_drag() {
            self.say("Move cancelled", false);
        } else if self.timeline.cancel_drag() {
            // a trim in progress goes back the way it was
            self.trim_t = None;
            self.say("Trim cancelled", false);
        } else if self.timeline.selected >= 0 {
            self.timeline.selected = -1;
        }
    }

    pub fn zoom_in(&mut self) {
        self.timeline.set_zoom(self.timeline.pps * 1.4, None);
    }

    pub fn zoom_out(&mut self) {
        self.timeline.set_zoom(self.timeline.pps / 1.4, None);
    }

    // ------------------------------------------------------------ actions
    fn part_at(st: &AppState, t: f64) -> i32 {
        spans(st).iter().position(|(a, b)| *a <= t && t <= *b).map(|i| i as i32).unwrap_or(-1)
    }

    /// (can split, can delete, can add) and the tooltips saying why not.
    pub fn actions_state(&self, st: &AppState) -> [(bool, &'static str); 3] {
        let t = self.timeline.playhead;
        let sp = spans(st);
        let i = Self::part_at(st, t);
        let can_split = i >= 0 && sp[i as usize].0 + MIN_SPLIT < t && t < sp[i as usize].1 - MIN_SPLIT;
        let sel = self.timeline.selected;
        let sel_ok = sel >= 0 && (sel as usize) < sp.len();
        let can_delete = sel_ok || i >= 0;
        let can_add = st.h.is_some() && i < 0;
        [
            (can_split, if can_split { "Split the part at the playhead  (S)" } else { "Split  (S) · put the playhead inside a kept part" }),
            (
                can_delete,
                if !can_delete {
                    "Delete part  (Del) · select a part or put the playhead on one"
                } else if sel_ok {
                    "Delete the selected part  (Del)"
                } else {
                    "Delete the part under the playhead  (Del)"
                },
            ),
            (
                can_add,
                if can_add {
                    "Keep 4 s around the playhead  (A)"
                } else {
                    "Add part  (A) · in Full video, put the playhead on footage that was cut"
                },
            ),
        ]
    }

    pub fn quick(&mut self, key: &str, st: &mut AppState) {
        st.apply_quick_cut(key, true);
        let n = st.ed.parts.len();
        let name = quick_name(key);
        self.say_later(if n > 0 {
            format!("{name} · {:.1} s kept", st.kept_duration())
        } else {
            format!("{name} keeps nothing in this clip")
        });
    }

    fn parts_edited(&mut self, parts: Vec<(f64, f64)>, st: &mut AppState) {
        // after trimming an end, sit a moment before the new end so Space plays into the cut
        if let Some(t) = self.trim_t.take() {
            match parts.iter().find(|(a, b)| (b - t).abs() < 1e-6 && b - a > 0.5) {
                Some((a, b)) => self.timeline.set_playhead(a.max(b - 2.0), false),
                None => self.timeline.set_playhead(t, false),
            }
        }
        st.set_parts(to_parts(&parts), true);
    }

    /// While a handle is dragged, the preview shows the frame at the handle.
    fn preview_trim(&mut self, src_t: f64) {
        self.trim_t = Some(src_t);
        self.trimming_log.push(src_t);
        self.timeline.set_playhead(src_t, false);
        if self.mode == Mode::Source {
            self.player.seek(src_t);
        } else if let Some(o) = self.el.as_ref().and_then(|e| e.to_output(src_t)) {
            self.player.seek(o);
        }
    }

    pub fn split(&mut self, st: &mut AppState) {
        let t = self.timeline.playhead;
        let mut parts = spans(st);
        for i in 0..parts.len() {
            let (a, b) = parts[i];
            if a <= t && t <= b {
                if !(a + MIN_SPLIT < t && t < b - MIN_SPLIT) {
                    self.say("Too close to the end of the part to split there", false);
                    return;
                }
                parts.splice(i..=i, [(a, t), (t, b)]);
                st.set_parts(to_parts(&parts), true);
                self.say_later(format!("Split at {}", fmt_time(t)));
                return;
            }
        }
        self.say("Put the playhead inside a kept part to split it", false);
    }

    pub fn delete(&mut self, st: &mut AppState) {
        let mut parts = spans(st);
        let mut i = self.timeline.selected;
        if !(i >= 0 && (i as usize) < parts.len()) {
            i = Self::part_at(st, self.timeline.playhead);
        }
        if !(i >= 0 && (i as usize) < parts.len()) {
            self.say("Select a part, or put the playhead on one, to delete it", false);
            return;
        }
        let (a, b) = parts.remove(i as usize);
        self.timeline.selected = -1;
        if self.mode == Mode::Edit && a <= self.timeline.playhead && self.timeline.playhead <= b {
            // carry on from the next kept part instead of jumping back to the start of the edit
            let nxt = parts.iter().find(|(pa, _)| *pa >= b).map(|(pa, _)| *pa);
            let t = nxt.unwrap_or_else(|| parts.last().map(|p| p.1).unwrap_or(a));
            self.timeline.set_playhead(t, false);
        }
        st.set_parts(to_parts(&parts), true);
        self.say_later(format!("Deleted a {:.1} s part · Ctrl+Z brings it back", b - a));
    }

    pub fn add_part(&mut self, st: &mut AppState) {
        let t = self.timeline.playhead;
        let parts = spans(st);
        if parts.iter().any(|(a, b)| *a <= t && t <= *b) {
            self.say(
                if self.mode == Mode::Edit {
                    "The playhead is already in a kept part · switch to Full video to add footage that was cut"
                } else {
                    "The playhead is already in a kept part"
                },
                false,
            );
            return;
        }
        let (mut a, mut b) = ((t - ADD_HALF).max(0.0), (t + ADD_HALF).min(st.duration()));
        for (pa, pb) in &parts {
            if *pb <= t {
                a = a.max(*pb);
            }
            if *pa >= t {
                b = b.min(*pa);
            }
        }
        if b - a > 0.3 {
            let mut np = parts.clone();
            np.push((a, b));
            st.set_parts(to_parts(&np), true);
            self.say_later(format!("Added {:.1} s at {}", b - a, fmt_time(t)));
        } else {
            self.say("No room for a new part here", false);
        }
    }

    fn kill(st: &AppState, kid: &str) -> Option<Kill> {
        st.ev.as_ref()?.kills.iter().find(|k| k.id == kid).cloned()
    }

    pub fn kill_toggled(&mut self, kid: &str, on: bool, st: &mut AppState) {
        let k = Self::kill(st, kid);
        let custom = st.cut_is_custom();
        let mut dis: Vec<String> = st.ed.kills_disabled.iter().filter(|d| d.as_str() != kid).cloned().collect();
        if !on {
            dis.push(kid.to_string());
        }
        dis.sort();
        dis.dedup();
        st.begin_step();
        st.ed.kills_disabled = dis;
        if !custom {
            let q = st.ed.quick_cut.clone();
            st.apply_quick_cut(&q, false);
        }
        st.touch();
        st.end_step();
        let who = k.as_ref().map(|k| names(st).get(&k.victim).cloned().unwrap_or_else(|| "kill".into())).unwrap_or_else(|| "kill".into());
        let at = k.as_ref().map(|k| format!(" at {}", ft1(k.t))).unwrap_or_default();
        let verb = if on { "Kept" } else { "Dropped" };
        if custom {
            self.say_later(format!("{verb} {who}{at} · your own trim is unchanged"));
        } else {
            self.say_later(format!(
                "{verb} {who}{at} · {} updated",
                QUICK_CUTS.iter().find(|(q, _)| *q == st.ed.quick_cut).map(|(_, n)| *n).unwrap_or("")
            ));
        }
    }

    pub fn kill_item_clicked(&mut self, kid: &str, st: &mut AppState) {
        if let Some(k) = Self::kill(st, kid) {
            self.seek_source((k.t - KILL_LEAD).max(0.0), true, st);
        }
    }

    pub fn timeline_kill(&mut self, kid: &str, st: &mut AppState) {
        if self.kills.iter().any(|k| k.id == kid) {
            self.cur_kill = Some(kid.to_string());
            self.scroll_to_kill = true;
        }
        if let Some(k) = Self::kill(st, kid) {
            self.seek_source((k.t - KILL_LEAD).max(0.0), false, st);
        }
    }

    pub fn remove_manual_kill(&mut self, kid: &str, st: &mut AppState) {
        let Some(k) = Self::kill(st, kid) else { return };
        let custom = st.cut_is_custom();
        st.begin_step();
        if let Some(ev) = st.ev.as_mut() {
            ev.kills.retain(|x| x.id != kid);
        }
        st.save_events();
        if !custom {
            let q = st.ed.quick_cut.clone();
            st.apply_quick_cut(&q, false);
        }
        st.touch();
        st.end_step();
        self.say_later(format!("Removed the kill at {} · Ctrl+Z brings it back", ft1(k.t)));
    }

    pub fn seg_clicked(&mut self, a: f64, b: f64, st: &mut AppState) {
        self.timeline.zoom_to(a - 3.0, b + 3.0);
        self.seek_source(a, false, st);
    }

    /// "Add a missing kill at the playhead": asks who it was (or uses `victim_override`).
    pub fn add_manual_kill(&mut self, st: &mut AppState, shell: &mut Shell) {
        if st.ev.is_none() {
            return;
        }
        if st.me().is_empty() {
            self.say("Choose who you're playing as first", false);
            shell.push(Action::WhoAreYou);
            return;
        }
        let t = self.timeline.playhead;
        self.player.pause();
        if let Some(v) = self.victim_override.clone() {
            self.finish_add_kill(t, &v, st);
        } else {
            self.add_kill = Some(AddKill { t, text: String::new(), focus: true });
        }
    }

    fn finish_add_kill(&mut self, t: f64, victim: &str, st: &mut AppState) {
        let victim = victim.trim().to_string();
        let custom = st.cut_is_custom();
        let me0 = st.me().first().cloned().unwrap_or_default();
        st.begin_step();
        let Some(ev) = st.ev.as_mut() else { return };
        let mut vid =
            ev.players.iter().find(|p| !victim.is_empty() && p.display.to_lowercase() == victim.to_lowercase()).map(|p| p.id.clone());
        if vid.is_none() {
            let mut n = ev.players.len() + 1;
            while ev.players.iter().any(|p| p.id == format!("m{n}")) {
                n += 1;
            }
            let id = format!("m{n}");
            let shown = if victim.is_empty() { "Unknown".to_string() } else { victim.clone() };
            ev.players.push(EvPlayer { id: id.clone(), names: vec![shown.clone()], display: shown, ..Default::default() });
            vid = Some(id);
        }
        let k = Kill {
            id: format!("m{}_{}", ev.kills.len() + 1, (t * 1000.0) as i64),
            t,
            t_conf: 1.0,
            round: backend::round_of(t, &ev.rounds),
            killer: me0,
            victim: vid.unwrap_or_default(),
            killer_side: "teal".into(),
            headshot: false,
            modifiers: vec![],
            gold: false,
            confidence: 1.0,
            victim_portrait: None,
            killer_portrait: None,
            row_image: None,
            manual: true,
        };
        let kid = k.id.clone();
        ev.kills.push(k);
        ev.kills.sort_by(|a, b| a.t.total_cmp(&b.t));
        st.save_events();
        if !custom {
            let q = st.ed.quick_cut.clone();
            st.apply_quick_cut(&q, false);
        }
        st.touch();
        st.end_step(); // the kill and the cut that follows from it are one undo step
        self.cur_kill = Some(kid);
        self.scroll_to_kill = true;
        self.say_later(format!("Added a kill at {}{}", ft1(t), if custom { " · your own trim is unchanged" } else { "" }));
    }

    pub fn pick_music(&mut self, st: &mut AppState) {
        let f = match self.pick_override.take() {
            Some(f) => Some(f),
            None => rfd::FileDialog::new()
                .set_title("Choose music")
                .set_directory(vl_core::paths::music_dir())
                .add_filter("Audio", &["mp3", "wav", "m4a", "aac", "flac", "ogg"])
                .pick_file()
                .map(|p| p.to_string_lossy().into_owned()),
        };
        let Some(f) = f else { return };
        st.begin_step();
        let m = &mut st.ed.music;
        m.path = f.clone();
        m.enabled = true;
        // a new song starts as background music
        m.song_in = 0.0;
        m.start = 0.0;
        m.end = None;
        m.synced = false;
        st.touch();
        st.end_step();
        let name = Path::new(&f).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(f);
        self.say_later(format!("{name} plays in the background · drag it under the timeline, or Sync to the beat"));
    }

    // ------------------------------------------------------------ music placement
    /// Load the edit preview, with the music mixed in when there is music (prepared off the UI thread).
    fn load_edit(&mut self, st: &AppState, url: &str, start: f64, subs: Option<String>) {
        let mus = st.ed.music.clone();
        self.music_sig = Some(music_sig(st));
        let has_audio = st.h.as_ref().map(|h| h.project.source.has_audio).unwrap_or(true);
        let (Some(_h), true) = (st.h.as_ref(), mus.enabled && !mus.path.is_empty() && Path::new(&mus.path).exists()) else {
            self.loaded_music = None;
            self.player.load(url, Load { start, subs, has_audio, ..Default::default() });
            return;
        };
        let Some(cache) = Self::cache_dir(st) else { return };
        self.loaded_music = None;
        self.music_gen += 1;
        let (gen, parts, tr) = (self.music_gen, st.ed.parts.clone(), st.ed.effects.transition.clone());
        let playing = self.player.is_playing();
        let (tx, ctx, url) = (self.tx.clone(), self.egui_ctx.clone(), url.to_string());
        std::thread::spawn(move || {
            let res = backend::music_preview_wav_result(&mus, &parts, &tr, &cache).map_err(|e| e.to_string());
            let _ = tx.send(Bg::Wav { gen, url, start, subs, playing, res });
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    fn bg_done(&mut self, b: Bg, st: &mut AppState, ctx: &egui::Context) {
        match b {
            Bg::Thumbs { path, strip } => {
                let Some(h) = &st.h else { return };
                let mine = path.starts_with(&h.dir) || Self::cache_dir(st).map(|d| path.starts_with(d)).unwrap_or(false);
                if !mine {
                    return;
                }
                if let Some((imgs, per, n, cell)) = strip {
                    let chunks = imgs
                        .into_iter()
                        .enumerate()
                        .map(|(i, im)| ctx.load_texture(format!("strip-{}-{i}", path.display()), im, egui::TextureOptions::LINEAR))
                        .collect();
                    self.timeline.thumbs = Some(Strip { chunks, per_chunk: per, count: n, cell });
                }
                self.timeline.strip_note.clear();
            }
            Bg::Song { key, res } => {
                if key != self.song_path {
                    return;
                }
                match res {
                    Ok(s) => {
                        let s = Arc::new(s);
                        self.song = Some(s.clone());
                        self.lane.set_song(Some(s), "");
                    }
                    Err(e) => {
                        log::warn!("song analysis: {e}");
                        self.song = None;
                        self.lane.set_song(None, "This song could not be read");
                    }
                }
                self.music_refresh(st);
            }
            Bg::Wav { gen, url, start, subs, playing, res } => {
                if gen != self.music_gen || self.mode != Mode::Edit {
                    return;
                }
                let mus = st.ed.music.clone();
                let has_audio = st.h.as_ref().map(|h| h.project.source.has_audio).unwrap_or(true);
                match res {
                    Ok(Some(wav)) => {
                        let wav = wav.to_string_lossy().into_owned();
                        self.loaded_music = Some(wav.clone());
                        self.player.load(
                            &url,
                            Load {
                                start,
                                subs,
                                pause: !playing,
                                has_audio,
                                music: Some(wav),
                                game_volume: mus.game_volume,
                                duck: mus.duck,
                                ..Default::default()
                            },
                        );
                    }
                    Ok(None) => self.player.load(&url, Load { start, subs, pause: !playing, has_audio, ..Default::default() }),
                    Err(e) => {
                        log::warn!("preview music: {e}");
                        self.player.load(&url, Load { start, subs, pause: !playing, has_audio, ..Default::default() });
                        self.say("The music could not be prepared for the preview (the export still adds it)", false);
                    }
                }
            }
        }
    }

    pub fn music_refresh(&mut self, st: &AppState) {
        let mus = st.ed.music.clone();
        let on = st.h.is_some() && !mus.path.is_empty() && mus.enabled;
        self.music_visible = on;
        if !on {
            return;
        }
        if mus.path != self.song_path {
            self.song_path = mus.path.clone();
            self.song = None;
            if !Path::new(&mus.path).exists() {
                self.lane.set_song(None, "Music file not found · choose it again");
            } else {
                self.song = backend::song_cached(&mus.path).map(Arc::new);
                self.lane.set_song(self.song.clone(), if self.song.is_some() { "" } else { "Reading the song…" });
                if self.song.is_none() {
                    let (tx, ctx, path) = (self.tx.clone(), self.egui_ctx.clone(), mus.path.clone());
                    std::thread::spawn(move || {
                        let res = backend::song_analyze(&path).map_err(|e| e.to_string());
                        let _ = tx.send(Bg::Song { key: path, res });
                        if let Some(c) = ctx {
                            c.request_repaint();
                        }
                    });
                }
            }
        }
        self.el_out = if st.ed.parts.is_empty() { None } else { Some(EditList::new(&st.ed.parts, &st.ed.effects.transition)) };
        let total = self.el_out.as_ref().map(|e| e.duration).unwrap_or(0.0);
        self.lane.set_placement(mus.song_in, mus.start, mus.end, total);
        let (a, b) = backend::music_span(&mus, total);
        self.music_labels(mus.song_in, a, b, &mus);
    }

    fn music_labels(&mut self, song_in: f64, a: f64, b: f64, mus: &MusicSettings) {
        let mut txt = format!(
            "song {} – {}  ·  plays {} – {}",
            music_lane::fmt(song_in),
            music_lane::fmt(song_in + b - a),
            music_lane::fmt(a),
            music_lane::fmt(b)
        );
        if let Some(s) = &self.song {
            txt += &format!("  ·  {:.0} BPM", s.bpm);
        }
        if mus.synced {
            txt += "  ·  synced";
        }
        self.mplace = txt;
    }

    /// Song time heard at preview time `t` (the preview has no transitions, the export does).
    pub fn song_at(&self, t: f64, st: &AppState) -> Option<f64> {
        if self.mode != Mode::Edit {
            return None;
        }
        let (el, out) = (self.el.as_ref()?, self.el_out.as_ref()?);
        let mus = &st.ed.music;
        let (a, b) = backend::music_span(mus, out.duration);
        for (pp, po) in el.placed.iter().zip(out.placed.iter()) {
            if pp.out_start <= t && t <= pp.out_start + pp.dur() {
                let r = po.out_start + (t - pp.out_start);
                return (a <= r && r <= b).then_some(mus.song_in + (r - a));
            }
        }
        None
    }

    fn lane_seek(&mut self, r: f64) {
        if self.mode != Mode::Edit {
            return;
        }
        let (Some(el), Some(out)) = (self.el.as_ref(), self.el_out.as_ref()) else {
            return;
        };
        for (pp, po) in el.placed.iter().zip(out.placed.iter()) {
            if po.out_start <= r && r <= po.out_start + po.dur() {
                let t = pp.out_start + (r - po.out_start);
                self.player.seek(t);
                return;
            }
        }
    }

    fn lane_placed(&mut self, song_in: f64, start: f64, end: Option<f64>, st: &mut AppState) {
        let m = &st.ed.music;
        let r3 = |v: f64| (v * 1000.0).round() as i64;
        if (r3(m.song_in), r3(m.start), m.end) == (r3(song_in), r3(start), end) {
            return;
        }
        st.begin_step();
        let m = &mut st.ed.music;
        m.song_in = song_in;
        m.start = start;
        m.end = end;
        m.synced = false;
        st.touch();
        st.end_step();
    }

    pub fn music_reset(&mut self, st: &mut AppState) {
        st.begin_step();
        let m = &mut st.ed.music;
        m.song_in = 0.0;
        m.start = 0.0;
        m.end = None;
        m.synced = false;
        st.touch();
        st.end_step();
        self.say_later("Background music: the song from its start, under the whole video");
    }

    /// Best part of the song (its drop on the first kill) and every cut on a beat. One undo step.
    pub fn music_sync(&mut self, find_part: bool, st: &mut AppState) {
        let Some(song) = self.song.clone() else { return };
        if st.ed.parts.is_empty() {
            return;
        }
        let tr = st.ed.effects.transition.clone();
        let el = EditList::new(&st.ed.parts, &tr);
        let kills = st.my_kills(false);
        let mut mus = st.ed.music.clone();
        if find_part {
            let first = kills.iter().filter_map(|k| el.to_output(k.t)).fold(f64::INFINITY, f64::min);
            mus.song_in = backend::music_best_part(&song, el.duration, if first.is_finite() { first } else { 0.0 });
            mus.start = 0.0;
            mus.end = None;
            mus.synced = true;
        }
        let beats = backend::music_edit_beats(&song, &mus, el.duration, false);
        let kt: Vec<f64> = kills.iter().map(|k| k.t).collect();
        let (new, moved, total) = backend::music_sync_cuts(&st.ed.parts, &tr, &beats, &kt, st.duration());
        st.begin_step();
        st.ed.music = mus.clone();
        st.set_parts(new, false);
        st.touch();
        st.end_step();
        let on_beat = if moved < total { format!("{moved} of {total} cuts on the beat") } else { "every cut on the beat".to_string() };
        if find_part {
            let wh = if mus.song_in > 0.0 {
                format!("the song from {}, its drop on your first kill", music_lane::fmt(mus.song_in))
            } else {
                "the song from its start (it is shorter than your edit)".to_string()
            };
            self.say_later(format!("Synced: {wh} · {on_beat} · Ctrl+Z to undo"));
        } else {
            self.say_later(format!("Cuts snapped: {on_beat} · Ctrl+Z to undo"));
        }
    }

    // ------------------------------------------------------------ textures
    fn pix(&mut self, ctx: &egui::Context, st: &AppState, rel: &str) -> Option<TextureHandle> {
        if let Some(t) = self.pix_cache.get(rel) {
            return t.clone();
        }
        let h = st.h.as_ref()?;
        let tex = image::open(h.dir.join(rel)).ok().map(|im| {
            let im = im.to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw());
            ctx.load_texture(format!("portrait-{}-{rel}", h.project.id), ci, egui::TextureOptions::LINEAR)
        });
        self.pix_cache.insert(rel.to_string(), tex.clone());
        tex
    }

    // ------------------------------------------------------------ input
    fn shortcuts(&mut self, ui: &Ui, st: &mut AppState) {
        if self.add_kill.is_some() || ui.ctx().memory(|m| m.top_modal_layer().is_some()) || ui.ctx().text_edit_focused() {
            return;
        }
        if self.kill_menu.is_some() {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.kill_menu = None;
            }
            return;
        }
        #[derive(Clone, Copy)]
        enum K {
            Play,
            Split,
            Delete,
            Add,
            Esc,
            Step(f64),
            Sec(f64),
            Home,
            End,
        }
        let keys: Vec<K> = ui.input_mut(|i| {
            let mut v = vec![];
            let (n, s) = (Modifiers::NONE, Modifiers::SHIFT);
            for (m, k, a) in [
                (s, Key::ArrowLeft, K::Sec(-1.0)),
                (s, Key::ArrowRight, K::Sec(1.0)),
                (n, Key::ArrowLeft, K::Step(-1.0)),
                (n, Key::ArrowRight, K::Step(1.0)),
                (n, Key::Space, K::Play),
                (n, Key::S, K::Split),
                (n, Key::Delete, K::Delete),
                (n, Key::Backspace, K::Delete),
                (n, Key::A, K::Add),
                (n, Key::Escape, K::Esc),
                (n, Key::Home, K::Home),
                (n, Key::End, K::End),
            ] {
                let cnt = i.count_and_consume_key(m, k);
                for _ in 0..cnt {
                    v.push(a);
                }
            }
            v
        });
        let fr = Self::frame_dur(st);
        for k in keys {
            match k {
                K::Play => self.toggle_play(),
                K::Split => self.split(st),
                K::Delete => self.delete(st),
                K::Add => self.add_part(st),
                K::Esc => self.escape(),
                K::Step(d) => self.nudge(d * fr),
                K::Sec(d) => self.nudge(d),
                K::Home => self.jump(Some(0.0)),
                K::End => self.jump(None),
            }
        }
    }

    // ------------------------------------------------------------ drawing
    fn side(&mut self, ui: &mut Ui, cx: &mut PageCtx) {
        let st = &mut *cx.st;
        let shell = &mut *cx.shell;
        let ctx = ui.ctx().clone();
        // portraits used by the list
        let rels: Vec<String> = self.kills.iter().filter_map(|k| k.portrait.clone()).collect();
        for r in rels {
            self.pix(&ctx, st, &r);
        }
        let mut quick_clicked: Option<String> = None;
        let mut seg_clicked: Option<(f64, f64)> = None;
        let mut toggled: Option<(String, bool)> = None;
        let mut row_clicked: Option<String> = None;
        let mut add_kill = false;
        let mut pick = false;
        let mut music_on: Option<bool> = None;
        let mut vol: Option<f64> = None;
        let mut gvol: Option<f64> = None;
        self.kill_rects.clear();
        self.quick_rects.clear();
        ui::side_panel(ui, SIDE_W, |p| {
            p.section(None, |ui| {
                ui.spacing_mut().item_spacing.y = S4;
                ui.horizontal(|ui| {
                    let w = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(w - 96.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui::label(ui, "Playing as", Role::Muted);
                        ui::label_wrap(ui, &self.stats.me, Role::Title);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                        let r = ui.add(Button::ghost("Change").icon("user-round"));
                        self.rects.insert("change", r.rect);
                        if r.clicked() {
                            shell.push(Action::WhoAreYou);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = S6;
                    ui::figure(ui, &self.stats.kills_kept.to_string(), "Kills kept");
                    ui::figure(ui, &self.stats.headshots.to_string(), "Headshots");
                    ui::figure(ui, &format!("{:.1}s", self.stats.length), "Length");
                });
                if let Some(h) = &st.h {
                    let s = &h.project.source;
                    let name = Path::new(&s.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let ga = h.project.game_area.as_ref().map(|g| format!(" · {}", g.aspect_label())).unwrap_or_default();
                    ui::label_wrap(ui, format!("{name}\n{}×{} · {:.0} fps{ga}", s.width, s.height, s.fps), Role::Mono);
                }
            });
            p.section(Some("Quick cut"), |ui| {
                let rows: Vec<ChoiceRow<String>> = self
                    .stats
                    .quick
                    .iter()
                    .map(|(k, n, d)| ChoiceRow::new(k.clone(), n.clone(), if *d < 0.0 { "new".to_string() } else { format!("{d:.1} s") }))
                    .collect();
                let sel = if self.stats.custom { None } else { Some(st.ed.quick_cut.clone()) };
                let top = ui.cursor().top();
                if let Some(k) = ui::choice_list(ui, "trim-quick", &rows, sel.as_ref(), 36.0) {
                    quick_clicked = Some(k);
                }
                let w = ui.available_width();
                for (i, (k, _, _)) in self.stats.quick.iter().enumerate() {
                    let r = Rect::from_min_size(pos2(ui.min_rect().left(), top + i as f32 * 37.0), vec2(w, 36.0));
                    self.quick_rects.push((k.clone(), r));
                }
                if let Some((p, msg)) = self.smart_job.clone() {
                    ui.add_space(S1);
                    ui.horizontal(|ui| {
                        ui::progress_bar(ui, p as f32, (ui.available_width() - 80.0).max(60.0));
                        let r = ui.add(Button::ghost("Cancel").tooltip("Stop listening"));
                        self.quick_rects.push(("smart_cancel".into(), r.rect));
                        if r.clicked() {
                            shell.push(Action::CancelJob);
                        }
                    });
                    ui::label_wrap(ui, &msg, Role::Muted);
                } else if self.stats.custom {
                    ui::label_wrap(ui, "Your own trim · choose a quick cut to start over from it", Role::Muted);
                } else if st.ed.quick_cut == "smart" {
                    ui::label_wrap(
                        ui,
                        "Kills plus laughter, shouting and hype in voice chat. Each highlight below says why it's in",
                        Role::Muted,
                    );
                }
            });
            p.section(Some("Highlights"), |ui| {
                if self.stats.segs.is_empty() {
                    ui::label_wrap(ui, "No highlights · tick at least one kill below", Role::Muted);
                } else {
                    let rows: Vec<ChoiceRow<usize>> = self
                        .stats
                        .segs
                        .iter()
                        .enumerate()
                        .map(|(i, s)| ChoiceRow::new(i, s.title.clone(), s.meta.clone()).sub(s.sub.clone()))
                        .collect();
                    let h = (rows.len().min(6) as f32) * 45.0;
                    let w = ui.available_width();
                    ui.allocate_ui(vec2(w, h), |ui| {
                        egui::ScrollArea::vertical().id_salt("trim-segs").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                            if let Some(i) = ui::choice_list(ui, "trim-segs-list", &rows, None, 44.0) {
                                seg_clicked = self.stats.segs.get(i).map(|s| (s.start, s.end));
                            }
                        });
                    });
                }
            });
            p.section(Some("Kills"), |ui| {
                ui.spacing_mut().item_spacing.y = S1;
                if self.kills.is_empty() {
                    ui::label_wrap(
                        ui,
                        "No kills found for you in this clip. Check who you're playing as, or add one that was missed",
                        Role::Muted,
                    );
                } else {
                    let n = self.kills.len().min(8);
                    let h = (n as f32 * 50.0 + 4.0).max(48.0);
                    let dis = st.ed.kills_disabled.clone();
                    let w = ui.available_width();
                    ui.allocate_ui(vec2(w, h), |ui| {
                        egui::ScrollArea::vertical().id_salt("trim-kills").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for k in &self.kills {
                                let enabled = !dis.contains(&k.id);
                                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
                                let cur = self.cur_kill.as_deref() == Some(k.id.as_str());
                                if cur && self.scroll_to_kill {
                                    ui.scroll_to_rect(rect, None);
                                    self.scroll_to_kill = false;
                                }
                                let bg = if cur {
                                    c::RAISED
                                } else if resp.hovered() {
                                    c::CARD
                                } else {
                                    Color32::TRANSPARENT
                                };
                                ui.painter().rect_filled(rect, 8.0, bg);
                                // the switch keeps / drops the kill
                                let sw = Rect::from_min_size(pos2(rect.left() + S2, rect.center().y - 12.0), vec2(36.0, 24.0));
                                let swr = ui.interact(sw, Id::unique(("trim-kill-sw", &k.id)), Sense::click());
                                let t = ctx.animate_bool_with_time_and_easing(swr.id, enabled, 0.14, egui::emath::easing::cubic_out);
                                ui::paint_switch(
                                    ui.painter(),
                                    Rect::from_min_size(
                                        pos2(sw.left() + 3.0, sw.center().y - ui::SWITCH_H / 2.0),
                                        vec2(ui::SWITCH_W, ui::SWITCH_H),
                                    ),
                                    t,
                                    true,
                                    swr.hovered(),
                                    false,
                                );
                                let swr = swr.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if enabled {
                                    "Kept in quick cuts · untick to leave it out"
                                } else {
                                    "Left out of quick cuts · tick to keep it"
                                });
                                if swr.clicked() {
                                    toggled = Some((k.id.clone(), !enabled));
                                }
                                // portrait
                                let pr = Rect::from_min_size(pos2(sw.right() + S3, rect.center().y - 16.0), vec2(32.0, 32.0));
                                if let Some(tex) = k.portrait.as_ref().and_then(|p| self.pix_cache.get(p).cloned().flatten()) {
                                    let [tw, th] = tex.size();
                                    let (tw, th) = (tw as f32, th as f32);
                                    // square crop, centred
                                    let uv = if tw > th {
                                        let f = th / tw / 2.0;
                                        Rect::from_min_max(pos2(0.5 - f, 0.0), pos2(0.5 + f, 1.0))
                                    } else {
                                        let f = tw / th / 2.0;
                                        Rect::from_min_max(pos2(0.0, 0.5 - f), pos2(1.0, 0.5 + f))
                                    };
                                    ui.painter().add(RectShape::filled(pr, 6.0, Color32::WHITE).with_texture(tex.id(), uv));
                                }
                                let col_t = if enabled { c::TEXT } else { c::DISABLED };
                                let col_m = if enabled { c::MUTED } else { c::DISABLED };
                                // time on the right
                                let tg = ui.painter().layout_no_wrap(ft1(k.t), Role::Mono.font(), col_m);
                                let tx = rect.right() - S3 - tg.size().x;
                                ui.painter().galley(pos2(tx, rect.center().y - tg.size().y / 2.0), tg, col_m);
                                let lx = pr.right() + S3;
                                let lw = (tx - S3 - lx).max(10.0);
                                let ng = ui::elided(ui, &k.victim, Role::Body.font(), col_t, lw);
                                let ny = rect.center().y - 17.0;
                                ui.painter().galley(pos2(lx, ny), ng, col_t);
                                let mut sx = lx;
                                let sy = rect.center().y + 9.0;
                                if k.headshot {
                                    ui::paint_crosshair(
                                        ui.painter(),
                                        pos2(sx + 5.5, sy),
                                        4.5,
                                        if enabled { c::ACCENT } else { c::DISABLED },
                                    );
                                    sx += 11.0 + S1 + 2.0;
                                }
                                let sub = format!(
                                    "{}{}",
                                    if k.headshot { "Headshot" } else { "Body shot" },
                                    if k.manual { " · added by you" } else { "" }
                                );
                                let sg = ui::elided(ui, &sub, Role::Muted.font(), col_m, (tx - S3 - sx).max(10.0));
                                ui.painter().galley(pos2(sx, sy - sg.size().y / 2.0), sg, col_m);
                                let resp = resp.on_hover_text("Click to jump to this kill · right-click for more");
                                if resp.clicked() {
                                    self.cur_kill = Some(k.id.clone());
                                    row_clicked = Some(k.id.clone());
                                }
                                if resp.secondary_clicked() {
                                    if let Some(p) = resp.interact_pointer_pos() {
                                        self.kill_menu = Some((k.id.clone(), p));
                                    }
                                }
                                self.kill_rects.push((k.id.clone(), rect, sw));
                            }
                        });
                    });
                }
                let r = ui.add(
                    Button::ghost("Add a missing kill at the playhead")
                        .icon("plus")
                        .tooltip("Put the playhead where the kill shows in the kill feed, then add it here"),
                );
                self.rects.insert("add_kill", r.rect);
                if r.clicked() {
                    add_kill = true;
                }
            });
            p.section(Some("Audio"), |ui| {
                let m = st.ed.music.clone();
                let has = !m.path.is_empty();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = S2;
                    let name = if has {
                        Path::new(&m.path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                    } else {
                        "No music chosen".to_string()
                    };
                    let bw = 100.0;
                    let lw = (ui.available_width() - bw - S2).max(20.0);
                    let r = ui.allocate_ui(vec2(lw, 30.0), |ui| {
                        ui.set_min_size(vec2(lw, 30.0));
                        ui.centered_and_justified(|ui| {
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui::label_elided(ui, &name, Role::Secondary)
                            })
                        })
                    });
                    if has {
                        r.response.on_hover_text(m.path.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui.add(Button::new("Choose").icon("music"));
                        self.rects.insert("choose_music", r.rect);
                        if r.clicked() {
                            pick = true;
                        }
                    });
                });
                let mut on = m.enabled && has;
                let r = ui.add(Toggle::new(&mut on).text("Play music under the video").enabled(has));
                let r = if has { r } else { r.on_hover_text("Choose a music file first") };
                self.rects.insert("music_toggle", r.rect);
                if r.changed() {
                    music_on = Some(on);
                }
                let mut v = (m.volume * 100.0).round();
                if Self::slider_row(ui, "Music", &mut v, 100.0, has && m.enabled) {
                    vol = Some(v);
                }
                let mut g = (m.game_volume * 100.0).round();
                if Self::slider_row(ui, "Game", &mut g, 150.0, true) {
                    gvol = Some(g);
                }
            });
            p.section(None, |ui| {
                ui::label_wrap(
                    ui,
                    "Music plays in Edit preview: place it under the timeline. Transitions between cuts are in Layout",
                    Role::Muted,
                );
            });
        });
        if let Some(k) = quick_clicked {
            if k == "smart" && !st.smart_ready() {
                // first time: listen to the clip, the cut follows when that's done
                if self.smart_job.is_none() {
                    shell.push(Action::SmartCut);
                }
            } else {
                self.quick(&k, st);
            }
        }
        if let Some((a, b)) = seg_clicked {
            self.seg_clicked(a, b, st);
        }
        if let Some((kid, on)) = toggled {
            self.kill_toggled(&kid, on, st);
        }
        if let Some(kid) = row_clicked {
            self.kill_item_clicked(&kid, st);
        }
        if add_kill {
            self.add_manual_kill(st, shell);
        }
        if pick {
            self.pick_music(st);
        }
        if let Some(on) = music_on {
            st.ed.music.enabled = on;
            st.touch();
        }
        if let Some(v) = vol {
            st.ed.music.volume = v / 100.0;
            st.touch();
        }
        if let Some(g) = gvol {
            st.ed.music.game_volume = g / 100.0;
            st.touch();
        }
    }

    /// "Music  [slider]  80%"; true when the value changed (whole percent steps).
    fn slider_row(ui: &mut Ui, name: &str, v: &mut f64, hi: f64, enabled: bool) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = S3;
            ui.add_enabled_ui(enabled, |ui| {
                ui.allocate_ui(vec2(48.0, 20.0), |ui| {
                    ui.set_min_width(48.0);
                    ui::label(ui, name, Role::Secondary);
                });
            });
            let sw = (ui.available_width() - 40.0 - S3).max(40.0);
            let before = *v;
            if ui.add(Slider::new(v, 0.0, hi).step(1.0).width(sw).enabled(enabled)).changed() {
                *v = v.round();
                changed = (*v - before).abs() > 0.0;
            }
            let (r, _) = ui.allocate_exact_size(vec2(40.0, 20.0), Sense::hover());
            ui.painter().text(pos2(r.right(), r.center().y), Align2::RIGHT_CENTER, format!("{}%", *v as i64), Role::Mono.font(), c::MUTED);
        });
        changed
    }

    fn kill_menu_ui(&mut self, ctx: &egui::Context, st: &mut AppState) {
        let Some((kid, pos)) = self.kill_menu.clone() else { return };
        let Some(k) = Self::kill(st, &kid) else {
            self.kill_menu = None;
            return;
        };
        let on = !st.ed.kills_disabled.contains(&k.id);
        let mut choice: Option<u8> = None;
        let area = egui::Area::new(Id::unique("trim-kill-menu"))
            .fade_in(false)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain(true)
            .show(ctx, |ui| {
                theme::menu_frame().show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let item = |ui: &mut Ui, text: &str| -> bool {
                        let (r, resp) = ui.allocate_exact_size(vec2(220.0, 30.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 6.0, c::RAISED_HI);
                        }
                        ui.painter().text(pos2(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, text, Role::Body.font(), c::TEXT);
                        resp.clicked()
                    };
                    if item(ui, "Jump to this kill") {
                        choice = Some(0);
                    }
                    if item(ui, if on { "Leave out of quick cuts" } else { "Keep in quick cuts" }) {
                        choice = Some(1);
                    }
                    if k.manual && item(ui, "Remove this kill") {
                        choice = Some(2);
                    }
                });
            });
        self.rects.insert("kill_menu", area.response.rect);
        let clicked_out =
            ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() && !ctx.input(|i| i.pointer.secondary_pressed());
        match choice {
            Some(0) => {
                self.kill_menu = None;
                self.kill_item_clicked(&k.id, st);
            }
            Some(1) => {
                self.kill_menu = None;
                self.kill_toggled(&k.id, !on, st);
            }
            Some(_) => {
                self.kill_menu = None;
                self.remove_manual_kill(&k.id, st);
            }
            None if clicked_out => self.kill_menu = None,
            None => {}
        }
    }

    fn add_kill_ui(&mut self, ctx: &egui::Context, st: &mut AppState) {
        let Some(d) = self.add_kill.as_mut() else { return };
        let mut done: Option<bool> = None;
        let (esc, enter) = ctx.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Enter)));
        egui::Modal::new(Id::unique("trim-add-kill"))
            .area(egui::Modal::default_area(Id::unique("trim-add-kill")).fade_in(false))
            .frame(theme::dialog_frame())
            .backdrop_color(Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                ui::label(ui, "Add a missing kill", Role::Title);
                ui.add_space(S2);
                ui::label_wrap(ui, format!("Who did you kill at {}? Leave empty if you don't know", fmt_time(d.t)), Role::Body);
                ui.add_space(S3);
                let r = ui::text_input(ui, &mut d.text, "", 420.0);
                if d.focus {
                    r.request_focus();
                    d.focus = false;
                }
                ui.add_space(S4);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(Button::new("Cancel").min_width(72.0)).clicked() {
                        done = Some(false);
                    }
                    if ui.add(Button::primary("OK").min_width(72.0)).clicked() {
                        done = Some(true);
                    }
                });
            });
        if esc {
            done = Some(false);
        } else if enter {
            done = Some(true);
        }
        if let Some(ok) = done {
            let d = self.add_kill.take().unwrap();
            if ok {
                self.finish_add_kill(d.t, &d.text, st);
            }
        }
    }

    fn centre(&mut self, ui: &mut Ui, cx: &mut PageCtx) {
        let st = &mut *cx.st;
        ui.spacing_mut().item_spacing = vec2(S1, S3);
        // top row: mode + round
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            let mut mode = self.mode;
            let r = ui.allocate_ui(vec2(176.0, 32.0), |ui| {
                ui::segmented(ui, "trim-mode", &[(Mode::Edit, "Edit preview"), (Mode::Source, "Full video")], &mut mode, 0)
            });
            self.rects.insert("mode", r.response.rect);
            if r.inner.changed() {
                self.set_mode(mode, st);
            }
            if !self.round_label.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui::pill(ui, &self.round_label);
                });
            }
        });
        // heights below the player
        let w = ui.available_width();
        let hint_w = w * 3.0 / 5.0 - S4;
        let hint_h = {
            let mut job = egui::text::LayoutJob::single_section(HINT.to_string(), egui::TextFormat::simple(Role::Muted.font(), c::MUTED));
            job.wrap.max_width = hint_w;
            ui.fonts_mut(|f| f.layout_job(job)).size().y
        };
        let music_h = if self.music_visible { 30.0 + S2 + music_lane::HEIGHT + S3 } else { 0.0 };
        let below = S1 + 32.0 + S3 + timeline::HEIGHT + S3 + music_h + hint_h;
        let ph = (ui.available_height() - below - S3).max(220.0);
        let frame = &mut *cx.frame;
        ui.allocate_ui(vec2(w, ph), |ui| {
            ui.set_min_size(vec2(w, ph));
            self.player.show(ui, frame);
            if !self.player_note.is_empty() {
                let r = ui.min_rect();
                ui.painter().text(
                    pos2(r.center().x, r.top() + (ph - 44.0) / 2.0),
                    Align2::CENTER_CENTER,
                    &self.player_note,
                    Role::Muted.font(),
                    c::MUTED,
                );
            }
        });
        for e in self.player.take_events() {
            if let PlayerEvent::Position(t) = e {
                self.on_player_pos(t, st);
            }
        }
        ui.add_space(S1 - S3 + S3);
        // toolbar
        let [(can_split, tip_split), (can_del, tip_del), (can_add, tip_add)] = self.actions_state(st);
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            ui.spacing_mut().item_spacing.x = S1;
            ui::label(ui, &self.src_t_label, Role::Mono);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = S1;
                let r = ui.add(IconButton::new("zoom-in", "Zoom in  (Ctrl+wheel)"));
                self.rects.insert("zoom_in", r.rect);
                if r.clicked() {
                    self.zoom_in();
                }
                let r = ui.add(IconButton::new("maximize-2", "Fit the whole video"));
                self.rects.insert("fit", r.rect);
                if r.clicked() {
                    self.timeline.fit();
                }
                let r = ui.add(IconButton::new("zoom-out", "Zoom out  (Ctrl+wheel)"));
                self.rects.insert("zoom_out", r.rect);
                if r.clicked() {
                    self.zoom_out();
                }
                ui.add_space(S2);
                let (r, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
                ui.painter().rect_filled(r, 0.0, c::HAIR);
                ui.add_space(S2);
                let r = ui.add(Button::ghost("Add part").icon("plus").enabled(can_add).tooltip(tip_add));
                self.rects.insert("add_part", r.rect);
                if r.clicked() {
                    self.add_part(st);
                }
                let r = ui.add(Button::ghost("Delete part").icon("trash-2").enabled(can_del).tooltip(tip_del));
                self.rects.insert("delete", r.rect);
                if r.clicked() {
                    self.delete(st);
                }
                let r = ui.add(Button::ghost("Split").icon("scissors").enabled(can_split).tooltip(tip_split));
                self.rects.insert("split", r.rect);
                if r.clicked() {
                    self.split(st);
                }
            });
        });
        // timeline (portraits first)
        let ctx = ui.ctx().clone();
        let want: Vec<(usize, String)> = self
            .timeline
            .kills
            .iter()
            .enumerate()
            .filter(|(_, k)| k.pix.is_none())
            .filter_map(|(i, k)| k.pix_path.clone().map(|p| (i, p)))
            .collect();
        for (i, p) in want {
            if !self.pix_cache.contains_key(&p) {
                self.pix(&ctx, st, &p);
            }
            self.timeline.kills[i].pix = self.pix_cache.get(&p).cloned().flatten();
        }
        self.timeline.show(ui);
        for e in self.timeline.take_events() {
            match e {
                TlEvent::Seek(t) => self.seek_source(t, false, st),
                TlEvent::PartsEdited(p) => self.parts_edited(p, st),
                TlEvent::KillClicked(id) => self.timeline_kill(&id, st),
                TlEvent::SelectionChanged(_) => {}
                TlEvent::Trimming(t) => self.preview_trim(t),
            }
        }
        // music: the song under the edit
        if self.music_visible {
            let ready = self.song.is_some() && !st.ed.parts.is_empty();
            let m = st.ed.music.clone();
            let reset_on = m.song_in > 0.0 || m.start > 0.0 || m.end.is_some();
            let (mut sync, mut snap, mut reset) = (false, false, false);
            ui.horizontal(|ui| {
                ui.set_height(30.0);
                ui.spacing_mut().item_spacing.x = S1;
                ui::label(ui, "Music", Role::Section);
                ui.add_space(S2);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add(
                        Button::new("Sync to the beat")
                            .icon("sparkles")
                            .enabled(ready)
                            .tooltip("Find the best part of the song, put its drop on your first kill and move every cut onto a beat"),
                    );
                    self.rects.insert("sync", r.rect);
                    sync = r.clicked();
                    let r = ui.add(
                        Button::ghost("Snap cuts")
                            .icon("magnet")
                            .enabled(ready)
                            .tooltip("Keep the song where it is and move every cut onto the nearest beat"),
                    );
                    self.rects.insert("snap", r.rect);
                    snap = r.clicked();
                    let r = ui.add(
                        Button::ghost("Background")
                            .icon("rotate-ccw")
                            .enabled(reset_on)
                            .tooltip("Plain background music: the song from its start, under the whole video"),
                    );
                    self.rects.insert("background", r.rect);
                    reset = r.clicked();
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui::label_elided(ui, &self.mplace, Role::Mono);
                    });
                });
            });
            ui.add_space(S2 - S3);
            let r = self.lane.show(ui);
            r.on_hover_text(
                "Drag the pink part to choose what plays · drag its ends to trim · click elsewhere to move it there · click inside to listen from there",
            );
            for e in self.lane.take_events() {
                match e {
                    LaneEvent::Placed(si, a, b) => self.lane_placed(si, a, b, st),
                    LaneEvent::Dragging(si, a, b) => {
                        let m = st.ed.music.clone();
                        self.music_labels(si, a, b, &m);
                    }
                    LaneEvent::Seek(r) => self.lane_seek(r),
                }
            }
            if sync {
                self.music_sync(true, st);
            }
            if snap {
                self.music_sync(false, st);
            }
            if reset {
                self.music_reset(st);
            }
        }
        // hints + feedback
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = S4;
            ui.allocate_ui_with_layout(vec2(hint_w, hint_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(hint_w);
                ui::label_wrap(ui, HINT, Role::Muted);
            });
            let mw = ui.available_width();
            ui.allocate_ui_with_layout(vec2(mw, hint_h), egui::Layout::top_down(egui::Align::Max), |ui| {
                ui.set_width(mw);
                let r = ui.add(
                    egui::Label::new(egui::RichText::new(&self.msg).font(Role::Secondary.font()).color(c::SECONDARY))
                        .wrap()
                        .halign(egui::Align::Max)
                        .selectable(false),
                );
                self.rects.insert("msg", r.rect);
            });
        });
    }
}

impl Page for TrimPage {
    fn on_job(&mut self, n: &JobNotice, st: &mut AppState, _shell: &mut Shell) {
        if n.kind != "smart" {
            return;
        }
        match n.progress {
            Some(p) => self.smart_job = Some((p, n.msg.clone())),
            None => {
                self.smart_job = None;
                if !n.msg.is_empty() {
                    self.say_later(n.msg.clone());
                }
                self.refresh_stats(st);
            }
        }
    }

    fn on_state(&mut self, ev: &StateEvent, st: &mut AppState, _shell: &mut Shell) {
        match ev {
            StateEvent::ProjectChanged => self.load_project(st),
            StateEvent::EventsChanged => self.load_events(st),
            StateEvent::EditsChanged => self.edits_changed(st),
            _ => {}
        }
    }

    fn on_show(&mut self, st: &mut AppState, _shell: &mut Shell) {
        self.shown = true;
        // captions may have been edited on the Subtitles page meanwhile
        if st.h.is_some() && self.subs_sig.is_some() && Some(self.subs_key(st)) != self.subs_sig {
            self.refresh_subs(st);
        }
    }

    fn on_hide(&mut self, _st: &mut AppState, _shell: &mut Shell) {
        self.shown = false;
        self.kill_menu = None;
        self.player.pause();
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut PageCtx) {
        let ctx = ui.ctx().clone();
        if self.egui_ctx.is_none() {
            self.egui_ctx = Some(ctx.clone());
        }
        while let Ok(b) = self.rx.try_recv() {
            self.bg_done(b, cx.st, &ctx);
        }
        for (m, sticky) in std::mem::take(&mut self.pending_say) {
            self.say(m, sticky);
        }
        if let Some(due) = self.music_due {
            if Instant::now() >= due {
                self.music_due = None;
                if self.mode == Mode::Edit {
                    self.reload_player(cx.st, true);
                }
            } else {
                ctx.request_repaint_after(due - Instant::now());
            }
        }
        if let Some(until) = self.msg_until {
            if Instant::now() >= until {
                self.msg.clear();
                self.msg_until = None;
            } else {
                ctx.request_repaint_after(until - Instant::now());
            }
        }
        self.shortcuts(ui, cx.st);
        let full = ui.available_rect_before_wrap();
        let side = Rect::from_min_size(full.min, vec2(SIDE_W, full.height()));
        let centre = Rect::from_min_max(pos2(side.right(), full.top()), full.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(side).layout(egui::Layout::top_down(egui::Align::Min)), |ui| self.side(ui, cx));
        ui.painter().rect_filled(Rect::from_min_size(pos2(side.right() - 1.0, side.top()), vec2(1.0, side.height())), 0.0, c::HAIR);
        let inner = Rect::from_min_max(pos2(centre.left() + S6, centre.top() + S4 + 4.0), pos2(centre.right() - S6, centre.bottom() - S4));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)), |ui| self.centre(ui, cx));
        self.kill_menu_ui(&ctx, cx.st);
        self.add_kill_ui(&ctx, cx.st);
        ui.allocate_rect(full, Sense::hover());
        let _ = Weight::Regular;
    }

    fn stop_players(&mut self) {
        self.player.stop();
        self.el = None;
    }

    fn shutdown(&mut self) {
        self.player.shutdown();
    }
}
