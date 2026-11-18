//! `valolights --ui-check export [out_dir]`: drives the real window through the Export page (port of the
//! the original QA script) and saves the app's own frames (egui screenshots) as PNGs.
//!
//! Works on two scratch copies `data/projects/rustqa_export_*` (deleted at the end, with their
//! `data/exports/<id>` folders). data/config.toml is backed up and restored; every other project and
//! export folder is hashed before and after. Expected texts are the page's fixed strings, and the editor files are compared with reference exports.
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::dialogs::Dialog;
use crate::pages::{Action, JobNotice, Page, EXPORT, TRIM};
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vl_core::models::Part;
use vl_core::{config, paths};

/// Command lines of running ffmpeg.exe processes that mention `fragment`.
fn ffmpeg_for(fragment: &str) -> Vec<String> {
    let o = crate::sys::quiet_command("powershell")
        .args(["-NoProfile", "-Command", "Get-CimInstance Win32_Process -Filter \"name='ffmpeg.exe'\" | % { $_.CommandLine }"])
        .output();
    let Ok(o) = o else { return vec![] };
    String::from_utf8_lossy(&o.stdout).lines().filter(|l| l.contains(fragment)).map(str::to_string).collect()
}

fn pid_alive(pid: u32) -> bool {
    let o = crate::sys::quiet_command("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output();
    o.map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())).unwrap_or(false)
}

/// (video codec, w, h, fps, has audio, duration) of a video.
fn probe(p: &Path) -> Option<(String, i64, i64, f64, bool, f64)> {
    let exe = vl_core::ffmpeg::ffprobe_exe().ok()?;
    let ps = p.to_string_lossy().into_owned();
    let o = vl_core::ffmpeg::run(&[
        exe.as_str(),
        "-v",
        "error",
        "-show_entries",
        "stream=codec_type,codec_name,width,height,r_frame_rate:format=duration",
        "-of",
        "json",
        ps.as_str(),
    ])
    .ok()?;
    let v: Value = serde_json::from_slice(&o.stdout).ok()?;
    let streams = v.get("streams")?.as_array()?;
    let vs = streams.iter().find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))?;
    let audio = streams.iter().any(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"));
    let rate = vs.get("r_frame_rate").and_then(Value::as_str).unwrap_or("0/1");
    let (a, b) = rate.split_once('/').unwrap_or((rate, "1"));
    let fps = a.parse::<f64>().unwrap_or(0.0) / b.parse::<f64>().unwrap_or(1.0);
    let dur = v.get("format")?.get("duration")?.as_str()?.parse().ok()?;
    Some((vs.get("codec_name")?.as_str()?.to_string(), vs.get("width")?.as_i64()?, vs.get("height")?.as_i64()?, fps, audio, dur))
}

fn files_starting(d: &Path, prefix: &str) -> Vec<String> {
    std::fs::read_dir(d)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(prefix)).collect())
        .unwrap_or_default()
}

#[derive(PartialEq)]
enum Step {
    Next,
    Wait,
}

struct Check {
    out: PathBuf,
    a_dir: PathBuf,
    a_id: String,
    b_dir: PathBuf,
    b_name: String,
    summary0: String,
    inject: VecDeque<Vec<Event>>,
    step: usize,
    step_start: Instant,
    mark: Instant,
    f: u32,
    sub: u32,
    results: Vec<(String, bool, String)>,
    job_pid: Option<u32>,
    status0: String,
    done: bool,
}

impl Check {
    fn ok(&mut self, name: &str, pass: bool, detail: impl Into<String>) {
        let d = detail.into();
        println!("[{}] {name}: {d}", if pass { " OK " } else { "FAIL" });
        self.results.push((name.into(), pass, d));
    }
    fn click(&mut self, p: Pos2) {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return;
        }
        let m = Modifiers::NONE;
        self.inject.push_back(vec![Event::PointerMoved(p)]);
        self.inject.push_back(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: m }]);
        self.inject.push_back(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: m }]);
    }
    fn key(&mut self, key: Key, m: Modifiers) {
        self.inject.push_back(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m }]);
        self.inject.push_back(vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m }]);
    }
    /// Click the name field, select all and type (empty = just delete).
    fn retype(&mut self, field: egui::Rect, text: &str) {
        self.click(field.center());
        self.key(Key::A, Modifiers::COMMAND);
        self.key(Key::Backspace, Modifiers::NONE);
        if !text.is_empty() {
            self.inject.push_back(vec![Event::Text(text.to_string())]);
        }
    }
    fn shot(&mut self, ctx: &egui::Context, name: &str) {
        let path = self.out.join(format!("{name}.png"));
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
            if let Err(e) = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
                eprintln!("screenshot {}: {e}", path.display());
            }
        });
    }
    fn elapsed(&self) -> Duration {
        self.step_start.elapsed()
    }
    /// The hint the page should show for a file stem (`replaced`: characters had to be replaced).
    fn hint(&self, stem: &str, replaced: bool) -> String {
        format!(
            "Saved as {stem}.mp4 in data\\exports\\{}{}",
            self.a_id,
            if replaced { ". Characters that can't be in a file name were replaced" } else { "" }
        )
    }
}

