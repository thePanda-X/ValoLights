//! Smart cut: the kill highlights plus what the clip *sounds* like — laughter, shouting, sudden loud voices,
//! and funny or hype words in the voice-chat transcript.
//!
//! 1. Reactions: runs of sound frames (see `vl_engine::audio`) where laughter, shouting or cheering stand out,
//!    or the volume jumps well above the surrounding minute while someone talks; plus matched transcript words.
//!    Laughter under game sound scores low in absolute terms, so the thresholds are modest and judged per frame.
//! 2. Moments: every kill highlight (scored like "Best moments") gains points for reactions during or just after
//!    it, and is stretched to keep the reaction ("NO WAY" after the clutch). Strong reactions away from any
//!    kill become moments of their own (a funny moment between rounds).
//! 3. The best moments up to the "Best moments" length are kept, in clip order, close parts merged.
//!
//! Every moment carries its reasons ("Triple kill", "Laughter"…), shown in Trim's Highlights list.
use crate::highlights::{build_segments, Segment};
use vl_core::config::HighlightsCfg;
use vl_core::models::{AudioSignals, Events, Part};

/// laughter frame (0..100), shouting frame
pub const LAUGH_MIN: u8 = 12;
pub const SHOUT_MIN: u8 = 8;
pub const CHEER_MIN: u8 = 15;
/// loudness above the surrounding minute (0..100 scale; 26 = about 16 dB) while someone talks. The bar is
/// the higher of this and the clip's own 99th percentile: streamers talk all the time and every gunfight
/// is loud, so only the clip's loudest voice moments count.
pub const SPIKE_MIN: f64 = 26.0;
pub const SPIKE_PERCENTILE: f64 = 0.99;
pub const SPEECH_MIN: u8 = 50;
/// frames on either side for the surrounding loudness (about half a minute)
const AROUND: usize = 60;
/// a reaction counts for a kill highlight from 1 s before it to this long after it
pub const REACT_AFTER: f64 = 5.0;
/// a highlight grows by at most this much to keep a reaction
pub const MAX_STRETCH: f64 = 6.0;
/// parts closer than this are joined
pub const JOIN_GAP: f64 = 1.5;

/// Phrases in the voice chat that mark a funny moment (lowercase, punctuation removed).
pub const FUNNY_WORDS: &[&str] =
    &["haha", "hahaha", "hahahaha", "lol", "lmao", "lmfao", "bruh", "bro what", "oops", "whoops", "oh no", "what was that", "why"];
/// Phrases that mark an intense one.
pub const HYPE_WORDS: &[&str] = &[
    "no way",
    "let's go",
    "lets go",
    "oh my god",
    "omg",
    "holy",
    "insane",
    "clutch",
    "sheesh",
    "wow",
    "what the",
    "wtf",
    "crazy",
    "sick",
    "gg",
    "yes yes",
    "come on",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Laugh,
    Hype,
}

/// A stretch of the clip where someone reacts.
#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    pub t0: f64,
    pub t1: f64,
    pub kind: Kind,
    /// 0..1, how strong
    pub peak: f64,
    /// what was heard ("Laughter", "Shouting", "\"no way\""…)
    pub what: String,
}

/// A candidate part of the Smart cut.
#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    pub start: f64,
    pub end: f64,
    pub score: f64,
    pub reasons: Vec<String>,
}

fn median(v: &mut [u8]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable();
    v[v.len() / 2] as f64
}

/// Words of the transcript that mark a reaction: (time, phrase). Matches whole words and two/three-word phrases.
pub fn transcript_words(words: &[(f64, String)]) -> Vec<(f64, String)> {
    let clean: Vec<(f64, String)> = words
        .iter()
        .map(|(t, w)| (*t, w.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>()))
        .filter(|(_, w)| !w.is_empty())
        .collect();
    let mut out: Vec<(f64, String)> = vec![];
    let mut i = 0;
    while i < clean.len() {
        let mut hit = None;
        for n in [3usize, 2, 1] {
            if i + n > clean.len() {
                continue;
            }
            let phrase = clean[i..i + n].iter().map(|(_, w)| w.as_str()).collect::<Vec<_>>().join(" ");
            if FUNNY_WORDS.contains(&phrase.as_str()) || HYPE_WORDS.contains(&phrase.as_str()) {
                hit = Some((n, phrase));
                break;
            }
        }
        match hit {
            Some((n, phrase)) => {
                out.push((clean[i].0, phrase));
                i += n;
            }
            None => i += 1,
        }
    }
    out
}

