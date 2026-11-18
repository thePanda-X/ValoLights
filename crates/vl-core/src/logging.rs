//! Minimal file logger: data/logs/<name>.log (rotated at 2 MB), optionally mirrored to stderr.
use crate::paths;
use std::io::Write;
use std::sync::Mutex;

struct FileLogger {
    file: Mutex<Option<std::fs::File>>,
    console: bool,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info || m.target().starts_with("vl")
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) || r.target().starts_with("ort") && r.level() > log::Level::Warn {
            return;
        }
        let line = format!("{} {} {}: {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), r.level(), r.target(), r.args());
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
        if self.console {
            eprint!("{line}");
        }
    }
    fn flush(&self) {}
}

pub fn setup(name: &str, console: bool) {
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(format!("{name}.log"));
    if std::fs::metadata(&p).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&p, dir.join(format!("{name}.log.1")));
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&p).ok();
    let logger = FileLogger { file: Mutex::new(file), console };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
