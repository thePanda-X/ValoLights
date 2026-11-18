//! Shared application state for the open project, with app-wide undo/redo.
//!
//! Changes are announced as [`StateEvent`]s: every mutation pushes events into a queue; the main window drains
//! it once per frame ([`AppState::take_events`]) and hands each event to every page. Timers (debounced
//! save, undo grouping) are deadlines checked by [`AppState::tick`], which also says when to wake up.
#![allow(dead_code)]

use crate::backend;
use serde_json::Value;
use std::time::{Duration, Instant};
use vl_core::config;
use vl_core::models::{Edits, Events, Kill, Part};
use vl_core::projects::ProjectHandle;
use vl_core::transcript::Transcript;

pub const HISTORY_LIMIT: usize = 200;
/// Changes closer together than this become one undo step (slider drags, typing).
pub const GROUP: Duration = Duration::from_millis(600);
/// Edits are written this long after the last change.
pub const SAVE_DELAY: Duration = Duration::from_millis(400);

/// What changed -> how the step is named in "Undo …".
const EDIT_LABELS: &[(&str, &str)] = &[
    ("parts", "trim"),
    ("quick_cut", "trim"),
    ("kills_disabled", "kill selection"),
    ("layout", "layout"),
    ("format", "layout"),
    ("killfeed", "kill feed"),
    ("effects", "effects"),
    ("subtitles", "subtitle style"),
    ("highlight_color", "subtitle style"),
    ("music", "music"),
    ("vertical", "layout"),
];

/// Notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum StateEvent {
    /// a project was opened or closed
    ProjectChanged,
    EditsChanged,
    EventsChanged,
    TranscriptChanged,
    /// undo/redo replaced the state; the text is "Undid …" / "Redid …"
    Restored(String),
    HistoryChanged,
    /// short feedback shown in the top bar (one place for every page)
    Message(String),
}

#[derive(Debug, Clone)]
struct Snapshot {
    edits: String,
    transcript: Option<String>,
    events: Option<String>,
    me: Vec<String>,
    label: String,
}

impl Snapshot {
    fn same(&self, o: &Snapshot) -> bool {
        (&self.edits, &self.transcript, &self.events, &self.me) == (&o.edits, &o.transcript, &o.events, &o.me)
    }
}

fn describe(a: &Snapshot, b: &Snapshot) -> String {
    if a.transcript != b.transcript {
        return "caption edit".into();
    }
    if a.events != b.events || a.me != b.me {
        return "kill list change".into();
    }
    let (Ok(da), Ok(db)) = (serde_json::from_str::<Value>(&a.edits), serde_json::from_str::<Value>(&b.edits)) else {
        return "change".into();
    };
    if da.get("music") != db.get("music") && da.get("parts") != db.get("parts") {
        return "beat sync".into();
    }
    for (key, name) in EDIT_LABELS {
        if da.get(key) != db.get(key) {
            return format!("{name} change");
        }
    }
    "change".into()
}

pub struct AppState {
    pub h: Option<ProjectHandle>,
    pub ev: Option<Events>,
    pub ed: Edits,
    pub tr: Option<Transcript>,
    /// QA mode: never write projects or settings.
    pub read_only: bool,
    hist: Vec<Snapshot>,
    pos: isize,
    restoring: bool,
    save_at: Option<Instant>,
    record_at: Option<Instant>,
    events: Vec<StateEvent>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(false)
    }
}

fn r3(v: f64) -> i64 {
    (v * 1000.0).round() as i64
}

impl AppState {
    pub fn new(read_only: bool) -> Self {
        AppState {
            h: None,
            ev: None,
            ed: Edits::default(),
            tr: None,
            read_only,
            hist: vec![],
            pos: -1,
            restoring: false,
            save_at: None,
            record_at: None,
            events: vec![],
        }
    }

    fn emit(&mut self, e: StateEvent) {
        self.events.push(e);
    }

    /// Notifications since the last call (the main window dispatches them to the pages).
    pub fn take_events(&mut self) -> Vec<StateEvent> {
        std::mem::take(&mut self.events)
    }

    /// Run due timers (save, undo grouping). Returns how long until the next one is due.
    pub fn tick(&mut self) -> Option<Duration> {
        let now = Instant::now();
        if self.save_at.is_some_and(|t| now >= t) {
            self.save_now();
        }
        if self.record_at.is_some_and(|t| now >= t) {
            self.record();
        }
        [self.save_at, self.record_at].iter().flatten().map(|t| t.saturating_duration_since(now)).min()
    }

