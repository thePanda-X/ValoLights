//! Small Windows helpers: console attachment for the windowed exe, child processes without console
//! windows, killing process trees, opening folders.
use std::process::Command;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The GUI build has no console. `cli` and `--selftest` print to the parent's console (or to whatever
/// pipes the parent passed, which are inherited and need nothing).
pub fn attach_parent_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
        use windows_sys::Win32::System::Console::{
            AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let err = GetStdHandle(STD_ERROR_HANDLE);
        let missing = |h: windows_sys::Win32::Foundation::HANDLE| h.is_null() || h == INVALID_HANDLE_VALUE;
        if !missing(out) && !missing(err) {
            return; // redirected to a pipe or file by the parent
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let con = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if missing(con) {
            return;
        }
        if missing(out) {
            SetStdHandle(STD_OUTPUT_HANDLE, con);
        }
        if missing(err) {
            SetStdHandle(STD_ERROR_HANDLE, con);
        }
    }
}

/// A command that never flashes a console window.
pub fn quiet_command(exe: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Kill a process and all its children (the worker starts ffmpeg / RIFE / whisper).
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = quiet_command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
}

/// Open a folder (or file) with the shell, like `os.startfile`.
pub fn open_path(p: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = quiet_command("explorer").arg(p).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("xdg-open").arg(p).spawn();
    }
}

/// Open a web page in the default browser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = quiet_command("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// An NVIDIA driver is installed (its CUDA driver DLL is in System32). Cheap, no GPU work.
pub fn nvidia_driver_present() -> bool {
    #[cfg(windows)]
    {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::path::Path::new(&sys).join("System32").join("nvcuda.dll").exists()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