/// Reactions found in the sound signals and transcript words.
pub fn reactions(a: &AudioSignals) -> Vec<Reaction> {
    let n = a.loud.len();
    let step = if a.step > 0.0 { a.step } else { 0.48 };
    let at = |v: &Vec<u8>, i: usize| v.get(i).copied().unwrap_or(0);
    // loudness of the surrounding minute, per frame
    let around: Vec<f64> = (0..n)
        .map(|i| {
            let mut w: Vec<u8> = a.loud[i.saturating_sub(AROUND)..(i + AROUND + 1).min(n)].to_vec();
            median(&mut w)
        })
        .collect();
    let spikes: Vec<f64> = (0..n).map(|i| (at(&a.loud, i) as f64 - around[i]).max(0.0)).collect();
    let bar = {
        let mut s = spikes.clone();
        s.sort_by(|a, b| a.total_cmp(b));
        let p = s.get(((s.len().max(1) - 1) as f64 * SPIKE_PERCENTILE) as usize).copied().unwrap_or(0.0);
        p.max(SPIKE_MIN)
    };
    let spike = |i: usize| spikes[i];
    let speech = |i: usize| a.speech.get(i).copied().unwrap_or(100); // older signals: no speech channel
    let loud_voice = |i: usize| spike(i) >= bar && speech(i) >= SPEECH_MIN;
    let laugh_f = |i: usize| at(&a.laugh, i) >= LAUGH_MIN;
    let hype_f = |i: usize| at(&a.shout, i) >= SHOUT_MIN || at(&a.cheer, i) >= CHEER_MIN || loud_voice(i);
    let mut out = vec![];
    for kind in [Kind::Laugh, Kind::Hype] {
        let flag = |i: usize| if kind == Kind::Laugh { laugh_f(i) } else { hype_f(i) };
        let mut i = 0;
        while i < n {
            if !flag(i) {
                i += 1;
                continue;
            }
            // a run, bridging single-frame gaps
            let mut j = i;
            while j + 1 < n && (flag(j + 1) || (j + 2 < n && flag(j + 2))) {
                j += 1;
            }
            let (peak, what) = if kind == Kind::Laugh {
                let m = (i..=j).map(|k| at(&a.laugh, k)).max().unwrap_or(0) as f64;
                ((m / 40.0).min(1.0), "Laughter".to_string())
            } else {
                let sh = (i..=j).map(|k| at(&a.shout, k)).max().unwrap_or(0) as f64;
                let ch = (i..=j).map(|k| at(&a.cheer, k)).max().unwrap_or(0) as f64;
                let sp = (i..=j).map(spike).fold(0.0, f64::max);
                let p = (sh / 30.0).max(ch / 40.0).max(sp / 45.0).min(1.0);
                let what = if sh >= SHOUT_MIN as f64 {
                    "Shouting"
                } else if ch >= CHEER_MIN as f64 {
                    "Cheering"
                } else {
                    "Loud reaction"
                };
                // a loud voice alone must last more than one frame (a gunshot next to talking doesn't)
                if what == "Loud reaction" && (i..=j).filter(|&k| loud_voice(k)).count() < 2 {
                    i = j + 1;
                    continue;
                }
                (p, what.to_string())
            };
            out.push(Reaction { t0: i as f64 * step, t1: (j + 2) as f64 * step, kind, peak, what });
            i = j + 1;
        }
    }
    for (t, w) in &a.words {
        let kind = if FUNNY_WORDS.contains(&w.as_str()) { Kind::Laugh } else { Kind::Hype };
        out.push(Reaction { t0: *t, t1: t + 1.0, kind, peak: 0.6, what: format!("\u{201c}{w}\u{201d}") });
    }
    out.sort_by(|a, b| a.t0.total_cmp(&b.t0));
    out
}

