//! `valolights --perf`: measure the video paths of the real GUI (read-only, the user's latest ready clip).
//!
//! Per scenario: UI frame cost (time inside the app's frame function: mean / p95 / max), video frames shown
//! per second, process CPU, and what mpv reports (decoder, dropped frames). Writes
//! scratch/perf/perf_report.txt and prints it.
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::pages::{EXPORT, SUBTITLES, TRIM};
use std::time::{Duration, Instant};
use vl_core::paths;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Open,
    Settle,
    Run,
}

struct Scenario {
    name: &'static str,
    page: usize,
    scrub: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "Trim: play", page: TRIM, scrub: false },
    Scenario { name: "Trim: scrub (10 seeks/s)", page: TRIM, scrub: true },
    Scenario { name: "Subtitles: play (composed preview)", page: SUBTITLES, scrub: false },
    Scenario { name: "Subtitles: scrub (10 seeks/s)", page: SUBTITLES, scrub: true },
    Scenario { name: "Export: 1080p render from the app", page: EXPORT, scrub: false },
];

const RUN: Duration = Duration::from_secs(6);

fn cpu_ms() -> f64 {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut a, mut b, mut k, mut u) = (z, z, z, z);
        if GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u) != 0 {
            let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 10_000.0;
            return f(k) + f(u);
        }
    }
    0.0
}

struct Perf {
    app: ValoApp,
    project: std::path::PathBuf,
    sc: usize,
    phase: Phase,
    t0: Instant,
    cpu0: f64,
    frames0: u64,
    costs: Vec<f64>,
    last_seek: Instant,
    seek_i: u32,
    report: Vec<String>,
    done: bool,
}

impl Perf {
    fn player(&mut self, page: usize) -> &mut crate::widgets::player::Player {
        if page == TRIM {
            &mut self.app.win.trim.player
        } else {
            &mut self.app.win.subs.player
        }
    }

