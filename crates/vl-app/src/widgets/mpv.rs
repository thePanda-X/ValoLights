//! Minimal libmpv binding: `bin/libmpv-2.dll` loaded at runtime with `libloading` (no import library),
//! client API + OpenGL render API.
#![allow(non_camel_case_types)]

use anyhow::{anyhow, bail, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

pub type mpv_handle = c_void;
pub type mpv_render_context = c_void;

#[repr(C)]
pub struct mpv_render_param {
    pub type_: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address: Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

#[repr(C)]
pub struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_DOUBLE: c_int = 5;
pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_COMMAND_REPLY: c_int = 5;
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

/// What `drain_events` reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ev {
    /// an observed property changed: (observe id, value; None = unavailable). Flags come as 0.0 / 1.0.
    Prop(u64, Option<f64>),
    /// an async command finished: (reply id, ok)
    Reply(u64, bool),
    /// a seek finished and playback (or the paused frame) is ready again
    Restart,
    Shutdown,
}

type WakeupCb = unsafe extern "C" fn(*mut c_void);

/// Function table of the loaded DLL.
pub struct Api {
    _lib: Library,
    pub client_api_version: unsafe extern "C" fn() -> u64,
    pub create: unsafe extern "C" fn() -> *mut mpv_handle,
    pub initialize: unsafe extern "C" fn(*mut mpv_handle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(*mut mpv_handle),
    pub set_option_string: unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub set_property_string: unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub get_property: unsafe extern "C" fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int,
    pub command: unsafe extern "C" fn(*mut mpv_handle, *mut *const c_char) -> c_int,
    pub error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    pub wait_event: unsafe extern "C" fn(*mut mpv_handle, f64) -> *mut mpv_event,
    pub set_wakeup_callback: unsafe extern "C" fn(*mut mpv_handle, Option<WakeupCb>, *mut c_void),
    pub render_context_create: unsafe extern "C" fn(*mut *mut mpv_render_context, *mut mpv_handle, *mut mpv_render_param) -> c_int,
    pub render_context_set_update_callback: unsafe extern "C" fn(*mut mpv_render_context, Option<WakeupCb>, *mut c_void),
    pub render_context_update: unsafe extern "C" fn(*mut mpv_render_context) -> u64,
    pub render_context_render: unsafe extern "C" fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int,
    pub render_context_report_swap: unsafe extern "C" fn(*mut mpv_render_context),
    pub render_context_free: unsafe extern "C" fn(*mut mpv_render_context),
    pub free: unsafe extern "C" fn(*mut c_void),
    pub observe_property: unsafe extern "C" fn(*mut mpv_handle, u64, *const c_char, c_int) -> c_int,
    pub command_async: unsafe extern "C" fn(*mut mpv_handle, u64, *mut *const c_char) -> c_int,
}

static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();

/// Load `bin/libmpv-2.dll` once.
pub fn api() -> Result<&'static Api> {
    API.get_or_init(|| load().map_err(|e| e.to_string())).as_ref().map_err(|e| anyhow!("{e}"))
}

fn load() -> Result<Api> {
    let dir = vl_core::paths::bin_dir();
    let path = dir.join(if cfg!(windows) { "libmpv-2.dll" } else { "libmpv.so.2" });
    if !path.exists() {
        bail!("{} not found", path.display());
    }
    // its own dependencies sit next to it
    #[cfg(windows)]
    let lib = unsafe {
        libloading::os::windows::Library::load_with_flags(
            &path,
            libloading::os::windows::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | libloading::os::windows::LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
        .map(Library::from)?
    };
    #[cfg(not(windows))]
    let lib = unsafe { Library::new(&path)? };
    unsafe {
        macro_rules! sym {
            ($name:literal) => {
                *lib.get(concat!($name, "\0").as_bytes())?
            };
        }
        Ok(Api {
            client_api_version: sym!("mpv_client_api_version"),
            create: sym!("mpv_create"),
            initialize: sym!("mpv_initialize"),
            terminate_destroy: sym!("mpv_terminate_destroy"),
            set_option_string: sym!("mpv_set_option_string"),
            set_property_string: sym!("mpv_set_property_string"),
            get_property: sym!("mpv_get_property"),
            command: sym!("mpv_command"),
            error_string: sym!("mpv_error_string"),
            wait_event: sym!("mpv_wait_event"),
            set_wakeup_callback: sym!("mpv_set_wakeup_callback"),
            render_context_create: sym!("mpv_render_context_create"),
            render_context_set_update_callback: sym!("mpv_render_context_set_update_callback"),
            render_context_update: sym!("mpv_render_context_update"),
            render_context_render: sym!("mpv_render_context_render"),
            render_context_report_swap: sym!("mpv_render_context_report_swap"),
            render_context_free: sym!("mpv_render_context_free"),
            free: sym!("mpv_free"),
            observe_property: sym!("mpv_observe_property"),
            command_async: sym!("mpv_command_async"),
            _lib: lib,
        })
    }
}

/// "2.3" style version of the client API.
pub fn version() -> Result<String> {
    let a = api()?;
    let v = unsafe { (a.client_api_version)() };
    Ok(format!("{}.{}", v >> 16, v & 0xffff))
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// An mpv instance (client handle). Dropping it destroys the instance.
pub struct Mpv {
    pub api: &'static Api,
    pub h: *mut mpv_handle,
}

// libmpv handles are thread-safe
unsafe impl Send for Mpv {}

impl Mpv {
    /// Create with options set before `mpv_initialize`. Options libmpv doesn't know are skipped.
    pub fn new(options: &[(&str, &str)]) -> Result<Mpv> {
        let api = api()?;
        let h = unsafe { (api.create)() };
        if h.is_null() {
            bail!("mpv_create failed");
        }
        let m = Mpv { api, h };
        for (k, v) in options {
            if let Err(e) = m.set_option(k, v) {
                log::warn!("mpv option {k}={v}: {e}");
            }
        }
        let r = unsafe { (api.initialize)(h) };
        m.check(r)?;
        Ok(m)
    }

    fn check(&self, r: c_int) -> Result<()> {
        if r < 0 {
            let s = unsafe { CStr::from_ptr((self.api.error_string)(r)) }.to_string_lossy().into_owned();
            bail!("mpv: {s}");
        }
        Ok(())
    }

    pub fn set_option(&self, k: &str, v: &str) -> Result<()> {
        let (k, v) = (cstr(k), cstr(v));
        self.check(unsafe { (self.api.set_option_string)(self.h, k.as_ptr(), v.as_ptr()) })
    }

    pub fn set_property(&self, k: &str, v: &str) -> Result<()> {
        let (k, v) = (cstr(k), cstr(v));
        self.check(unsafe { (self.api.set_property_string)(self.h, k.as_ptr(), v.as_ptr()) })
    }

    pub fn get_string(&self, k: &str) -> Option<String> {
        let k = cstr(k);
        let mut v: *mut c_char = std::ptr::null_mut();
        let r = unsafe { (self.api.get_property)(self.h, k.as_ptr(), MPV_FORMAT_STRING, &mut v as *mut *mut c_char as *mut c_void) };
        if r < 0 || v.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(v) }.to_string_lossy().into_owned();
        unsafe { (self.api.free)(v as *mut c_void) };
        Some(s)
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        self.check(unsafe { (self.api.command)(self.h, ptrs.as_mut_ptr()) })
    }

    /// Watch a property: changes arrive as `Ev::Prop(id, value)` from `drain_events` (DOUBLE or FLAG).
    pub fn observe(&self, id: u64, name: &str, flag: bool) {
        let k = cstr(name);
        let f = if flag { MPV_FORMAT_FLAG } else { MPV_FORMAT_DOUBLE };
        unsafe { (self.api.observe_property)(self.h, id, k.as_ptr(), f) };
    }

    /// Run a command without waiting for it (the reply arrives as `Ev::Reply(id, ok)`).
    pub fn command_async(&self, id: u64, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        self.check(unsafe { (self.api.command_async)(self.h, id, ptrs.as_mut_ptr()) })
    }

    /// Drain pending events (never blocks).
    pub fn drain_events(&self) -> Vec<Ev> {
        let mut out = vec![];
        loop {
            let e = unsafe { (self.api.wait_event)(self.h, 0.0) };
            if e.is_null() {
                break;
            }
            let (id, err, ud, data) = unsafe { ((*e).event_id, (*e).error, (*e).reply_userdata, (*e).data) };
            match id {
                MPV_EVENT_NONE => break,
                MPV_EVENT_SHUTDOWN => out.push(Ev::Shutdown),
                MPV_EVENT_COMMAND_REPLY => out.push(Ev::Reply(ud, err >= 0)),
                MPV_EVENT_PLAYBACK_RESTART => out.push(Ev::Restart),
                MPV_EVENT_PROPERTY_CHANGE if !data.is_null() => {
                    let pr = unsafe { &*(data as *const mpv_event_property) };
                    let v = unsafe {
                        match pr.format {
                            MPV_FORMAT_DOUBLE if !pr.data.is_null() => Some(*(pr.data as *const f64)),
                            MPV_FORMAT_FLAG if !pr.data.is_null() => Some(f64::from(*(pr.data as *const c_int))),
                            _ => None,
                        }
                    };
                    out.push(Ev::Prop(ud, v));
                }
                _ => {}
            }
        }
        out
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.h) };
    }
}
