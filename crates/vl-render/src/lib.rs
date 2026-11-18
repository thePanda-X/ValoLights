//! The render pipeline: highlights, edit list, layouts, subtitles (ASS), kill feed, overlays, looks,
//! super smooth (optical-flow motion blur or RIFE), music, the ffmpeg render and editor exporters.
pub mod ass;
pub mod editlist;
pub mod exporters;
pub mod flow;
pub mod highlights;
pub mod killfeed_plan;
pub mod layoutspec;
pub mod looks;
pub mod music;
pub mod numfmt;
pub mod overlays;
pub mod render;
pub mod smart;
pub mod smooth;
pub mod vecblur;

pub use editlist::{EditList, Placed};
