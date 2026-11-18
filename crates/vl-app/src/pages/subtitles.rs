//! Subtitles: transcribe voice chat, then fix, retime, add, split and style every caption.
//!
//! Everything on this page uses *edit time* (the time shown in the preview player and on the caption track):
//! the transcript stores source time and is converted at the edges. Captions outside the cut keep showing
//! their source time and say so.
use super::{Action, JobNotice, Page, PageCtx, Shell};
use crate::backend::{self, EditList};
use crate::dialogs::Dialog;
use crate::state::{AppState, StateEvent};
use crate::theme::{self, c, Role, Weight, S1, S2, S3, S4, S5, S6};
use crate::widgets::caption_track::{CaptionTrack, TrackEvent, TrackItem, HEIGHT as TRACK_H};
use crate::widgets::player::{edl_url, Load, Player, PlayerEvent};
use crate::widgets::ui::{self, Button, Toggle};
use egui::{pos2, vec2, Align, Color32, Key, Layout, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder};
use std::time::{Duration, Instant};
use vl_core::models::Part;
use vl_core::transcript::{self as tm, Segment, Transcript};

const TYPE_DELAY: Duration = Duration::from_millis(450);
const MSG_TIME: Duration = Duration::from_millis(3500);
const SIDE_W: f32 = 328.0;
const EDITOR_W: f32 = 340.0;
const ROW_H: f32 = 36.0;
/// "Words at a time" choices (0 = the whole caption)
pub const MAX_WORDS: [(i32, &str); 7] = [(0, "All"), (1, "1"), (2, "2"), (3, "3"), (4, "4"), (6, "6"), (8, "8")];
const HEADER_H: f32 = 32.0;
/// table column widths (toggle, time, voice; the caption takes the rest)
const COLS: [f32; 3] = [40.0, 132.0, 110.0];

/// "m:ss.ss".
fn fmt_t(v: f64) -> String {
    let v = v.max(0.0);
    let (m, s) = ((v / 60.0).floor(), v.rem_euclid(60.0));
    format!("{}:{:05.2}", m as i64, s)
}

/// `_t(v)[:-1]`: one decimal, truncated.
fn fmt_t1(v: f64) -> String {
    let mut s = fmt_t(v);
    s.pop();
    s
}

fn el_of(st: &AppState) -> Option<EditList> {
    (!st.ed.parts.is_empty()).then(|| EditList::plain(&st.ed.parts))
}

pub(crate) fn edit_duration(st: &AppState) -> f64 {
    el_of(st).map(|e| e.duration).unwrap_or_else(|| st.duration())
}

fn to_src(st: &AppState, t: f64) -> f64 {
    el_of(st).map(|e| e.to_source(t)).unwrap_or(t)
}

/// (start, end, lo, hi) of a caption in edit time, clamped to the kept part it starts in.
/// None when the caption lies entirely outside the cut.
pub(crate) fn seg_edit(st: &AppState, seg: &Segment) -> Option<(f64, f64, f64, f64)> {
    let Some(el) = el_of(st) else {
        return Some((seg.start, seg.end, 0.0, st.duration()));
    };
    for pl in &el.placed {
        let (lo, hi) = (seg.start.max(pl.src_in), seg.end.min(pl.src_out));
        if hi > lo {
            let a = pl.out_start + lo - pl.src_in;
            let b = pl.out_start + hi - pl.src_in;
            return Some((a, b, pl.out_start, pl.out_start + pl.dur()));
        }
    }
    None
}

fn seg_text(seg: &Segment) -> String {
    if seg.words.is_empty() {
        seg.text.clone()
    } else {
        seg.words.iter().map(|w| if w.censored { "***" } else { w.text.as_str() }).collect::<Vec<_>>().join(" ")
    }
}

fn speaker_label(st: &AppState, sid: &str) -> String {
    match st.ed.subtitles.speakers.get(sid) {
        Some(n) if !n.is_empty() => n.clone(),
        _ => format!("Speaker {}", sid.get(1..).unwrap_or("")),
    }
}

