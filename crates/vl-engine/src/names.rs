//! Player-name normalisation, fuzzy matching and image hashing of name crops.
use crate::imgproc::{self, Interp, Plane};
use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::UnicodeNormalization;
use vl_core::image::BgrImage;

/// Characters the OCR commonly confuses in the Valorant font.
fn confusion(c: char) -> char {
    match c {
        '0' => 'o',
        '1' | '|' | 'i' => 'l',
        '5' | '$' => 's',
        '×' => 'x',
        _ => c,
    }
}

fn keep(c: char) -> bool {
    matches!(c, '0'..='9' | 'a'..='z' | ' ')
        || ('\u{0400}'..='\u{04FF}').contains(&c)
        || ('\u{3040}'..='\u{30FF}').contains(&c)
        || ('\u{3130}'..='\u{318F}').contains(&c)
        || ('\u{AC00}'..='\u{D7AF}').contains(&c)
        || ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

/// Canonical comparable form: lowercase, ascii-folded, confusions merged, no punctuation.
pub fn normalize(name: &str) -> String {
    let s: String = name.nfkd().filter(|&c| canonical_combining_class(c) == 0).collect();
    let s: String = s.to_lowercase().chars().map(confusion).filter(|&c| keep(c)).collect();
    s.split(' ').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- difflib.SequenceMatcher (no junk)
fn longest_match(a: &[char], b: &[char], alo: usize, ahi: usize, blo: usize, bhi: usize) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len = vec![0usize; b.len() + 1];
    let mut new = vec![0usize; b.len() + 1];
    for i in alo..ahi {
        for j in blo..bhi {
            new[j + 1] = if a[i] == b[j] {
                let k = j2len[j] + 1;
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
                k
            } else {
                0
            };
        }
        std::mem::swap(&mut j2len, &mut new);
        new.iter_mut().for_each(|v| *v = 0);
    }
    (besti, bestj, bestsize)
}

fn matching_chars(a: &[char], b: &[char]) -> usize {
    let mut total = 0;
    let mut queue = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, b, alo, ahi, blo, bhi);
        if k > 0 {
            total += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    total
}

/// `difflib.SequenceMatcher(None, a, b).ratio()` (strings shorter than 200 chars: no auto-junk).
pub fn seq_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let t = a.len() + b.len();
    if t == 0 {
        return 1.0;
    }
    2.0 * matching_chars(&a, &b) as f64 / t as f64
}

pub fn similarity(a: &str, b: &str) -> f64 {
    let (na, nb) = (normalize(a), normalize(b));
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na == nb {
        return 1.0;
    }
    // tolerate a trailing glyph picked up from the portrait ("pepe/p")
    let mut r = seq_ratio(&na, &nb);
    if na.starts_with(&nb) || nb.starts_with(&na) {
        let (la, lb) = (na.chars().count(), nb.chars().count());
        let (short, long) = if la <= lb { (la, lb) } else { (lb, la) };
        if short >= 3 && long - short <= 2 {
            r = r.max(0.9);
        }
    }
    r
}

pub fn is_me_text(name: &str) -> bool {
    normalize(name) == "me"
}

pub const HASH_LEN: usize = 48 * 12;

