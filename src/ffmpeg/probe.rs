//! Video metadata extraction through `ffprobe`.

use std::path::Path;

use super::json::{self, Json};
use super::process::MediaTools;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// Exact average frame rate, e.g. 29.97002997.
    pub fps: f64,
    /// Frame rate rounded to a friendly value when it matches a common rate.
    pub fps_display: f64,
    pub duration: f64,
    /// Total frames when the container reports it.
    pub frame_count: u64,
    pub has_audio: bool,
    pub video_codec: String,
    pub audio_codec: String,
    /// Rotation metadata in degrees (0/90/180/270), if present.
    pub rotation: i32,
}

impl MediaInfo {
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            1.0
        } else {
            self.width as f64 / self.height as f64
        }
    }

    /// Clamp a time to the media bounds.
    pub fn clamp_time(&self, t: f64) -> f64 {
        if !t.is_finite() {
            return 0.0;
        }
        t.max(0.0).min(self.duration.max(0.0))
    }

    /// Time of the centre of frame `index`.
    pub fn frame_time(&self, index: i64) -> f64 {
        if self.fps > 0.0 {
            (index as f64 + 0.5) / self.fps
        } else {
            0.0
        }
    }

    /// Nearest frame index for a timestamp.
    pub fn time_to_frame(&self, t: f64) -> i64 {
        if self.fps > 0.0 {
            (t * self.fps).floor().max(0.0) as i64
        } else {
            0
        }
    }

    pub fn total_frames(&self) -> u64 {
        if self.frame_count > 0 {
            self.frame_count
        } else if self.fps > 0.0 {
            (self.duration * self.fps).round().max(1.0) as u64
        } else {
            1
        }
    }

    /// Whether there is enough information to do useful work.
    pub fn is_usable(&self) -> bool {
        self.width > 0 && self.height > 0 && self.duration > 0.0
    }
}

/// Round a raw fps to a common friendly value when it is close enough.
pub fn friendly_fps(fps: f64) -> f64 {
    const COMMON: [f64; 9] = [
        23.976023976, 24.0, 25.0, 29.97002997, 30.0, 48.0, 50.0, 59.94005994, 60.0,
    ];
    for c in COMMON {
        if (fps - c).abs() < 0.02 {
            return c;
        }
    }
    // Otherwise give two decimals of precision.
    (fps * 100.0).round() / 100.0
}

pub fn probe(tools: &MediaTools, path: &Path) -> Result<MediaInfo> {
    if !path.exists() {
        return Err(Error::BadMedia(format!(
            "file does not exist: {}",
            path.display()
        )));
    }
    let text = tools.probe_json(path)?;
    let root = json::parse(&text)?;

    let streams = root
        .get("streams")
        .and_then(|v| v.as_arr())
        .ok_or_else(|| Error::BadMedia("ffprobe returned no streams".into()))?;

    let mut info = MediaInfo {
        path: path.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let format = root.get("format");
    if let Some(d) = format.and_then(|f| f.f64_field("duration")) {
        info.duration = d;
    }

    let mut found_video = false;

    for s in streams {
        let codec_type = s.str_field("codec_type").unwrap_or("");
        match codec_type {
            "video" => {
                // Ignore cover art / attached pictures.
                let is_attached = s
                    .get("disposition")
                    .and_then(|d| d.f64_field("attached_pic"))
                    .map(|v| v == 1.0)
                    .unwrap_or(false);
                if is_attached && found_video {
                    continue;
                }
                if found_video {
                    continue; // first real video stream wins
                }
                found_video = true;

                info.width = s.f64_field("width").unwrap_or(0.0) as u32;
                info.height = s.f64_field("height").unwrap_or(0.0) as u32;
                info.video_codec = s.str_field("codec_name").unwrap_or("").to_string();

                let fps = s
                    .str_field("avg_frame_rate")
                    .and_then(json::parse_rational)
                    .or_else(|| s.str_field("r_frame_rate").and_then(json::parse_rational))
                    .unwrap_or(0.0);
                info.fps = fps;
                info.fps_display = if fps > 0.0 { friendly_fps(fps) } else { 0.0 };

                if let Some(fc) = s
                    .str_field("nb_frames")
                    .and_then(|v| v.parse::<u64>().ok())
                    .or_else(|| s.f64_field("nb_frames").map(|v| v as u64))
                {
                    info.frame_count = fc;
                }

                // A stream duration can rescue files with a broken container duration.
                if info.duration <= 0.0 {
                    if let Some(d) = s.f64_field("duration") {
                        info.duration = d;
                    }
                }

                info.rotation = read_rotation(s);
            }
            "audio" => {
                if !info.has_audio {
                    info.has_audio = true;
                    info.audio_codec = s.str_field("codec_name").unwrap_or("").to_string();
                }
            }
            _ => {}
        }
    }

    if !found_video {
        return Err(Error::BadMedia(
            "no video stream found in this file".into(),
        ));
    }

    // Derive a duration from frame count when the container does not give one.
    if info.duration <= 0.0 && info.frame_count > 0 && info.fps > 0.0 {
        info.duration = info.frame_count as f64 / info.fps;
    }

    // A 90/270 rotation swaps the displayed dimensions.
    if info.rotation == 90 || info.rotation == 270 {
        std::mem::swap(&mut info.width, &mut info.height);
    }

    if !info.is_usable() {
        return Err(Error::BadMedia(format!(
            "unusable video: {}x{}, duration {}s",
            info.width, info.height, info.duration
        )));
    }

    Ok(info)
}

fn read_rotation(stream: &Json) -> i32 {
    // Modern ffprobe: side_data_list[].rotation
    if let Some(list) = stream.get("side_data_list").and_then(|v| v.as_arr()) {
        for item in list {
            if let Some(r) = item.f64_field("rotation") {
                return normalize_rotation(r as i32);
            }
        }
    }
    // Legacy: tags.rotate
    if let Some(tags) = stream.get("tags") {
        if let Some(r) = tags.f64_field("rotate") {
            return normalize_rotation(r as i32);
        }
    }
    0
}

fn normalize_rotation(r: i32) -> i32 {
    let mut r = r % 360;
    if r < 0 {
        r += 360;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_rates() {
        assert_eq!(friendly_fps(29.97002997), 29.97002997);
        assert_eq!(friendly_fps(29.97), 29.97002997);
        assert_eq!(friendly_fps(12.345), 12.35);
    }

    #[test]
    fn frame_math() {
        let m = MediaInfo {
            fps: 25.0,
            duration: 4.0,
            frame_count: 100,
            ..Default::default()
        };
        assert_eq!(m.time_to_frame(0.0), 0);
        assert_eq!(m.time_to_frame(1.99), 49);
        assert_eq!(m.total_frames(), 100);
        assert!((m.frame_time(0) - 0.02).abs() < 1e-9);
    }
}