    // ---------------------------------------------------------------- files (all writes go through here)
    fn write_edits(&self) {
        if let (Some(h), false) = (&self.h, self.read_only) {
            if let Err(e) = h.save_edits(&self.ed) {
                log::error!("saving edits failed: {e}");
            }
        }
    }
    fn write_events(&self) {
        if let (Some(h), Some(ev), false) = (&self.h, &self.ev, self.read_only) {
            if let Err(e) = h.save_events(ev) {
                log::error!("saving events failed: {e}");
            }
        }
    }
    fn write_project(&self) {
        if let (Some(h), false) = (&self.h, self.read_only) {
            if let Err(e) = h.save() {
                log::error!("saving project failed: {e}");
            }
        }
    }
    fn write_transcript(&self) {
        if let (Some(h), Some(tr), false) = (&self.h, &self.tr, self.read_only) {
            if let Err(e) = vl_core::models::save_json(tr, &h.transcript_path()) {
                log::error!("saving transcript failed: {e}");
            }
        }
    }

    // ---------------------------------------------------------------- project
    pub fn open(&mut self, h: ProjectHandle) {
        self.ev = h.events();
        self.ed = h.edits();
        self.tr = if h.transcript_path().exists() { h.transcript() } else { None };
        self.h = Some(h);
        if self.ev.is_some() && self.ed.parts.is_empty() {
            let q = if self.ed.quick_cut.is_empty() { "kills".to_string() } else { self.ed.quick_cut.clone() };
            self.apply_quick_cut(&q, false);
        }
        self.record_at = None;
        self.hist = vec![self.snapshot()];
        self.pos = 0;
        self.emit(StateEvent::HistoryChanged);
        self.emit(StateEvent::ProjectChanged);
    }

    pub fn close(&mut self) {
        self.save_now();
        self.h = None;
        self.ev = None;
        self.ed = Edits::default();
        self.tr = None;
        self.hist.clear();
        self.pos = -1;
        self.record_at = None;
        self.emit(StateEvent::HistoryChanged);
        self.emit(StateEvent::ProjectChanged);
    }

    pub fn pid(&self) -> Option<&str> {
        self.h.as_ref().map(|h| h.project.id.as_str())
    }

    pub fn duration(&self) -> f64 {
        self.h.as_ref().map(|h| h.project.source.duration).unwrap_or(0.0)
    }

    pub fn me(&self) -> Vec<String> {
        self.h.as_ref().map(|h| h.project.identity.me.clone()).unwrap_or_default()
    }

    /// Change who "me" is. A hand-trimmed cut is kept; otherwise the quick cut is rebuilt.
    pub fn set_me(&mut self, ids: Vec<String>) {
        if self.h.is_none() || self.ev.is_none() {
            return;
        }
        let custom = self.cut_is_custom(); // judged against the current identity, before anything changes
        self.begin_step();
        if let Some(h) = self.h.as_mut() {
            h.project.identity.me = ids.clone();
            h.project.identity.confirmed = true;
        }
        if let Some(ev) = self.ev.as_mut() {
            for p in ev.players.iter_mut() {
                if ids.contains(&p.id) && p.me_hint.is_empty() {
                    p.me_hint = "user".into();
                } else if !ids.contains(&p.id) && p.me_hint == "user" {
                    p.me_hint.clear();
                }
            }
            // recompute round results relative to the new identity
            backend::finalize_identity(ev, &[]);
        }
        self.write_project();
        self.write_events();
        if custom {
            self.say("Kills updated · your trimmed cut was kept");
        } else {
            let q = self.ed.quick_cut.clone();
            self.apply_quick_cut(&q, false);
        }
        self.emit(StateEvent::EventsChanged);
        self.end_step();
    }

    pub fn reload_events(&mut self) {
        if let Some(h) = &self.h {
            self.ev = h.events();
            self.emit(StateEvent::EventsChanged);
        }
    }

    /// Kill list edited in place (`st.ev`): write it and record an undo step.
    pub fn save_events(&mut self) {
        if self.h.is_some() && self.ev.is_some() {
            self.write_events();
            self.emit(StateEvent::EventsChanged);
            self.changed();
        }
    }

