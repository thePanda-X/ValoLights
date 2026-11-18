//! Highlight segments, scoring and quick-cut generation.
use crate::numfmt::round_n;
use anyhow::{bail, Result};
use std::collections::{BTreeSet, HashSet};
use vl_core::config::HighlightsCfg;
use vl_core::models::{Events, Kill, Part};

fn multi(n: usize) -> f64 {
    match n {
        1 => 1.0,
        2 => 1.5,
        3 => 2.2,
        4 => 3.0,
        5 => 4.0,
        _ => 4.0 + 0.5 * (n as f64 - 5.0),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub round: i32,
    pub kills: Vec<Kill>,
    pub start: f64,
    pub end: f64,
    pub score: f64,
    pub tags: Vec<String>,
}

impl Segment {
    pub fn label(&self) -> String {
        let n = self.kills.len();
        let base = match n {
            1 => "Kill".to_string(),
            2 => "Double kill".into(),
            3 => "Triple kill".into(),
            4 => "Quadra kill".into(),
            5 => "ACE".into(),
            _ => format!("{n} kills"),
        };
        let extra: Vec<&str> = self.tags.iter().filter(|t| t.as_str() != "ace").map(|s| s.as_str()).collect();
        if extra.is_empty() {
            base
        } else {
            format!("{base} \u{b7} {}", extra.join(", "))
        }
    }
}

pub fn my_kills(ev: &Events, me: &[String], include_team: bool, disabled: &[String]) -> Vec<Kill> {
    let disabled: HashSet<&str> = disabled.iter().map(|s| s.as_str()).collect();
    let me_set: HashSet<&str> = me.iter().map(|s| s.as_str()).collect();
    let my_team: HashSet<&str> =
        ev.players.iter().filter(|p| me_set.contains(p.id.as_str()) && !p.team.is_empty()).map(|p| p.team.as_str()).collect();
    let mut out = vec![];
    for k in &ev.kills {
        if disabled.contains(k.id.as_str()) || k.killer == k.victim {
            continue;
        }
        if me_set.contains(k.killer.as_str()) {
            out.push(k.clone());
        } else if include_team && !my_team.is_empty() {
            if let Some(p) = ev.player(&k.killer) {
                if my_team.contains(p.team.as_str()) {
                    out.push(k.clone());
                }
            }
        }
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

pub fn my_deaths(ev: &Events, me: &[String]) -> Vec<Kill> {
    ev.kills.iter().filter(|k| me.contains(&k.victim)).cloned().collect()
}

pub fn round_bounds(ev: &Events, idx: i32, duration: f64) -> (f64, f64) {
    if idx >= 0 && (idx as usize) < ev.rounds.len() {
        let r = &ev.rounds[idx as usize];
        return (r.start, r.end + 8.0);
    }
    (0.0, duration)
}

/// ace / clutch / round result tags for a round, derived from kills + score.
pub fn round_tags(ev: &Events, me: &[String], round_idx: i32) -> Vec<String> {
    let mut tags: Vec<String> = vec![];
    let me_set: HashSet<&str> = me.iter().map(|s| s.as_str()).collect();
    let ks: Vec<&Kill> = ev.kills.iter().filter(|k| k.round == round_idx && k.killer != k.victim).collect();
    let mine: Vec<&Kill> = ks.iter().copied().filter(|k| me_set.contains(k.killer.as_str())).collect();
    if mine.iter().map(|k| k.victim.as_str()).collect::<HashSet<_>>().len() >= 5 {
        tags.push("ace".into());
    }
    if round_idx >= 0 && (round_idx as usize) < ev.rounds.len() {
        let rd = &ev.rounds[round_idx as usize];
        if !rd.result.is_empty() {
            tags.push(rd.result.clone());
        }
        // the end-of-round banner shows the clutch of whichever team won it: only ours counts
        if rd.result != "loss" && rd.banners.iter().any(|b| b.to_uppercase() == "CLUTCH") {
            tags.push("clutch".into());
        }
    }
    // clutch: all teammates dead before my last kill, I survive the round and we win
    let my_team: HashSet<&str> =
        ev.players.iter().filter(|p| me_set.contains(p.id.as_str()) && !p.team.is_empty()).map(|p| p.team.as_str()).collect();
    if !my_team.is_empty() && !mine.is_empty() && !tags.iter().any(|t| t == "clutch") {
        let mates: HashSet<&str> = ev
            .players
            .iter()
            .filter(|p| my_team.contains(p.team.as_str()) && !me_set.contains(p.id.as_str()))
            .map(|p| p.id.as_str())
            .collect();
        let last = mine.iter().map(|k| k.t).fold(f64::NEG_INFINITY, f64::max);
        let dead_mates: HashSet<&str> =
            ks.iter().filter(|k| mates.contains(k.victim.as_str()) && k.t <= last).map(|k| k.victim.as_str()).collect();
        let i_died = ks.iter().any(|k| me_set.contains(k.victim.as_str()));
        let won = tags.iter().any(|t| t == "win");
        let need = 4.min(2.max(mates.len()));
        if dead_mates.len() >= need && !i_died && (won || tags.is_empty()) {
            tags.push("clutch".into());
        }
    }
    tags
}

pub fn build_segments(ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Vec<Segment> {
    let kills = my_kills(ev, me, cfg.include_team_kills, disabled);
    let mut segs: Vec<Segment> = vec![];
    for k in &kills {
        if let Some(last) = segs.last_mut() {
            if last.round == k.round && k.t - last.kills.last().unwrap().t <= cfg.merge_gap {
                last.kills.push(k.clone());
                continue;
            }
        }
        segs.push(Segment { round: k.round, kills: vec![k.clone()], start: 0.0, end: 0.0, score: 0.0, tags: vec![] });
    }
    for s in segs.iter_mut() {
        let (lo, hi) = round_bounds(ev, s.round, duration);
        s.start = 0.0f64.max(lo).max(s.kills[0].t - cfg.pre_roll);
        s.end = duration.min(hi).min(s.kills.last().unwrap().t + cfg.post_roll);
        let tags = round_tags(ev, me, s.round);
        let n_round = kills.iter().filter(|k| k.round == s.round).count();
        s.tags = tags.iter().filter(|t| *t == "ace" || *t == "clutch").cloned().collect();
        if n_round >= 3 && s.kills.len() < n_round {
            s.tags.push(format!("{n_round}k round"));
        }
        // the sum starts from 0
        let mut val: f64 = 0.0;
        for k in &s.kills {
            val += 1.0 + cfg.w_headshot * if k.headshot { 1.0 } else { 0.0 };
        }
        val *= multi(s.kills.len());
        if tags.iter().any(|t| t == "ace") {
            val += cfg.ace_bonus;
        }
        if tags.iter().any(|t| t == "clutch") {
            val += cfg.clutch_bonus;
        }
        let last_in_round =
            kills.iter().filter(|k| k.round == s.round).map(|k| k.t).fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))));
        if tags.iter().any(|t| t == "win") && s.kills.last().unwrap().t >= last_in_round.unwrap_or(0.0) {
            val += cfg.round_win_bonus;
        }
        if s.kills.windows(2).any(|w| w[1].t - w[0].t <= 1.5) {
            val += 0.3;
        }
        s.score = round_n(val, 2);
    }
    // merge overlapping segments (can happen across round boundaries with long pre-roll)
    segs.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<Segment> = vec![];
    for s in segs {
        if let Some(m) = merged.last_mut() {
            if s.start <= m.end {
                m.end = m.end.max(s.end);
                m.kills.extend(s.kills);
                m.score += s.score;
                let set: BTreeSet<String> = m.tags.iter().chain(s.tags.iter()).cloned().collect();
                m.tags = set.into_iter().collect();
                continue;
            }
        }
        merged.push(s);
    }
    merged
}

