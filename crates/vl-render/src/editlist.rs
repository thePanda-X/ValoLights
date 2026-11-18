//! Map the kept source parts onto the output timeline (with optional transitions).
use vl_core::models::Part;

pub const TRANSITION_DUR: f64 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub src_in: f64,
    pub src_out: f64,
    /// where this part starts on the output timeline
    pub out_start: f64,
}

impl Placed {
    pub fn dur(&self) -> f64 {
        self.src_out - self.src_in
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditList {
    pub parts: Vec<Part>,
    pub transition: String,
    pub td: f64,
    pub placed: Vec<Placed>,
    pub duration: f64,
}

impl EditList {
    pub fn new(parts: &[Part], transition: &str) -> Self {
        let parts: Vec<Part> = parts.iter().copied().filter(|p| p.src_out - p.src_in > 0.05).collect();
        let mut td = if transition != "none" && parts.len() > 1 { TRANSITION_DUR } else { 0.0 };
        // transitions need both neighbours to be longer than the overlap
        if td != 0.0 && parts.iter().any(|p| p.src_out - p.src_in <= td * 2.0) {
            td = 0.0;
        }
        let mut placed = Vec::with_capacity(parts.len());
        let mut t = 0.0;
        let n = parts.len();
        for (i, p) in parts.iter().enumerate() {
            placed.push(Placed { src_in: p.src_in, src_out: p.src_out, out_start: t });
            t += (p.src_out - p.src_in) - if i + 1 < n { td } else { 0.0 };
        }
        EditList { parts, transition: transition.to_string(), td, placed, duration: t }
    }

    /// No transitions.
    pub fn plain(parts: &[Part]) -> Self {
        Self::new(parts, "none")
    }

    /// Output time for a source time, or None when the moment is cut out.
    pub fn to_output(&self, src_t: f64) -> Option<f64> {
        self.placed.iter().find(|pl| pl.src_in <= src_t && src_t <= pl.src_out).map(|pl| pl.out_start + (src_t - pl.src_in))
    }

    pub fn to_source(&self, out_t: f64) -> f64 {
        for pl in self.placed.iter().rev() {
            if out_t >= pl.out_start {
                return pl.src_out.min(pl.src_in + (out_t - pl.out_start));
            }
        }
        self.placed.first().map(|p| p.src_in).unwrap_or(0.0)
    }

    /// Visible output ranges of a source interval (may span several parts).
    pub fn map_range(&self, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out = vec![];
        for pl in &self.placed {
            let (lo, hi) = (a.max(pl.src_in), b.min(pl.src_out));
            if hi > lo {
                out.push((pl.out_start + lo - pl.src_in, pl.out_start + hi - pl.src_in));
            }
        }
        out
    }

    /// Output time of a source moment, or of the last kept frame before it when it was cut out.
    pub fn out_at_or_before(&self, src_t: f64) -> f64 {
        let mut best = 0.0;
        for pl in &self.placed {
            if pl.src_in <= src_t && src_t <= pl.src_out {
                return pl.out_start + (src_t - pl.src_in);
            }
            if src_t > pl.src_out {
                best = pl.out_start + pl.dur();
            } else {
                break;
            }
        }
        best
    }

    /// Output start of the kept part that contains a source moment.
    pub fn part_start_out(&self, src_t: f64) -> Option<f64> {
        self.placed.iter().find(|pl| pl.src_in <= src_t && src_t <= pl.src_out).map(|pl| pl.out_start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editlist_mapping() {
        let el = EditList::plain(&[Part::new(10.0, 15.0), Part::new(30.0, 32.0)]);
        assert_eq!(el.duration, 7.0);
        assert_eq!(el.to_output(12.0), Some(2.0));
        assert_eq!(el.to_output(31.0), Some(6.0));
        assert_eq!(el.to_output(20.0), None);
        assert_eq!(el.to_source(6.0), 31.0);
        assert_eq!(el.map_range(14.0, 31.0), vec![(4.0, 5.0), (5.0, 6.0)]);
    }

    #[test]
    fn editlist_transitions_overlap() {
        let el = EditList::new(&[Part::new(0.0, 5.0), Part::new(10.0, 15.0)], "fade");
        assert!(el.td > 0.0);
        assert_eq!(el.duration, 10.0 - el.td);
        assert_eq!(el.placed[1].out_start, 5.0 - el.td);
    }

    #[test]
    fn editlist_transitions_disabled_for_short_parts() {
        let el = EditList::new(&[Part::new(0.0, 0.5), Part::new(10.0, 15.0)], "fade");
        assert_eq!(el.td, 0.0);
    }
}