fn kill_reasons(s: &Segment) -> Vec<String> {
    let mut r = vec![];
    let n = s.kills.len();
    r.push(
        match n {
            1 => "Kill",
            2 => "Double kill",
            3 => "Triple kill",
            4 => "Quadra kill",
            _ => "Ace",
        }
        .to_string(),
    );
    let hs = s.kills.iter().filter(|k| k.headshot).count();
    if hs > 0 {
        r.push(if hs == 1 { "Headshot".to_string() } else { format!("{hs} headshots") });
    }
    for (tag, name) in [("clutch", "Clutch"), ("ace", "Ace")] {
        if s.tags.iter().any(|t| t == tag) && !r.iter().any(|x| x == name) {
            r.push(name.to_string());
        }
    }
    r
}

fn push_reason(r: &mut Vec<String>, what: &str) {
    if !r.iter().any(|x| x == what) {
        r.push(what.to_string());
    }
}

/// All candidate moments, highest score first.
pub fn moments(ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Vec<Moment> {
    let segs = build_segments(ev, me, cfg, duration, disabled);
    let rx = ev.audio.as_ref().map(reactions).unwrap_or_default();
    let mut used = vec![false; rx.len()];
    let mut out: Vec<Moment> = vec![];
    for s in &segs {
        let mut m = Moment { start: s.start, end: s.end, score: s.score, reasons: kill_reasons(s) };
        for (i, r) in rx.iter().enumerate() {
            if r.t0 > s.end + REACT_AFTER || r.t1 < s.start - 1.0 {
                continue;
            }
            used[i] = true;
            m.score += match r.kind {
                Kind::Laugh => 1.5 + 1.5 * r.peak,
                Kind::Hype => 0.8 + r.peak,
            };
            push_reason(&mut m.reasons, &r.what);
            if r.t1 > m.end {
                m.end = (r.t1 + 0.6).min(s.end + MAX_STRETCH).min(duration.max(s.end));
            }
        }
        out.push(m);
    }
    // reactions away from any kill: funny or loud moments of their own
    for (i, r) in rx.iter().enumerate() {
        if used[i] {
            continue;
        }
        let (keep, score) = match r.kind {
            Kind::Laugh => (r.peak >= 0.3, 1.0 + 2.0 * r.peak),
            Kind::Hype => (r.peak >= 0.5, 0.5 + 1.2 * r.peak),
        };
        if !keep {
            continue;
        }
        let end = if duration > 0.0 { (r.t1 + 2.0).min(duration) } else { r.t1 + 2.0 };
        out.push(Moment { start: (r.t0 - 4.0).max(0.0), end, score, reasons: vec![r.what.clone()] });
    }
    // overlapping moments become one
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<Moment> = vec![];
    for m in out {
        if let Some(last) = merged.last_mut() {
            if m.start <= last.end {
                last.end = last.end.max(m.end);
                last.score += m.score;
                for r in &m.reasons {
                    push_reason(&mut last.reasons, r);
                }
                continue;
            }
        }
        merged.push(m);
    }
    for m in merged.iter_mut() {
        m.score = (m.score * 100.0).round() / 100.0;
    }
    merged.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.start.total_cmp(&b.start)));
    merged
}