    // ---------------------------------------------------------------- edits
    /// Edits changed (`st.ed` mutated in place): save soon, notify, and record an undo step once things settle.
    pub fn touch(&mut self) {
        self.save_at = Some(Instant::now() + SAVE_DELAY);
        self.emit(StateEvent::EditsChanged);
        self.changed();
    }

    /// Write pending edits now.
    pub fn save_now(&mut self) {
        self.save_at = None;
        self.write_edits();
    }

    pub fn set_parts(&mut self, parts: Vec<Part>, record: bool) {
        if record {
            self.begin_step();
        }
        let mut parts = parts;
        parts.sort_by(|a, b| a.src_in.total_cmp(&b.src_in));
        self.ed.parts = parts;
        if record {
            self.touch();
            self.end_step();
        } else {
            self.save_at = Some(Instant::now() + SAVE_DELAY);
            self.emit(StateEvent::EditsChanged);
        }
    }

    pub fn apply_quick_cut(&mut self, name: &str, record: bool) {
        if self.ev.is_none() || self.h.is_none() {
            return;
        }
        let Some(parts) = self.quick_cut_parts(Some(name)) else {
            log::warn!("quick cut '{name}' unavailable");
            return;
        };
        self.ed.quick_cut = name.to_string();
        self.set_parts(parts, record);
    }

    /// Parts of a quick cut with the current settings (None when highlights are unavailable).
    pub fn quick_cut_parts(&self, name: Option<&str>) -> Option<Vec<Part>> {
        let name = name.unwrap_or(&self.ed.quick_cut);
        let cfg = config::load().highlights;
        if name == "smart" {
            let ev = self.smart_events()?;
            return backend::quick_cut(name, &ev, &self.me(), &cfg, self.duration(), &self.ed.kills_disabled);
        }
        let ev = self.ev.as_ref()?;
        backend::quick_cut(name, ev, &self.me(), &cfg, self.duration(), &self.ed.kills_disabled)
    }

    /// The Smart cut's sound pass has run for this clip (with the current signal version).
    pub fn smart_ready(&self) -> bool {
        self.ev.as_ref().and_then(|e| e.audio.as_ref()).map(|a| a.version >= backend::SMART_VERSION).unwrap_or(false)
    }

    /// The events as the Smart cut sees them: the transcript's funny / hype words are taken from the current
    /// transcript (it may have been made or edited after the sound pass).
    fn smart_events(&self) -> Option<Events> {
        let mut ev = self.ev.clone()?;
        if let (Some(a), Some(tr)) = (ev.audio.as_mut(), self.tr.as_ref()) {
            let words: Vec<(f64, String)> =
                tr.segments.iter().filter(|s| !s.hidden).flat_map(|s| s.words.iter().map(|w| (w.start, w.text.clone()))).collect();
            a.words = backend::smart_words(&words);
        }
        Some(ev)
    }

    /// The moments the Smart cut keeps, with their reasons (clip order).
    pub fn smart_moments(&self) -> Vec<backend::SmartMoment> {
        let Some(ev) = self.smart_events() else { return vec![] };
        backend::smart_moments(&ev, &self.me(), &config::load().highlights, self.duration(), &self.ed.kills_disabled)
    }

    /// The cut was trimmed by hand since its quick cut was applied (so it must never be rebuilt silently).
    pub fn cut_is_custom(&self) -> bool {
        let Some(want) = self.quick_cut_parts(None) else { return true };
        let want: Vec<(i64, i64)> = want.iter().map(|p| (r3(p.src_in), r3(p.src_out))).collect();
        let have: Vec<(i64, i64)> = self.ed.parts.iter().map(|p| (r3(p.src_in), r3(p.src_out))).collect();
        want != have
    }

    /// Short feedback in the top bar.
    pub fn say(&mut self, msg: impl Into<String>) {
        self.emit(StateEvent::Message(msg.into()));
    }

    pub fn quick_cut_duration(&self, name: &str) -> f64 {
        self.quick_cut_parts(Some(name)).map(|p| p.iter().map(Part::dur).sum()).unwrap_or(0.0)
    }

    /// Length of the exported video (transitions overlap neighbouring parts).
    pub fn kept_duration(&self) -> f64 {
        if self.ed.parts.is_empty() {
            return 0.0;
        }
        backend::edit_duration(&self.ed.parts, &self.ed.effects.transition)
    }

