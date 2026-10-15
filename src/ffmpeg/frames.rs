//! Decoding single frames and filmstrip thumbnails out of a video.

use std::path::Path;

use super::probe::MediaInfo;
use super::process::{ffargs, fmt_time, run_ffmpeg_to_temp, MediaTools};
use crate::error::{Error, Result};

/// An 8-bit RGB image, 3 bytes per pixel.
#[derive(Debug, Clone)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
    /// Display interval relative to the selected stream.
    pub time: f64,
    pub end_time: f64,
}

impl Frame {
    pub fn as_rgb32(&self) -> Vec<u8> {
        // Tightly packed RGB -> RGBA bytes, independent of host endianness.
        let mut out = vec![0u8; self.w as usize * self.h as usize * 4];
        for (i, px) in self.rgb.chunks_exact(3).enumerate() {
            let o = i * 4;
            out[o] = px[0];
            out[o + 1] = px[1];
            out[o + 2] = px[2];
            out[o + 3] = 0xff;
        }
        out
    }
}

/// A row of thumbnails plus the exact time each one was sampled at.
#[derive(Debug, Clone)]
pub struct Filmstrip {
    pub thumbs: Vec<Frame>,
    /// One entry per thumbnail: source timestamp in seconds.
    pub times: Vec<f64>,
    pub total: u64,
}

/// Build the `fps=N/D` filter argument for a target sample count.
fn fps_filter(count: u32, duration: f64) -> String {
    let count = count.max(1);
    if duration <= 0.0 {
        // Fall back to one sample per second.
        return format!("fps={count}/1");
    }
    // frames per second = count / duration. Both terms are scaled by 1000 so a
    // fractional duration keeps its precision; the ratio is what ffmpeg reads.
    // Scaling only the denominator would silently ask for 1000x too few frames.
    let den = (duration * 1000.0).round().max(1.0) as u64;
    let num = count as u64 * 1000;
    format!("fps={num}/{den}")
}

/// Decode the frame displayed at timestamp `time`.
pub fn decode_frame(tools: &MediaTools, path: &Path, info: &MediaInfo, time: f64) -> Result<Frame> {
    let t = info
        .clamp_time(time)
        .min((info.duration - 0.000001).max(0.0));
    // Read actual PTS, including VFR and the last frame; never add half a frame.
    let pts_json = tools.frame_times(
        path,
        info.video_stream,
        (t + info.start_time - 1.0).max(0.0),
        t + info.start_time + 1.0,
    )?;
    let parsed = super::json::parse(&pts_json)?;
    let pts = parsed
        .get("frames")
        .and_then(|v| v.as_arr())
        .into_iter()
        .flatten()
        .filter_map(|v| v.f64_field("best_effort_timestamp_time"))
        .filter(|v| v.is_finite() && *v <= t + info.start_time + 1e-7)
        .max_by(f64::total_cmp)
        .ok_or_else(|| Error::BadMedia("no display frame at the requested time".into()))?;
    let seek = (pts - info.start_time).max(0.0);
    let end_time = parsed
        .get("frames")
        .and_then(|v| v.as_arr())
        .into_iter()
        .flatten()
        .filter_map(|v| v.f64_field("best_effort_timestamp_time"))
        .filter(|v| v.is_finite() && *v > pts + 1e-7)
        .min_by(f64::total_cmp)
        .map(|v| v - info.start_time)
        .unwrap_or((t + 1.0).min(info.duration));

    let tmp = run_ffmpeg_to_temp(
        &tools.ffmpeg,
        &ffargs![
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-ss",
            format!("{:.9}", (pts - info.format_start_time).max(0.0)),
            "-i",
            path.as_os_str(),
            "-map",
            format!("0:{}", info.video_stream),
            "-vf",
            format!(
                "scale={}:{}:flags=bilinear,setsar=1",
                info.width, info.height
            ),
            "-frames:v",
            "1",
            "-an",
            "-sn",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            // `run_ffmpeg_to_temp` redirects stdout to a file, so the output
            // target has to be named explicitly as "-".
            "-"
        ],
        "poster",
    )?;

    if !tmp.success {
        return Err(Error::ToolFailed {
            cmd: "ffmpeg (frame decode)".into(),
            detail: tmp.stderr,
        });
    }

    let expect = info.width as usize * info.height as usize * 3;
    let bytes = tmp.read_all()?;
    if bytes.len() != expect {
        return Err(Error::BadMedia(format!(
            "decoded frame is {} bytes, expected {} ({}x{}). The stream may use an \
             unsupported pixel layout.",
            bytes.len(),
            expect,
            info.width,
            info.height
        )));
    }

    Ok(Frame {
        w: info.width,
        h: info.height,
        rgb: bytes[..expect].to_vec(),
        time: seek,
        end_time,
    })
}

