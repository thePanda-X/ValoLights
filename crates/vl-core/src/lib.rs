//! Shared foundation: data models, paths, config, projects, ffmpeg, jobs.
pub mod config;
pub mod ffmpeg;
pub mod image;
pub mod jobs;
pub mod logging;
pub mod models;
pub mod paths;
pub mod projects;
pub mod speech_words;
pub mod transcript;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