pub(crate) fn time_label(st: &AppState, seg: &Segment) -> String {
    match seg_edit(st, seg) {
        None => format!("cut · src {}", fmt_t1(seg.start)),
        Some((a, b, _, _)) => format!("{} – {}", fmt_t1(a), fmt_t1(b)),
    }
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The Censor… dialog.
pub(crate) struct CensorDlg {
    seg: usize,
    words: Vec<String>,
    on: Vec<bool>,
}

/// Screen rects of the last frame (for the UI check).
#[derive(Clone)]
pub struct Rects {
    pub text: Rect,
    pub rows: Vec<(usize, Rect)>,
    pub new_caption: Rect,
    pub outside: Rect,
    pub censor_chips: Vec<Rect>,
}

impl Default for Rects {
    fn default() -> Self {
        Rects { text: Rect::NOTHING, rows: vec![], new_caption: Rect::NOTHING, outside: Rect::NOTHING, censor_chips: vec![] }
    }
}

pub struct SubtitlesPage {
    /// the "Words at a time" control, for checks
    pub max_words_rect: Rect,
    /// last progress of this page's job ("transcribe"): Some(p) while running, and its status text
    pub progress: Option<f64>,
    pub status: String,
    pub player: Player,
    pub track: CaptionTrack,
    /// selected segment index (-1 = none)
    pub sel: isize,
    /// segment index of each list row
    pub rows: Vec<usize>,
    /// the caption whose text is in the editor (typed text only ever goes there)
    editing: Option<usize>,
    /// "Play caption" stops here (edit time)
    stop_at: Option<f64>,
    /// what the player currently shows: (project id, cut + layout + music) – keeps the position across visits
    loaded_sig: Option<(String, String)>,
    pub last_pos: f64,
    /// playhead in edit time (set on seek and from the player's position)
    pub playhead: f64,
    /// the last seek and when it was sent: re-sent once if mpv dropped it (a burst of exact seeks while
    /// scrubbing can lose the last one)
    settle: Option<(f64, Instant, u8)>,
    portrait: bool,
    // caption editor
    pub ed_text: String,
    pub ed_start: f64,
    pub ed_end: f64,
    ed_hint: String,
    type_at: Option<Instant>,
    /// the message next to the key hints
    pub msg: String,
    msg_until: Option<Instant>,
    pub include_outside: bool,
    preview_label: String,
    cancelling: bool,
    /// inline text edit in the caption list: (segment index, text, focus requested)
    row_edit: Option<(usize, String, bool)>,
    /// focus the caption text box (and select all) on the next frame
    focus_text: bool,
    /// the list "has focus" (Enter jumps into the caption text, F2 edits inline)
    list_focus: bool,
    scroll_to_sel: bool,
    pub(crate) censor: Option<CensorDlg>,
    /// TranscriptChanged events caused by this page's own saves (skipped, which never sent them)
    own_saves: usize,
    need_show: bool,
    visible: bool,
    editor_h: f32,
    pub rects: Rects,
}

impl SubtitlesPage {
    pub fn new(shell: &Shell) -> Self {
        SubtitlesPage {
            max_words_rect: Rect::NOTHING,
            progress: None,
            status: String::new(),
            player: Player::new(shell.gl.as_ref(), "subtitles"),
            track: CaptionTrack::new(),
            sel: -1,
            rows: vec![],
            editing: None,
            stop_at: None,
            loaded_sig: None,
            last_pos: 0.0,
            playhead: 0.0,
            settle: None,
            portrait: true,
            ed_text: String::new(),
            ed_start: 0.0,
            ed_end: 0.0,
            ed_hint: String::new(),
            type_at: None,
            msg: String::new(),
            msg_until: None,
            include_outside: false,
            preview_label: String::new(),
            cancelling: false,
            row_edit: None,
            focus_text: false,
            list_focus: false,
            scroll_to_sel: false,
            censor: None,
            own_saves: 0,
            need_show: false,
            visible: false,
            editor_h: 400.0,
            rects: Rects::default(),
        }
    }

    fn say(&mut self, msg: &str) {
        self.msg = msg.to_string();
        self.msg_until = Some(Instant::now() + MSG_TIME);
    }

    fn seg<'a>(&self, st: &'a AppState) -> Option<&'a Segment> {
        let tr = st.tr.as_ref()?;
        (self.sel >= 0).then(|| tr.segments.get(self.sel as usize)).flatten()
    }

    fn sel_idx(&self, st: &AppState) -> Option<usize> {
        self.seg(st).map(|_| self.sel as usize)
    }

    // ------------------------------------------------------------ persistence
    /// Persist the transcript and refresh the preview captions. `step`: a discrete action (its own undo
    /// step); typing and spin-box nudges pass false so they group into one step.
    fn save(&mut self, st: &mut AppState, refill: bool, step: bool) {
        if st.h.is_none() {
            return;
        }
        let Some(tr) = st.tr.as_mut() else { return };
        tr.sort();
        self.own_saves += 1;
        st.save_transcript(); // writes, and records a grouped change
        if step {
            st.end_step();
        }
        if refill {
            self.fill_table(st);
        } else {
            self.fill_track(st);
        }
        self.update_subs(st);
    }

    fn update_subs(&mut self, st: &AppState) {
        let p = self.subs_path(st);
        self.player.set_subs(p.as_deref());
        // paused: ask for the frame again so the new captions are drawn (the shared player only redraws
        // its texture when mpv delivers a new frame)
        if !self.player.is_playing() && self.player.duration() > 0.0 {
            self.player.seek(self.playhead);
        }
    }

    // ------------------------------------------------------------ state -> ui
    fn refresh(&mut self, st: &mut AppState) {
        let legacy = match st.tr.as_mut() {
            Some(tr) => tm::apply_legacy(tr, &mut st.ed.subtitles),
            None => false,
        };
        if legacy {
            st.touch();
            self.save(st, false, true);
        }
        self.status = match &st.tr {
            Some(tr) => format!("Model {}", tr.model),
            None => "Not transcribed yet".into(),
        };
        self.fill_table(st);
    }

    fn fill_track(&mut self, st: &AppState) {
        let mut items = vec![];
        if let Some(tr) = &st.tr {
            for (i, seg) in tr.segments.iter().enumerate() {
                if let Some((a, b, lo, hi)) = seg_edit(st, seg) {
                    items.push(TrackItem { index: i, a, b, text: seg_text(seg), hidden: seg.hidden, lo, hi });
                }
            }
        }
        let cuts = el_of(st).map(|e| e.placed.iter().skip(1).map(|p| p.out_start).collect()).unwrap_or_default();
        self.track.set_data(edit_duration(st), cuts, items);
        self.track.select(self.sel);
        self.track.set_playhead(self.playhead);
    }

    fn fill_table(&mut self, st: &AppState) {
        self.rows.clear();
        self.row_edit = None;
        let Some(tr) = &st.tr else {
            self.fill_track(st);
            self.load_editor(st);
            return;
        };
        for (i, seg) in tr.segments.iter().enumerate() {
            if seg_edit(st, seg).is_some() || self.include_outside {
                self.rows.push(i);
            }
        }
        if self.sel < 0 || !self.rows.contains(&(self.sel as usize)) {
            self.sel = self.rows.first().map(|&i| i as isize).unwrap_or(-1);
        }
        self.scroll_to_sel = true;
        self.fill_track(st);
        self.load_editor(st);
    }

    fn count_label(&self, st: &AppState) -> String {
        let Some(tr) = &st.tr else { return String::new() };
        let inside = tr.segments.iter().filter(|s| seg_edit(st, s).is_some()).count();
        let hidden = tr.segments.iter().filter(|s| s.hidden).count();
        format!("{inside} in the cut · {hidden} hidden")
    }

    fn load_editor(&mut self, st: &AppState) {
        let seg = self.seg(st);
        self.editing = seg.map(|_| self.sel as usize);
        self.type_at = None;
        match seg {
            None => {
                self.ed_text.clear();
                self.ed_start = 0.0;
                self.ed_end = 0.0;
                self.ed_hint = match &st.tr {
                    None => "Transcribe, or press N to add a caption at the playhead".into(),
                    Some(tr) if !tr.segments.is_empty() && self.rows.is_empty() => {
                        "No captions fall inside your cut. Tick \u{201c}Include lines outside the cut\u{201d} to see them all, or press N to add one at the playhead".into()
                    }
                    Some(_) => "Select a caption in the list or on the track".into(),
                };
            }
            Some(seg) => {
                self.ed_text = seg.text.clone();
                let (a, b) = seg_edit(st, seg).map(|r| (r.0, r.1)).unwrap_or((seg.start, seg.end));
                self.ed_start = r2(a);
                self.ed_end = r2(b);
                self.ed_hint = if seg.voice_line { "Looked like an agent voice line, so it starts hidden".into() } else { String::new() };
            }
        }
    }

    // ------------------------------------------------------------ selection
    pub(crate) fn select(&mut self, st: &mut AppState, i: usize, seek: bool) {
        self.commit_text(st);
        let n = st.tr.as_ref().map(|t| t.segments.len()).unwrap_or(0);
        if i >= n {
            return;
        }
        if !self.rows.contains(&i) {
            self.fill_table(st);
        }
        self.sel = i as isize;
        self.scroll_to_sel = true;
        self.row_edit = None;
        self.track.select(self.sel);
        self.load_editor(st);
        if seek {
            let r = st.tr.as_ref().and_then(|t| seg_edit(st, &t.segments[i]));
            if let Some((a, ..)) = r {
                self.seek_edit(st, a);
            }
        }
    }

    // ------------------------------------------------------------ playback
    pub(crate) fn seek_edit(&mut self, st: &AppState, t: f64) {
        self.stop_at = None;
        let t = t.min(edit_duration(st)).max(0.0);
        self.player.seek(t);
        self.playhead = t;
        self.settle = Some((t, Instant::now(), 0));
        self.track.set_playhead(t);
    }

    /// Re-send a seek that didn't land (see `settle`).
    fn settle_seek(&mut self, ctx: &egui::Context) {
        let Some((t, at, tries)) = self.settle else { return };
        if self.player.is_playing() || tries >= 2 {
            self.settle = None;
            return;
        }
        let wait = Duration::from_millis(350);
        if at.elapsed() < wait {
            ctx.request_repaint_after(wait - at.elapsed());
            return;
        }
        if (self.player.pos() - t).abs() > 0.03 {
            self.player.seek(t);
            self.settle = Some((t, Instant::now(), tries + 1));
            ctx.request_repaint_after(wait);
        } else {
            self.settle = None;
        }
    }

    fn on_pos(&mut self, t: f64) {
        self.last_pos = t;
        self.playhead = t;
        if let Some(stop) = self.stop_at {
            if t >= stop - 0.02 {
                self.player.pause();
                self.stop_at = None;
            }
        }
        self.track.set_playhead(t);
    }

    fn toggle_play(&mut self) {
        self.stop_at = None;
        self.player.toggle();
    }

    /// One frame, the same arrow-key step as Trim (Shift steps 1 s).
    fn frame_step(st: &AppState) -> f64 {
        let fps = st.h.as_ref().map(|h| h.project.source.fps).unwrap_or(0.0);
        if fps > 1.0 {
            1.0 / fps
        } else {
            1.0 / 30.0
        }
    }

    fn nudge(&mut self, st: &AppState, dt: f64) {
        self.seek_edit(st, self.playhead + dt);
    }

    // ------------------------------------------------------------ editor actions
    fn do_focus_text(&mut self, st: &AppState) {
        if self.seg(st).is_some() {
            self.focus_text = true;
        }
    }

    fn commit_text(&mut self, st: &mut AppState) {
        self.type_at = None;
        let Some(i) = self.editing else { return };
        let text = self.ed_text.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(seg) = st.tr.as_mut().and_then(|t| t.segments.get_mut(i)) else { return };
        if !text.is_empty() && text != seg.text {
            tm::retime_words(seg, &text);
            seg.edited = true;
            if seg.voice_line {
                seg.hidden = false;
            }
            self.save(st, false, false);
        }
    }

    fn text_escape(&mut self, st: &AppState) {
        self.type_at = None;
        if let Some(seg) = self.editing.and_then(|i| st.tr.as_ref().and_then(|t| t.segments.get(i))) {
            self.ed_text = seg.text.clone();
        }
        self.list_focus = true;
    }

    /// Set a caption's times from edit-time values (or source values when it is outside the cut).
    fn apply_times(st: &mut AppState, i: usize, a_edit: f64, b_edit: f64) {
        let Some(seg) = st.tr.as_ref().and_then(|t| t.segments.get(i)) else { return };
        let (a, b) = match seg_edit(st, seg) {
            None => (a_edit, b_edit),
            Some((_, _, lo, hi)) => {
                let a_edit = a_edit.max(lo).min(hi - tm::MIN_LEN);
                let b_edit = b_edit.min(hi).max(a_edit + tm::MIN_LEN);
                let (mut a, mut b) = (to_src(st, a_edit), to_src(st, b_edit));
                // map within the same part (to_source picks the right part for interior points)
                if let Some(el) = el_of(st) {
                    let mid = (a_edit + b_edit) / 2.0;
                    if let Some(pl) = el.placed.iter().find(|p| p.out_start <= mid && mid <= p.out_start + p.dur()) {
                        a = pl.src_in + a_edit - pl.out_start;
                        b = pl.src_in + b_edit - pl.out_start;
                    }
                }
                (a, b)
            }
        };
        if let Some(seg) = st.tr.as_mut().and_then(|t| t.segments.get_mut(i)) {
            tm::set_times(seg, a, b);
            seg.edited = true;
        }
    }

    fn time_set(&mut self, st: &mut AppState, which: &str, v: f64) {
        let Some(i) = self.sel_idx(st) else { return };
        let seg = &st.tr.as_ref().unwrap().segments[i];
        let (mut a, mut b) = seg_edit(st, seg).map(|r| (r.0, r.1)).unwrap_or((seg.start, seg.end));
        if which == "start" {
            a = v;
            b = b.max(v + tm::MIN_LEN);
        } else {
            b = v;
            a = a.min(v - tm::MIN_LEN);
        }
        Self::apply_times(st, i, a, b);
        self.after_retime(st, i, false);
    }

    fn after_retime(&mut self, st: &mut AppState, i: usize, step: bool) {
        if let Some(tr) = st.tr.as_mut() {
            let key = tr.segments[i].clone();
            tr.sort();
            self.sel = tr.segments.iter().position(|s| *s == key).map(|k| k as isize).unwrap_or(-1);
        }
        self.save(st, true, step);
    }

    fn track_retimed(&mut self, st: &mut AppState, i: usize, a: f64, b: f64) {
        st.begin_step();
        if st.tr.as_ref().map(|t| i >= t.segments.len()).unwrap_or(true) {
            return;
        }
        Self::apply_times(st, i, a, b);
        self.after_retime(st, i, true);
    }

    fn time_playhead(&mut self, st: &mut AppState, which: &str) {
        let Some(seg) = self.seg(st) else { return };
        if seg_edit(st, seg).is_none() {
            self.say("This caption is outside your cut, so the playhead can't reach it");
            return;
        }
        let v = r2(self.playhead);
        let cur = if which == "start" { &mut self.ed_start } else { &mut self.ed_end };
        if (*cur - v).abs() > 1e-9 {
            *cur = v;
            self.time_set(st, which, v);
        }
    }

    fn speaker_changed(&mut self, st: &mut AppState, sid: &str) {
        st.begin_step();
        let Some(i) = self.sel_idx(st) else { return };
        let seg = &mut st.tr.as_mut().unwrap().segments[i];
        if sid.is_empty() || seg.speaker == sid {
            return;
        }
        seg.speaker = sid.to_string();
        seg.edited = true;
        self.save(st, false, true);
    }

    fn show_toggled(&mut self, st: &mut AppState, v: bool) {
        st.begin_step();
        let Some(i) = self.sel_idx(st) else { return };
        let seg = &mut st.tr.as_mut().unwrap().segments[i];
        seg.hidden = !v;
        seg.edited = true;
        self.save(st, false, true);
    }

    pub(crate) fn play_line(&mut self, st: &AppState) {
        let r = self.seg(st).and_then(|s| seg_edit(st, s));
        let Some((a, b, ..)) = r else {
            self.say("This caption is outside your cut, so it isn't in the preview");
            return;
        };
        self.player.seek(a);
        self.playhead = a;
        self.settle = None;
        self.stop_at = Some(b);
        self.player.play();
    }

    fn split(&mut self, st: &mut AppState) {
        st.begin_step();
        self.commit_text(st);
        let Some(i) = self.sel_idx(st) else { return };
        let t = to_src(st, self.playhead);
        match tm::split_at(st.tr.as_mut().unwrap(), i, t) {
            None => self.say("Put the playhead inside the selected caption to split it"),
            Some(j) => {
                self.sel = j as isize;
                self.save(st, true, true);
                self.say("Split into two captions");
            }
        }
    }

    fn merge(&mut self, st: &mut AppState) {
        st.begin_step();
        self.commit_text(st);
        let Some(i) = self.sel_idx(st) else { return };
        if tm::merge_next(st.tr.as_mut().unwrap(), i) {
            self.save(st, true, true);
            self.say("Joined with the next caption");
        } else {
            self.say("This is the last caption");
        }
    }

    fn delete(&mut self, st: &mut AppState) {
        st.begin_step();
        let Some(i) = self.sel_idx(st) else { return };
        let tr = st.tr.as_mut().unwrap();
        tr.segments.remove(i);
        self.sel = self.sel.min(tr.segments.len() as isize - 1);
        self.save(st, true, true);
        self.say("Caption deleted · Ctrl+Z brings it back");
    }

    pub(crate) fn censor_words(&mut self, st: &mut AppState) {
        st.begin_step();
        self.commit_text(st);
        let Some(i) = self.sel_idx(st) else { return };
        let seg = &st.tr.as_ref().unwrap().segments[i];
        if seg.words.is_empty() {
            return;
        }
        self.censor = Some(CensorDlg {
            seg: i,
            words: seg.words.iter().map(|w| w.text.clone()).collect(),
            on: seg.words.iter().map(|w| w.censored).collect(),
        });
    }

    fn censor_accepted(&mut self, st: &mut AppState, d: CensorDlg) {
        let Some(seg) = st.tr.as_mut().and_then(|t| t.segments.get_mut(d.seg)) else { return };
        for (k, w) in seg.words.iter_mut().enumerate() {
            w.censored = d.on.get(k).copied().unwrap_or(false);
        }
        seg.edited = true;
        self.save(st, false, true);
    }

    fn retranscribe(&mut self, st: &mut AppState, shell: &mut Shell) {
        self.commit_text(st);
        if let Some(seg) = self.seg(st) {
            shell.push(Action::Transcribe { whole: false, only: Some((seg.start, seg.end)) });
        }
    }

    fn add_caption(&mut self, st: &mut AppState) {
        st.begin_step();
        if st.h.is_none() {
            return;
        }
        if st.tr.is_none() {
            st.tr = Some(Transcript { model: "added by hand".into(), ..Default::default() });
        }
        let mut t = self.playhead;
        let el = el_of(st);
        let mut hi = edit_duration(st);
        if let Some(el) = &el {
            let pl = el.placed.iter().find(|p| p.out_start <= t && t < p.out_start + p.dur()).or(el.placed.last());
            if let Some(pl) = pl {
                hi = pl.out_start + pl.dur();
            }
        }
        // never overlap: start after a caption under the playhead, stop before the next one
        let tr = st.tr.as_ref().unwrap();
        let mut spans: Vec<(f64, f64)> = tr.segments.iter().filter_map(|s| seg_edit(st, s)).map(|r| (r.0, r.1)).collect();
        spans.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
        for &(a, b) in &spans {
            if a <= t && t < b {
                t = b;
            }
        }
        let nxt = spans.iter().map(|s| s.0).filter(|&a| a > t).fold(hi, f64::min);
        let end = (t + 2.0).min(nxt).min(hi);
        if end - t < 0.3 {
            self.say("No room here. Move the playhead to a gap between captions");
            return;
        }
        let mut a_src = to_src(st, t);
        if let Some(el) = &el {
            if let Some(pl) = el.placed.iter().find(|p| p.out_start <= t && t < p.out_start + p.dur() + 1e-6) {
                a_src = pl.src_in + t - pl.out_start;
            }
        }
        let tr = st.tr.as_mut().unwrap();
        let spk = tr.speakers.first().cloned().unwrap_or_else(|| "s1".into());
        self.sel = tm::add_caption(tr, a_src, end - t, "", &spk) as isize;
        if !st.ed.subtitles.enabled {
            self.enable(st, true);
        }
        self.save(st, true, true);
        self.do_focus_text(st);
    }

    // ------------------------------------------------------------ style
    fn enable(&mut self, st: &mut AppState, v: bool) {
        st.ed.subtitles.enabled = v;
        self.changed(st);
    }

    fn set_style(&mut self, st: &mut AppState, key: &str) {
        let s = &mut st.ed.subtitles.style;
        s.preset = key.to_string();
        if key == "pop" {
            s.font = "Anton".into();
        } else if key == "clean" || key == "boxed" {
            s.font = "Inter".into();
        }
        self.changed(st);
    }

    fn add_speaker(&mut self, st: &mut AppState) {
        let Some(tr) = st.tr.as_mut() else { return };
        let n = tr.speakers.len();
        tr.speakers.push(format!("s{}", n + 1));
        self.save(st, false, true);
        self.load_editor(st);
    }

    fn changed(&mut self, st: &mut AppState) {
        st.touch();
        self.update_subs(st);
    }

    // ------------------------------------------------------------ preview
    /// The preview's .ass file (scripted checks).
    pub fn subs_path_for_check(&self, st: &AppState) -> Option<String> {
        self.subs_path(st)
    }

    fn subs_path(&self, st: &AppState) -> Option<String> {
        let (h, tr) = (st.h.as_ref()?, st.tr.as_ref()?);
        if !st.ed.subtitles.enabled {
            return None;
        }
        let ga = h.project.game_area.as_ref()?;
        let parts = if st.ed.parts.is_empty() { vec![Part::new(0.0, st.duration())] } else { st.ed.parts.clone() };
        let el = EditList::plain(&parts);
        let (_, w, hh, bx) = backend::preview_composition(&st.ed, ga);
        let out = if st.read_only {
            vl_core::paths::tmp_dir().join(format!("preview_subs_{}.ass", h.project.id))
        } else {
            h.dir.join("cache").join("preview_subs.ass")
        };
        match backend::build_ass(tr, &st.ed.subtitles, &el, w, hh, &st.ed.highlight_color, &out, bx) {
            Ok(p) => Some(p.to_string_lossy().into_owned()),
            Err(e) => {
                log::warn!("preview subtitles: {e}");
                None
            }
        }
    }

    /// On show: (re)load the preview when the cut, layout or music changed; keep the position.
    fn do_show(&mut self, st: &mut AppState) {
        if let Some(h) = st.h.as_ref().filter(|h| h.source_ok()) {
            if let Some(ga) = h.project.game_area.as_ref() {
                let parts: Vec<(f64, f64)> = if st.ed.parts.is_empty() {
                    vec![(0.0, st.duration())]
                } else {
                    st.ed.parts.iter().map(|p| (p.src_in, p.src_out)).collect()
                };
                let (graph, w, hh, _) = backend::preview_composition(&st.ed, ga);
                self.portrait = w <= hh;
                let mus = &st.ed.music;
                let pid = h.project.id.clone();
                let fx = &st.ed.effects;
                let rest = format!(
                    "{parts:?}|{graph}|{}|{}:{}",
                    serde_json::to_string(mus).unwrap_or_default(),
                    fx.transition,
                    fx.transition_secs
                );
                let sig = (pid.clone(), rest);
                if self.loaded_sig.as_ref() != Some(&sig) {
                    let start = if self.loaded_sig.as_ref().map(|s| s.0 == pid).unwrap_or(false) { self.last_pos } else { 0.0 };
                    let out_dir = if st.read_only { vl_core::paths::tmp_dir() } else { h.dir.join("cache") };
                    // the same music track as Trim's preview (cached, so usually instant)
                    let wav =
                        if st.ed.parts.is_empty() { None } else { backend::music_preview_wav(mus, &st.ed.parts, &st.ed.effects, &out_dir) };
                    let start = start.min(edit_duration(st));
                    let load = Load {
                        start,
                        subs: self.subs_path(st),
                        pause: true,
                        compose: Some(graph),
                        has_audio: h.project.source.has_audio,
                        music: wav.map(|p| p.to_string_lossy().into_owned()),
                        game_volume: mus.game_volume,
                        duck: mus.duck,
                    };
                    self.player.load(&edl_url(h.source(), &parts), load);
                    self.playhead = start;
                    self.loaded_sig = Some(sig);
                } else {
                    self.update_subs(st);
                }
                let shape = st.ed.layout.as_ref().map(|l| l.aspect.clone()).unwrap_or_else(|| st.ed.format.clone());
                self.preview_label =
                    format!("{shape} preview · Space play · \u{2190}/\u{2192} one frame · Shift+\u{2190}/\u{2192} 1 s · [ ] set start/end · S split · N new caption");
            }
        }
        self.refresh(st);
    }

    // ------------------------------------------------------------ keys
    /// Page shortcuts, only while the page is shown and no text field / dialog / popup has the keyboard.
    fn shortcuts(&mut self, ui: &Ui, st: &mut AppState) {
        let ctx = ui.ctx();
        if ctx.text_edit_focused() || self.censor.is_some() || ctx.memory(|m| m.top_modal_layer().is_some()) || ctx.any_popup_open() {
            return;
        }
        let n = Modifiers::NONE;
        let sh = Modifiers::SHIFT;
        let focused_other = ctx.memory(|m| m.focused().is_some());
        let k = ctx.input_mut(|i| {
            // most specific first: consume_key ignores extra Shift
            let shl = i.consume_key(sh, Key::ArrowLeft);
            let shr = i.consume_key(sh, Key::ArrowRight);
            [
                i.consume_key(n, Key::Space),
                i.consume_key(n, Key::ArrowLeft),
                i.consume_key(n, Key::ArrowRight),
                shl,
                shr,
                i.consume_key(n, Key::OpenBracket),
                i.consume_key(n, Key::CloseBracket),
                i.consume_key(n, Key::S),
                i.consume_key(n, Key::N),
                i.consume_key(n, Key::Delete),
                i.consume_key(n, Key::Home),
                i.consume_key(n, Key::End),
                self.list_focus && !focused_other && i.consume_key(n, Key::Enter),
                self.list_focus && !focused_other && i.consume_key(n, Key::F2),
                self.list_focus && !focused_other && i.consume_key(n, Key::ArrowUp),
                self.list_focus && !focused_other && i.consume_key(n, Key::ArrowDown),
            ]
        });
        if k[1..5].iter().any(|&b| b) || k[14] || k[15] {
            // arrows are ours here: don't let egui move the keyboard focus to a neighbouring widget
            ctx.memory_mut(|m| m.stop_text_input());
        }
        let fr = Self::frame_step(st);
        if k[0] {
            self.toggle_play();
        }
        if k[1] {
            self.nudge(st, -fr);
        }
        if k[2] {
            self.nudge(st, fr);
        }
        if k[3] {
            self.nudge(st, -1.0);
        }
        if k[4] {
            self.nudge(st, 1.0);
        }
        if k[5] {
            self.time_playhead(st, "start");
        }
        if k[6] {
            self.time_playhead(st, "end");
        }
        if k[7] {
            self.split(st);
        }
        if k[8] {
            self.add_caption(st);
        }
        if k[9] {
            self.delete(st);
        }
        if k[10] {
            self.seek_edit(st, 0.0);
        }
        if k[11] {
            self.seek_edit(st, (edit_duration(st) - 0.05).max(0.0));
        }
        if k[12] {
            self.do_focus_text(st);
        }
        if k[14] || k[15] {
            // the list's current row moves: select and seek
            let cur = self.rows.iter().position(|&i| i as isize == self.sel);
            let next = match (cur, k[15]) {
                (Some(r), true) => self.rows.get(r + 1).copied(),
                (Some(r), false) => r.checked_sub(1).and_then(|r| self.rows.get(r).copied()),
                (None, _) => self.rows.first().copied(),
            };
            if let Some(i) = next {
                self.select(st, i, true);
            }
        }
        if k[13] {
            if let Some(i) = self.sel_idx(st) {
                let text = seg_text(&st.tr.as_ref().unwrap().segments[i]);
                self.row_edit = Some((i, text, true));
            }
        }
    }

    // ------------------------------------------------------------ drawing
    fn side(&mut self, ui: &mut Ui, st: &mut AppState, shell: &mut Shell) {
        ui::side_panel(ui, SIDE_W, |p| {
            p.section(None, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui::label(ui, "Subtitles", Role::Title);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.allocate_ui(vec2(82.0, 32.0), |ui| {
                            let mut on = st.ed.subtitles.enabled;
                            if ui::segmented(ui, "subs-onoff", &[(false, "Off"), (true, "On")], &mut on, 0).changed() {
                                self.enable(st, on);
                            }
                        });
                    });
                });
                ui::hint(ui, "Transcribed on this PC from the video's audio. Fix anything it misheard in the caption editor");
            });
            p.section(Some("Transcription"), |ui| {
                ui.horizontal(|ui| {
                    // the ghost button's own padding is the gap (Inter renders a little wide here,
                    // and the row must stay inside the panel)
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if self.progress.is_some() {
                        if ui.add(Button::ghost("Cancel").icon("x").enabled(!self.cancelling).tooltip("Stop transcribing")).clicked() {
                            self.cancelling = true;
                            self.status = "Cancelling…".into();
                            shell.push(Action::CancelJob);
                        }
                    } else {
                        if ui.add(Button::primary("Transcribe kept parts").icon("mic")).clicked() {
                            shell.push(Action::Transcribe { whole: false, only: None });
                        }
                        if ui.add(Button::ghost("Whole video")).clicked() {
                            shell.push(Action::Transcribe { whole: true, only: None });
                        }
                    }
                });
                let w = ui.available_width();
                if ui.add(Button::ghost("Words it should know").icon("user-round").min_width(w)).clicked() {
                    shell.dialog(Dialog::vocabulary());
                }
                if let Some(p) = self.progress {
                    ui::progress_bar(ui, p as f32, w);
                }
                if !self.status.is_empty() {
                    ui::label_wrap(ui, &self.status, Role::Mono);
                }
            });
            p.section(Some("Speakers"), |ui| {
                let speakers = st.tr.as_ref().map(|t| t.speakers.clone()).unwrap_or_default();
                for sid in &speakers {
                    let n = st.tr.as_ref().map(|t| t.segments.iter().filter(|s| &s.speaker == sid).count()).unwrap_or(0);
                    let lines = format!("{n} line{}", if n != 1 { "s" } else { "" });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = S3;
                        let lw = ui::text_width(ui, &lines, &Role::Mono.font());
                        let mut name = st.ed.subtitles.speakers.get(sid).cloned().unwrap_or_default();
                        let r = ui.push_id(("spk", sid), |ui| {
                            ui::text_input(
                                ui,
                                &mut name,
                                &format!("Speaker {}", sid.get(1..).unwrap_or("")),
                                ui.available_width() - lw - S3,
                            )
                        });
                        if r.inner.changed() {
                            st.ed.subtitles.speakers.insert(sid.clone(), name);
                            self.changed(st);
                        }
                        ui::label(ui, lines, Role::Mono);
                    });
                }
                let w = ui.available_width();
                if ui.add(Button::ghost("Add a speaker").icon("plus").min_width(w)).clicked() {
                    self.add_speaker(st);
                }
                let mut v = st.ed.subtitles.show_names;
                if ui.add(Toggle::new(&mut v).text("Show names in subtitles")).changed() {
                    st.ed.subtitles.show_names = v;
                    self.changed(st);
                }
            });
            p.section(Some("Style"), |ui| {
                let mut preset = st.ed.subtitles.style.preset.clone();
                let opts = [
                    ("clean".to_string(), "Clean"),
                    ("pop".to_string(), "Pop"),
                    ("boxed".to_string(), "Boxed"),
                    ("custom".to_string(), "Custom"),
                ];
                if ui::segmented(ui, "subs-style", &opts, &mut preset, 0).changed() {
                    self.set_style(st, &preset);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = S2;
                    let mut size = st.ed.subtitles.style.size as f64;
                    if ui::spin(ui, &mut size, 20.0, 140.0, 0, " px", 82.0).changed() {
                        st.ed.subtitles.style.size = size.round() as i32;
                        self.changed(st);
                    }
                    let mut font = st.ed.subtitles.style.font.clone();
                    let fonts = [("Inter".to_string(), "Inter"), ("Anton".to_string(), "Anton")];
                    let w = ui.available_width() - 16.0;
                    if ui::combo(ui, "subs-font", &mut font, &fonts, w).changed() {
                        st.ed.subtitles.style.font = font;
                        self.changed(st);
                    }
                });
                ui::label(ui, "Text", Role::Muted);
                if let Some(col) = swatches(ui, "text", &st.ed.subtitles.style.color) {
                    st.ed.subtitles.style.color = col;
                    self.changed(st);
                }
                ui::label(ui, "Spoken word", Role::Muted);
                if let Some(col) = swatches(ui, "spoken", &st.ed.highlight_color) {
                    st.ed.highlight_color = col;
                    self.changed(st);
                }
                ui::label(ui, "Words at a time", Role::Muted);
                let opts: Vec<(String, &str)> = MAX_WORDS.iter().map(|(k, t)| (k.to_string(), *t)).collect();
                let mut mw = st.ed.subtitles.max_words.to_string();
                let r = ui::segmented(ui, "subs-max-words", &opts, &mut mw, 0);
                self.max_words_rect = r.rect;
                let r = r.on_hover_text("The most words shown at once: a long line is shown a few words at a time as they're said");
                if r.changed() {
                    st.ed.subtitles.max_words = mw.parse().unwrap_or(0);
                    self.changed(st);
                }
                ui::label(ui, "Position the captions in Layout", Role::Muted);
            });
            p.section(Some("Censor"), |ui| {
                let mut v = st.ed.subtitles.censor_audio;
                if ui.add(Toggle::new(&mut v).text("Bleep censored words")).changed() {
                    st.ed.subtitles.censor_audio = v;
                    self.changed(st);
                }
            });
        });
    }

    fn editor(&mut self, ui: &mut Ui, st: &mut AppState, shell: &mut Shell, columns: usize) {
        let has = self.seg(st).is_some();
        let resp = egui::Frame::new()
            .fill(c::CARD)
            .stroke(Stroke::new(1.0, c::HAIR))
            .corner_radius(12)
            .inner_margin(egui::Margin { left: S5 as i8, right: S5 as i8, top: S4 as i8, bottom: S5 as i8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = S3;
                ui.add_enabled_ui(has, |ui| self.editor_body(ui, st, shell, columns));
            });
        self.editor_h = resp.response.rect.height();
    }

    fn editor_body(&mut self, ui: &mut Ui, st: &mut AppState, shell: &mut Shell, columns: usize) {
        let seg = self.seg(st).cloned();
        let has = seg.is_some();
        ui.horizontal(|ui| {
            ui::section(ui, "Caption");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(s) = &seg {
                    ui::label(ui, format!("{:.2} s", s.end - s.start), Role::Mono);
                }
            });
        });
        // caption text: Enter confirms, Esc reverts
        let te = egui::TextEdit::singleline(&mut self.ed_text)
            .id_salt("subs-caption-text")
            .hint_text(egui::RichText::new("What was actually said").color(c::MUTED))
            .desired_width(f32::INFINITY)
            .min_size(vec2(0.0, 36.0))
            .margin(egui::Margin::symmetric(10, 9))
            .font(theme::ui_font(13.0, Weight::Regular))
            .text_color(c::TEXT)
            .background_color(c::RAISED)
            .align(egui::Align2::LEFT_CENTER);
        let r = ui
            .scope(|ui| {
                let v = &mut ui.visuals_mut().widgets;
                for w in [&mut v.inactive, &mut v.hovered, &mut v.active] {
                    w.corner_radius = egui::CornerRadius::same(8);
                    w.bg_stroke = Stroke::new(1.0, c::HAIR);
                }
                ui.add(te)
            })
            .inner;
        self.rects.text = r.rect;
        if self.focus_text && has {
            self.focus_text = false;
            r.request_focus();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                let n = self.ed_text.chars().count();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                state.store(ui.ctx(), r.id);
            }
        }
        if r.changed() {
            self.type_at = Some(Instant::now() + TYPE_DELAY);
        }
        if r.lost_focus() {
            let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            if esc {
                self.text_escape(st);
            } else if enter {
                self.commit_text(st);
                self.list_focus = true;
            }
        }
        // start / end
        for (name, which, key) in [("Start", "start", "["), ("End", "end", "]")] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = S2;
                let (lr, _) = ui.allocate_exact_size(vec2(40.0, 30.0), Sense::hover());
                ui.painter().text(
                    pos2(lr.left(), lr.center().y),
                    egui::Align2::LEFT_CENTER,
                    name,
                    Role::Secondary.font(),
                    if has { c::SECONDARY } else { c::DISABLED },
                );
                let bw = ui::text_width(ui, "To playhead", &theme::ui_font(13.0, Weight::Regular)) + 24.0;
                let w = ui.available_width() - bw - S2;
                let v = if which == "start" { self.ed_start } else { self.ed_end };
                let tip = "Arrow keys or the mouse wheel move it in 0.05 s steps";
                if let Some(v) = time_spin(ui, which, v, w, tip) {
                    let v = r2(v);
                    if which == "start" {
                        self.ed_start = v;
                    } else {
                        self.ed_end = v;
                    }
                    self.time_set(st, which, v);
                }
                if ui
                    .add(Button::ghost("To playhead").tooltip(format!("Set the {} to the playhead  ({key})", name.to_lowercase())))
                    .clicked()
                {
                    self.time_playhead(st, which);
                }
            });
        }
        let base = match &seg {
            None => "",
            Some(s) if seg_edit(st, s).is_some() => "Times are in your edit, like the player",
            Some(_) => "Outside your cut: times are in the original recording",
        };
        if !base.is_empty() {
            ui::label(ui, base, Role::Muted);
        }
        // voice
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = S2;
            let (lr, _) = ui.allocate_exact_size(vec2(40.0, 30.0), Sense::hover());
            ui.painter().text(
                pos2(lr.left(), lr.center().y),
                egui::Align2::LEFT_CENTER,
                "Voice",
                Role::Secondary.font(),
                if has { c::SECONDARY } else { c::DISABLED },
            );
            let speakers: Vec<String> = if seg.is_some() { st.tr.as_ref().map(|t| t.speakers.clone()).unwrap_or_default() } else { vec![] };
            let labels: Vec<String> = speakers.iter().map(|s| speaker_label(st, s)).collect();
            let opts: Vec<(String, &str)> = speakers.iter().cloned().zip(labels.iter().map(String::as_str)).collect();
            let mut cur = match &seg {
                Some(s) if speakers.contains(&s.speaker) => s.speaker.clone(),
                _ => speakers.first().cloned().unwrap_or_default(),
            };
            let w = ui.available_width() - 16.0;
            if ui::combo(ui, "subs-voice", &mut cur, &opts, w).changed() {
                self.speaker_changed(st, &cur);
            }
        });
        let mut show = seg.as_ref().map(|s| !s.hidden).unwrap_or(false);
        if ui.add(Toggle::new(&mut show).text("Show this caption")).changed() {
            self.show_toggled(st, show);
        }
        // actions, in n columns packed to the left (two columns share the width when narrow)
        let acts: [(&str, &str, &str); 6] = [
            ("play", "Play caption", "Play just this caption"),
            ("scissors", "Split here", "Split at the playhead  (S)"),
            ("chevron-down", "Merge next", "Join with the next caption"),
            ("x", "Censor…", "Pick words to hide and bleep"),
            ("mic", "Listen again", "Re-transcribe only this caption"),
            ("trash-2", "Delete", "Delete caption  (Delete)"),
        ];
        let font = theme::ui_font(13.0, Weight::Regular);
        let nat: Vec<f32> = acts.iter().map(|a| ui::text_width(ui, a.1, &font) + 22.0 + 24.0).collect();
        let total = ui.available_width();
        let col_w: Vec<f32> = if columns == 2 {
            vec![(total - S1) / 2.0; 2]
        } else {
            (0..columns).map(|cix| (0..acts.len()).filter(|k| k % columns == cix).map(|k| nat[k]).fold(0.0, f32::max)).collect()
        };
        let mut clicked = None;
        let top = ui.cursor().top();
        let left = ui.cursor().left();
        let rows = acts.len().div_ceil(columns);
        let (_, _) = ui.allocate_exact_size(vec2(total, rows as f32 * 30.0 + (rows - 1) as f32 * S1), Sense::hover());
        for (k, (icon, text, tip)) in acts.iter().enumerate() {
            let (r, cix) = (k / columns, k % columns);
            let x = left + col_w[..cix].iter().sum::<f32>() + cix as f32 * S1;
            let rect = Rect::from_min_size(pos2(x, top + r as f32 * (30.0 + S1)), vec2(nat[k].min(col_w[cix]), 30.0));
            if ui.put(rect, Button::ghost(*text).icon(icon).tooltip(*tip)).clicked() {
                clicked = Some(k);
            }
        }
        match clicked {
            Some(0) => self.play_line(st),
            Some(1) => self.split(st),
            Some(2) => self.merge(st),
            Some(3) => self.censor_words(st),
            Some(4) => self.retranscribe(st, shell),
            Some(5) => self.delete(st),
            _ => {}
        }
        if !self.ed_hint.is_empty() {
            ui::label_wrap(ui, &self.ed_hint, Role::Muted);
        }
    }

    fn track_box(&mut self, ui: &mut Ui, st: &mut AppState) {
        let evs = self.track.show(ui);
        for e in evs {
            self.list_focus = true;
            match e {
                TrackEvent::Seek(t) => self.seek_edit(st, t),
                TrackEvent::Selected(i) => self.select(st, i, false),
                TrackEvent::Retimed(i, a, b) => self.track_retimed(st, i, a, b),
            }
        }
        ui.add_space(S2);
        ui.horizontal(|ui| {
            let mw = if self.msg.is_empty() { 0.0 } else { ui::text_width(ui, &self.msg, &Role::Secondary.font()) + S2 };
            let w = (ui.available_width() - mw).max(10.0);
            ui.allocate_ui(vec2(w, 18.0), |ui| {
                ui.set_width(w);
                ui::label_wrap(ui, &self.preview_label, Role::Muted);
            });
            if !self.msg.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| ui::label(ui, &self.msg, Role::Secondary));
            }
        });
    }

    fn list_box(&mut self, ui: &mut Ui, st: &mut AppState) {
        ui.add_space(S1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = S2;
            ui.set_height(32.0);
            ui::section(ui, "Captions");
            let count = self.count_label(st);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let r = ui.add(Button::new("New caption").icon("plus").tooltip("Add a caption starting at the playhead  (N)"));
                self.rects.new_caption = r.rect;
                if r.clicked() {
                    self.add_caption(st);
                }
                ui.add_space(S3 - S2);
                let mut v = self.include_outside;
                let r = ui.add(Toggle::new(&mut v).text("Include lines outside the cut"));
                self.rects.outside = r.rect;
                if r.changed() {
                    self.include_outside = v;
                    self.fill_table(st);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let w = ui.available_width();
                    let g = ui::elided(ui, &count, Role::Mono.font(), c::MUTED, w);
                    let (r, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
                    ui.painter().galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g, c::MUTED);
                });
            });
        });
        ui.add_space(S2);
        self.table(ui, st);
    }

    fn table(&mut self, ui: &mut Ui, st: &mut AppState) {
        let full = ui.available_rect_before_wrap();
        let header = Rect::from_min_size(full.min, vec2(full.width(), HEADER_H));
        ui.allocate_rect(header, Sense::hover());
        let p = ui.painter();
        let hx = [full.left() + COLS[0], full.left() + COLS[0] + COLS[1], full.left() + COLS[0] + COLS[1] + COLS[2]];
        let hf = theme::ui_font(12.0, Weight::SemiBold);
        for (x, t) in hx.iter().zip(["Time", "Voice", "Caption"]) {
            p.text(pos2(x + 8.0, header.center().y), egui::Align2::LEFT_CENTER, t, hf.clone(), c::MUTED);
        }
        p.line_segment(
            [pos2(header.left(), header.bottom() - 0.5), pos2(header.right(), header.bottom() - 0.5)],
            Stroke::new(1.0, c::HAIR),
        );
        self.rects.rows.clear();
        let Some(tr) = st.tr.clone() else { return };
        let mut action: Option<(&str, usize)> = None;
        let mut commit_inline: Option<(usize, String)> = None;
        let mut cancel_inline = false;
        let scroll_to = std::mem::take(&mut self.scroll_to_sel);
        egui::ScrollArea::vertical().id_salt("subs-list").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            for &i in &self.rows {
                let Some(seg) = tr.segments.get(i) else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
                self.rects.rows.push((i, rect));
                let sel = i as isize == self.sel;
                if sel && scroll_to {
                    ui.scroll_to_rect(rect, None);
                }
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let p = ui.painter();
                if sel {
                    p.rect_filled(rect, 0.0, c::RAISED);
                } else if resp.hovered() {
                    p.rect_filled(rect, 0.0, c::CARD);
                }
                p.line_segment(
                    [pos2(rect.left(), rect.bottom() - 0.5), pos2(rect.right(), rect.bottom() - 0.5)],
                    Stroke::new(1.0, c::HAIR),
                );
                // show toggle
                let sw = Rect::from_min_size(pos2(rect.left() + 10.0, rect.center().y - 9.0), vec2(30.0, 18.0));
                let sw_hit = sw.expand(3.0);
                let hover_sw = resp.hover_pos().map(|q| sw_hit.contains(q)).unwrap_or(false);
                ui::paint_switch(p, sw, if seg.hidden { 0.0 } else { 1.0 }, true, hover_sw, false);
                let x1 = rect.left() + COLS[0] + 8.0;
                p.text(pos2(x1, rect.center().y), egui::Align2::LEFT_CENTER, time_label(st, seg), theme::mono_font(12.0), c::SECONDARY);
                let x2 = rect.left() + COLS[0] + COLS[1] + 8.0;
                let g = ui::elided(ui, &speaker_label(st, &seg.speaker), Role::Body.font(), c::TEXT, COLS[2] - 16.0);
                p.galley(pos2(x2, rect.center().y - g.size().y / 2.0), g, c::TEXT);
                let x3 = rect.left() + COLS[0] + COLS[1] + COLS[2] + 8.0;
                let cell = Rect::from_min_max(pos2(x3 - 8.0, rect.top()), rect.max);
                let editing_here = matches!(&self.row_edit, Some((k, ..)) if *k == i);
                if editing_here {
                    let (_, text, focus) = self.row_edit.as_mut().unwrap();
                    let te = egui::TextEdit::singleline(text)
                        .id_salt(("subs-row-edit", i))
                        .desired_width(cell.width() - 8.0)
                        .font(theme::ui_font(13.0, Weight::Regular))
                        .margin(egui::Margin::symmetric(8, 6))
                        .background_color(c::RAISED_HI);
                    let r = ui.put(cell.shrink2(vec2(0.0, 3.0)), te);
                    if *focus {
                        *focus = false;
                        r.request_focus();
                    }
                    if r.lost_focus() {
                        if ui.input(|i| i.key_pressed(Key::Escape)) {
                            cancel_inline = true;
                        } else {
                            commit_inline = Some((i, text.clone()));
                        }
                    }
                } else {
                    let now = i as isize == self.track.now;
                    let col = if seg.hidden { c::MUTED } else { c::TEXT };
                    let font = theme::ui_font(13.0, if now { Weight::SemiBold } else { Weight::Regular });
                    let g = ui::elided(ui, &seg_text(seg), font, col, cell.width() - 16.0);
                    p.galley(pos2(x3, rect.center().y - g.size().y / 2.0), g, col);
                }
                let resp = if editing_here {
                    resp
                } else {
                    resp.on_hover_text_at_pointer("Double-click to edit the text here, or use the caption panel")
                };
                if resp.clicked() || resp.double_clicked() {
                    let q = resp.interact_pointer_pos().unwrap_or(rect.center());
                    if sw_hit.contains(q) && resp.clicked() {
                        action = Some(("toggle", i));
                    } else if resp.double_clicked() && q.x >= cell.left() {
                        action = Some(("edit", i));
                    } else if resp.clicked() {
                        action = Some(("select", i));
                    }
                }
            }
        });
        if cancel_inline {
            self.row_edit = None;
            self.list_focus = true;
        }
        if let Some((i, text)) = commit_inline {
            self.row_edit = None;
            self.list_focus = true;
            self.item_text_changed(st, i, &text);
        }
        match action {
            Some(("select", i)) => {
                self.list_focus = true;
                ui.ctx().memory_mut(|m| m.stop_text_input());
                if i as isize != self.sel {
                    self.select(st, i, true);
                }
            }
            Some(("toggle", i)) => {
                self.list_focus = true;
                if i as isize != self.sel {
                    self.select(st, i, true);
                }
                self.item_toggled(st, i);
            }
            Some(("edit", i)) => {
                self.list_focus = true;
                if i as isize != self.sel {
                    self.select(st, i, true);
                }
                let text = st.tr.as_ref().and_then(|t| t.segments.get(i)).map(seg_text).unwrap_or_default();
                self.row_edit = Some((i, text, true));
            }
            _ => {}
        }
    }

    /// The show tick of a list row.
    fn item_toggled(&mut self, st: &mut AppState, i: usize) {
        st.begin_step();
        let Some(seg) = st.tr.as_mut().and_then(|t| t.segments.get_mut(i)) else { return };
        seg.hidden = !seg.hidden;
        seg.edited = true;
        self.sel = i as isize;
        self.save(st, true, true);
    }

    /// Text edited in the list.
    fn item_text_changed(&mut self, st: &mut AppState, i: usize, text: &str) {
        st.begin_step();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(seg) = st.tr.as_mut().and_then(|t| t.segments.get_mut(i)) else { return };
        if !text.is_empty() && text != seg_text(seg) {
            tm::retime_words(seg, &text);
            seg.edited = true;
            if seg.voice_line {
                seg.hidden = false;
            }
            self.sel = i as isize;
            self.save(st, true, true);
        }
    }

    fn censor_dialog(&mut self, ctx: &egui::Context, st: &mut AppState) {
        let Some(d) = self.censor.as_mut() else { return };
        let mut result: Option<bool> = None;
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Modal::new(egui::Id::unique("subs-censor"))
            .area(egui::Modal::default_area(egui::Id::unique("subs-censor")).fade_in(false))
            .frame(theme::dialog_frame())
            .backdrop_color(Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = vec2(S2, S4);
                ui.set_max_width(520.0);
                ui::label(ui, "Censor words", Role::Title);
                ui::label_wrap(ui, "Pick the words to hide. They are bleeped in the audio when bleeping is on", Role::Secondary);
                let chips = &mut self.rects.censor_chips;
                chips.clear();
                egui::Grid::new("subs-censor-grid").spacing(vec2(S2, S2)).show(ui, |ui| {
                    for (k, w) in d.words.iter().enumerate() {
                        let font = theme::ui_font(13.0, Weight::Regular);
                        let tw = ui::text_width(ui, w, &font) + 20.0;
                        let (r, resp) = ui.allocate_exact_size(vec2(tw.max(40.0), 28.0), Sense::click());
                        chips.push(r);
                        let on = d.on[k];
                        let bg = if on {
                            c::SEG_CHECKED
                        } else if resp.hovered() {
                            c::RAISED_HI
                        } else {
                            c::RAISED
                        };
                        ui.painter().rect_filled(r, 6.0, bg);
                        if on {
                            ui.painter().rect_stroke(r, 6.0, Stroke::new(1.0, c::ACCENT), StrokeKind::Inside);
                        }
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            w,
                            font,
                            if on || resp.hovered() { c::TEXT } else { c::SECONDARY },
                        );
                        if resp.clicked() {
                            d.on[k] = !on;
                        }
                        if k % 6 == 5 {
                            ui.end_row();
                        }
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new("Cancel").min_width(72.0)).clicked() {
                        result = Some(false);
                    }
                    if ui.add(Button::primary("OK").min_width(72.0)).clicked() {
                        result = Some(true);
                    }
                });
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    result = Some(true);
                }
            });
        if esc && result.is_none() {
            result = Some(false);
        }
        match result {
            Some(true) => {
                let d = self.censor.take().unwrap();
                self.censor_accepted(st, d);
            }
            Some(false) => self.censor = None,
            None => {}
        }
    }
}