fn merge_parts(parts: &[Part]) -> Vec<Part> {
    let mut ps = parts.to_vec();
    ps.sort_by(|a, b| a.src_in.total_cmp(&b.src_in));
    let mut out: Vec<Part> = vec![];
    for p in ps {
        if let Some(last) = out.last_mut() {
            if p.src_in <= last.src_out + 0.05 {
                last.src_out = last.src_out.max(p.src_out);
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// (key, label) in display order.
pub const QUICK_CUTS: &[(&str, &str)] = &[
    ("kills", "Kills only"),
    ("segments", "First to last kill"),
    ("best", "Best moments"),
    ("rounds", "Whole rounds with kills"),
    ("plays", "Kills, plants, round ends"),
    ("smart", "Smart cut: funny and intense"),
    ("whole", "Whole clip"),
];

/// "plays": the spike plant (the plant finishing + the SPIKE PLANTED banner) ...
pub const PLANT_PRE: f64 = 3.0;
pub const PLANT_POST: f64 = 2.0;
/// ... and the round end (the last moment + the end-of-round banner), in the rounds with your kills
pub const END_PRE: f64 = 2.5;
pub const END_POST: f64 = 3.5;

pub fn quick_cut(name: &str, ev: &Events, me: &[String], cfg: &HighlightsCfg, duration: f64, disabled: &[String]) -> Result<Vec<Part>> {
    let segs = build_segments(ev, me, cfg, duration, disabled);
    Ok(match name {
        "whole" => vec![Part::new(0.0, duration)],
        // kill highlights + laughter, shouting and voice-chat reactions (see `smart`)
        "smart" => crate::smart::parts(ev, me, cfg, duration, disabled),
        "kills" => merge_parts(
            &segs
                .iter()
                .flat_map(|s| s.kills.iter())
                .map(|k| Part::new(0.0f64.max(k.t - cfg.kill_pre), duration.min(k.t + cfg.kill_post)))
                .collect::<Vec<_>>(),
        ),
        "segments" => merge_parts(&segs.iter().map(|s| Part::new(s.start, s.end)).collect::<Vec<_>>()),
        "plays" => {
            // your kills (as "kills"), plus the spike plant and the round end of every round you have a kill in
            let mut parts: Vec<Part> = segs
                .iter()
                .flat_map(|s| s.kills.iter())
                .map(|k| Part::new(0.0f64.max(k.t - cfg.kill_pre), duration.min(k.t + cfg.kill_post)))
                .collect();
            let idx: BTreeSet<i32> = segs.iter().map(|s| s.round).collect();
            for rd in idx.into_iter().filter_map(|i| usize::try_from(i).ok().and_then(|i| ev.rounds.get(i))) {
                if let Some(p) = rd.planted {
                    parts.push(Part::new((p - PLANT_PRE).max(0.0), (p + PLANT_POST).min(duration)));
                }
                if let Some(d) = rd.decided {
                    parts.push(Part::new((d - END_PRE).max(0.0), (d + END_POST).min(duration)));
                }
            }
            merge_parts(&parts)
        }
        "rounds" => {
            let idx: BTreeSet<i32> = segs.iter().map(|s| s.round).collect();
            let parts: Vec<Part> = idx
                .into_iter()
                .map(|i| {
                    let (lo, hi) = round_bounds(ev, i, duration);
                    Part::new(lo, hi.min(duration))
                })
                .collect();
            merge_parts(&parts)
        }
        "best" => {
            let mut ranked: Vec<&Segment> = segs.iter().collect();
            ranked.sort_by(|a, b| (-a.score).total_cmp(&-b.score));
            let mut chosen: Vec<&Segment> = vec![];
            let mut total = 0.0;
            for s in ranked {
                let d = s.end - s.start;
                if !chosen.is_empty() && total + d > cfg.best_of_target {
                    continue;
                }
                chosen.push(s);
                total += d;
            }
            merge_parts(&chosen.iter().map(|s| Part::new(s.start, s.end)).collect::<Vec<_>>())
        }
        _ => bail!("unknown quick cut {name}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_core::models::{Player, Round};

    fn kill(id: &str, t: f64, round: i32, killer: &str, victim: &str) -> Kill {
        serde_json::from_value(serde_json::json!({"id": id, "t": t, "round": round, "killer": killer, "victim": victim})).unwrap()
    }

    fn events() -> Events {
        let mut ev = Events::default();
        for (id, team) in [("me", "A"), ("m1", "A"), ("e1", "B"), ("e2", "B"), ("e3", "B")] {
            ev.players.push(Player { id: id.into(), team: team.into(), ..Default::default() });
        }
        ev.rounds = vec![
            Round {
                index: 0,
                start: 0.0,
                end: 30.0,
                score_before: None,
                score_after: None,
                result: "win".into(),
                banners: vec![],
                planted: None,
                decided: None,
            },
            Round {
                index: 1,
                start: 38.0,
                end: 80.0,
                score_before: None,
                score_after: None,
                result: "".into(),
                banners: vec![],
                planted: None,
                decided: None,
            },
        ];
        ev.kills = vec![
            kill("a", 10.0, 0, "me", "e1"),
            kill("b", 11.0, 0, "me", "e2"),
            kill("c", 50.0, 1, "me", "e3"),
            kill("d", 60.0, 1, "e1", "me"),
        ];
        ev
    }

    #[test]
    fn segments_and_cuts() {
        let ev = events();
        let me = vec!["me".to_string()];
        let cfg = HighlightsCfg::default();
        let segs = build_segments(&ev, &me, &cfg, 90.0, &[]);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].kills.len(), 2);
        assert_eq!(segs[0].label(), "Double kill");
        // 2 kills * 1.5 + win bonus + quick-succession bonus
        assert_eq!(segs[0].score, 3.8);
        let kills = quick_cut("kills", &ev, &me, &cfg, 90.0, &[]).unwrap();
        assert_eq!(kills.len(), 2);
        assert!((kills[0].src_in - 8.4).abs() < 1e-9 && (kills[0].src_out - 11.9).abs() < 1e-9);
        let rounds = quick_cut("rounds", &ev, &me, &cfg, 90.0, &[]).unwrap();
        assert_eq!(rounds.len(), 1); // 0..38 and 38..88 touch -> merged
        assert_eq!(quick_cut("whole", &ev, &me, &cfg, 90.0, &[]).unwrap(), vec![Part::new(0.0, 90.0)]);
        assert!(quick_cut("nope", &ev, &me, &cfg, 90.0, &[]).is_err());
        assert_eq!(my_kills(&ev, &me, false, &["a".into()]).len(), 2);
    }

    #[test]
    fn plays_adds_plants_and_round_ends_of_rounds_with_kills() {
        let mut ev = events();
        ev.rounds[0].planted = Some(20.0);
        ev.rounds[0].decided = Some(28.0);
        ev.rounds[1].decided = Some(75.0); // round 1 has a kill of mine too
        ev.rounds.push(Round {
            index: 2,
            start: 80.0,
            end: 120.0,
            score_before: None,
            score_after: None,
            result: "".into(),
            banners: vec![],
            planted: Some(100.0),
            decided: Some(110.0),
        });
        let cfg = HighlightsCfg::default();
        let parts = quick_cut("plays", &ev, &["me".to_string()], &cfg, 120.0, &[]).unwrap();
        let spans: Vec<(f64, f64)> = parts.iter().map(|p| ((p.src_in * 10.0).round() / 10.0, (p.src_out * 10.0).round() / 10.0)).collect();
        // kills 10+11 merged, the plant, the round end; round 1: kill 50 + its end; round 2 (no kill of mine): nothing
        assert_eq!(spans, vec![(8.4, 11.9), (17.0, 22.0), (25.5, 31.5), (48.4, 50.9), (72.5, 78.5)]);
        // without plant / end times (older analyses) it is "kills only"
        let plain = quick_cut("plays", &events(), &["me".to_string()], &cfg, 120.0, &[]).unwrap();
        assert_eq!(plain, quick_cut("kills", &events(), &["me".to_string()], &cfg, 120.0, &[]).unwrap());
    }
}
