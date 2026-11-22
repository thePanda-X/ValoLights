//! ValoLights: Valorant highlight editor.
//!
//!     valolights                       -> desktop app
//!     valolights --worker job.json     -> background job (spawned by the app)
//!     valolights cli ...               -> command line tools
//!     valolights --selftest            -> packaging smoke test
//!     valolights --ui-check [out_dir]  -> scripted GUI check, renders the app's own frames to PNG
//!     valolights --read-only           -> GUI that never writes projects or settings (QA)
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod backend;
mod cli;
mod dialogs;
mod icons;
mod jobs;
mod pages;
mod perf;
mod selftest;
mod shot;
mod speech;
mod state;
mod sys;
mod theme;
mod timefmt;
mod uicheck;
mod uicheck_export;
mod uicheck_layout;
mod uicheck_subs;
mod uicheck_trim;
mod update;
mod update_ui;
mod widgets;
mod worker;

use vl_core::{logging, paths};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--worker") => {
            // the parent passes pipes, which a windowed exe inherits as its std handles
            sys::attach_parent_console();
            paths::ensure_data_dirs();
            logging::setup("worker", false);
            match args.get(1) {
                Some(spec) => worker::run_job(spec),
                None => {
                    eprintln!("usage: valolights --worker <job.json>");
                    2
                }
            }
        }
        Some("cli") => {
            sys::attach_parent_console();
            paths::ensure_data_dirs();
            cli::main(&args[1..])
        }
        Some("--selftest") => {
            sys::attach_parent_console();
            paths::ensure_data_dirs();
            logging::setup("selftest", false);
            selftest::run()
        }
        Some("--ui-check") if args.get(1).map(String::as_str) == Some("export") => {
            sys::attach_parent_console();
            uicheck_export::run(&args[2..])
        }
        Some("--ui-check") if args.get(1).map(String::as_str) == Some("subs") => {
            sys::attach_parent_console();
            uicheck_subs::run(&args[2..])
        }
        Some("--ui-check") if args.get(1).map(String::as_str) == Some("trim") => {
            sys::attach_parent_console();
            uicheck_trim::run(&args[2..])
        }
        Some("--ui-check") if args.get(1).map(String::as_str) == Some("layout") => {
            sys::attach_parent_console();
            uicheck_layout::run(&args[2..])
        }
        Some("--shot") => {
            sys::attach_parent_console();
            shot::run(&args[1..])
        }
        Some("--perf") => {
            sys::attach_parent_console();
            perf::run(&args[1..])
        }
        Some("--ui-check") => {
            sys::attach_parent_console();
            uicheck::run(&args[1..])
        }
        _ => {
            let read_only = args.iter().any(|a| a == "--read-only") || std::env::var_os("VALOLIGHTS_READONLY").is_some();
            if !read_only {
                // leftovers of interrupted renders / closed apps (a day old, so a running job is never touched)
                std::thread::spawn(|| paths::clean_tmp(std::time::Duration::from_secs(24 * 3600)));
            }
            app::run(app::Options { read_only, ..Default::default() })
        }
    };
    std::process::exit(code);
}