    fn step(&mut self) {
        let Some(s) = SCENARIOS.get(self.sc) else {
            self.done = true;
            return;
        };
        let el = self.t0.elapsed();
        match self.phase {
            Phase::Open => {
                if self.sc == 0 {
                    let dir = self.project.clone();
                    self.app.win.open_project(&dir, true);
                }
                self.app.win.go(s.page);
                self.phase = Phase::Settle;
                self.t0 = Instant::now();
            }
            Phase::Settle => {
                if el > Duration::from_millis(2500) {
                    let page = s.page;
                    let scrub = s.scrub;
                    if page == EXPORT {
                        let out = paths::app_root().join("scratch").join("perf").join("app_export.mp4");
                        let _ = std::fs::remove_file(&out);
                        self.app.win.render(out.to_string_lossy().into_owned(), serde_json::json!({}));
                    } else {
                        let p = self.player(page);
                        p.seek(0.5);
                        if !scrub {
                            p.play();
                        }
                    }
                    self.frames0 = if page == EXPORT { 0 } else { self.player(page).frames_rendered };
                    self.cpu0 = cpu_ms();
                    self.costs.clear();
                    self.seek_i = 0;
                    self.last_seek = Instant::now();
                    self.phase = Phase::Run;
                    self.t0 = Instant::now();
                }
            }
            Phase::Run => {
                if s.scrub && self.last_seek.elapsed() >= Duration::from_millis(100) {
                    self.last_seek = Instant::now();
                    self.seek_i += 1;
                    let seek_i = self.seek_i;
                    let page = s.page;
                    let p = self.player(page);
                    let d = p.duration().max(1.0);
                    let t = (seek_i as f64 * 0.37) % (d - 0.2);
                    p.seek(t);
                }
                if s.page == EXPORT && el > Duration::from_millis(300) && self.app.win.job.is_none() {
                    let secs = el.as_secs_f64();
                    let cpu = (cpu_ms() - self.cpu0) / (secs * 1000.0) * 100.0;
                    let ok = paths::app_root().join("scratch").join("perf").join("app_export.mp4").exists();
                    let line = format!(
                        "{:38} {:6.2} s from click to finished file ({}) | GUI CPU {:5.1}% | ui frames {}",
                        s.name,
                        secs,
                        if ok { "written" } else { "MISSING" },
                        cpu,
                        self.costs.len()
                    );
                    println!("{line}");
                    self.report.push(line);
                    self.sc += 1;
                    self.phase = Phase::Open;
                    return;
                }
                if el >= RUN && s.page != EXPORT {
                    let page = s.page;
                    let secs = el.as_secs_f64();
                    let cpu = (cpu_ms() - self.cpu0) / (secs * 1000.0) * 100.0;
                    let frames0 = self.frames0;
                    let p = self.player(page);
                    let shown = (p.frames_rendered - frames0) as f64 / secs;
                    let hw = p.prop("hwdec-current").unwrap_or_else(|| "?".into());
                    let drops = p.prop("frame-drop-count").unwrap_or_default();
                    let ddrops = p.prop("decoder-frame-drop-count").unwrap_or_default();
                    let vfps = p.prop("container-fps").unwrap_or_default();
                    p.pause();
                    let mut c = self.costs.clone();
                    c.sort_by(|a, b| a.total_cmp(b));
                    let n = c.len().max(1);
                    let mean = c.iter().sum::<f64>() / n as f64;
                    let p95 = c.get(n * 95 / 100).copied().unwrap_or(0.0);
                    let max = c.last().copied().unwrap_or(0.0);
                    let line = format!(
                        "{:38} ui frames {:4} ({:5.1}/s)  frame cost mean {:5.2} ms p95 {:6.2} ms max {:7.2} ms | video shown {:5.1} fps (source {vfps}) | CPU {:5.1}% | hwdec {hw} | drops {drops}/{ddrops}",
                        s.name,
                        c.len(),
                        c.len() as f64 / secs,
                        mean,
                        p95,
                        max,
                        shown,
                        cpu
                    );
                    println!("{line}");
                    self.report.push(line);
                    self.sc += 1;
                    self.phase = Phase::Open;
                }
            }
        }
    }
}

impl eframe::App for Perf {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let t = Instant::now();
        self.app.win.frame_ui(ui, frame);
        if self.phase == Phase::Run {
            self.costs.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        if !self.done {
            self.step();
            // the harness only needs a heartbeat; repaints otherwise come from the app itself (as in real use)
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        } else {
            let out = paths::app_root().join("scratch").join("perf");
            let _ = std::fs::create_dir_all(&out);
            let _ = std::fs::write(out.join("perf_report.txt"), self.report.join("\n") + "\n");
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        self.app.win.shutdown();
    }
}

pub fn run(_args: &[String]) -> i32 {
    paths::ensure_data_dirs();
    vl_core::logging::setup("perf", false);
    let Some(h) = vl_core::projects::list_all()
        .into_iter()
        .filter(|h| h.project.status == "ready" && h.source_ok() && h.edits_path().exists())
        .max_by_key(|h| h.project.id.contains("short"))
    else {
        eprintln!("no ready project");
        return 2;
    };
    println!("project: {}", h.project.id);
    let opts = Options { read_only: true, writable_prefix: None, no_resume: true, no_first_run: true };
    let dir = h.dir.clone();
    let r = eframe::run_native(
        "ValoLights perf",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            let win = MainWindow::new(app::gl_env(cc), opts);
            Ok(Box::new(Perf {
                app: ValoApp { win },
                project: dir,
                sc: 0,
                phase: Phase::Open,
                t0: Instant::now(),
                cpu0: 0.0,
                frames0: 0,
                costs: vec![],
                last_seek: Instant::now(),
                seek_i: 0,
                report: vec![],
                done: false,
            }))
        }),
    );
    if let Err(e) = r {
        eprintln!("GUI failed: {e}");
        return 1;
    }
    0
}