    pub fn my_kills(&self, include_disabled: bool) -> Vec<Kill> {
        let Some(ev) = &self.ev else { return vec![] };
        let cfg = config::load().highlights;
        backend::my_kills(ev, &self.me(), cfg.include_team_kills, if include_disabled { None } else { Some(&self.ed.kills_disabled) })
    }

    pub fn segments(&self) -> Vec<backend::HighlightSegment> {
        let Some(ev) = &self.ev else { return vec![] };
        backend::build_segments(ev, &self.me(), &config::load().highlights, self.duration(), &self.ed.kills_disabled)
    }

    /// Transcript edited in place (`st.tr`): write it and record an undo step.
    pub fn save_transcript(&mut self) {
        self.write_transcript();
        self.emit(StateEvent::TranscriptChanged);
        self.changed();
    }

    /// The transcription job wrote transcript.json: load it.
    pub fn load_transcript_from_disk(&mut self) {
        if let Some(h) = &self.h {
            self.tr = h.transcript();
        }
    }

    pub fn notify_transcript(&mut self) {
        self.emit(StateEvent::TranscriptChanged);
    }

    // ---------------------------------------------------------------- undo / redo
    /// Something undoable changed. Recorded as one step once changes pause for GROUP.
    pub fn changed(&mut self) {
        if self.h.is_some() && !self.restoring {
            self.record_at = Some(Instant::now() + GROUP);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            edits: serde_json::to_string(&self.ed).unwrap_or_default(),
            transcript: self.tr.as_ref().and_then(|t| serde_json::to_string(t).ok()),
            // the Smart cut's sound signals aren't an edit (and can be large): kept out of the history
            events: self.ev.as_ref().and_then(|e| {
                let mut e = e.clone();
                e.audio = None;
                serde_json::to_string(&e).ok()
            }),
            me: self.me(),
            label: String::new(),
        }
    }

    fn record(&mut self) {
        self.record_at = None;
        if self.h.is_none() || self.pos < 0 {
            return;
        }
        let mut snap = self.snapshot();
        let cur = &self.hist[self.pos as usize];
        if snap.same(cur) {
            return;
        }
        snap.label = describe(cur, &snap);
        self.hist.truncate(self.pos as usize + 1); // a new change drops the redo branch
        self.hist.push(snap);
        if self.hist.len() > HISTORY_LIMIT {
            self.hist.remove(0);
        }
        self.pos = self.hist.len() as isize - 1;
        self.emit(StateEvent::HistoryChanged);
    }

    /// Call before a discrete action (add, delete, split…): closes any pending group of edits.
    pub fn begin_step(&mut self) {
        self.flush_history();
    }

    /// Call after a discrete action: records it as its own undo step right away.
    pub fn end_step(&mut self) {
        if self.h.is_some() && !self.restoring {
            self.record();
        }
    }

    pub fn flush_history(&mut self) {
        if self.record_at.is_some() {
            self.record();
        }
    }

    fn grouping(&self) -> bool {
        self.record_at.is_some()
    }

    pub fn can_undo(&self) -> bool {
        self.pos > 0 || self.grouping()
    }

    pub fn can_redo(&self) -> bool {
        self.pos >= 0 && (self.pos as usize) + 1 < self.hist.len() && !self.grouping()
    }

    pub fn undo_label(&self) -> String {
        if self.grouping() {
            return "last change".into();
        }
        if self.pos > 0 {
            self.hist[self.pos as usize].label.clone()
        } else {
            String::new()
        }
    }

    pub fn redo_label(&self) -> String {
        if self.can_redo() {
            self.hist[self.pos as usize + 1].label.clone()
        } else {
            String::new()
        }
    }

    pub fn history_len(&self) -> usize {
        self.hist.len()
    }

    /// Index of the current step in the history.
    pub fn history_pos(&self) -> isize {
        self.pos
    }

    pub fn undo(&mut self) {
        self.flush_history();
        if self.pos > 0 {
            let label = self.hist[self.pos as usize].label.clone();
            self.pos -= 1;
            let snap = self.hist[self.pos as usize].clone();
            self.restore(&snap, format!("Undid {label}"));
        }
    }

