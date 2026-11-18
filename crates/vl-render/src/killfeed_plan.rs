//! Timing and motion of the kill feed rows (pure maths, turned into ffmpeg expressions).
//!
//! Each row enters at its kill time and leaves according to the settings: after N seconds, when the round
//! ends, or never (kept through the whole clip). With a row limit, the oldest visible row leaves the moment a
//! new one needs its place. Rows glide to their new slot instead of jumping, and enter with an animation.

/// seconds for the appear animation
pub const ENTER: f64 = 0.28;
/// seconds for rows to glide to a new slot
pub const SHIFT: f64 = 0.22;
/// fade-out
pub const EXIT: f64 = 0.3;

#[derive(Debug, Clone, PartialEq)]
pub struct RowPlan {
    pub key: String,
    pub start: f64,
    pub end: f64,
    /// stays until the clip ends (no fade-out)
    pub keep_to_end: bool,
}

/// items = (key, output time, round index), any order.
pub fn plan_rows(
    items: &[(String, f64, i32)],
    keep: &str,
    seconds: f64,
    rows: i32,
    clip_end: f64,
    round_end: &dyn Fn(i32, f64) -> Option<f64>,
) -> Vec<RowPlan> {
    let mut items = items.to_vec();
    items.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut plans: Vec<RowPlan> = vec![];
    for (key, t, rnd) in items {
        let mut end = if keep == "clip" {
            clip_end
        } else if keep == "round" {
            // the round's end, else the clip's end (0.0 counts as missing too)
            let e = match round_end(rnd, t) {
                Some(v) if v != 0.0 => v,
                _ => clip_end,
            };
            e.max(t + 1.0)
        } else {
            t + seconds.max(0.5)
        };
        end = end.min(clip_end);
        plans.push(RowPlan { key, start: t, end, keep_to_end: end >= clip_end - 1e-3 });
    }
    // row limit: when a new row arrives and the feed is full, the oldest visible row leaves
    let rows = rows.max(1) as usize;
    for i in 0..plans.len() {
        let start = plans[i].start;
        let mut visible: Vec<usize> = (0..i).filter(|&q| plans[q].end > start).collect();
        while visible.len() >= rows {
            let oldest = visible.remove(0);
            plans[oldest].end = start;
            plans[oldest].keep_to_end = false;
        }
    }
    plans
}

fn f3(v: f64) -> String {
    format!("{v:.3}")
}

/// Per-frame slot index (fractional while gliding) for row i, counted from the top.
pub fn slot_expr(plans: &[RowPlan], i: usize, newest: &str) -> String {
    let me = &plans[i];
    let mut terms = vec![];
    if newest == "bottom" {
        // rows that arrived earlier sit above; when one leaves, this row glides up
        for q in &plans[..i] {
            if q.end > me.start {
                terms.push(format!("clip(({}+{SHIFT}-t)/{SHIFT},0,1)", f3(q.end)));
            }
        }
    } else {
        // newer rows push this one down as they arrive and let it back up when they leave
        for q in &plans[i + 1..] {
            if q.start < me.end {
                terms.push(format!("clip((t-{})/{SHIFT},0,1)*clip(({}+{SHIFT}-t)/{SHIFT},0,1)", f3(q.start), f3(q.end)));
            }
        }
    }
    if terms.is_empty() {
        "0".into()
    } else {
        terms.join("+")
    }
}

/// Eased 0..1 progress of the appear animation (ease-out cubic).
pub fn enter_progress(start: f64) -> String {
    format!("(1-pow(1-clip((t-{})/{ENTER},0,1),3))", f3(start))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<(String, f64, i32)> {
        vec![("a".into(), 1.0, 0), ("b".into(), 2.0, 0), ("c".into(), 3.0, 0), ("d".into(), 12.0, 1)]
    }
    fn none(_: i32, _: f64) -> Option<f64> {
        None
    }

    #[test]
    fn timed_rows_leave_after_n_seconds() {
        let p = plan_rows(&items(), "timed", 4.5, 4, 20.0, &none);
        let got: Vec<(&str, f64, f64)> = p.iter().map(|x| (x.key.as_str(), x.start, x.end)).collect();
        assert_eq!(got, vec![("a", 1.0, 5.5), ("b", 2.0, 6.5), ("c", 3.0, 7.5), ("d", 12.0, 16.5)]);
        assert!(!p.iter().any(|x| x.keep_to_end));
    }

    #[test]
    fn whole_clip_keeps_rows_until_the_end() {
        let p = plan_rows(&items(), "clip", 4.5, 4, 20.0, &none);
        assert!(p.iter().all(|x| x.end == 20.0 && x.keep_to_end));
    }

    #[test]
    fn row_limit_pushes_out_the_oldest() {
        let p = plan_rows(&items(), "clip", 4.5, 2, 20.0, &none);
        let by = |k: &str| p.iter().find(|x| x.key == k).unwrap().clone();
        assert!(by("a").end == 3.0 && !by("a").keep_to_end); // left when "c" arrived
        assert_eq!(by("b").end, 12.0); // left when "d" arrived
        assert!(by("c").keep_to_end && by("d").keep_to_end);
    }

    #[test]
    fn round_end_clears_the_feed() {
        let p = plan_rows(&items(), "round", 4.5, 4, 20.0, &|r, _| if r == 0 { Some(9.0) } else { None });
        assert_eq!(p.iter().map(|x| x.end).collect::<Vec<_>>(), vec![9.0, 9.0, 9.0, 20.0]);
    }

    #[test]
    fn slot_expressions_reference_neighbours() {
        let p = plan_rows(&items()[..3], "clip", 4.5, 4, 20.0, &none);
        assert_eq!(slot_expr(&p, 0, "bottom"), "0");
        assert_eq!(slot_expr(&p, 2, "bottom").matches("clip(").count(), 2); // two rows above it
        assert_eq!(slot_expr(&p, 0, "top").matches("clip((t-").count(), 2); // two newer rows push it down
        assert_eq!(slot_expr(&p, 1, "bottom"), "clip((20.000+0.22-t)/0.22,0,1)");
        assert_eq!(enter_progress(1.0), "(1-pow(1-clip((t-1.000)/0.28,0,1),3))");
    }
}
