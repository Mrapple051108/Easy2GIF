//! `gifcore` — the Rust core of the video-to-GIF converter.
//!
//! Responsibilities:
//!   * locate and drive the bundled `ffmpeg` / `ffprobe` executables,
//!   * probe video metadata and decode individual frames or filmstrips,
//!   * quantise frames to a palette and write a GIF89a file from scratch,
//!   * expose all of the above to the C++ / QML layer through a C ABI.
//!
//! The front end is a pure interaction layer: it never touches pixel data or
//! media containers itself.
//!
//! # Layout
//!
//! * [`error`] — the shared error type.
//! * [`ffmpeg`] — process handling, probing, and frame decoding.
//! * [`gif`] — colour quantisation and the GIF encoder.
//! * [`export`] — the end-to-end conversion pipeline with progress reporting.
//! * [`ffi`] — the C ABI used by the Qt front end.
//! * [`platform`] — facts about the host the front end needs reported to it.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod error;
pub mod export;
pub mod ffmpeg;
pub mod ffi;
pub mod gif;
pub mod platform;

pub use error::{Error, Result};
pub use export::{check_output, export, Outcome, Progress, Settings};
pub use ffmpeg::probe::{probe, MediaInfo};
pub use ffmpeg::process::{scratch_dir, MediaTools};
pub use gif::Dither;
pub use platform::Integrity;

/// Semantic version of the core.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_exposed() {
        assert!(!VERSION.is_empty());
    }
}
