//! Editor exports: DaVinci Resolve EDL (+markers), Premiere Pro XML (FCP7 xmeml), full JSON.
//!
use crate::editlist::EditList;
use crate::highlights;
use crate::numfmt::round_i;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use vl_core::config;
use vl_core::models::{Edits, Events, Kill, Part, Project};
use vl_core::projects::ProjectHandle;

/// Integer timebase + NTSC flag for a source frame rate.
pub fn rate(fps: f64) -> (i64, bool) {
    for base in [24i64, 30, 60] {
        if (fps - base as f64 * 1000.0 / 1001.0).abs() < 0.01 {
            return (base, true);
        }
    }
    (1.max(round_i(fps)), false)
}

pub fn tc(seconds: f64, fps: i64) -> String {
    let f = round_i(seconds * fps as f64);
    let (hh, rem) = (f.div_euclid(3600 * fps), f.rem_euclid(3600 * fps));
    let (mm, rem) = (rem / (60 * fps), rem % (60 * fps));
    let (ss, ff) = (rem / fps, rem % fps);
    format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}")
}

struct Ctx {
    ev: Events,
    ed: Edits,
    parts: Vec<Part>,
    kills: Vec<Kill>,
    names: HashMap<String, String>,
}

fn context(h: &ProjectHandle) -> Result<Ctx> {
    let cfg = config::load();
    let ev = h.events().ok_or_else(|| anyhow!("Project has not been analysed yet"))?;
    let ed = h.edits();
    let me = &h.project.identity.me;
    let parts = if ed.parts.is_empty() {
        highlights::quick_cut("segments", &ev, me, &cfg.highlights, h.project.source.duration, &ed.kills_disabled)?
    } else {
        ed.parts.clone()
    };
    let kills = highlights::my_kills(&ev, me, cfg.highlights.include_team_kills, &ed.kills_disabled);
    let names = ev.players.iter().map(|p| (p.id.clone(), p.display.clone())).collect();
    Ok(Ctx { ev, ed, parts, kills, names })
}

fn kill_label(k: &Kill, names: &HashMap<String, String>) -> String {
    format!("Kill: {}{}", names.get(&k.victim).map(|s| s.as_str()).unwrap_or("?"), if k.headshot { " (HS)" } else { "" })
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn export_resolve_edl(h: &ProjectHandle, out: &Path) -> Result<PathBuf> {
    let c = context(h)?;
    let (fps, _) = rate(if h.project.source.fps > 0.0 { h.project.source.fps } else { 60.0 });
    let el = EditList::plain(&c.parts);
    let src_name = file_name(&h.project.source.path);
    let mut lines = vec![format!("TITLE: {} highlights", h.project.name), "FCM: NON-DROP FRAME".into(), String::new()];
    for (i, pl) in el.placed.iter().enumerate() {
        let rec_in = 3600.0 + pl.out_start;
        lines.push(format!(
            "{:03}  AX       V     C        {} {} {} {}",
            i + 1,
            tc(pl.src_in, fps),
            tc(pl.src_out, fps),
            tc(rec_in, fps),
            tc(rec_in + pl.dur(), fps)
        ));
        lines.push(format!("* FROM CLIP NAME: {src_name}"));
        for k in &c.kills {
            if pl.src_in <= k.t && k.t <= pl.src_out {
                lines.push(format!(" |C:ResolveColorRed |M:{} |D:1", kill_label(k, &c.names)));
            }
        }
        lines.push(String::new());
    }
    crate::numfmt::write_text(out, &lines.join("\n"))?;
    // a markers-only EDL for "Timelines > Import > Timeline Markers from EDL"
    let mut mk = vec!["TITLE: kill markers".to_string(), "FCM: NON-DROP FRAME".into(), String::new()];
    for (i, k) in c.kills.iter().enumerate() {
        let Some(t) = el.to_output(k.t) else { continue };
        let rec = 3600.0 + t;
        let one = 1.0 / fps as f64;
        mk.push(format!(
            "{:03}  001      V     C        {} {} {} {}",
            i + 1,
            tc(rec, fps),
            tc(rec + one, fps),
            tc(rec, fps),
            tc(rec + one, fps)
        ));
        mk.push(format!(" |C:ResolveColorRed |M:{} |D:1", kill_label(k, &c.names)));
        mk.push(String::new());
    }
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    crate::numfmt::write_text(&out.with_file_name(format!("{stem}_markers.edl")), &mk.join("\n"))?;
    let _ = (&c.ev, &c.ed);
    Ok(out.to_path_buf())
}

/// `xml.sax.saxutils.escape`
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('>', "&gt;").replace('<', "&lt;")
}

/// `urllib.parse.quote(s, safe="/%")`
fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/%".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn resolve(p: &str) -> PathBuf {
    let pb = Path::new(p);
    match pb.canonicalize() {
        Ok(c) => vl_core::paths::clean(c),
        Err(_) => std::path::absolute(pb).unwrap_or_else(|_| pb.to_path_buf()),
    }
}

