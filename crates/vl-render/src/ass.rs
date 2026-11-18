//! ASS subtitle generation (libass renders it in both the mpv preview and the ffmpeg export).
use crate::editlist::EditList;
use crate::numfmt::{f, trunc_i};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use vl_core::models::SubtitleSettings;
use vl_core::transcript::{apply_legacy, Segment, Transcript};

/// (font, bold, uppercase, border_style, outline, shadow, size_scale)
pub type Preset = (&'static str, i32, bool, i32, f64, f64, f64);

pub const PRESETS: &[(&str, Preset)] = &[
    ("clean", ("Inter", 1, false, 1, 2.5, 1.5, 1.0)),
    ("pop", ("Anton", 0, true, 1, 5.0, 0.0, 1.25)),
    ("boxed", ("Inter", 1, false, 3, 10.0, 0.0, 0.95)),
    ("custom", ("Inter", 1, false, 1, 3.0, 1.0, 1.0)),
];

pub const CENSOR_CHAR: &str = "*";

pub fn preset(name: &str) -> Preset {
    PRESETS.iter().find(|p| p.0 == name).map(|p| p.1).unwrap_or(PRESETS[2].1)
}

fn hex_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let c = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(255);
    (c(0), c(2), c(4))
}

pub fn ass_color(hex: &str, alpha: u8) -> String {
    let (r, g, b) = hex_rgb(hex);
    format!("&H{alpha:02X}{b:02X}{g:02X}{r:02X}")
}

pub fn ts(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0).floor() as i64;
    let m = ((t % 3600.0) / 60.0).floor() as i64;
    let s = t % 60.0;
    format!("{h}:{m:02}:{s:05.2}")
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('{', "(").replace('}', ")").replace('\n', " ")
}

/// Copy of the transcript with any old index-based choices folded into the segments.
pub fn prepared<'a>(tr: &'a Transcript, st: &SubtitleSettings) -> Cow<'a, Transcript> {
    let legacy = !(st.hidden_lines.is_empty() && st.edits.is_empty() && st.censored.is_empty());
    if !legacy && !tr.segments.iter().any(|sg| sg.voice_line && !sg.edited && !sg.hidden) {
        return Cow::Borrowed(tr);
    }
    let mut tr2 = tr.clone();
    let mut st2 = st.clone();
    apply_legacy(&mut tr2, &mut st2);
    Cow::Owned(tr2)
}

/// (start, end, text, censored) for a caption.
pub fn visible_words(seg: &Segment) -> Vec<(f64, f64, String, bool)> {
    if !seg.words.is_empty() {
        return seg
            .words
            .iter()
            .filter(|w| !w.text.trim().is_empty())
            .map(|w| (w.start, w.end, w.text.trim().to_string(), w.censored))
            .collect();
    }
    let t = seg.text.trim();
    if t.is_empty() {
        vec![]
    } else {
        vec![(seg.start, seg.end, t.to_string(), false)]
    }
}

pub fn censor_ranges(tr: &Transcript, st: &SubtitleSettings) -> Vec<(f64, f64)> {
    let tr = prepared(tr, st);
    tr.segments.iter().filter(|sg| !sg.hidden).flat_map(|sg| sg.words.iter().filter(|w| w.censored).map(|w| (w.start, w.end))).collect()
}