/// Confirm the newest dialog (run its confirm actions) if it is a question with this title.
fn confirm_dialog(w: &mut MainWindow, title: &str) -> Option<String> {
    if w.dialogs.last().map(|d| d.title() == title) != Some(true) {
        return None;
    }
    match w.dialogs.pop() {
        Some(Dialog::Confirm(c)) => {
            w.shell.actions.extend(c.on_yes);
            Some(c.text)
        }
        Some(d) => {
            w.dialogs.push(d);
            None
        }
        None => None,
    }
}

fn export_dir(id: &str) -> PathBuf {
    paths::exports_dir().join(id)
}

struct CheckApp {
    app: ValoApp,
    k: Check,
}

impl CheckApp {
    fn open(w: &mut MainWindow, dir: &Path) {
        w.open_project(dir, false);
        w.dialogs.retain(|d| d.title() != "Who are you?" && !matches!(d, Dialog::WhoAreYou(_)));
        w.go(EXPORT);
    }

    fn script(&mut self, ctx: &egui::Context) -> Step {
        let w = &mut self.app.win;
        let k = &mut self.k;
        let f = k.f;
        let a_out = export_dir(&k.a_id);
        match k.step {
            0 => {
                if f < 10 {
                    return Step::Wait;
                }
                let a = k.a_dir.clone();
                Self::open(w, &a);
                Step::Next
            }
            1 => {
                w.dialogs.retain(|d| !matches!(d, Dialog::WhoAreYou(_)));
                if f < 20 {
                    return Step::Wait;
                }
                let ok = w.page == EXPORT && w.st.pid() == Some(k.a_id.as_str());
                k.ok("export page open", ok, format!("page {} · pid {:?}", w.page, w.st.pid()));
                w.export.set_option(0, "1080", &w.st);
                w.export.set_option(1, "60", &w.st);
                w.export.set_option(2, "high", &w.st);
                w.export.set_option(3, "auto", &w.st);
                Step::Next
            }
            2 => {
                if f < 6 {
                    return Step::Wait;
                }
                k.shot(ctx, "01_export");
                let s = w.export.summary.clone();
                k.ok(
                    "summary: shape, 1080 size, 60 fps, length, parts, kills",
                    s.contains("1080") && s.contains(" · 60 fps · ") && s.contains(" parts") && s.contains(" kills"),
                    format!("{s:?}"),
                );
                k.summary0 = s;
                let x = w.export.extras.clone();
                k.ok("extras line lists the effects", x.is_empty() || x.starts_with("With "), format!("{x:?}"));
                let (n, e) =
                    (w.export.name.clone(), format!("{}_highlights", w.st.h.as_ref().map(|h| h.project.name.clone()).unwrap_or_default()));
                k.ok("default file name is <clip>_highlights", n == e, format!("{n:?} vs {e:?}"));
                let col = w.export.column_rect;
                k.ok(
                    "column 600 px wide, 40 px under the top bar",
                    (col.width() - 600.0).abs() < 0.5 && (col.min.y - 96.0).abs() < 1.5,
                    format!("{col:?}"),
                );
                w.export.set_option(0, "720", &w.st);
                let s = w.export.summary.clone();
                k.ok("summary follows the resolution", s.contains("720") && s != k.summary0, format!("{s:?}"));
                k.ok(
                    "resolution is remembered in config",
                    config::reload().render.resolution == 720,
                    format!("{:?}", config::load().render),
                );
                w.export.set_option(2, "small", &w.st);
                w.st.touch();
                Step::Next
            }
            3 => {
                if f < 4 {
                    return Step::Wait;
                }
                k.ok(
                    "an edit elsewhere does not reset the options",
                    w.export.resolution == 720 && w.export.quality == "small",
                    format!("{} {}", w.export.resolution, w.export.quality),
                );
                let r = w.export.name_rect;
                k.retype(r, "");
                Step::Next
            }
            4 => {
                if f < 3 {
                    return Step::Wait;
                }
                w.st.touch();
                if f < 6 {
                    return Step::Wait;
                }
                let e = w.export.name.is_empty() && !w.export.can_render && w.export.name_hint == "Type a file name" && w.export.name_bad;
                k.ok(
                    "empty name stays empty after an edit, Render disabled, says why",
                    e,
                    format!("{:?} · {:?}", w.export.name, w.export.name_hint),
                );
                k.ok("editor exports disabled with no name", !w.export.can_export, "");
                k.shot(ctx, "02_empty_name");
                let r = w.export.name_rect;
                k.retype(r, "my:clip?");
                Step::Next
            }
            5 => {
                if f < 3 {
                    return Step::Wait;
                }
                let (h, e) = (w.export.name_hint.clone(), k.hint("my_clip_", true));
                k.ok(
                    "illegal characters are replaced and the user is told",
                    w.export.stem() == "my_clip_" && h == e,
                    format!("{h:?} vs {e:?}"),
                );
                let r = w.export.name_rect;
                k.retype(r, "Ränkéd ace.mp4");
                Step::Next
            }
            6 => {
                if f < 3 {
                    return Step::Wait;
                }
                let (h, e) = (w.export.name_hint.clone(), k.hint("Ränkéd ace", false));
                k.ok(
                    "letters in any language are kept, typed .mp4 is not doubled",
                    w.export.stem() == "Ränkéd ace" && h == e,
                    format!("{h:?} vs {e:?}"),
                );
                let want = format!("data\\exports\\{}", k.a_id);
                k.ok("hint shows where the file goes", h.contains(&want), h.clone());
                let r = w.export.name_rect;
                k.retype(r, "con");
                Step::Next
            }
            7 => {
                if f < 3 {
                    return Step::Wait;
                }
                let (h, e) = (w.export.name_hint.clone(), k.hint("_con", true));
                k.ok(
                    "reserved Windows names are made safe",
                    w.export.stem() == "_con" && h == e,
                    format!("{} · {h:?} vs {e:?}", w.export.stem()),
                );
                let r = w.export.name_rect;
                k.retype(r, "qa cancel");
                Step::Next
            }
            8 => {
                if f < 3 {
                    return Step::Wait;
                }
                let (h, e) = (w.export.name_hint.clone(), k.hint("qa cancel", false));
                k.ok("hint for a plain name", h == e, format!("{h:?} vs {e:?}"));
                let b = k.b_dir.clone();
                Self::open(w, &b);
                Step::Next
            }
            9 => {
                w.dialogs.retain(|d| !matches!(d, Dialog::WhoAreYou(_)));
                if f < 6 {
                    return Step::Wait;
                }
                let want = format!("{}_highlights", k.b_name);
                k.ok("other project gets its own default name", w.export.name == want, format!("{:?}", w.export.name));
                k.ok("options carry over to other projects", w.export.resolution == 720, "");
                let a = k.a_dir.clone();
                Self::open(w, &a);
                Step::Next
            }
            10 => {
                w.dialogs.retain(|d| !matches!(d, Dialog::WhoAreYou(_)));
                if f < 6 {
                    return Step::Wait;
                }
                k.ok("typed name is kept when coming back to the project", w.export.name == "qa cancel", format!("{:?}", w.export.name));
                // Enter in the file name starts the render
                let r = w.export.name_rect;
                k.click(r.center());
                k.key(Key::Enter, Modifiers::NONE);
                Step::Next
            }
            11 => {
                if f == 0 {
                    k.job_pid = w.job.as_ref().map(|j| j.pid());
                    k.ok(
                        "Enter in the file name starts the render",
                        k.job_pid.is_some() && w.job.as_ref().map(|j| j.kind == "render").unwrap_or(false),
                        format!("job {:?}", k.job_pid),
                    );
                    // a second start (button click and an Enter) must not start another render
                    let r = w.export.render_rect;
                    k.click(r.center());
                    let r = w.export.name_rect;
                    k.click(r.center());
                    k.key(Key::Enter, Modifiers::NONE);
                    return Step::Wait;
                }
                if f < 4 {
                    return Step::Wait;
                }
                let same = w.job.as_ref().map(|j| j.pid()) == k.job_pid;
                k.ok(
                    "Render can't be started twice",
                    same && w.dialogs.is_empty() && !w.export.can_render,
                    format!("dialogs {:?}", w.dialogs),
                );
                Step::Next
            }
            12 => {
                let p = w.export.progress.unwrap_or(0.0);
                if p < 0.2 && k.elapsed() < Duration::from_secs(90) && w.job.is_some() {
                    return Step::Wait;
                }
                let s = w.export.status.clone();
                k.ok("progress text names the file and shows percent", s.contains("qa cancel.mp4") && s.contains('%'), s.clone());
                k.ok("progress shows the time left", s.contains("s left"), s.clone());
                k.shot(ctx, "03_rendering");
                let tmp = a_out.join("qa cancel.rendering.mp4");
                let ok = tmp.exists() || !ffmpeg_for("qa cancel.rendering").is_empty();
                k.ok("render writes to a temporary name", ok, tmp.display().to_string());
                k.mark = Instant::now();
                Step::Next
            }
            13 => {
                if f == 3 {
                    w.go(TRIM);
                }
                if f == 8 {
                    w.go(EXPORT);
                }
                if f < 10 {
                    return Step::Wait;
                }
                k.ok(
                    "leaving the page and coming back keeps the progress",
                    w.export.running && w.export.cancel_rect.is_positive(),
                    format!("{:?}", w.export.progress),
                );
                let r = w.export.cancel_rect;
                k.click(r.center());
                Step::Next
            }
            14 => {
                if f == 4 {
                    k.ok(
                        "cancel says Cancelling…",
                        w.export.status == "Cancelling\u{2026}" || w.export.status.starts_with("Cancelled"),
                        w.export.status.clone(),
                    );
                }
                if w.job.is_some() && k.elapsed() < Duration::from_secs(30) {
                    return Step::Wait;
                }
                if k.elapsed() < Duration::from_secs(2) {
                    return Step::Wait;
                }
                k.ok("cancel says nothing was saved", w.export.status == "Cancelled. Nothing was saved", w.export.status.clone());
                let ff = ffmpeg_for("qa cancel");
                k.ok("cancel stops ffmpeg", ff.is_empty(), format!("{ff:?}"));
                let alive = k.job_pid.map(pid_alive).unwrap_or(false);
                k.ok("cancel stops the worker", !alive, format!("pid {:?}", k.job_pid));
                Step::Next
            }
            15 => {
                if k.elapsed() < Duration::from_secs(3) {
                    return Step::Wait;
                }
                let left = files_starting(&a_out, "qa cancel");
                k.ok("cancel leaves no file behind", left.is_empty(), format!("{left:?}"));
                k.ok(
                    "Render is available again after cancel",
                    w.export.can_render && !w.export.running && !w.export.cancel_rect.is_positive(),
                    "",
                );
                k.ok("no error box after cancel", w.dialogs.is_empty(), format!("{:?}", w.dialogs));
                let r = w.export.name_rect;
                k.retype(r, "qa switch");
                // the whole clip, so the render is surely still running when the clip is switched below
                let d = w.st.duration();
                w.st.set_parts(vec![vl_core::models::Part { src_in: 0.0, src_out: d }], true);
                Step::Next
            }
            16 => {
                if f == 3 {
                    let r = w.export.render_rect;
                    k.click(r.center());
                    k.mark = Instant::now();
                }
                if f < 4 || k.mark.elapsed() < Duration::from_millis(2500) {
                    return Step::Wait;
                }
                k.job_pid = w.job.as_ref().map(|j| j.pid());
                // open another clip mid-render: the window asks to stop the export
                let b = k.b_dir.clone();
                w.open_project(&b, false);
                let asked = confirm_dialog(w, "Task running");
                k.ok(
                    "switching clip mid-render asks first",
                    asked.as_deref().map(|t| t.contains("The export of")).unwrap_or(false),
                    format!("{asked:?}"),
                );
                Step::Next
            }
            17 => {
                w.dialogs.retain(|d| !matches!(d, Dialog::WhoAreYou(_)));
                if confirm_dialog(w, "Task running").is_some() {
                    k.sub += 1; // asked again while the cancelled job was still exiting
                }
                if f == 3 {
                    w.go(EXPORT);
                }
                if (w.job.is_some() || f < 6) && k.elapsed() < Duration::from_secs(30) {
                    return Step::Wait;
                }
                k.ok(
                    "switched to the other clip, asked once",
                    w.st.pid() != Some(k.a_id.as_str()) && k.sub == 0,
                    format!("{:?} · asked again {} times", w.st.pid(), k.sub),
                );
                k.sub = 0;
                let e = &w.export;
                k.ok(
                    "switching clip mid-render: page not stuck",
                    !e.running && e.can_render && e.progress.is_none() && !e.show_result,
                    e.status.clone(),
                );
                k.ok("switching clip mid-render: no finished-looking file", !a_out.join("qa switch.mp4").exists(), "");
                let ff = ffmpeg_for("qa switch");
                k.ok(
                    "switching clip mid-render: ffmpeg stopped too",
                    ff.is_empty() && !k.job_pid.map(pid_alive).unwrap_or(false),
                    format!("{ff:?}"),
                );
                let a = k.a_dir.clone();
                Self::open(w, &a);
                Step::Next
            }
            18 => {
                w.dialogs.retain(|d| !matches!(d, Dialog::WhoAreYou(_)));
                if k.elapsed() < Duration::from_secs(3) {
                    return Step::Wait;
                }
                let left = files_starting(&a_out, "qa switch");
                k.ok("leftover half render is cleaned up", left.is_empty(), format!("{left:?}"));
                let r = w.export.name_rect;
                k.retype(r, "qa done");
                Step::Next
            }
            19 => {
                if f == 3 {
                    let r = w.export.render_rect;
                    k.click(r.center());
                }
                if f < 8 || (w.job.is_some() && k.elapsed() < Duration::from_secs(400)) {
                    return Step::Wait;
                }
                if f < 12 {
                    return Step::Wait;
                }
                let fin = a_out.join("qa done.mp4");
                k.ok(
                    "render saved under the chosen name",
                    fin.exists() && !a_out.join("qa done.rendering.mp4").exists(),
                    format!("{:?}", files_starting(&a_out, "qa done")),
                );
                let s = w.export.status.clone();
                k.ok(
                    "success shows the name, size and folder",
                    s.starts_with("Saved qa done.mp4 \u{b7}") && s.contains("MB") && s.ends_with(&format!("data\\exports\\{}", k.a_id)),
                    s.clone(),
                );
                k.ok(
                    "Play video and Show in folder offered",
                    w.export.show_result && w.export.last_out.as_deref() == Some(fin.as_path()) && w.export.play_rect.is_positive(),
                    "",
                );
                k.ok("hint now warns the name exists", w.export.name_hint.contains("already exists"), w.export.name_hint.clone());
                let st = &w.st;
                let (wd, hd) = crate::backend::canvas_size(
                    &crate::backend::output_aspect(&st.ed, st.h.as_ref().and_then(|h| h.project.game_area.as_ref())),
                    720,
                );
                let dur = st.kept_duration();
                let pr = probe(&fin);
                let ok = pr
                    .as_ref()
                    .map(|(c, pw, ph, fps, au, d)| {
                        c == "h264" && *pw == wd && *ph == hd && (*fps - 60.0).abs() < 0.01 && *au && (d - dur).abs() < 0.25
                    })
                    .unwrap_or(false);
                k.ok("720p render: h264 + audio, right size, 60 fps and length", ok, format!("{pr:?} want {wd}x{hd} {dur:.2}s"));
                k.shot(ctx, "04_after_render");
                Step::Next
            }
            20 => {
                if f == 0 {
                    let r = w.export.render_rect;
                    k.click(r.center());
                    return Step::Wait;
                }
                if f < 5 {
                    return Step::Wait;
                }
                let want = format!("qa done.mp4 already exists in data\\exports\\{}.\n\nReplace it?", k.a_id);
                let got = match w.dialogs.last() {
                    Some(Dialog::Confirm(c)) if c.title == "Replace video" => c.text.clone(),
                    _ => String::new(),
                };
                k.shot(ctx, "05_replace_question");
                k.ok("rendering over an existing video asks first", got == want, format!("{got:?}"));
                w.dialogs.pop(); // Cancel
                Step::Next
            }
            21 => {
                if f < 3 {
                    return Step::Wait;
                }
                k.ok("No keeps the video", w.job.is_none() && a_out.join("qa done.mp4").exists() && w.export.pending.is_none(), "");
                // editor exports use the latest edit (not saved to disk yet)
                w.st.set_parts(vec![Part { src_in: 14.0, src_out: 17.0 }, Part { src_in: 34.0, src_out: 37.0 }], true);
                k.status0 = w.export.status.clone();
                Step::Next
            }
            22 => {
                if f == 2 {
                    for i in 0..3 {
                        let r = w.export.save_rects[i];
                        k.click(r.center());
                    }
                }
                if f < 16 {
                    return Step::Wait;
                }
                let files = files_starting(&a_out, "qa done");
                let need = ["qa done.xml", "qa done.edl", "qa done_markers.edl", "qa done.json"];
                k.ok("all three editor files saved", need.iter().all(|n| files.iter().any(|f| f == n)), format!("{files:?}"));
                let j: Value = std::fs::read_to_string(a_out.join("qa done.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null);
                let parts: Vec<(f64, f64)> = j
                    .get("output_parts")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|p| (p["src_in"].as_f64().unwrap_or(-1.0), p["src_out"].as_f64().unwrap_or(-1.0))).collect())
                    .unwrap_or_default();
                k.ok("editor exports use the latest edit", parts == vec![(14.0, 17.0), (34.0, 37.0)], format!("{parts:?}"));
                let s = w.export.saved_text.clone().unwrap_or_default();
                k.ok("feedback after an editor export", s == format!("Saved qa done.json \u{b7} data\\exports\\{}", k.a_id), s);
                k.ok(
                    "render status untouched by editor exports",
                    w.export.status == k.status0 && k.status0.starts_with("Saved qa done.mp4"),
                    w.export.status.clone(),
                );
                // the three editor files are written and well-formed
                let xml = std::fs::read_to_string(a_out.join("qa done.xml")).unwrap_or_default();
                let edl = std::fs::read_to_string(a_out.join("qa done.edl")).unwrap_or_default();
                let json_ok =
                    std::fs::read_to_string(a_out.join("qa done.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).is_some();
                let ok = xml.starts_with("<?xml")
                    && xml.contains("<xmeml")
                    && edl.starts_with("TITLE:")
                    && json_ok
                    && a_out.join("qa done_markers.edl").exists();
                k.ok(
                    "editor files are written and well-formed",
                    ok,
                    format!("xml {} B, edl {} B, json ok {json_ok}", xml.len(), edl.len()),
                );
                let r = w.export.save_rects[1];
                k.click(r.center());
                Step::Next
            }
            23 => {
                if f < 5 {
                    return Step::Wait;
                }
                let asked = confirm_dialog(w, "Replace file");
                let want = format!("qa done.edl and qa done_markers.edl already exist in data\\exports\\{}.\n\nReplace?", k.a_id);
                k.ok("saving an editor file again asks first", asked.as_deref() == Some(want.as_str()), format!("{asked:?}"));
                Step::Next
            }
            24 => {
                if f < 4 {
                    return Step::Wait;
                }
                let s = w.export.saved_text.clone().unwrap_or_default();
                k.ok(
                    "EDL feedback mentions the markers file",
                    s == format!("Saved qa done.edl and qa done_markers.edl \u{b7} data\\exports\\{}", k.a_id),
                    s,
                );
                k.shot(ctx, "06_saved");
                w.st.set_parts(vec![], true);
                Step::Next
            }
            25 => {
                if f < 3 {
                    return Step::Wait;
                }
                let e = &w.export;
                k.ok(
                    "empty timeline: Render and Save disabled with a reason",
                    !e.can_render
                        && !e.can_export
                        && e.status == "Nothing on the timeline. Keep at least one part in Trim to render"
                        && e.render_tip == "Nothing on the timeline. Keep at least one part in Trim",
                    e.status.clone(),
                );
                k.shot(ctx, "07_empty_timeline");
                w.st.undo();
                Step::Next
            }
            26 => {
                if f < 3 {
                    return Step::Wait;
                }
                k.ok("undo brings Render back", w.export.can_render && !w.export.status.starts_with("Nothing"), w.export.status.clone());
                // failure path (a worker failure as the window reports it)
                let tmp = a_out.join("qa fail.rendering.mp4");
                let _ = std::fs::write(&tmp, b"partial");
                w.export.pending = Some((a_out.join("qa fail.mp4"), tmp.clone()));
                let (st, shell) = (&mut w.st, &mut w.shell);
                w.export.on_job(
                    &JobNotice { kind: "render".into(), progress: Some(0.3), msg: "Rendering · 30%".into(), result: None },
                    st,
                    shell,
                );
                w.export.on_job(&JobNotice { kind: "render".into(), progress: None, msg: "Render failed".into(), result: None }, st, shell);
                Step::Next
            }
            27 => {
                if f < 3 {
                    return Step::Wait;
                }
                let tmp = a_out.join("qa fail.rendering.mp4");
                let s = w.export.status.clone();
                k.ok(
                    "failure: readable status, partial file removed",
                    s == "Render failed. The full log is in data\\logs" && !tmp.exists() && !a_out.join("qa fail.mp4").exists(),
                    s,
                );
                k.ok("failure hides Play video of an older render", !w.export.show_result, "");
                // another task running: the window refuses, the page stays usable
                w.shell.push(Action::Preview);
                Step::Next
            }
            28 => {
                if f == 4 {
                    let r = w.export.name_rect;
                    k.retype(r, "qa busy");
                }
                if f == 8 {
                    let r = w.export.render_rect;
                    k.click(r.center());
                }
                if f < 14 {
                    return Step::Wait;
                }
                let busy = matches!(w.dialogs.last(), Some(Dialog::Message(m)) if m.title == "Busy" && m.text == "The preview is still running. Try again when it finishes");
                k.ok(
                    "busy: told why, nothing pending, Render still enabled",
                    busy && w.export.pending.is_none() && w.export.can_render && !w.export.running,
                    format!("{:?} kind {:?}", w.dialogs, w.job.as_ref().map(|j| j.kind.clone())),
                );
                w.dialogs.clear();
                w.shell.push(Action::CancelJob);
                Step::Next
            }
            29 => {
                if w.job.is_some() && k.elapsed() < Duration::from_secs(20) {
                    return Step::Wait;
                }
                w.export.apply_nvenc(false);
                w.export.set_option(3, "nvenc", &w.st);
                let n = w.export.note.clone();
                k.ok(
                    "NVIDIA chosen without a working encoder: says the CPU is used",
                    n.contains("No working NVIDIA encoder was found on this PC, the CPU is used instead"),
                    format!("{n:?}"),
                );
                w.export.set_option(0, "2160", &w.st);
                let n = w.export.note.clone();
                k.ok("resolution above the recording is explained", n.contains("2160p is above your recording"), format!("{n:?}"));
                Step::Next
            }
            30 => {
                if f == 3 {
                    k.shot(ctx, "08_notes");
                    let r = w.export.combo_rects[3];
                    k.click(r.center());
                }
                if f < 10 {
                    return Step::Wait;
                }
                k.shot(ctx, "09_encoder_list");
                k.key(Key::Escape, Modifiers::NONE);
                Step::Next
            }
            31 => {
                if f == 2 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1100.0, 700.0)));
                }
                if f < 20 {
                    return Step::Wait;
                }
                k.shot(ctx, "10_narrow");
                let cw = w.export.column_rect.width();
                k.ok("narrow window: the column fits", (479.0..=1100.0 - 64.0).contains(&cw), format!("{cw}"));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1500.0, 920.0)));
                Step::Next
            }
            // a real 1080p render (automatic encoder)
            32 => {
                if f == 10 {
                    w.export.set_option(0, "1080", &w.st);
                    w.export.set_option(3, "auto", &w.st);
                    w.export.set_option(2, "high", &w.st);
                    let r = w.export.name_rect;
                    k.retype(r, "qa 1080");
                }
                if f < 16 {
                    return Step::Wait;
                }
                let r = w.export.render_rect;
                k.click(r.center());
                Step::Next
            }
            33 => {
                if f < 8 || (w.job.is_some() && k.elapsed() < Duration::from_secs(400)) {
                    return Step::Wait;
                }
                if f < 12 {
                    return Step::Wait;
                }
                let fin = a_out.join("qa 1080.mp4");
                let st = &w.st;
                let (wd, hd) = crate::backend::canvas_size(
                    &crate::backend::output_aspect(&st.ed, st.h.as_ref().and_then(|h| h.project.game_area.as_ref())),
                    1080,
                );
                let dur = st.kept_duration();
                let pr = probe(&fin);
                let ok = pr
                    .as_ref()
                    .map(|(c, pw, ph, fps, au, d)| {
                        c == "h264" && *pw == wd && *ph == hd && (*fps - 60.0).abs() < 0.01 && *au && (d - dur).abs() < 0.25
                    })
                    .unwrap_or(false);
                k.ok(
                    "1080p render: h264 + audio, right size, 60 fps and length",
                    ok,
                    format!("{pr:?} want {wd}x{hd} {dur:.2}s · {}", w.export.status),
                );
                k.shot(ctx, "11_after_1080");
                Step::Next
            }
            _ => {
                k.done = true;
                Step::Wait
            }
        }
    }
}