pub fn export_premiere_xml(h: &ProjectHandle, out: &Path) -> Result<PathBuf> {
    let c = context(h)?;
    let src = &h.project.source;
    let (fps, ntsc) = rate(if src.fps > 0.0 { src.fps } else { 60.0 });
    let fpsf = fps as f64;
    let el = EditList::plain(&c.parts);
    let total = round_i(el.duration * fpsf);
    let path = resolve(&src.path);
    let posix = path.to_string_lossy().replace('\\', "/");
    let url = format!("file://localhost/{}", quote(&posix.replace(':', "%3A")));
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let rate_el = format!("<rate><timebase>{fps}</timebase><ntsc>{}</ntsc></rate>", if ntsc { "TRUE" } else { "FALSE" });
    let file_dur = round_i(src.duration * fpsf);
    let file_el = format!(
        "<file id=\"file-1\"><name>{}</name><pathurl>{}</pathurl>{rate_el}<duration>{file_dur}</duration><media><video><samplecharacteristics>{rate_el}\
         <width>{}</width><height>{}</height></samplecharacteristics></video><audio><channelcount>2</channelcount></audio></media></file>",
        escape(&name),
        escape(&url),
        src.width,
        src.height
    );
    let (mut vclips, mut aclips) = (String::new(), String::new());
    for (i, pl) in el.placed.iter().enumerate() {
        let s = round_i(pl.out_start * fpsf);
        let e = s + round_i(pl.dur() * fpsf);
        let a = round_i(pl.src_in * fpsf);
        let b = a + (e - s);
        let fref = if i == 0 { file_el.clone() } else { "<file id=\"file-1\"/>".to_string() };
        let common = format!(
            "<name>{}</name><enabled>TRUE</enabled><duration>{file_dur}</duration>{rate_el}<start>{s}</start><end>{e}</end><in>{a}</in><out>{b}</out>",
            escape(&name)
        );
        vclips += &format!("<clipitem id=\"v{}\">{common}{fref}</clipitem>", i + 1);
        aclips += &format!(
            "<clipitem id=\"a{}\">{common}<file id=\"file-1\"/><sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack></clipitem>",
            i + 1
        );
    }
    let mut markers = String::new();
    for k in &c.kills {
        let Some(t) = el.to_output(k.t) else { continue };
        let fr = round_i(t * fpsf);
        markers +=
            &format!("<marker><name>{}</name><comment></comment><in>{fr}</in><out>-1</out></marker>", escape(&kill_label(k, &c.names)));
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"4\">\
         <sequence id=\"seq-1\"><name>{} highlights</name><duration>{total}</duration>{rate_el}\
         {markers}<media><video><format><samplecharacteristics>{rate_el}<width>{}</width>\
         <height>{}</height></samplecharacteristics></format><track>{vclips}</track></video>\
         <audio><track>{aclips}</track></audio></media></sequence></xmeml>\n",
        escape(&h.project.name),
        src.width,
        src.height
    );
    crate::numfmt::write_text(out, &xml)?;
    Ok(out.to_path_buf())
}

#[derive(Serialize)]
struct MyKill<'a> {
    id: &'a str,
    source_time: f64,
    output_time: Option<f64>,
    victim: Option<&'a str>,
    headshot: bool,
    round: i32,
}

#[derive(Serialize)]
struct Highlight {
    start: f64,
    end: f64,
    round: i32,
    score: f64,
    label: String,
    kills: Vec<String>,
}

#[derive(Serialize)]
struct OutPart {
    src_in: f64,
    src_out: f64,
    out_start: f64,
}

#[derive(Serialize)]
struct Everything<'a> {
    project: &'a Project,
    events: &'a Events,
    edits: &'a Edits,
    my_kills: Vec<MyKill<'a>>,
    highlights: Vec<Highlight>,
    output_parts: Vec<OutPart>,
}

pub fn export_json(h: &ProjectHandle, out: &Path) -> Result<PathBuf> {
    let c = context(h)?;
    let cfg = config::load();
    let el = EditList::plain(&c.parts);
    let segs = highlights::build_segments(&c.ev, &h.project.identity.me, &cfg.highlights, h.project.source.duration, &c.ed.kills_disabled);
    let data = Everything {
        project: &h.project,
        events: &c.ev,
        edits: &c.ed,
        my_kills: c
            .kills
            .iter()
            .map(|k| MyKill {
                id: &k.id,
                source_time: k.t,
                output_time: el.to_output(k.t),
                victim: c.names.get(&k.victim).map(|s| s.as_str()),
                headshot: k.headshot,
                round: k.round,
            })
            .collect(),
        highlights: segs
            .iter()
            .map(|s| Highlight {
                start: s.start,
                end: s.end,
                round: s.round,
                score: s.score,
                label: s.label(),
                kills: s.kills.iter().map(|k| k.id.clone()).collect(),
            })
            .collect(),
        output_parts: el.placed.iter().map(|p| OutPart { src_in: p.src_in, src_out: p.src_out, out_start: p.out_start }).collect(),
    };
    // json.dumps(ensure_ascii=True): non-ASCII only occurs inside strings, so escaping it everywhere is valid
    let mut s = String::new();
    for c in serde_json::to_string_pretty(&data)?.chars() {
        if (c as u32) > 0x7e {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                s.push_str(&format!("\\u{u:04x}"));
            }
        } else {
            s.push(c);
        }
    }
    crate::numfmt::write_text(out, &s)?;
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timecodes_and_rates() {
        assert_eq!(rate(59.94), (60, true));
        assert_eq!(rate(60.0), (60, false));
        assert_eq!(tc(3600.0 + 1.5, 60), "01:00:01:30");
        assert_eq!(quote("C%3A/a b/é"), "C%3A/a%20b/%C3%A9");
    }
}
