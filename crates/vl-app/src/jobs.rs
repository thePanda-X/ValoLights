//! Background jobs in a worker subprocess (`valolights --worker job.json`, the same exe).
//!
//! The worker prints JSON lines (`vl_core::jobs`); a reader thread parses them and wakes the UI. The UI
//! calls [`Job::poll`] every frame and gets [`JobEvent`]s. Cancelling kills the whole process tree.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Called from the reader thread whenever something arrives (the GUI passes `ctx.request_repaint`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    Progress {
        p: f64,
        msg: String,
    },
    Finished(Value),
    /// The error text, or exactly "Cancelled".
    Failed(String),
}

enum Line {
    Msg(Value),
    Exit(Option<i32>),
}

pub struct Job {
    pub kind: String,
    pub spec: Value,
    pid: u32,
    rx: Receiver<Line>,
    stderr: Arc<Mutex<String>>,
    finished: Arc<AtomicBool>,
    result: Option<Value>,
    error: Option<String>,
    pub cancelled: bool,
    done: bool,
    pub started_at: Instant,
    pub last_p: f64,
    pub last_msg: String,
}

/// The command line that starts a worker: this same exe.
pub fn worker_command() -> std::process::Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| "valolights".into());
    crate::sys::quiet_command(exe)
}

impl Job {
    /// Write the spec (`{"kind": kind, ...spec}`) to data/cache/tmp and start the worker.
    pub fn start(kind: &str, spec: Value, waker: Option<Waker>) -> anyhow::Result<Job> {
        let mut full = json!({ "kind": kind });
        if let (Some(m), Some(s)) = (full.as_object_mut(), spec.as_object()) {
            for (k, v) in s {
                m.insert(k.clone(), v.clone());
            }
        }
        let d = vl_core::paths::tmp_dir();
        std::fs::create_dir_all(&d)?;
        let spec_path = d.join(format!("job_{}.json", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::write(&spec_path, serde_json::to_string(&full)?)?;
        let mut child = worker_command()
            .arg("--worker")
            .arg(&spec_path)
            .current_dir(vl_core::paths::app_root())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pid = child.id();
        let (tx, rx) = channel();
        let stderr = Arc::new(Mutex::new(String::new()));
        let finished = Arc::new(AtomicBool::new(false));
        // stderr: keep the tail for error messages
        if let Some(mut err) = child.stderr.take() {
            let buf = stderr.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = err.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut b = buf.lock().unwrap();
                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    if b.len() > 20_000 {
                        let cut = b.len() - 10_000;
                        let cut = (cut..b.len()).find(|&i| b.is_char_boundary(i)).unwrap_or(0);
                        b.drain(..cut);
                    }
                }
            });
        }
        let out = child.stdout.take();
        let fin = finished.clone();
        std::thread::spawn(move || {
            if let Some(out) = out {
                for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                    let s = String::from_utf8_lossy(&line);
                    if let Ok(v) = serde_json::from_str::<Value>(s.trim()) {
                        let _ = tx.send(Line::Msg(v));
                        if let Some(w) = &waker {
                            w();
                        }
                    }
                }
            }
            let code = child.wait().ok().and_then(|s| s.code());
            std::thread::sleep(Duration::from_millis(30)); // let the stderr reader finish
            let _ = std::fs::remove_file(&spec_path);
            fin.store(true, Ordering::SeqCst);
            let _ = tx.send(Line::Exit(code));
            if let Some(w) = &waker {
                w();
            }
        });
        Ok(Job {
            kind: kind.into(),
            spec: full,
            pid,
            rx,
            stderr,
            finished,
            result: None,
            error: None,
            cancelled: false,
            done: false,
            started_at: Instant::now(),
            last_p: 0.0,
            last_msg: String::new(),
        })
    }

    /// Everything that happened since the last call. The last event of a job is Finished or Failed.
    pub fn poll(&mut self) -> Vec<JobEvent> {
        let mut out = vec![];
        while let Ok(l) = self.rx.try_recv() {
            match l {
                Line::Msg(v) => match v.get("type").and_then(Value::as_str) {
                    Some("progress") => {
                        self.last_p = v.get("p").and_then(Value::as_f64).unwrap_or(0.0);
                        self.last_msg = v.get("msg").and_then(Value::as_str).unwrap_or("").to_string();
                        if !self.cancelled {
                            out.push(JobEvent::Progress { p: self.last_p, msg: self.last_msg.clone() });
                        }
                    }
                    Some("done") => self.result = Some(v.get("result").cloned().unwrap_or(json!({}))),
                    Some("error") => self.error = Some(v.get("msg").and_then(Value::as_str).unwrap_or("error").to_string()),
                    _ => {}
                },
                Line::Exit(code) => {
                    self.done = true;
                    if self.cancelled {
                        out.push(JobEvent::Failed("Cancelled".into()));
                    } else if let Some(r) = self.result.take() {
                        out.push(JobEvent::Finished(r));
                    } else {
                        let err = self.error.clone().unwrap_or_else(|| {
                            let e = self.stderr.lock().unwrap();
                            let tail: String = e.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
                            let tail = tail.trim().to_string();
                            if tail.is_empty() {
                                format!("worker exited with code {}", code.map(|c| c.to_string()).unwrap_or("?".into()))
                            } else {
                                tail
                            }
                        });
                        out.push(JobEvent::Failed(err));
                    }
                }
            }
        }
        out
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        if self.running() {
            crate::sys::kill_tree(self.pid);
        }
    }

    /// The process is still alive (its final event may not have been polled yet).
    pub fn running(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }

    /// The job ended and its last event was handed out by `poll`.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Block until the process is gone (used before deleting a project folder).
    pub fn wait(&self, timeout: Duration) -> bool {
        let t = Instant::now();
        while self.running() && t.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(20));
        }
        !self.running()
    }

    /// Seconds left, from the progress rate so far (None early on).
    pub fn eta(&self) -> Option<f64> {
        if self.last_p <= 0.03 {
            return None;
        }
        let el = self.started_at.elapsed().as_secs_f64();
        Some((el / self.last_p - el).max(0.0))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}
