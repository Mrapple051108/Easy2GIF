//! Everything that talks to the outside world for media: probing, decoding.
//!
//! The core drives the bundled `ffmpeg` / `ffprobe` executables instead of
//! linking libav*, so it stays dependency-free and the shipped build contains
//! only what we put there.

pub mod frames;
pub mod json;
pub mod probe;
pub mod process;

pub use frames::{decode_frame, filmstrip, Frame, Filmstrip};
pub use probe::{friendly_fps, probe, MediaInfo};
pub use process::{fmt_time, run_ffmpeg_stream, DataFile, MediaTools, TempFile};
