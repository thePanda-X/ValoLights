//! Background jobs run in a worker subprocess (same exe, `--worker job.json`).
//!
//! Protocol: the worker prints JSON lines on stdout:
//!   {"type": "progress", "p": 0.42, "msg": "Scanning"}
//!   {"type": "done", "result": {...}}
//!   {"type": "error", "msg": "...", "trace": "..."}
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Progress callback: (fraction 0..1, message).
pub type Progress<'a> = &'a (dyn Fn(f64, &str) + Sync);

/// Cancellation flag shared with long-running work.
#[derive(Clone, Default)]
pub struct Cancel(pub Arc<AtomicBool>);

impl Cancel {
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub fn set(&self) {
        self.0.store(true, Ordering::Relaxed)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Cancelled")]
pub struct Cancelled;

pub fn emit(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

pub fn emit_progress(p: f64, msg: &str) {
    emit(&json!({"type": "progress", "p": (p * 10000.0).round() / 10000.0, "msg": msg}));
}

pub fn emit_done(result: Value) {
    emit(&json!({"type": "done", "result": result}));
}

pub fn emit_error(msg: &str, trace: &str) {
    emit(&json!({"type": "error", "msg": msg, "trace": trace}));
}
