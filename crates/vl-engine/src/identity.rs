//! Player clustering, team assignment and "who is me" resolution.
use crate::detectors::killfeed::color_masks;
use crate::imgproc::{self, Interp};
use crate::names;
use crate::rounds::most_common;
use std::collections::HashMap;
use vl_core::image::BgrImage;
use vl_core::models::{Events, Player};

// ---------------------------------------------------------------- low-res identity: agent portraits
// On low-res video names are a few pixels tall and can't be read, but every kill feed row shows the killer's and
// the victim's agent portrait in their team colour, and within a team every agent is unique: (colour, portrait)
// tells the 10 players apart. The killer's portrait is drawn mirrored relative to the victim's.
/// average similarity to join a group
pub const GROUP_SAME: f32 = 0.65;
/// leftovers join the nearest big group above this
pub const GROUP_FLOOR: f32 = 0.50;
/// sightings that make a group a player
pub const GROUP_BIG: usize = 5;

/// A real portrait: not a slice of row colour (a misplaced name/weapon crop) and not flat background.
pub fn portrait_valid(img: Option<&BgrImage>) -> bool {
    let Some(img) = img else { return false };
    if img.is_empty() || img.w.min(img.h) < 6 {
        return false;
    }
    let (t, r) = color_masks(img, false);
    let both = t.data.iter().zip(&r.data).filter(|(&a, &b)| a | b != 0).count() as f64 / t.data.len() as f64;
    if both > 0.30 {
        return false;
    }
    imgproc::std_u8(&imgproc::bgr2gray(img).data) > 18.0
}

/// Blurred 8x8 colour thumbnail of the portrait's centre, normalised (tolerant of blur and 1-2 px shifts).
pub fn portrait_sig(img: &BgrImage, mirrored: bool) -> Vec<f32> {
    let img = if mirrored { imgproc::flip_h(img) } else { img.clone() };
    let (h, w) = (img.h as i64, img.w as i64);
    let (dy, dx) = ((h as f64 * 0.2) as i64, (w as f64 * 0.2) as i64);
    let c = imgproc::slice(&img, dy, h - dy, dx, w - dx);
    let c = imgproc::gaussian_blur_bgr(&c, 0, 1f64.max(c.h as f64 / 10.0));
    let v: Vec<f32> = imgproc::resize(&c, 8, 8, Interp::Area).data.iter().map(|&x| x as f32).collect();
    let m = (v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64) as f32;
    let v: Vec<f32> = v.iter().map(|x| x - m).collect();
    let n = v.iter().map(|&x| x as f64 * x as f64).sum::<f64>().sqrt() as f32 + 1e-6;
    v.iter().map(|x| x / n).collect()
}