/// Seconds field (2 decimals, " s", left-aligned, no keyboard tracking): the value
/// is applied on Enter or when the field loses focus; Up/Down and the mouse wheel step 0.05 s.
fn time_spin(ui: &mut Ui, salt: &str, value: f64, width: f32, tip: &str) -> Option<f64> {
    let id = ui.make_persistent_id(("subs-time", salt));
    let te_id = id.with("te");
    let focused = ui.memory(|m| m.has_focus(te_id));
    let shown = format!("{value:.2} s");
    let mut buf = if focused { ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| shown.clone()) } else { shown.clone() };
    let mut out = None;
    let step = |v: f64, k: f64| ((v + 0.05 * k) * 100.0).round() / 100.0;
    if focused {
        let (up, down) = ui.input_mut(|i| {
            (i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp), i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown))
        });
        if up + down > 0 {
            let v = step(value, up as f64 - down as f64).max(0.0);
            buf = format!("{v:.2} s");
            out = Some(v);
        }
    }
    let te = egui::TextEdit::singleline(&mut buf)
        .id(te_id)
        .desired_width(width)
        .min_size(vec2(0.0, 30.0))
        .margin(egui::Margin::symmetric(8, 7))
        .font(theme::ui_font(13.0, Weight::Regular))
        .text_color(c::TEXT)
        .background_color(c::RAISED)
        .align(egui::Align2::LEFT_CENTER);
    let r = ui
        .scope(|ui| {
            let v = &mut ui.visuals_mut().widgets;
            for w in [&mut v.inactive, &mut v.hovered, &mut v.active] {
                w.corner_radius = egui::CornerRadius::same(8);
                w.bg_stroke = Stroke::new(1.0, c::HAIR);
            }
            ui.visuals_mut().selection.stroke = Stroke::new(1.0, c::HAIR_STRONG);
            ui.add(te)
        })
        .inner
        .on_hover_text(tip);
    if r.hovered() {
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
            let v = step(out.unwrap_or(value), wheel.signum() as f64).max(0.0);
            buf = format!("{v:.2} s");
            out = Some(v);
        }
    }
    if r.lost_focus() && !ui.input(|i| i.key_pressed(Key::Escape)) {
        let t = buf.trim().trim_end_matches('s').trim().replace(',', ".");
        if let Ok(v) = t.parse::<f64>() {
            let v = r2(v.clamp(0.0, 1e6));
            if (v - value).abs() > 1e-9 {
                out = Some(v);
            }
        }
        ui.data_mut(|d| d.remove::<String>(id));
    } else if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, buf));
    }
    out
}

