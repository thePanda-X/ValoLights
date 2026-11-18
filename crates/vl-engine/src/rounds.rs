//! Round segmentation from noisy per-second score readings.
use vl_core::models::Round;

/// kills shortly after the score changes still belong to the ended round
pub const POST_ROUND: f64 = 8.0;

/// Most common item; ties go to the first one seen.
pub fn most_common<T: PartialEq + Clone>(items: impl IntoIterator<Item = T>) -> Option<(T, usize)> {
    let mut counts: Vec<(T, usize)> = vec![];
    for it in items {
        match counts.iter_mut().find(|c| c.0 == it) {
            Some(c) => c.1 += 1,
            None => counts.push((it, 1)),
        }
    }
    let mut best: Option<(T, usize)> = None;
    for (k, n) in counts {
        if best.as_ref().map(|b| n > b.1).unwrap_or(true) {
            best = Some((k, n));
        }
    }
    best
}

/// Confirmed score states [(t_change, left, right)], monotonic non-decreasing.
///
/// A new state is accepted when it is >= the current state on both sides, has a larger total, and appears in at
/// least `confirm` of the next `window` readings. Score jumps > 1 are allowed (replay extracts skip rounds).
pub fn smooth_scores(readings: &[(f64, i32, i32)], confirm: usize, window: usize) -> Vec<(f64, i32, i32)> {
    if readings.is_empty() {
        return vec![];
    }
    let head = window.max(8).min(readings.len());
    let mut cur = most_common(readings[..head].iter().map(|r| (r.1, r.2))).unwrap().0;
    let mut states = vec![(readings[0].0, cur.0, cur.1)];
    for i in 0..readings.len() {
        let (t, l, r) = readings[i];
        if (l, r) != cur && l >= cur.0 && r >= cur.1 && l + r > cur.0 + cur.1 && (l + r) - (cur.0 + cur.1) <= 6 {
            let nxt = &readings[i..(i + window).min(readings.len())];
            let hits = nxt.iter().filter(|x| (x.1, x.2) == (l, r)).count();
            if hits >= confirm.min(nxt.len()) {
                cur = (l, r);
                states.push((t, l, r));
            }
        }
    }
    states
}

/// Rounds between confirmed score changes. `left_color` = team colour of the left score.
pub fn build_rounds(states: &[(f64, i32, i32)], duration: f64, left_color: &str) -> Vec<Round> {
    if states.is_empty() {
        return vec![Round {
            index: 0,
            start: 0.0,
            end: duration,
            score_before: None,
            score_after: None,
            result: String::new(),
            banners: vec![],
            planted: None,
            decided: None,
        }];
    }
    let mut rounds = vec![];
    let mut start = 0.0;
    for k in 0..states.len() {
        let (_, l, r) = states[k];
        let (end, after, res_side) = if k + 1 < states.len() {
            let (t2, l2, r2) = states[k + 1];
            let side = if l2 > l && r2 == r {
                "left"
            } else if r2 > r && l2 == l {
                "right"
            } else {
                ""
            };
            (t2, Some((l2, r2)), side)
        } else {
            (duration, None, "")
        };
        let mut winner = "";
        if !res_side.is_empty() && !left_color.is_empty() {
            let other = if left_color == "teal" { "red" } else { "teal" };
            winner = if res_side == "left" { left_color } else { other };
        }
        rounds.push(Round {
            index: l + r,
            start,
            end,
            score_before: Some((l, r)),
            score_after: after,
            result: String::new(), // filled once we know the user's team colour
            banners: if winner.is_empty() { vec![] } else { vec![format!("winner:{winner}")] },
            planted: None,
            decided: None,
        });
        start = end;
    }
    rounds
}

/// Index (position in list) of the round a timestamp belongs to; post-round kills stay in the ended round.
pub fn round_of(t: f64, rounds: &[Round]) -> i32 {
    for (i, rd) in rounds.iter().enumerate() {
        if rd.start <= t && t < rd.end {
            if i > 0 && t - rd.start < POST_ROUND {
                return i as i32 - 1;
            }
            return i as i32;
        }
    }
    rounds.len() as i32 - 1
}

/// Share of red pixels in the timer area above which the spike icon is showing (the icon fills ~40-55% of it;
/// white digits and the post-round countdown stay below 1%).
pub const SPIKE_SHARE: f32 = 0.2;
/// The post-round countdown starts at 0:07 when the round is decided.
const POST_ROUND_COUNTDOWN: i32 = 7;

/// When rounds were decided, from the 1 Hz timer readings: the timer jumps to the post-round countdown
/// (0:07 down to 0:00). A reading of 1-7 s that breaks the countdown before it (or follows a hidden timer,
/// e.g. a planted spike) and is followed by a continuing countdown marks the start.
pub fn round_ends(timer: &[(f64, i32)]) -> Vec<f64> {
    let mut out: Vec<f64> = vec![];
    for i in 0..timer.len() {
        let (t, v) = timer[i];
        if !(1..=POST_ROUND_COUNTDOWN).contains(&v) {
            continue;
        }
        // continuous with the previous reading? then it's just the same timer running low
        if i > 0 {
            let (tp, vp) = timer[i - 1];
            if t - tp <= 1.6 && ((vp as f64 - (t - tp)) - v as f64).abs() <= 2.5 {
                continue;
            }
        }
        // a real countdown keeps going (one OCR miss allowed)
        let goes_on = timer[i + 1..].iter().take_while(|r| r.0 - t <= 2.6).any(|&(tn, vn)| {
            let want = v as f64 - (tn - t);
            (vn as f64 - want).abs() <= 1.0 && vn < v
        });
        if !goes_on {
            continue;
        }
        let decided = t - (POST_ROUND_COUNTDOWN - v) as f64;
        if out.last().map(|&e| decided - e > 10.0).unwrap_or(true) {
            out.push(decided);
        }
    }
    out
}

