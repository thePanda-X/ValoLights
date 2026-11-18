//! User-editable speech word lists in data/: the vocabulary the speech model should expect, and the agent
//! voice lines to hide from subtitles. Shared by the app (vocabulary dialog) and the transcriber.
use crate::paths;

pub const DEFAULT_VOCAB: &str = "# Words the speech model should expect in your comms. One per line or comma separated.
# Player names from each clip's killfeed are added automatically.
Astra, Breach, Brimstone, Chamber, Clove, Cypher, Deadlock, Fade, Gekko, Harbor, Iso, Jett, KAY/O, Killjoy,
Neon, Omen, Phoenix, Raze, Reyna, Sage, Skye, Sova, Tejo, Viper, Vyse, Waylay, Yoru
Ascent, Bind, Breeze, Fracture, Haven, Icebox, Lotus, Pearl, Split, Sunset, Abyss, Corrode
A site, B site, C site, mid, heaven, hell, main, link, lobby, garage, market, tree, showers, hookah, elbow
spike, plant, planted, defuse, defusing, rotate, lurk, flank, push, retake, eco, force buy, full buy, save
Vandal, Phantom, Operator, Op, Sheriff, Ghost, Spectre, Odin, Judge, Marshal, Outlaw
one shot, one HP, lit, tagged, dink, ace, clutch, nice, nice try, trade, util, smoke, flash, wall, ult
";

pub const DEFAULT_VOICE_LINES: &str = "# Agent voice lines to hide from subtitles (one per line, case-insensitive, regex allowed).
# Lines starting with # are ignored.
";

/// data/vocabulary.txt (created with the defaults when missing).
pub fn vocabulary_path() -> std::path::PathBuf {
    paths::data_dir().join("vocabulary.txt")
}

/// The vocabulary words plus clip-specific extras (player names), in order, without duplicates.
pub fn vocabulary(extra: &[String]) -> Vec<String> {
    let p = vocabulary_path();
    if !p.exists() {
        let _ = std::fs::create_dir_all(paths::data_dir());
        let _ = std::fs::write(&p, DEFAULT_VOCAB);
    }
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| DEFAULT_VOCAB.into());
    let mut words: Vec<String> = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        words.extend(line.split(',').map(str::trim).filter(|w| !w.is_empty()).map(String::from));
    }
    for w in extra {
        if !w.is_empty() && !words.contains(w) && w != "?" && w.chars().count() >= 2 {
            words.push(w.clone());
        }
    }
    words
}

/// The voice-line patterns of data/voice_lines.txt (case-insensitive regex; invalid lines match literally).
pub fn voice_line_patterns() -> Vec<regex::Regex> {
    let p = paths::data_dir().join("voice_lines.txt");
    if !p.exists() {
        let _ = std::fs::write(&p, DEFAULT_VOICE_LINES);
    }
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            regex::RegexBuilder::new(l)
                .case_insensitive(true)
                .build()
                .or_else(|_| regex::RegexBuilder::new(&regex::escape(l)).case_insensitive(true).build())
                .ok()
        })
        .collect()
}