fn dotf(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// One sighting for portrait grouping: (colour, portrait image, is_killer, gold row).
pub type Sighting<'a> = (&'a str, Option<&'a BgrImage>, bool, bool);

/// Group sightings into players; a group label per item. Gold rows are always the recording player's kills, so
/// they form the "gold" group by the gold itself (the gradient tints and shifts their portrait crops too much to
/// match them to a portrait group reliably).
pub fn portrait_groups(items: &[Sighting]) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; items.len()];
    let gold: Vec<bool> = items.iter().map(|it| it.2 && it.3).collect();
    for (i, &g) in gold.iter().enumerate() {
        if g {
            out[i] = Some("gold".into());
        }
    }
    let mut cols: Vec<&str> = items.iter().map(|it| it.0).filter(|c| !c.is_empty()).collect();
    cols.sort();
    cols.dedup();
    for col in cols {
        let idx: Vec<usize> = (0..items.len()).filter(|&i| items[i].0 == col && !gold[i] && portrait_valid(items[i].1)).collect();
        if idx.is_empty() {
            continue;
        }
        let s: Vec<Vec<f32>> = idx.iter().map(|&i| portrait_sig(items[i].1.unwrap(), items[i].2)).collect();
        let n = s.len();
        let sim: Vec<Vec<f32>> = (0..n).map(|i| (0..n).map(|j| dotf(&s[i], &s[j])).collect()).collect();
        let mut groups: Vec<Vec<usize>> = vec![];
        for i in 0..n {
            let (mut best, mut bs) = (None, GROUP_SAME);
            for (gi, g) in groups.iter().enumerate() {
                let m = g.iter().map(|&j| sim[i][j]).sum::<f32>() / g.len() as f32;
                if m > bs {
                    best = Some(gi);
                    bs = m;
                }
            }
            match best {
                None => groups.push(vec![i]),
                Some(gi) => groups[gi].push(i),
            }
        }
        let big: Vec<usize> = (0..groups.len()).filter(|&g| groups[g].len() >= GROUP_BIG).collect();
        let cents: Vec<Vec<f32>> = big
            .iter()
            .map(|&g| {
                let mut c = vec![0f32; s[0].len()];
                for &i in &groups[g] {
                    for (a, b) in c.iter_mut().zip(&s[i]) {
                        *a += b;
                    }
                }
                let len = groups[g].len() as f32;
                c.iter_mut().for_each(|v| *v /= len);
                let nrm = c.iter().map(|v| v * v).sum::<f32>().sqrt() + 1e-6;
                c.iter().map(|v| v / nrm).collect()
            })
            .collect();
        let mut extra: Vec<Vec<usize>> = vec![vec![]; big.len()];
        for g in &groups {
            if g.len() >= GROUP_BIG {
                continue;
            }
            for &i in g {
                let sc: Vec<f32> = cents.iter().map(|ce| dotf(&s[i], ce)).collect();
                let mut am = None;
                for (k, &v) in sc.iter().enumerate() {
                    if am.map(|a: usize| v > sc[a]).unwrap_or(true) {
                        am = Some(k);
                    }
                }
                if let Some(a) = am {
                    if sc[a] >= GROUP_FLOOR {
                        extra[a].push(i);
                    }
                }
            }
        }
        for (k, &g) in big.iter().enumerate() {
            for &i in groups[g].iter().chain(&extra[k]) {
                out[idx[i]] = Some(format!("{col}{k}"));
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct Cluster {
    pub id: String,
    pub texts: Vec<(String, f64)>,
    pub hashes: Vec<Vec<u8>>,
    pub seen: u32,
    pub gold: u32,
    pub crop: Option<BgrImage>,
    pub crop_score: f64,
    /// portrait group (low-res sources)
    pub group: String,
    pub portrait_img: Option<BgrImage>,
}

impl Cluster {
    fn new(id: String) -> Self {
        Cluster { id, crop_score: -1.0, ..Default::default() }
    }

    pub fn best(&self) -> String {
        names::best_text(&self.texts).0
    }

    pub fn matches(&self, text: &str, score: f64, h: Option<&[u8]>) -> f64 {
        let bt = self.best();
        let (nt, nb) = (names::normalize(text), names::normalize(&bt));
        if !nt.is_empty() && !nb.is_empty() && score >= 0.5 {
            let s = names::similarity(text, &bt);
            if s >= 0.8 {
                return s;
            }
            let (short, long) = if nb.chars().count() < nt.chars().count() { (&nb, &nt) } else { (&nt, &nb) };
            if short.chars().count() >= 4 && long.contains(short.as_str()) {
                return 0.8;
            }
            return 0.0;
        }
        // unreadable text: compare the shape of the rendered name
        if let Some(h) = h.filter(|h| h.iter().any(|&v| v != 0)) {
            if !self.hashes.is_empty() {
                let d = self.hashes[self.hashes.len().saturating_sub(5)..]
                    .iter()
                    .map(|x| names::hash_distance(h, x))
                    .fold(f64::INFINITY, f64::min);
                if d <= 0.12 {
                    return 1.0 - d;
                }
            }
        }
        0.0
    }
}

#[derive(Debug, Default)]
pub struct PlayerRegistry {
    pub clusters: Vec<Cluster>,
}

impl PlayerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `group`: the sighting's portrait group (low-res sources): it decides the player, names don't.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        &mut self,
        text: &str,
        score: f64,
        h: Option<&[u8]>,
        crop: Option<&BgrImage>,
        gold: bool,
        group: Option<&str>,
        portrait: Option<&BgrImage>,
    ) -> String {
        let mut best: Option<usize> = None;
        let mut best_s = 0.0;
        if let Some(g) = group.filter(|g| !g.is_empty()) {
            let i = match self.clusters.iter().position(|c| c.group == g) {
                Some(i) => i,
                None => {
                    let mut c = Cluster::new(format!("p{}", self.clusters.len() + 1));
                    c.group = g.to_string();
                    self.clusters.push(c);
                    self.clusters.len() - 1
                }
            };
            if self.clusters[i].portrait_img.is_none() {
                if let Some(p) = portrait {
                    self.clusters[i].portrait_img = Some(p.clone());
                }
            }
            best = Some(i);
            best_s = 1.0;
        } else if gold && self.clusters.iter().any(|c| !c.group.is_empty() && c.gold > 0) {
            // unreadable portrait on a gold row: the local player's own kill
            let mut bi = None;
            for (i, c) in self.clusters.iter().enumerate() {
                if !c.group.is_empty() && bi.map(|b: usize| c.gold > self.clusters[b].gold).unwrap_or(true) {
                    bi = Some(i);
                }
            }
            best = bi;
            best_s = 1.0;
        }
        if best.is_none() {
            for (i, c) in self.clusters.iter().enumerate() {
                let s = c.matches(text, score, h);
                if s > best_s {
                    best = Some(i);
                    best_s = s;
                }
            }
        }
        let bi = match best {
            Some(i) => i,
            None => {
                self.clusters.push(Cluster::new(format!("p{}", self.clusters.len() + 1)));
                self.clusters.len() - 1
            }
        };
        let b = &mut self.clusters[bi];
        if !text.is_empty() && score >= 0.3 {
            b.texts.push((text.to_string(), score));
        }
        if let Some(h) = h.filter(|h| h.iter().any(|&v| v != 0)) {
            b.hashes.push(h.to_vec());
        }
        b.seen += 1;
        b.gold += gold as u32;
        if let Some(c) = crop {
            if score > b.crop_score {
                b.crop = Some(c.clone());
                b.crop_score = score;
            }
        }
        b.id.clone()
    }

    pub fn players(&self) -> Vec<Player> {
        self.clusters
            .iter()
            .map(|c| {
                let mut sorted: Vec<&(String, f64)> = c.texts.iter().collect();
                sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let mut variants: Vec<String> = vec![];
                for (t, _) in sorted {
                    if !variants.contains(t) {
                        variants.push(t.clone());
                    }
                }
                variants.truncate(5);
                let best = c.best();
                Player {
                    id: c.id.clone(),
                    names: variants,
                    display: if best.is_empty() { "?".into() } else { best },
                    seen: c.seen,
                    ..Default::default()
                }
            })
            .collect()
    }
}

/// Killer and victim are on opposite teams: 2-colour the kill graph (majority vote).
pub fn assign_teams(ev: &mut Events) {
    let mut order: Vec<String> = vec![];
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for k in &ev.kills {
        if k.killer == k.victim {
            continue;
        }
        for (a, b) in [(&k.killer, &k.victim), (&k.victim, &k.killer)] {
            if !adj.contains_key(a) {
                order.push(a.clone());
            }
            adj.entry(a.clone()).or_default().push(b.clone());
        }
    }
    let mut team: HashMap<String, &'static str> = HashMap::new();
    let mut starts = order.clone();
    starts.sort_by_key(|p| std::cmp::Reverse(adj[p].len()));
    for start in starts {
        if team.contains_key(&start) {
            continue;
        }
        team.insert(start.clone(), "A");
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(p) = queue.pop_front() {
            for q in &adj[&p] {
                if team.contains_key(q) {
                    continue;
                }
                let votes: Vec<&str> = adj[q].iter().filter_map(|x| team.get(x).copied()).collect();
                let a = votes.iter().filter(|&&v| v == "A").count();
                let b = votes.len() - a;
                team.insert(q.clone(), if a >= b { "B" } else { "A" });
                queue.push_back(q.clone());
            }
        }
    }
    for p in &mut ev.players {
        p.team = team.get(&p.id).copied().unwrap_or("").to_string();
    }
}

/// Player ids that are the local user, setting `me_hint` on them.
pub fn resolve_me(ev: &mut Events, aliases: &[String]) -> Vec<String> {
    let mut me = vec![];
    for p in &mut ev.players {
        if p.me_hint == "user" {
            // picked by the user: always wins
            me.push(p.id.clone());
            continue;
        }
        let mut hint = "";
        if p.names.iter().any(|n| names::is_me_text(n)) || names::is_me_text(&p.display) {
            hint = "me_text";
        } else if !aliases.is_empty() && p.names.iter().take(3).any(|n| aliases.iter().any(|a| names::matches_alias(n, a))) {
            hint = "alias";
        }
        if !hint.is_empty() {
            p.me_hint = hint.into();
            me.push(p.id.clone());
        }
    }
    if me.is_empty() {
        // gold-tinted kill feed rows are only drawn for the local player (live footage)
        if let Some((pid, n)) = most_common(ev.kills.iter().filter(|k| k.gold).map(|k| k.killer.clone())) {
            if n >= 2 {
                if let Some(p) = ev.player_mut(&pid) {
                    p.me_hint = "gold".into();
                    me.push(pid);
                }
            }
        }
    }
    me
}

/// Players ranked for the "who are you" dialog (most kill feed appearances first).
pub fn candidates_for_picker(ev: &Events) -> Vec<Player> {
    let mut v = ev.players.clone();
    v.sort_by_key(|p| std::cmp::Reverse(p.seen));
    v
}