/// When the spike was planted: starts of runs (at least 1 s, gaps up to 0.6 s) where the red spike icon
/// replaces the round timer. `samples` = (time, red share of the timer area) for every scanned frame.
pub fn spike_plants(samples: &[(f64, f32)]) -> Vec<f64> {
    let mut out = vec![];
    let mut run: Option<(f64, f64)> = None; // (start, last)
    let close = |run: Option<(f64, f64)>, out: &mut Vec<f64>| {
        if let Some((a, b)) = run {
            if b - a >= 1.0 {
                out.push(a);
            }
        }
    };
    for &(t, share) in samples {
        if share < SPIKE_SHARE {
            continue;
        }
        run = match run {
            Some((a, b)) if t - b <= 0.6 => Some((a, t)),
            r => {
                close(r, &mut out);
                Some((t, t))
            }
        };
    }
    close(run, &mut out);
    out
}

/// Put the decided / planted times on their rounds (`end` is the score change, a few seconds after the
/// round was decided).
pub fn attach(rounds: &mut [Round], ends: &[f64], plants: &[f64]) {
    for &e in ends {
        if let Some(r) = rounds.iter_mut().find(|r| r.start < e && e <= r.end + 5.0) {
            r.decided = Some(e); // the last countdown inside the round wins
        }
    }
    for &p in plants {
        if let Some(r) = rounds.iter_mut().find(|r| r.start <= p && p < r.decided.unwrap_or(r.end)) {
            if r.planted.is_none() {
                r.planted = Some(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_scores_rejects_noise() {
        let mut reads: Vec<(f64, i32, i32)> = (0..10).map(|t| (t as f64, 2, 1)).collect();
        reads.push((10.0, 12, 1)); // one-off misread
        reads.extend((11..20).map(|t| (t as f64, 2, 1)));
        reads.extend((20..30).map(|t| (t as f64, 3, 1)));
        let st = smooth_scores(&reads, 3, 6);
        assert_eq!(st.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>(), vec![(2, 1), (3, 1)]);
        assert_eq!(st[1].0, 20.0);
    }

    #[test]
    fn rounds_and_post_round_kills() {
        let st = [(0.0, 2, 1), (60.0, 3, 1), (120.0, 3, 2)];
        let rounds = build_rounds(&st, 200.0, "teal");
        assert_eq!(rounds.len(), 3);
        assert!(rounds[0].banners.contains(&"winner:teal".to_string()));
        assert!(rounds[1].banners.contains(&"winner:red".to_string()));
        assert_eq!(round_of(30.0, &rounds), 0);
        assert_eq!(round_of(63.0, &rounds), 0); // post-round kill
        assert_eq!(round_of(80.0, &rounds), 1);
    }

    #[test]
    fn round_ends_from_the_post_round_countdown() {
        // 1 Hz readings: a round running out of time normally, a round decided at 39 s left, a planted spike
        // (timer hidden) then the countdown, and a misread "0:06" -> 96
        let mut tm: Vec<(f64, i32)> = (0..12).map(|k| (85.0 + k as f64, 51 - k)).collect(); // 51..40 at 85..96
        tm.push((97.0, 39));
        tm.extend([(98.0, 6), (99.0, 5), (100.0, 4), (101.0, 3)]);
        tm.extend([(165.0, 69), (190.0, 96), (191.0, 5), (192.0, 4), (193.0, 3)]);
        tm.extend((0..10).map(|k| (300.0 + k as f64, 9 - k))); // a timer simply counting down to 0: no end
        let ends = round_ends(&tm);
        assert_eq!(ends, vec![97.0, 189.0]);
    }

    #[test]
    fn spike_runs() {
        let mut s: Vec<(f64, f32)> = (0..60).map(|k| (k as f64 / 6.0, 0.0)).collect();
        s.extend((60..90).map(|k| (k as f64 / 6.0, 0.45))); // 5 s of icon from t=10
        s.extend((90..95).map(|k| (k as f64 / 6.0, 0.01)));
        s.push((16.0, 0.5)); // a single red blip: not a plant
        assert_eq!(spike_plants(&s), vec![10.0]);
    }

    #[test]
    fn attach_to_rounds() {
        let mut rounds = build_rounds(&[(0.0, 0, 0), (99.0, 1, 0), (192.0, 1, 1)], 300.0, "teal");
        attach(&mut rounds, &[97.0, 189.0], &[168.0, 250.0]);
        assert_eq!((rounds[0].decided, rounds[0].planted), (Some(97.0), None));
        assert_eq!((rounds[1].decided, rounds[1].planted), (Some(189.0), Some(168.0)));
        assert_eq!((rounds[2].decided, rounds[2].planted), (None, Some(250.0)));
    }
}