/// A row of colour swatches; returns the picked colour.
fn swatches(ui: &mut Ui, id: &str, current: &str) -> Option<String> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = S2;
        for col in theme::SWATCHES {
            let checked = col.eq_ignore_ascii_case(current);
            let r = ui.push_id((id, col), |ui| ui::swatch(ui, theme::parse_hex(col).unwrap_or(c::TEXT), checked, col)).inner;
            if r.clicked() {
                out = Some(col.to_string());
            }
        }
    });
    out
}

impl Page for SubtitlesPage {
    fn on_state(&mut self, ev: &StateEvent, st: &mut AppState, _shell: &mut Shell) {
        match ev {
            StateEvent::ProjectChanged => {
                self.editing = None;
                self.row_edit = None;
                self.refresh(st);
                if self.visible {
                    self.need_show = true;
                }
            }
            StateEvent::TranscriptChanged => {
                if self.own_saves > 0 {
                    self.own_saves -= 1;
                } else {
                    // a transcription finished (or the transcript was replaced): new captions in the preview
                    self.refresh(st);
                    self.update_subs(st);
                }
            }
            StateEvent::Restored(_) => {
                self.type_at = None;
                self.editing = None;
                self.row_edit = None;
                self.refresh(st);
                self.update_subs(st);
            }
            _ => {}
        }
    }