/// The .ass text (what `build_ass` writes, without the BOM).
pub fn ass_text(
    tr: &Transcript,
    st: &SubtitleSettings,
    el: &EditList,
    width: i64,
    height: i64,
    highlight: &str,
    bx: Option<(i64, i64, i64, i64)>,
) -> String {
    let (mut font, bold, upper, border, mut outline, shadow, sc) = preset(&st.style.preset);
    let custom_font;
    if st.style.preset == "custom" {
        if !st.style.font.is_empty() {
            custom_font = st.style.font.clone();
            font = custom_font.as_str();
        }
        outline = st.style.outline as f64;
    }
    // keep text size sane on vertical canvases
    let rf = if width < height { width.min(height) } else { height } as f64;
    let size = trunc_i(st.style.size as f64 * sc * rf / 1080.0);
    let mut margin_v = trunc_i(height as f64 * (1.0 - st.style.position));
    let mut margin_l = trunc_i(width as f64 * 0.06);
    let mut margin_r = margin_l;
    if let Some((x, y, w, h)) = bx {
        margin_v = 0.max(height - (y + h));
        margin_l = 0.max(x);
        margin_r = 0.max(width - (x + w));
    }
    let primary = ass_color(&st.style.color, 0);
    let back = if border == 3 { "&H64000000" } else { "&H80000000" };
    let outline_col = if border == 1 { "&H00000000" } else { "&H50000000" };
    let hl = ass_color(highlight, 0);
    let mut lines: Vec<String> = vec![
        "[Script Info]".into(),
        "ScriptType: v4.00+".into(),
        format!("PlayResX: {width}"),
        format!("PlayResY: {height}"),
        "WrapStyle: 0".into(),
        "ScaledBorderAndShadow: yes".into(),
        "".into(),
        "[V4+ Styles]".into(),
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, \
         Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, \
         MarginR, MarginV, Encoding"
            .into(),
        format!(
            "Style: Sub,{font},{size},{primary},{primary},{outline_col},{back},{},0,0,0,100,100,0,0,{border},{},{},2,{margin_l},{margin_r},{margin_v},1",
            if bold != 0 { -1 } else { 0 },
            f(outline * rf / 1080.0, 1),
            f(shadow * rf / 1080.0, 1)
        ),
        "".into(),
        "[Events]".into(),
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text".into(),
    ];
    let tr = prepared(tr, st);
    for seg in &tr.segments {
        if seg.hidden {
            continue;
        }
        let words = visible_words(seg);
        if words.is_empty() {
            continue;
        }
        let name = if st.show_names { st.speakers.get(&seg.speaker).cloned().unwrap_or_default() } else { String::new() };
        let prefix = if name.is_empty() { String::new() } else { format!("{{\\c{hl}}}{}:{{\\c{primary}}} ", esc(&name)) };
        let word_text = |w: &str, c: bool| -> String {
            let w = if c { CENSOR_CHAR.repeat(3.max(w.chars().count())) } else { w.to_string() };
            esc(&if upper { w.to_uppercase() } else { w })
        };
        // at most `max_words` on screen: the caption is shown in chunks, each until the next one starts
        let n = if st.max_words > 0 { st.max_words as usize } else { words.len() };
        // one event per word so the spoken word is highlighted
        for (j, (ws, we, _, _)) in words.iter().enumerate() {
            let end_src = if j + 1 < words.len() { words[j + 1].0 } else { we.max(seg.end) };
            let chunk = j / n * n;
            let parts: Vec<String> = words[chunk..(chunk + n).min(words.len())]
                .iter()
                .enumerate()
                .map(|(k, (_, _, w, c))| {
                    let k = chunk + k;
                    let t = word_text(w, *c);
                    if k == j {
                        format!("{{\\c{hl}}}{t}{{\\c{primary}}}")
                    } else {
                        t
                    }
                })
                .collect();
            let text = prefix.clone() + &parts.join(" ");
            for (a, b) in el.map_range(*ws, end_src) {
                if b - a < 0.02 {
                    continue;
                }
                lines.push(format!("Dialogue: 0,{},{},Sub,,0,0,0,,{text}", ts(a), ts(b)));
            }
        }
    }
    lines.join("\n") + "\n"
}