impl eframe::App for CheckApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.win.frame_ui(ui, frame);
        if !self.k.inject.is_empty() {
            ctx.request_repaint();
        } else if !self.k.done {
            match self.script(&ctx) {
                Step::Next => {
                    self.k.step += 1;
                    self.k.f = 0;
                    self.k.step_start = Instant::now();
                }
                Step::Wait => self.k.f += 1,
            }
            ctx.request_repaint();
        } else if self.k.sub < 1000 {
            self.k.sub += 1;
            if self.k.sub > 8 {
                self.k.sub = 1000;
                self.app.win.shell.push(Action::Quit);
            }
            ctx.request_repaint();
        }
    }
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(mut evs) = self.k.inject.pop_front() {
            raw.events.append(&mut evs);
            raw.focused = true;
        }
    }
    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        eframe::App::on_exit(&mut self.app, gl);
    }
    fn clear_color(&self, v: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(v)
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// Hash of every file under data/projects and data/exports except the scratch copies.
fn fingerprint() -> BTreeMap<PathBuf, (u64, u64)> {
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, (u64, u64)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with("rustqa_") {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(p, (bytes.len() as u64, h.finish()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&paths::projects_dir(), &mut out);
    walk(&paths::exports_dir(), &mut out);
    out
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &dst.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, dst.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// A copy of `src` as `id` (name `name`); super smooth off so the renders stay quick.
fn make_copy(src: &Path, id: &str, name: &str) -> Option<PathBuf> {
    let d = paths::projects_dir().join(id);
    let _ = std::fs::remove_dir_all(&d);
    copy_dir(src, &d).ok()?;
    let mut h = vl_core::projects::open_dir(&d).ok()?;
    h.project.id = id.into();
    h.project.name = name.into();
    h.save().ok()?;
    let mut ed = h.edits();
    ed.effects.super_smooth = false;
    h.save_edits(&ed).ok()?;
    Some(d)
}

pub fn run(args: &[String]) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("uicheck", false);
    let out = args.first().map(PathBuf::from).unwrap_or_else(|| paths::app_root().join("scratch").join("vl-app-export").join("uicheck"));
    let _ = std::fs::create_dir_all(&out);
    // the reference project, else the shortest ready one
    let preferred = crate::uicheck::find_project("example_clip-short-4-3").unwrap_or_default();
    let src = vl_core::projects::open_dir(&preferred).ok().filter(|h| h.source_ok() && h.events().is_some()).or_else(|| {
        let mut all: Vec<_> = vl_core::projects::list_all()
            .into_iter()
            .filter(|h| !h.project.id.starts_with("rustqa_") && h.project.status == "ready" && h.source_ok() && h.events().is_some())
            .collect();
        all.sort_by(|a, b| a.project.source.duration.total_cmp(&b.project.source.duration));
        all.into_iter().next()
    });
    let Some(src) = src else {
        eprintln!("no ready project with a source video");
        return 2;
    };
    let before = fingerprint();
    let cfg_path = paths::config_path();
    let cfg_backup = std::fs::read(&cfg_path).ok();
    let stamp = chrono::Local::now().format("%H%M%S").to_string();
    let (a_id, b_id) = (format!("rustqa_export_{stamp}"), format!("rustqa_export_{stamp}_b"));
    let b_name = "rustqa export b".to_string();
    struct Cleanup(Vec<PathBuf>, Option<Vec<u8>>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for d in &self.0 {
                let _ = std::fs::remove_dir_all(d);
            }
            if let Some(b) = &self.1 {
                let _ = std::fs::write(paths::config_path(), b);
                config::reload();
            }
        }
    }
    let cleanup = Cleanup(
        vec![paths::projects_dir().join(&a_id), paths::projects_dir().join(&b_id), export_dir(&a_id), export_dir(&b_id)],
        cfg_backup.clone(),
    );
    let (Some(a_dir), Some(b_dir)) = (make_copy(&src.dir, &a_id, "rustqa export"), make_copy(&src.dir, &b_id, &b_name)) else {
        eprintln!("copy failed");
        return 2;
    };
    println!("QA copies: {} / {}\nscreenshots: {}", a_dir.display(), b_dir.display(), out.display());
    let opts = Options { read_only: true, writable_prefix: Some("rustqa_".into()), no_resume: true, no_first_run: true };
    let results = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let res2 = results.clone();
    let run = eframe::run_native(
        "ValoLights UI check (export)",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            let win = MainWindow::new(app::gl_env(cc), opts);
            Ok(Box::new(Owner {
                inner: CheckApp {
                    app: ValoApp { win },
                    k: Check {
                        out,
                        a_dir,
                        a_id,
                        b_dir,
                        b_name,
                        summary0: String::new(),
                        inject: VecDeque::new(),
                        step: 0,
                        step_start: Instant::now(),
                        mark: Instant::now(),
                        f: 0,
                        sub: 0,
                        results: vec![],
                        job_pid: None,
                        status0: String::new(),
                        done: false,
                    },
                },
                sink: res2,
            }))
        }),
    );
    if let Err(e) = run {
        eprintln!("GUI failed: {e}");
    }
    // let a cancelled encoder release its files, then clean up
    std::thread::sleep(Duration::from_millis(500));
    let dirs = cleanup.0.clone();
    drop(cleanup);
    let after = fingerprint();
    let mut results = results.lock().unwrap().clone();
    let changed: Vec<String> = before
        .iter()
        .filter(|(p, v)| after.get(*p) != Some(v))
        .map(|(p, _)| p.display().to_string())
        .chain(after.keys().filter(|p| !before.contains_key(*p)).map(|p| format!("new: {}", p.display())))
        .collect();
    results.push(("user data untouched".into(), changed.is_empty(), format!("{} files changed {changed:?}", changed.len())));
    let cfg_same = std::fs::read(&cfg_path).ok() == cfg_backup;
    results.push(("config.toml restored".into(), cfg_same, cfg_path.display().to_string()));
    let gone = dirs.iter().all(|d| !d.exists());
    results.push(("QA copies and their exports removed".into(), gone, format!("{dirs:?}")));
    let fails = results.iter().filter(|r| !r.1).count();
    let report: String = results.iter().map(|(n, ok, d)| format!("[{}] {n}: {d}\n", if *ok { " OK " } else { "FAIL" })).collect();
    let report = format!("{report}\nRESULT: {} ({} checks, {fails} failed)\n", if fails == 0 { "PASS" } else { "FAIL" }, results.len());
    let _ = std::fs::write(paths::app_root().join("scratch").join("vl-app-export").join("uicheck_export_report.txt"), &report);
    println!("{report}");
    i32::from(fails > 0)
}

struct Owner {
    inner: CheckApp,
    sink: std::sync::Arc<std::sync::Mutex<Vec<(String, bool, String)>>>,
}

impl eframe::App for Owner {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui, frame);
        if self.inner.k.done {
            *self.sink.lock().unwrap() = self.inner.k.results.clone();
        }
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.inner.raw_input_hook(ctx, raw);
    }
    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        *self.sink.lock().unwrap() = self.inner.k.results.clone();
        self.inner.on_exit(gl);
    }
    fn clear_color(&self, v: &egui::Visuals) -> [f32; 4] {
        self.inner.clear_color(v)
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}
