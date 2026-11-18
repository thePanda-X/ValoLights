//! Speech-to-text bridge. Transcription runs in `vl-speech.exe` (whisper.cpp, crate vl-speech), a separate
//! process next to the app's exe, so the CUDA libraries never load into the app and a GPU failure can't take
//! the worker down: the `transcribe` worker job starts it with the same job spec and relays its JSON lines.
use std::path::{Path, PathBuf};
use std::process::Command;

/// `vl-speech.exe` next to `valolights.exe` (the shipped folder and `target/<profile>` alike).
pub fn helper_path() -> PathBuf {
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    exe.join(if cfg!(windows) { "vl-speech.exe" } else { "vl-speech" })
}

/// Command for one transcription job (`cpu`: don't use the GPU, after a GPU run failed).
pub fn transcribe_command(spec: &Path, cpu: bool) -> Command {
    let mut c = crate::sys::quiet_command(helper_path());
    c.arg("--worker").arg(spec);
    if cpu {
        c.arg("--cpu");
    }
    let root = vl_core::paths::app_root();
    c.current_dir(root).env("VALOLIGHTS_ROOT", root);
    c
}

/// What transcription runs on (used by the self-test).
pub fn probe() -> anyhow::Result<String> {
    let p = helper_path();
    if !p.exists() {
        anyhow::bail!("{} missing", p.display());
    }
    let out = crate::sys::quiet_command(&p).arg("--probe").output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("speech helper failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