/// The moments the Smart cut keeps (best first until the "Best moments" length), in clip order.
pub fn chosen(ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Vec<Moment> {
    let mut keep: Vec<Moment> = vec![];
    let mut total = 0.0;
    for m in moments(ev, me, cfg, duration, disabled) {
        let d = m.end - m.start;
        if !keep.is_empty() && total + d > cfg.best_of_target {
            continue;
        }
        total += d;
        keep.push(m);
    }
    keep.sort_by(|a, b| a.start.total_cmp(&b.start));
    keep
}

/// The Smart cut's parts.
pub fn parts(ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Vec<Part> {
    let mut out: Vec<Part> = vec![];
    for m in chosen(ev, me, cfg, duration, disabled) {
        if let Some(last) = out.last_mut() {
            if m.start - last.src_out <= JOIN_GAP {
                last.src_out = last.src_out.max(m.end);
                continue;
            }
        }
        out.push(Part::new(m.start, m.end));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_core::models::{Kill, Round};

    fn signals(n: usize) -> AudioSignals {
        AudioSignals {
            version: 1,
            step: 0.48,
            laugh: vec![0; n],
            shout: vec![0; n],
            cheer: vec![0; n],
            loud: vec![40; n],
            speech: vec![0; n],
            words: vec![],
        }
    }

    fn kill(id: &str, t: f64) -> Kill {
        serde_json::from_value(serde_json::json!({"id": id, "t": t, "round": 0, "killer": "p0", "victim": "p1"})).unwrap()
    }

    fn events() -> Events {
        let mut ev = Events::default();
        ev.rounds.push(Round {
            index: 0,
            start: 0.0,
            end: 200.0,
            score_before: None,
            score_after: None,
            result: "".into(),
            banners: vec![],
            planted: None,
            decided: None,
        });
        ev.kills = vec![kill("k1", 20.0), kill("k2", 120.0)];
        ev
    }

    #[test]
    fn finds_laughter_shouting_and_loud_voices() {
        let mut a = signals(400);
        for i in 100..104 {
            a.laugh[i] = 30;
        }
        a.shout[200] = 20;
        // a loud spoken burst well above the surrounding level
        for i in 300..303 {
            a.loud[i] = 70;
            a.speech[i] = 80;
        }
        // loud but no speech (gunfire): not a reaction
        a.loud[350] = 80;
        let r = reactions(&a);
        let kinds: Vec<(Kind, i64)> = r.iter().map(|x| (x.kind, x.t0.round() as i64)).collect();
        assert_eq!(kinds, vec![(Kind::Laugh, 48), (Kind::Hype, 96), (Kind::Hype, 144)], "{r:?}");
        assert_eq!(r[0].what, "Laughter");
        assert_eq!(r[1].what, "Shouting");
        assert_eq!(r[2].what, "Loud reaction");
    }

    #[test]
    fn transcript_phrases() {
        let w: Vec<(f64, String)> =
            ["oh", "NO", "way,", "that", "was", "sick", "haha"].iter().enumerate().map(|(i, w)| (i as f64, w.to_string())).collect();
        let hits = transcript_words(&w);
        // "oh no" is matched first (funny), then "way" alone isn't a phrase
        assert_eq!(hits, vec![(0.0, "oh no".to_string()), (5.0, "sick".to_string()), (6.0, "haha".to_string())]);
    }

    #[test]
    fn reactions_lift_and_stretch_a_highlight_and_stand_alone() {
        let me = vec!["p0".to_string()];
        let cfg = HighlightsCfg::default();
        let mut ev = events();
        let plain = moments(&ev, &me, &cfg, 200.0, &[]);
        let mut a = signals(420);
        // laughter just after the kill at 120 s
        for i in 252..256 {
            a.laugh[i] = 36;
        }
        // a funny moment at 160 s with no kill around
        for i in 333..337 {
            a.laugh[i] = 30;
        }
        ev.audio = Some(a);
        let ms = moments(&ev, &me, &cfg, 200.0, &[]);
        let k2 = ms.iter().find(|m| m.start <= 120.0 && m.end >= 120.0).unwrap();
        let k2_plain = plain.iter().find(|m| m.start <= 120.0 && m.end >= 120.0).unwrap();
        assert!(k2.score > k2_plain.score + 2.0 && k2.end > k2_plain.end, "{k2:?} vs {k2_plain:?}");
        assert!(k2.reasons.contains(&"Kill".to_string()) && k2.reasons.contains(&"Laughter".to_string()));
        // the laughed-at kill now outranks the other one
        assert!(ms[0].start <= 120.0 && ms[0].end >= 120.0);
        let alone = ms.iter().find(|m| m.reasons == vec!["Laughter".to_string()]).expect("a moment of its own");
        assert!(alone.start < 160.0 && alone.end > 161.0);
        // parts come out in clip order
        let p = parts(&ev, &me, &cfg, 200.0, &[]);
        assert!(p.windows(2).all(|w| w[0].src_out < w[1].src_in));
    }

    #[test]
    fn without_sound_it_is_the_kill_highlights() {
        let me = vec!["p0".to_string()];
        let cfg = HighlightsCfg::default();
        let ev = events();
        let ms = moments(&ev, &me, &cfg, 200.0, &[]);
        assert_eq!(ms.len(), 2);
        assert!(ms.iter().all(|m| m.reasons == vec!["Kill".to_string()]));
    }
}