    pub fn redo(&mut self) {
        self.flush_history();
        if self.pos >= 0 && (self.pos as usize) + 1 < self.hist.len() {
            self.pos += 1;
            let snap = self.hist[self.pos as usize].clone();
            let label = snap.label.clone();
            self.restore(&snap, format!("Redid {label}"));
        }
    }

    fn restore(&mut self, snap: &Snapshot, message: String) {
        if self.h.is_none() {
            return;
        }
        self.restoring = true;
        if let Ok(ed) = serde_json::from_str::<Edits>(&snap.edits) {
            self.ed = ed;
        }
        self.tr = snap.transcript.as_deref().and_then(|s| serde_json::from_str(s).ok());
        if let Some(evs) = &snap.events {
            if let Ok(mut ev) = serde_json::from_str::<Events>(evs) {
                ev.audio = self.ev.as_ref().and_then(|e| e.audio.clone());
                self.ev = Some(ev);
                self.write_events();
            }
        }
        if snap.me != self.me() {
            if let Some(h) = self.h.as_mut() {
                h.project.identity.me = snap.me.clone();
            }
            self.write_project();
        }
        self.save_at = None;
        self.write_edits();
        if self.tr.is_some() {
            self.write_transcript();
        } else if snap.transcript.is_none() && !self.read_only {
            if let Some(h) = &self.h {
                if h.transcript_path().exists() {
                    let _ = std::fs::remove_file(h.transcript_path());
                }
            }
        }
        self.emit(StateEvent::EventsChanged);
        self.emit(StateEvent::EditsChanged);
        self.emit(StateEvent::TranscriptChanged);
        self.emit(StateEvent::Restored(message));
        self.restoring = false;
        self.emit(StateEvent::HistoryChanged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_core::models::{Project, SourceInfo};

    fn handle(dir: &std::path::Path) -> ProjectHandle {
        let project = Project {
            schema_version: 1,
            id: "rustqa_test".into(),
            name: "test".into(),
            created: "2026-11-17T10:00:00".into(),
            source: SourceInfo {
                path: "x.mp4".into(),
                size: 1,
                duration: 60.0,
                width: 1920,
                height: 1080,
                fps: 60.0,
                vfr: false,
                has_audio: true,
            },
            game_area: None,
            identity: Default::default(),
            status: "ready".into(),
            error: String::new(),
            analysis_version: String::new(),
        };
        ProjectHandle { dir: dir.to_path_buf(), project }
    }

    #[test]
    fn undo_redo_groups_and_labels() {
        let dir = std::env::temp_dir().join(format!("vl_state_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = AppState::new(false);
        st.open(handle(&dir));
        assert!(!st.can_undo());
        st.set_parts(vec![Part::new(1.0, 2.0)], true);
        assert!(st.can_undo());
        assert_eq!(st.undo_label(), "trim change");
        // two quick edits while grouping -> one step
        st.ed.killfeed.rows = 3;
        st.touch();
        st.ed.killfeed.rows = 2;
        st.touch();
        assert_eq!(st.undo_label(), "last change");
        st.flush_history();
        assert_eq!(st.undo_label(), "kill feed change");
        st.undo();
        assert_eq!(st.ed.killfeed.rows, 4);
        assert_eq!(
            st.take_events()
                .iter()
                .filter_map(|e| if let StateEvent::Restored(m) = e { Some(m.clone()) } else { None })
                .next_back()
                .unwrap(),
            "Undid kill feed change"
        );
        assert!(st.can_redo());
        assert_eq!(st.redo_label(), "kill feed change");
        st.undo();
        assert!(st.ed.parts.is_empty());
        st.redo();
        st.redo();
        assert_eq!(st.ed.killfeed.rows, 2);
        assert_eq!(st.ed.parts.len(), 1);
        // a new change drops the redo branch
        st.undo();
        st.ed.music.volume = 0.5;
        st.touch();
        st.flush_history();
        assert!(!st.can_redo());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kept_duration_with_transition() {
        let mut st = AppState::new(true);
        st.ed.parts = vec![Part::new(0.0, 2.0), Part::new(5.0, 7.0)];
        assert!((st.kept_duration() - 4.0).abs() < 1e-9);
        st.ed.effects.transition = "fade".into();
        assert!((st.kept_duration() - 3.65).abs() < 1e-9);
    }
}