    fn on_job(&mut self, n: &JobNotice, _st: &mut AppState, _shell: &mut Shell) {
        if n.progress.is_none() {
            self.cancelling = false;
        }
        self.progress = n.progress;
        self.status = n.msg.clone();
    }

    fn on_show(&mut self, _st: &mut AppState, _shell: &mut Shell) {
        self.visible = true;
        self.need_show = true;
    }

    fn on_hide(&mut self, st: &mut AppState, _shell: &mut Shell) {
        self.visible = false;
        self.commit_text(st);
        self.row_edit = None;
        self.stop_at = None;
        self.player.pause();
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut PageCtx) {
        let st = &mut *cx.st;
        let shell = &mut *cx.shell;
        self.visible = true;
        if self.need_show {
            self.need_show = false;
            self.do_show(st);
        }
        // timers
        let now = Instant::now();
        if self.type_at.is_some_and(|t| now >= t) {
            self.commit_text(st);
        }
        if self.msg_until.is_some_and(|t| now >= t) {
            self.msg.clear();
            self.msg_until = None;
        }
        for t in [self.type_at, self.msg_until].into_iter().flatten() {
            ui.ctx().request_repaint_after(t.saturating_duration_since(now));
        }
        for e in self.player.take_events() {
            match e {
                PlayerEvent::Position(t) => self.on_pos(t),
                // a freshly loaded preview: ask for its frame again, so a paused first frame is drawn even
                // when the preview's size changed with the shape (the player keeps the last frame otherwise)
                PlayerEvent::Loaded(_) => {
                    if !self.player.is_playing() {
                        self.player.seek(self.playhead);
                    }
                }
            }
        }
        self.settle_seek(ui.ctx());
        if ui.ctx().memory(|m| m.focused().is_some()) {
            self.list_focus = false;
        }
        self.shortcuts(ui, st);

        let full = ui.available_rect_before_wrap();
        let side = Rect::from_min_size(full.min, vec2(SIDE_W, full.height()));
        let mut side_ui = ui.new_child(UiBuilder::new().max_rect(side).layout(Layout::top_down(Align::Min)));
        self.side(&mut side_ui, st, shell);
        ui.painter()
            .line_segment([pos2(side.right() - 0.5, side.top()), pos2(side.right() - 0.5, side.bottom())], Stroke::new(1.0, c::HAIR));

        let centre = Rect::from_min_max(pos2(side.right() + S6, full.top() + S4 + 4.0), pos2(full.right() - S6, full.bottom() - S4));
        let track_h = TRACK_H + S2 + 18.0;
        let child = |ui: &mut Ui, r: Rect| ui.new_child(UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Min)));
        if self.portrait {
            // tall previews take the full height on the left, the editor and the list on the right
            let top_h = centre.height() - track_h - S3;
            let col_w = ((centre.width() - S4) * 4.0 / 9.0).min(centre.width() - S4 - EDITOR_W - 10.0);
            let left = Rect::from_min_size(centre.min, vec2(col_w, top_h));
            let right_x = left.right() + S4;
            let ed_h = self.editor_h.min((top_h - S3 - 190.0).max(220.0));
            let ed_r = Rect::from_min_max(pos2(right_x, centre.top()), pos2(centre.right(), centre.top() + ed_h));
            let list_r = Rect::from_min_max(pos2(right_x, ed_r.bottom() + S3), pos2(centre.right(), left.bottom()));
            let track_r = Rect::from_min_max(pos2(centre.left(), centre.bottom() - track_h), centre.max);
            self.player.show(&mut child(ui, left), cx.frame);
            let mut eu = child(ui, ed_r);
            egui::ScrollArea::vertical().id_salt("subs-editor").auto_shrink([false, true]).max_height(ed_h).show(&mut eu, |ui| {
                self.editor(ui, st, shell, 3);
            });
            self.list_box(&mut child(ui, list_r), st);
            self.track_box(&mut child(ui, track_r), st);
        } else {
            // a wide preview sits on top next to the editor, the list below at full width
            let rest = centre.height() - track_h - 2.0 * S3;
            let row0 = rest * 5.0 / 8.0;
            let ed_w = EDITOR_W + 10.0;
            let pl_r = Rect::from_min_size(centre.min, vec2(centre.width() - S4 - ed_w, row0));
            let ed_r = Rect::from_min_size(pos2(pl_r.right() + S4, centre.top()), vec2(ed_w, row0));
            let track_r = Rect::from_min_size(pos2(centre.left(), pl_r.bottom() + S3), vec2(centre.width(), track_h));
            let list_r = Rect::from_min_max(pos2(centre.left(), track_r.bottom() + S3), centre.max);
            self.player.show(&mut child(ui, pl_r), cx.frame);
            let mut eu = child(ui, ed_r);
            egui::ScrollArea::vertical().id_salt("subs-editor").auto_shrink([false, true]).max_height(row0).show(&mut eu, |ui| {
                ui.set_max_width(EDITOR_W);
                self.editor(ui, st, shell, 2);
            });
            self.track_box(&mut child(ui, track_r), st);
            self.list_box(&mut child(ui, list_r), st);
        }
        ui.allocate_rect(full, Sense::hover());
        let ctx = ui.ctx().clone();
        self.censor_dialog(&ctx, st);
        if self.player.is_playing() || self.stop_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    fn stop_players(&mut self) {
        self.player.stop();
        self.loaded_sig = None;
        self.stop_at = None;
    }

    fn shutdown(&mut self) {
        self.player.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formats() {
        assert_eq!(fmt_t(0.25), "0:00.25");
        assert_eq!(fmt_t1(3.33), "0:03.3");
        assert_eq!(fmt_t1(75.56), "1:15.5");
        assert_eq!(fmt_t(-1.0), "0:00.00");
    }
}
