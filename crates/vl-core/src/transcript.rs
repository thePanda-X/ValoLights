//! Editable transcript: captions are the transcript segments themselves.
use crate::models::SubtitleSettings;
use serde::{Deserialize, Serialize};

pub const MIN_LEN: f64 = 0.2; // shortest caption, seconds

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub censored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default = "s1")]
    pub speaker: String,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub voice_line: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub edited: bool,
    #[serde(default)]
    pub added: bool,
}
fn s1() -> String {
    "s1".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Transcript {
    pub language: String,
    pub model: String,
    pub segments: Vec<Segment>,
    pub speakers: Vec<String>,
    pub ranges: Vec<(f64, f64)>,
}

impl Default for Transcript {
    fn default() -> Self {
        Transcript { language: "en".into(), model: String::new(), segments: vec![], speakers: vec!["s1".into()], ranges: vec![] }
    }
}

impl Transcript {
    pub fn sort(&mut self) {
        self.segments.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    }
}

/// Set a segment's text, keeping word timings when the word count is unchanged; otherwise words are
/// spread over the segment in proportion to their length. Censor flags survive for unchanged words.
pub fn retime_words(seg: &mut Segment, text: &str) {
    let new: Vec<&str> = text.split_whitespace().collect();
    seg.text = new.join(" ");
    if !seg.words.is_empty() && seg.words.len() == new.len() {
        for (w, t) in seg.words.iter_mut().zip(new.iter()) {
            if w.text != *t {
                w.censored = false;
            }
            w.text = t.to_string();
        }
        return;
    }
    let keep: std::collections::HashSet<String> = seg.words.iter().filter(|w| w.censored).map(|w| w.text.to_lowercase()).collect();
    let total: usize = new.iter().map(|w| w.chars().count() + 1).sum::<usize>().max(1);
    let dur = (seg.end - seg.start).max(MIN_LEN);
    let mut t = seg.start;
    let mut words = vec![];
    for w in new {
        let d = dur * (w.chars().count() + 1) as f64 / total as f64;
        words.push(Word { start: r3(t), end: r3(t + d), text: w.to_string(), censored: keep.contains(&w.to_lowercase()) });
        t += d;
    }
    seg.words = words;
}

/// Move/stretch a caption; word timings scale with it.
pub fn set_times(seg: &mut Segment, start: f64, end: f64) {
    let start = start.max(0.0);
    let end = end.max(start + MIN_LEN);
    let (old_s, old_d) = (seg.start, (seg.end - seg.start).max(1e-6));
    let new_d = end - start;
    for w in seg.words.iter_mut() {
        w.start = r3(start + (w.start - old_s) * new_d / old_d);
        w.end = r3(start + (w.end - old_s) * new_d / old_d);
    }
    seg.start = r3(start);
    seg.end = r3(end);
}

/// Split segment i at source time t; returns the index of the second half.
pub fn split_at(tr: &mut Transcript, i: usize, t: f64) -> Option<usize> {
    let seg = tr.segments.get(i)?.clone();
    if !(seg.start + MIN_LEN <= t && t <= seg.end - MIN_LEN) {
        return None;
    }
    let mut left: Vec<Word> = seg.words.iter().filter(|w| (w.start + w.end) / 2.0 < t).cloned().collect();
    let mut right: Vec<Word> = seg.words.iter().filter(|w| (w.start + w.end) / 2.0 >= t).cloned().collect();
    if !seg.words.is_empty() && (left.is_empty() || right.is_empty()) {
        let n_before = seg.words.iter().filter(|w| w.end <= t).count();
        let k = (if n_before == 0 { 1 } else { n_before }).clamp(1, seg.words.len().saturating_sub(1).max(1));
        left = seg.words[..k].to_vec();
        right = seg.words[k..].to_vec();
    }
    let mut a = seg.clone();
    let mut b = seg.clone();
    a.end = r3(t);
    b.start = r3(t);
    a.words = left;
    b.words = right;
    if !seg.words.is_empty() {
        a.text = a.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        b.text = b.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    } else {
        let ws: Vec<&str> = seg.text.split_whitespace().collect();
        let k = (ws.len() / 2).max(1).min(ws.len());
        a.text = ws[..k].join(" ");
        b.text = ws[k..].join(" ");
    }
    a.edited = true;
    b.edited = true;
    tr.segments.splice(i..=i, [a, b]);
    Some(i + 1)
}

pub fn merge_next(tr: &mut Transcript, i: usize) -> bool {
    if i + 1 >= tr.segments.len() {
        return false;
    }
    let b = tr.segments.remove(i + 1);
    let a = &mut tr.segments[i];
    a.end = a.end.max(b.end);
    a.text = format!("{} {}", a.text, b.text).trim().to_string();
    a.words.extend(b.words);
    a.hidden = a.hidden && b.hidden;
    a.edited = true;
    true
}

pub fn add_caption(tr: &mut Transcript, t: f64, length: f64, text: &str, speaker: &str) -> usize {
    let mut seg = Segment {
        start: r3(t),
        end: r3(t + length),
        text: String::new(),
        speaker: speaker.into(),
        words: vec![],
        voice_line: false,
        hidden: false,
        edited: true,
        added: true,
    };
    retime_words(&mut seg, if text.is_empty() { "New caption" } else { text });
    let key = (seg.start, seg.end);
    tr.segments.push(seg);
    tr.sort();
    tr.segments.iter().position(|s| (s.start, s.end) == key && s.added).unwrap_or(0)
}

/// Fold old index-based choices (hidden_lines / edits / censored on the settings) into the segments.
pub fn apply_legacy(tr: &mut Transcript, st: &mut SubtitleSettings) -> bool {
    let mut changed = false;
    let n = tr.segments.len() as i32;
    for &i in &st.hidden_lines {
        if 0 <= i && i < n {
            tr.segments[i as usize].hidden = true;
            changed = true;
        }
    }
    for &(i, w) in &st.censored {
        if 0 <= i && i < n && 0 <= w && (w as usize) < tr.segments[i as usize].words.len() {
            tr.segments[i as usize].words[w as usize].censored = true;
            changed = true;
        }
    }
    for (k, text) in &st.edits {
        if let Ok(i) = k.parse::<i32>() {
            if 0 <= i && i < n {
                let seg = &mut tr.segments[i as usize];
                if seg.voice_line {
                    seg.hidden = false;
                }
                retime_words(seg, text);
                seg.edited = true;
                changed = true;
            }
        }
    }
    for seg in tr.segments.iter_mut() {
        if seg.voice_line && !seg.edited && !seg.hidden {
            seg.hidden = true;
            changed = true;
        }
    }
    st.hidden_lines.clear();
    st.censored.clear();
    st.edits.clear();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: f64, b: f64, text: &str) -> Segment {
        let mut s = Segment {
            start: a,
            end: b,
            text: String::new(),
            speaker: "s1".into(),
            words: vec![],
            voice_line: false,
            hidden: false,
            edited: false,
            added: false,
        };
        retime_words(&mut s, text);
        s
    }

    #[test]
    fn split_and_merge() {
        let mut tr = Transcript { segments: vec![seg(0.0, 2.0, "nice one brother")], ..Default::default() };
        let j = split_at(&mut tr, 0, 1.0).unwrap();
        assert_eq!(j, 1);
        assert_eq!(tr.segments.len(), 2);
        assert!(merge_next(&mut tr, 0));
        assert_eq!(tr.segments[0].text, "nice one brother");
    }

    #[test]
    fn retime_keeps_censor() {
        let mut s = seg(0.0, 2.0, "oh no way");
        s.words[1].censored = true;
        retime_words(&mut s, "oh no way at all");
        assert!(s.words.iter().any(|w| w.text == "no" && w.censored));
        assert!((s.words.last().unwrap().end - 2.0).abs() < 0.01);
    }
}