/// Binary text-shape signature robust to background colour/opacity: bright (white) text, cropped to its
/// bounding box, resized to 48x12.
pub fn name_hash(crop: Option<&BgrImage>) -> Vec<u8> {
    let Some(crop) = crop.filter(|c| !c.is_empty()) else { return vec![0; HASH_LEN] };
    let hsv = imgproc::bgr2hsv(crop);
    let m: Vec<u8> = hsv.sat.iter().zip(&hsv.val).map(|(&s, &v)| if s <= 70 && v >= 190 { 255 } else { 0 }).collect();
    let mut m = Plane::from_raw(crop.w, crop.h, m);
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..m.h {
        for x in 0..m.w {
            if m.data[y * m.w + x] != 0 {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x0 != usize::MAX {
        m = m.crop(x0 as i64, y0 as i64, (x1 - x0 + 1) as i64, (y1 - y0 + 1) as i64);
    }
    let r = imgproc::resize_gray(&m, 48, 12, Interp::Area);
    r.data.iter().map(|&v| (v > 64) as u8).collect()
}

pub fn hash_distance(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    a.iter().zip(b).filter(|(x, y)| x != y).count() as f64 / a.len() as f64
}

/// Majority vote over OCR readings, weighted by confidence and grouped by normalized form.
pub fn best_text(votes: &[(String, f64)]) -> (String, f64) {
    let mut groups: Vec<(String, Vec<(String, f64)>)> = vec![];
    for (t, s) in votes {
        let n = normalize(t);
        if n.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|g| g.0 == n) {
            Some(g) => g.1.push((t.clone(), *s)),
            None => groups.push((n, vec![(t.clone(), *s)])),
        }
    }
    if groups.is_empty() {
        return (String::new(), 0.0);
    }
    let gsum = |g: &Vec<(String, f64)>| g.iter().map(|x| x.1).sum::<f64>();
    let mut best = &groups[0].1;
    for g in &groups[1..] {
        if gsum(&g.1) > gsum(best) {
            best = &g.1;
        }
    }
    let mut top = &best[0];
    for x in &best[1..] {
        if x.1 > top.1 {
            top = x;
        }
    }
    let total: f64 = groups.iter().map(|g| gsum(&g.1)).sum();
    (top.0.clone(), gsum(best) / total.max(1e-6) * top.1)
}

/// Alias match tolerant to OCR truncation at the portrait edge ("anda X" for "thePanda X").
pub fn matches_alias(name: &str, alias: &str) -> bool {
    if similarity(name, alias) >= 0.85 {
        return true;
    }
    let (n, a) = (normalize(name), normalize(alias));
    let (ln, la) = (n.chars().count(), a.chars().count());
    if ln >= 5 && ln as f64 >= 0.55 * la as f64 && a.contains(&n) {
        return true; // truncated reading
    }
    la >= 4 && n.starts_with(&a) && ln - la <= 6 // trailing junk (weapon glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_names() {
        for (a, b) in [("thePanda X", "thepanda x"), ("0men", "Omen"), ("pepe/P", "pepe"), ("Hiichamallow", "Hiichamal1ow")] {
            assert!(similarity(a, b) >= 0.8, "{a} {b}");
        }
    }

    #[test]
    fn me_text() {
        assert!(is_me_text("Me"));
        assert!(is_me_text(" me "));
        assert!(!is_me_text("Meme"));
    }

    #[test]
    fn alias_match() {
        for o in ["thePanda X", "thepanda x", "anda X", "thePanda X ⌐T"] {
            assert!(matches_alias(o, "thePanda X"), "{o}");
        }
        for o in ["pepe", "Panda", "X", "DawoodAK"] {
            assert!(!matches_alias(o, "thePanda X"), "{o}");
        }
    }

    #[test]
    fn best_text_vote() {
        let v: Vec<(String, f64)> =
            [("KoReSh", 0.99), ("KoReSh", 0.97), ("K0ReSh", 0.6), ("", 0.0)].iter().map(|(t, s)| (t.to_string(), *s)).collect();
        let (t, s) = best_text(&v);
        assert_eq!(t, "KoReSh");
        assert!(s > 0.5);
    }

    #[test]
    fn difflib_ratio() {
        // values from difflib.SequenceMatcher(None, a, b).ratio()
        assert!((seq_ratio("abcd", "bcde") - 0.75).abs() < 1e-12);
        assert!((seq_ratio("thepanda x", "anda x") - 0.75).abs() < 1e-12);
        assert!((seq_ratio("hllchamallow", "hllchamalow") - 2.0 * 11.0 / 23.0).abs() < 1e-12);
        assert_eq!(normalize("  Ünïcödé  ×1|$ "), "unlcode xlls");
    }
}