/// Write the subtitles as .ass (UTF-8 with BOM). `bx` = (x, y, w, h) pixel area the
/// text sits in (bottom-centred); defaults to the style position.
#[allow(clippy::too_many_arguments)]
pub fn build_ass(
    tr: &Transcript,
    st: &SubtitleSettings,
    el: &EditList,
    width: i64,
    height: i64,
    highlight: &str,
    out_path: &Path,
    bx: Option<(i64, i64, i64, i64)>,
) -> anyhow::Result<PathBuf> {
    let text = ass_text(tr, st, el, width, height, highlight, bx);
    if let Some(d) = out_path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(crate::numfmt::text_mode(&text).as_bytes());
    std::fs::write(out_path, bytes)?;
    Ok(out_path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_core::models::Part;
    use vl_core::transcript::{add_caption, merge_next, retime_words, set_times, split_at, Word};

    fn word(a: f64, b: f64, t: &str) -> Word {
        Word { start: a, end: b, text: t.into(), censored: false }
    }
    fn seg(a: f64, b: f64, text: &str, words: Vec<Word>) -> Segment {
        Segment {
            start: a,
            end: b,
            text: text.into(),
            speaker: "s1".into(),
            words,
            voice_line: false,
            hidden: false,
            edited: false,
            added: false,
        }
    }
    fn tr() -> Transcript {
        Transcript {
            segments: vec![seg(
                11.0,
                12.5,
                "nice shot man",
                vec![word(11.0, 11.4, "nice"), word(11.4, 12.0, "shot"), word(12.0, 12.5, "man")],
            )],
            ..Default::default()
        }
    }
    fn read(p: &Path) -> String {
        let b = std::fs::read(p).unwrap();
        assert_eq!(&b[..3], &[0xEF, 0xBB, 0xBF]);
        String::from_utf8(b[3..].to_vec()).unwrap()
    }
    fn events(txt: &str) -> Vec<String> {
        txt.lines().filter(|l| l.starts_with("Dialogue:")).map(|s| s.to_string()).collect()
    }

    #[test]
    fn ass_word_highlight_and_censor() {
        let d = tempfile::tempdir().unwrap();
        let mut st = SubtitleSettings { enabled: true, censored: vec![(0, 2)], ..Default::default() };
        st.speakers.insert("s1".into(), "Panda".into());
        let out = build_ass(&tr(), &st, &EditList::plain(&[Part::new(10.0, 15.0)]), 1920, 1080, "#FF7A45", &d.path().join("a.ass"), None)
            .unwrap();
        let ev = events(&read(&out));
        assert_eq!(ev.len(), 3); // one per word
        assert!(ev[0].contains("0:00:01.00")); // source 11s -> output 1s
        assert!(ev[0].contains("***") && !ev[0].contains("man"));
        assert!(ev[0].contains("Panda:"));
        assert_eq!(censor_ranges(&tr(), &st), vec![(12.0, 12.5)]);
    }

    #[test]
    fn hidden_lines() {
        let d = tempfile::tempdir().unwrap();
        let st = SubtitleSettings { enabled: true, hidden_lines: vec![0], ..Default::default() };
        let out = build_ass(&tr(), &st, &EditList::plain(&[Part::new(10.0, 15.0)]), 1920, 1080, "#FFFFFF", &d.path().join("b.ass"), None)
            .unwrap();
        assert!(!read(&out).contains("Dialogue:"));
    }

    fn tr2() -> Transcript {
        Transcript {
            segments: vec![
                seg(10.0, 12.0, "rotate b now", vec![word(10.0, 10.5, "rotate"), word(10.6, 11.0, "b"), word(11.2, 12.0, "now")]),
                seg(13.0, 14.0, "nice", vec![word(13.0, 14.0, "nice")]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn retime_keeps_timing_for_same_word_count() {
        let mut t = tr2();
        retime_words(&mut t.segments[0], "rotate B site");
        assert_eq!(t.segments[0].words.iter().map(|w| w.start).collect::<Vec<_>>(), vec![10.0, 10.6, 11.2]);
        assert_eq!(t.segments[0].text, "rotate B site");
    }

    #[test]
    fn retime_spreads_new_words_by_length() {
        let mut t = tr2();
        retime_words(&mut t.segments[0], "he is one shot on heaven");
        let ws = &t.segments[0].words;
        assert!(ws.len() == 6 && ws[0].start == 10.0 && (ws[5].end - 12.0).abs() < 1e-6);
        assert!((ws[5].end - ws[5].start) > (ws[0].end - ws[0].start));
    }

    #[test]
    fn split_merge_add_and_times() {
        let mut t = tr2();
        let j = split_at(&mut t, 0, 11.1);
        assert_eq!(j, Some(1));
        assert_eq!(t.segments[0].text, "rotate b");
        assert_eq!(t.segments[1].text, "now");
        assert!(merge_next(&mut t, 0) && t.segments[0].text == "rotate b now");
        let i = add_caption(&mut t, 12.2, 0.6, "go go", "s1");
        assert!(t.segments[i].added && t.segments[i].start == 12.2);
        set_times(&mut t.segments[0], 9.0, 13.0);
        assert!(t.segments[0].words[0].start == 9.0 && (t.segments[0].words.last().unwrap().end - 13.0).abs() < 1e-6);
    }

    #[test]
    fn max_words_shows_the_caption_in_chunks() {
        let w = |a: f64, t: &str| Word { start: a, end: a + 0.4, text: t.into(), censored: false };
        let words = vec![w(1.0, "one"), w(1.5, "two"), w(2.0, "three"), w(2.5, "four"), w(3.0, "five")];
        let tr = Transcript { segments: vec![seg(1.0, 3.6, "one two three four five", words)], ..Default::default() };
        let el = EditList::plain(&[Part::new(0.0, 10.0)]);
        let mut st = SubtitleSettings::default();
        let texts = |st: &SubtitleSettings| -> Vec<String> {
            ass_text(&tr, st, &el, 1920, 1080, "#FF0000", None)
                .lines()
                .filter(|l| l.starts_with("Dialogue"))
                .map(|l| l.split(",,0,0,0,,").nth(1).unwrap().replace("{\\c&H000000FF}", "[").replace("{\\c&H00FFFFFF}", "]"))
                .collect()
        };
        // no limit: every event shows the whole caption
        assert!(texts(&st).iter().all(|t| t.matches(' ').count() == 4));
        st.max_words = 2;
        assert_eq!(texts(&st), vec!["[one] two", "one [two]", "[three] four", "three [four]", "[five]"]);
        // the chunks follow each other without gaps
        let times: Vec<String> = ass_text(&tr, &st, &el, 1920, 1080, "#FF0000", None)
            .lines()
            .filter(|l| l.starts_with("Dialogue"))
            .map(|l| l.split(',').nth(2).unwrap().to_string())
            .collect();
        assert_eq!(times, vec!["0:00:01.50", "0:00:02.00", "0:00:02.50", "0:00:03.00", "0:00:03.60"]);
    }

    #[test]
    fn hidden_and_censored_flags_drive_the_ass() {
        let d = tempfile::tempdir().unwrap();
        let mut t = tr2();
        t.segments[1].hidden = true;
        t.segments[0].words[2].censored = true;
        let st = SubtitleSettings { enabled: true, ..Default::default() };
        let out =
            build_ass(&t, &st, &EditList::plain(&[Part::new(0.0, 20.0)]), 1920, 1080, "#FFFFFF", &d.path().join("c.ass"), None).unwrap();
        let ev = events(&read(&out));
        assert!(ev.len() == 3 && ev.iter().all(|e| !e.contains("nice")) && ev[0].contains("***"));
        assert_eq!(censor_ranges(&t, &st), vec![(11.2, 12.0)]);
    }

    #[test]
    fn legacy_index_choices_are_migrated() {
        let mut t = tr2();
        let mut st = SubtitleSettings { enabled: true, hidden_lines: vec![1], censored: vec![(0, 0)], ..Default::default() };
        st.edits.insert("0".into(), "rotate A".into());
        assert!(apply_legacy(&mut t, &mut st));
        assert!(t.segments[1].hidden && t.segments[0].text == "rotate A" && st.hidden_lines.is_empty());
    }

    #[test]
    fn timestamps() {
        assert_eq!(ts(1.0), "0:00:01.00");
        assert_eq!(ts(3725.456), "1:02:05.46");
        assert_eq!(ass_color("#F4A6C4", 0), "&H00C4A6F4");
    }
}