/// Decode `count` evenly spaced thumbnails, at most `thumb_w` pixels wide.
pub fn filmstrip(
    tools: &MediaTools,
    path: &Path,
    info: &MediaInfo,
    count: u32,
    thumb_w: u32,
) -> Result<Filmstrip> {
    let count = count.clamp(1, 256);
    let thumb_w = thumb_w.clamp(32, 480);

    let h = ((thumb_w as f64 / info.aspect() / 2.0).round() as u32 * 2).clamp(2, 4096);
    let filter = format!(
        "{},scale={thumb_w}:{h}:flags=bilinear,setsar=1,showinfo",
        fps_filter(count, info.duration)
    );

    let tmp = run_ffmpeg_to_temp(
        &tools.ffmpeg,
        &ffargs![
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "info",
            "-nostats",
            "-i",
            path.as_os_str(),
            "-map",
            format!("0:{}", info.video_stream),
            "-frames:v",
            count.to_string(),
            "-an",
            "-sn",
            "-vf",
            filter,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            // See the note in `decode_frame`: stdout is the output here.
            "-"
        ],
        "strip",
    )?;

    if !tmp.success {
        return Err(Error::ToolFailed {
            cmd: "ffmpeg (filmstrip)".into(),
            detail: tmp.stderr,
        });
    }

    let bytes = tmp.read_all()?;
    let times = parse_showinfo_times(&tmp.stderr);

    let frame_bytes = thumb_w as usize * h as usize * 3;
    if bytes.is_empty() || bytes.len() % frame_bytes != 0 {
        return Err(Error::BadMedia(
            "thumbnail output is empty or not frame-aligned".into(),
        ));
    }
    let n = bytes.len() / frame_bytes;
    let mut thumbs = Vec::with_capacity(n);
    for i in 0..n {
        let start = i * frame_bytes;
        thumbs.push(Frame {
            w: thumb_w,
            h,
            rgb: bytes[start..start + frame_bytes].to_vec(),
            time: times.get(i).copied().unwrap_or(0.0),
            end_time: info.duration,
        });
    }

    // Align timestamps with thumbs; synthesise uniform times if showinfo failed.
    let times = if times.len() == thumbs.len() {
        times
    } else {
        (0..thumbs.len())
            .map(|i| {
                if thumbs.len() <= 1 {
                    0.0
                } else {
                    info.duration * i as f64 / thumbs.len() as f64
                }
            })
            .collect()
    };

    let total = thumbs.len() as u64;
    Ok(Filmstrip {
        thumbs,
        times,
        total,
    })
}

/// Pull `pts_time:` values out of `showinfo` lines on stderr.
///
/// showinfo timestamps are relative to the start of the filter input, which is
/// the start of the file for the filmstrip pass.
fn parse_showinfo_times(stderr: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        if !line.contains("showinfo") {
            continue;
        }
        let mut n: Option<u64> = None;
        let mut t: Option<f64> = None;
        for tok in line.split_whitespace() {
            if let Some(rest) = tok.strip_prefix("n:") {
                n = Some(rest.parse::<u64>().unwrap_or(0));
            } else if let Some(rest) = tok.strip_prefix("pts_time:") {
                t = rest.parse::<f64>().ok();
            }
        }
        if let (Some(_), Some(t)) = (n, t) {
            out.push(t);
        }
    }
    out
}

/// Describe the sampled time span, used for logging.
pub fn describe_span(times: &[f64]) -> String {
    match (times.first(), times.last()) {
        (Some(a), Some(b)) => format!("{} .. {}", fmt_time(*a), fmt_time(*b)),
        _ => "empty".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_argument_is_a_rational() {
        assert_eq!(fps_filter(8, 0.0), "fps=8/1");

        // The value of the rational is what matters, not its spelling: it must
        // equal count/duration. Asserting the literal string is what let a
        // 1000x-too-small rate sit here unnoticed.
        for (count, duration) in [(9u32, 5.0f64), (1, 0.5), (40, 12.345), (12, 5.0)] {
            let arg = fps_filter(count, duration);
            let body = arg.strip_prefix("fps=").expect("fps= prefix");
            let (num, den) = body.split_once('/').expect("a rational");
            let num: f64 = num.parse().unwrap();
            let den: f64 = den.parse().unwrap();
            let got = num / den;
            let want = count as f64 / duration;
            assert!(
                (got - want).abs() < 1e-6,
                "{arg} is {got} fps but {count} frames over {duration}s is {want} fps"
            );
        }
    }

    #[test]
    fn parses_showinfo_lines() {
        let s = concat!(
            "[Parsed_showinfo_1 @ 0x1] n:   0 pts:      0 pts_time:0        duration:1\n",
            "[Parsed_showinfo_1 @ 0x2] n:   1 pts:   1500 pts_time:0.555556 duration:1\n"
        );
        let t = parse_showinfo_times(s);
        assert_eq!(t.len(), 2, "raw sample was {s:?}");
        assert!((t[1] - 0.555556).abs() < 1e-9);
    }
}
