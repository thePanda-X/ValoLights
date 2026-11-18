//! `valolights --shot <project_dir> <page> <out.png>`: open a project read-only on a page (clips, trim, layout,
//! subtitles, export), let it settle, save the app's own rendered frame (never the desktop) and quit.
//! An optional fourth argument sets the window height (to see long side panels whole); a fifth,
//! `crop:<layer>`, opens the Layout crop editor on that layer (`crop:updates`: the update dialog instead).
use crate::app::{self, MainWindow, Options, ValoApp};
use crate::pages::{CLIPS, EXPORT, LAYOUT, SUBTITLES, TRIM};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Shot {
    app: ValoApp,
    project: PathBuf,
    page: usize,
    out: PathBuf,
    t0: Instant,
    step: u8,
    height: Option<f32>,
    crop: Option<String>,
}

impl eframe::App for Shot {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.win.frame_ui(ui, frame);
        let ctx = ui.ctx().clone();
        match self.step {
            0 => {
                let p = self.project.clone();
                self.app.win.open_project(&p, true);
                self.app.win.go(self.page);
                if let Some(h) = self.height {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1500.0, h)));
                }
                self.t0 = Instant::now();
                self.step = 1;
            }
            1 if self.t0.elapsed() > Duration::from_secs(4) => {
                // the project has settled (opening it resets the page): now the crop editor
                if let Some(k) = self.crop.take() {
                    if k == "updates" {
                        // the update dialog (VALOLIGHTS_UPDATE_URL can point at a test manifest)
                        self.app.win.shell.push(crate::pages::Action::CheckUpdates);
                    } else {
                        let win = &mut self.app.win;
                        win.layout.start_crop(&win.st, &k);
                    }
                    self.t0 = Instant::now() - Duration::from_secs(2);
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                if std::env::var_os("VL_SHOT_DEBUG").is_some() {
                    let names: Vec<String> = self.app.win.dialogs.iter().map(|d| d.title().to_string()).collect();
                    eprintln!(
                        "shot: dialogs {names:?}, update {:?}",
                        self.app.win.update.as_ref().map(|s| format!("{:?}", s.lock().unwrap()))
                    );
                }
                let out = self.out.clone();
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
                    if let Err(e) = image::save_buffer(&out, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
                        eprintln!("screenshot {}: {e}", out.display());
                    }
                });
                self.t0 = Instant::now();
                self.step = 2;
            }
            2 if self.t0.elapsed() > Duration::from_millis(800) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        self.app.win.shutdown();
    }
}

pub fn run(args: &[String]) -> i32 {
    let (Some(project), Some(page), Some(out)) = (args.first(), args.get(1), args.get(2)) else {
        eprintln!("usage: valolights --shot <project_dir> <clips|trim|layout|subtitles|export> <out.png> [window_height]");
        return 2;
    };
    let page = match page.as_str() {
        "clips" => CLIPS,
        "trim" => TRIM,
        "layout" => LAYOUT,
        "subtitles" => SUBTITLES,
        _ => EXPORT,
    };
    vl_core::paths::ensure_data_dirs();
    vl_core::logging::setup("shot", false);
    let opts = Options { read_only: true, writable_prefix: None, no_resume: true, no_first_run: true };
    let (project, out) = (PathBuf::from(project), PathBuf::from(out));
    let height = args.get(3).and_then(|h| h.parse::<f32>().ok());
    let crop = args.get(4).and_then(|c| c.strip_prefix("crop:")).map(str::to_string);
    let r = eframe::run_native(
        "ValoLights shot",
        app::native_options(),
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            Ok(Box::new(Shot {
                app: ValoApp { win: MainWindow::new(app::gl_env(cc), opts) },
                project,
                page,
                out,
                t0: Instant::now(),
                step: 0,
                height,
                crop,
            }))
        }),
    );
    i32::from(r.is_err())
}
