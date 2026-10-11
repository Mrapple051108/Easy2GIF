//! GIF encoding: colour quantisation and the GIF89a container.
//!
//! Both are implemented here rather than pulled from a crate so the core has no
//! external dependencies and nothing extra has to be shipped.

pub mod encode;
pub mod quantize;

pub use encode::{frame_block, write_gif, Disposal, FrameSpec, GifWriter};
pub use quantize::{
    map_frame, map_frame_dithered, map_frame_dithered_with, map_frame_with, quantize,
    quantize_with_threads, ColorMap, Palette,
};

/// Dithering modes exposed to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    None,
    Ordered,
    Floyd,
}

impl Dither {
    pub fn from_str(s: &str) -> Dither {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "无" | "无抖动" => Dither::None,
            "ordered" | "bayer" | "有序" => Dither::Ordered,
            _ => Dither::Floyd,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Dither::None => "none",
            Dither::Ordered => "ordered",
            Dither::Floyd => "floyd",
        }
    }

    /// Human label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Dither::None => "None",
            Dither::Ordered => "Ordered (fast)",
            Dither::Floyd => "Floyd–Steinberg (best)",
        }
    }
}
