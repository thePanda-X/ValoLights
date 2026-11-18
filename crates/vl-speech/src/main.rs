//! vl-speech.exe: the speech-to-text helper of ValoLights (whisper.cpp).
//!
//!   vl-speech --worker job.json [--cpu]   transcribe a project (job kind "transcribe"), JSON lines on stdout
//!   vl-speech --probe                     one line: what transcription would run on (for the self-test)
//!
//! Started hidden by the app's worker, which relays the progress lines; a separate process so the CUDA
//! libraries never load into the app, and a GPU failure can be retried on the CPU.
use serde_json::{json, Value};
use std::path::Path;
use vl_core::jobs::{emit_done, emit_error, emit_progress};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    vl_core::paths::ensure_data_dirs();
    vl_core::logging::setup("speech", false);
    let code = match args.first().map(String::as_str) {
        Some("--probe") => {
            let gpu = cfg!(feature = "cuda") && vl_speech::nvidia_driver();
            println!("whisper.cpp, {}", if gpu { "CUDA (NVIDIA driver found)" } else { "CPU" });
            0
        }
        Some("--worker") if args.len() >= 2 => worker(Path::new(&args[1]), args.iter().any(|a| a == "--cpu")),
        _ => {
            eprintln!("usage: vl-speech --worker job.json [--cpu] | --probe");
            2
        }
    };
    std::process::exit(code);
}

fn worker(spec_path: &Path, cpu: bool) -> i32 {
    let run = || -> anyhow::Result<Value> {
        let spec: Value = serde_json::from_str(&std::fs::read_to_string(spec_path)?)?;
        let dir = spec["project_dir"].as_str().ok_or_else(|| anyhow::anyhow!("no project_dir in the job"))?;
        let h = vl_core::projects::open_dir(Path::new(dir))?;
        let whole = spec["whole"].as_bool().unwrap_or(false);
        let only = spec["only"].as_array().and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
        let prog: vl_speech::Progress = std::sync::Arc::new(emit_progress);
        let tr = vl_speech::transcribe_project(&h, &prog, whole, only, cpu)?;
        Ok(json!({"segments": tr.segments.len()}))
    };
    match run() {
        Ok(v) => {
            emit_done(v);
            0
        }
        Err(e) => {
            log::error!("transcription failed: {e:?}");
            emit_error(&e.to_string(), &format!("{e:?}"));
            1
        }
    }
}
