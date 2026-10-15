//! The export pipeline: decode a time range, quantise, and write a GIF.
//!
//! Progress is published through [`Progress`], an atomic meter that the C ABI
//! polls from the UI thread. Keeping it poll-based (rather than a callback into
//! C++) means the core needs no FFI knowledge of the front end.

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::{Error, Result};
use crate::ffmpeg::probe::{probe_with_cancel, MediaInfo};
use crate::ffmpeg::process::{fmt_time, run_ffmpeg_stream_cancel, MediaTools};
use crate::gif::encode::{frame_block, set_block_delay, Disposal, FrameSpec, GifWriter};
use crate::gif::quantize::{
    map_frame_dithered_with, map_frame_ordered_with, map_frame_with, posterize_in_place,
    posterize_levels, quantize_cancellable, quantize_iterations, ColorMap, Palette,
};
use crate::gif::Dither;

/// Progress phase codes shared with the UI.
pub const PHASE_IDLE: u32 = 0;
pub const PHASE_DECODING: u32 = 1;
pub const PHASE_ANALYSING: u32 = 2;
pub const PHASE_ENCODING: u32 = 3;
pub const PHASE_DONE: u32 = 4;
pub const PHASE_FAILED: u32 = 5;

/// A progress meter that is safe to read from another thread.
#[derive(Debug)]
pub struct Progress {
    phase: AtomicU32,
    done: AtomicU32,
    total: AtomicU32,
    completed: AtomicU32,
}

impl Default for Progress {
    fn default() -> Self {
        Progress::new()
    }
}

impl Progress {
    pub fn new() -> Progress {
        Progress {
            phase: AtomicU32::new(PHASE_IDLE),
            done: AtomicU32::new(0),
            total: AtomicU32::new(0),
            completed: AtomicU32::new(0),
        }
    }

    pub fn phase_code(&self) -> u32 {
        self.phase.load(Ordering::Relaxed)
    }

    pub fn phase_name(&self) -> &'static str {
        match self.phase_code() {
            PHASE_DECODING => "Decoding frames",
            PHASE_ANALYSING => "Analysing colours",
            PHASE_ENCODING => "Encoding GIF",
            PHASE_DONE => "Done",
            PHASE_FAILED => "Failed",
            _ => "Idle",
        }
    }

    pub fn done(&self) -> u32 {
        self.done.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> u32 {
        self.total.load(Ordering::Relaxed)
    }

    /// Overall completion in the range 0..=1.
    pub fn fraction(&self) -> f64 {
        let total = self.total();
        match self.phase_code() {
            PHASE_DONE => 1.0,
            PHASE_IDLE | PHASE_FAILED => 0.0,
            phase => {
                let within = if total == 0 {
                    0.0
                } else {
                    (self.done() as f64 / total as f64).clamp(0.0, 1.0)
                };
                let (base, weight) = match phase {
                    PHASE_DECODING => (0.0, 0.4),
                    PHASE_ANALYSING => (0.4, 0.2),
                    PHASE_ENCODING => (0.6, 0.4),
                    _ => (0.0, 0.0),
                };
                let current = ((base + weight * within) * 1_000_000.0).round() as u32;
                (self
                    .completed
                    .fetch_max(current, Ordering::Relaxed)
                    .max(current) as f64)
                    / 1_000_000.0
            }
        }
    }

    /// Begin a phase with `total` work items.
    pub fn begin(&self, code: u32, total: u32) {
        if code == PHASE_IDLE || code == PHASE_DECODING {
            self.completed.store(0, Ordering::Relaxed);
        }
        self.total.store(total.max(1), Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.phase.store(code, Ordering::Relaxed);
    }

    pub fn step(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    pub fn finish(&self, code: u32) {
        self.done.store(self.total(), Ordering::Relaxed);
        self.phase.store(code, Ordering::Relaxed);
    }
}

/// How the GIF should be produced.
#[derive(Debug, Clone)]
pub struct Settings {
    pub input: PathBuf,
    /// Selected stream, filled from the probe before decoding.
    pub video_stream: u32,
    pub seek_offset: f64,
    pub output: PathBuf,
    /// Start of the selection, in seconds from the beginning of the video.
    pub start: f64,
    /// End of the selection, in seconds.
    pub end: f64,
    /// Output frame rate (GIF frames per second).
    pub fps: f64,
    /// Maximum output width in pixels; the height follows the aspect ratio.
    pub max_width: u32,
    /// Encoded frame rate multiplier: 1.0 = real time, 0.5 = half speed.
    pub speed: f64,
    /// Palette size, 2..=256.
    pub colors: u32,
    pub dither: Dither,
    pub loop_forever: bool,
    pub overwrite: bool,
    /// Worker threads for the palette-mapping pass. `0` means "decide for me",
    /// which resolves to half of the machine's logical CPUs at run time.
    pub max_threads: u32,
    // ---- advanced settings ----
    /// 「编码质量」 1..=10: how much k-means effort goes into the palette.
    pub quality: u32,
    /// 「有损压缩」 0..=100: snap colours onto a coarse grid before quantising.
    pub lossy: u32,
    /// 「抖动强度」 0..=100: percentage of the dither amplitude to apply.
    pub dither_strength: u32,
    /// 「复用重复帧」: stretch a frame's delay while the next ones render
    /// identically instead of writing them again.
    pub dedupe: bool,
    /// 「帧间透明优化」: leave unchanged pixels transparent so the decoder
    /// keeps showing the previous frame.
    pub optimize: bool,
    /// Per-channel slack, 0..=64, for "this pixel did not really change".
    pub tolerance: u32,
    /// Set by the front end to abort a running export. Checked between frames.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// Default effort: three Lloyd iterations. Correctness fixes may change encoded bytes.
pub const DEFAULT_QUALITY: u32 = 5;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            input: PathBuf::new(),
            video_stream: 0,
            seek_offset: 0.0,
            output: PathBuf::new(),
            start: 0.0,
            end: 0.0,
            fps: 12.0,
            max_width: 480,
            speed: 1.0,
            colors: 256,
            dither: Dither::Floyd,
            loop_forever: true,
            overwrite: true,
            max_threads: 0,
            quality: DEFAULT_QUALITY,
            lossy: 0,
            dither_strength: 100,
            dedupe: false,
            optimize: false,
            tolerance: 0,
            cancel: None,
        }
    }
}

impl Settings {
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(false)
    }
}

fn cancelled_error() -> Error {
    Error::Cancelled
}

/// The per-frame knobs, resolved once per export instead of once per frame.
#[derive(Debug, Clone, Copy)]
struct FrameOptions {
    dither: Dither,
    /// Dither amplitude, 0..=1, already scaled by 「抖动强度」.
    strength: f64,
    /// Colour-grid steps for 「有损压缩」; below 2 means "do not touch".
    posterize_levels: u32,
    /// Index meaning "identical to the previous frame"; `None` turns the
    /// inter-frame diff off.
    transparent: Option<u8>,
    /// Per-channel slack for that comparison.
    tolerance: u32,
    /// Stretch the previous frame's delay instead of writing an identical one.
    dedupe: bool,
}

impl FrameOptions {
    fn new(settings: &Settings, transparent: Option<u8>) -> FrameOptions {
        FrameOptions {
            dither: settings.dither,
            strength: settings.dither_strength.min(100) as f64 / 100.0,
            posterize_levels: posterize_levels(settings.lossy),
            transparent,
            tolerance: settings.tolerance.min(64),
            dedupe: settings.dedupe,
        }
    }
}

/// Compare mapped colours to the actual retained canvas, never the source RGB.
fn apply_transparency(
    indices: &mut [u8],
    canvas: &mut Vec<u8>,
    palette: &Palette,
    tolerance: u32,
    transparent: u8,
) -> bool {
    if canvas.len() != indices.len() {
        *canvas = indices.to_vec();
        return false;
    }
    let mut unchanged = true;
    for (current, retained) in indices.iter_mut().zip(canvas.iter_mut()) {
        let a = palette.colors[*current as usize];
        let b = palette.colors[*retained as usize];
        if (0..3).all(|c| a[c].abs_diff(b[c]) as u32 <= tolerance) {
            *current = transparent;
        } else {
            *retained = *current;
            unchanged = false;
        }
    }
    unchanged
}

/// How many logical CPUs this machine reports, read at run time.
///
/// `available_parallelism` respects CPU affinity and cgroup limits, so this is
/// the number of workers the process can actually use, not the core count on
/// the box.
pub fn cpu_count() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .max(1)
}

/// The worker count used when the caller does not pick one: half the CPUs.
///
/// Half rather than all of them because the exporter also drives ffmpeg, which
/// runs threads of its own; saturating every core here would just make the two
/// fight over the same hardware.
pub fn default_threads() -> u32 {
    (cpu_count() / 2).max(1)
}

/// Turn a requested worker count into a usable one. `0` means "decide for me".
///
/// A request above the machine's CPU count is clamped down: more workers than
/// cores cannot map frames any faster and only adds contention.
pub fn resolve_threads(requested: u32) -> u32 {
    if requested == 0 {
        default_threads()
    } else {
        requested.clamp(1, cpu_count())
    }
}

/// What an export produced.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub output: PathBuf,
    pub frames: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub palette_colors: u32,
    pub elapsed_ms: u64,
    pub warning: Option<String>,
}

impl Outcome {
    /// A one-line summary suitable for a status bar.
    pub fn summary(&self) -> String {
        format!(
            "{} frames · {}x{} · {:.1} fps · {} · {:.2} s clip",
            self.frames,
            self.width,
            self.height,
            self.fps,
            human_bytes(self.bytes),
            self.duration
        )
    }
}

pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Frame size of the output, preserving the source aspect ratio.
///
/// Both dimensions are kept even, which is what the scale filter wants.
pub fn output_size(info: &MediaInfo, max_width: u32) -> (u32, u32) {
    // GIF supports odd dimensions. Even rounding is optional, never a reason to upscale.
    let cap = max_width.clamp(2, 4096);
    let aspect = info.aspect().max(1e-12);
    let mut w = info.width.min(cap).max(1);
    let mut h = (w as f64 / aspect).round().max(1.0);
    if h > 4096.0 {
        h = 4096.0;
        w = (h * aspect).round().clamp(1.0, cap as f64) as u32;
    }
    let even = |n: u32| if n > 1 { n - n % 2 } else { 1 };
    (even(w), even(h as u32))
}

fn validate_settings(settings: &Settings) -> Result<()> {
    check_cancel(settings)?;
    if !settings.fps.is_finite() || !(0.1..=120.0).contains(&settings.fps) {
        return Err(Error::Invalid(
            "frame rate must be finite and between 0.1 and 120".into(),
        ));
    }
    if !settings.speed.is_finite() || !(0.1..=10.0).contains(&settings.speed) {
        return Err(Error::Invalid(
            "speed must be finite and between 0.1 and 10".into(),
        ));
    }
    if !settings.start.is_finite() || !settings.end.is_finite() || settings.end <= settings.start {
        return Err(Error::Invalid(
            "the selection must contain finite increasing times".into(),
        ));
    }
    Ok(())
}

fn check_cancel(settings: &Settings) -> Result<()> {
    if settings.cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn timeline_cs(settings: &Settings, frames: u64) -> u64 {
    let planned = planned_frames(settings.start, settings.end, settings.fps);
    ((settings.end - settings.start) / settings.speed * 100.0 * frames as f64 / planned as f64)
        .round()
        .max(2.0 * frames as f64) as u64
}

fn delay_at(total_cs: u64, frames: u64, index: u64) -> u16 {
    let boundary =
        |i: u64| ((i as u128 * total_cs as u128 + frames as u128 / 2) / frames as u128) as u64;
    (boundary(index + 1) - boundary(index)).clamp(2, u16::MAX as u64) as u16
}

/// How many frames the request will produce.
pub fn planned_frames(start: f64, end: f64, fps: f64) -> u64 {
    let dur = (end - start).max(0.0);
    if !fps.is_finite() || !start.is_finite() || !end.is_finite() || fps <= 0.0 || dur <= 0.0 {
        return 1;
    }
    ((dur * fps).round() as u64).max(1)
}

/// Build the raw-frame decode command, starting at an arbitrary point of the
/// source. The size estimator uses this to read a small window out of the middle
/// of a clip.
fn decode_args_at(
    settings: &Settings,
    start: f64,
    w: u32,
    h: u32,
    frames: u64,
) -> Vec<std::ffi::OsString> {
    let vf = format!(
        "fps={},trim=end_frame={},scale={}:{}:flags=lanczos,setsar=1",
        fmt_time(settings.fps),
        frames,
        w,
        h
    );
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        fmt_time(start + settings.seek_offset).into(),
        "-i".into(),
        settings.input.as_os_str().to_os_string(),
        "-map".into(),
        format!("0:{}", settings.video_stream).into(),
        "-an".into(),
        "-sn".into(),
        "-dn".into(),
        "-vf".into(),
        vf.into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-f".into(),
        "rawvideo".into(),
        "-".into(),
    ]
}

/// The raw file holding every decoded frame.
struct RawFrames {
    temp: crate::ffmpeg::process::TempFile,
    count: u64,
    segments: Vec<u64>,
}

fn decode_all(
    tools: &MediaTools,
    settings: &Settings,
    start: f64,
    w: u32,
    h: u32,
    planned: u64,
    progress: &Progress,
) -> Result<RawFrames> {
    decode_segments(tools, settings, &[(start, planned)], w, h, progress)
}

/// Decode several short runs into one frame-aligned raw file.
///
/// The estimator uses this to read a few moments spread over the selection: a
/// single window would read the whole clip from one moment. Every run lands in
/// the same file, so the palette search and the encoder afterwards see an
/// ordinary frame sequence.
fn decode_segments(
    tools: &MediaTools,
    settings: &Settings,
    runs: &[(f64, u64)],
    w: u32,
    h: u32,
    progress: &Progress,
) -> Result<RawFrames> {
    let temp = crate::ffmpeg::process::TempFile::new("raw")?;
    let cap = w as usize * h as usize * 3; // one frame per chunk
    let out = std::fs::File::create(temp.path())?;
    let mut out = BufWriter::with_capacity(1 << 20, out);
    let mut written: u64 = 0;
    let mut segments = Vec::new();

    for (start, frames) in runs {
        check_cancel(settings)?;
        let before = written;
        decode_run_into(
            tools,
            settings,
            *start,
            w,
            h,
            *frames,
            cap,
            &mut out,
            &mut written,
            progress,
        )?;
        segments.push(written - before);
    }

    {
        use std::io::Write;
        out.flush()?;
    }

    if written == 0 {
        let start = runs.first().map(|(s, _)| *s).unwrap_or(settings.start);
        return Err(Error::BadMedia(format!(
            "no frames decoded for {}..{} at {} fps",
            fmt_time(start),
            fmt_time(settings.end),
            fmt_time(settings.fps)
        )));
    }

    Ok(RawFrames {
        temp,
        count: written,
        segments,
    })
}

/// Decode `frames` frames from `start` and append them to an open raw file.
#[allow(clippy::too_many_arguments)]
fn decode_run_into<W: std::io::Write>(
    tools: &MediaTools,
    settings: &Settings,
    start: f64,
    w: u32,
    h: u32,
    frames: u64,
    cap: usize,
    out: &mut W,
    written: &mut u64,
    progress: &Progress,
) -> Result<()> {
    let mut pending: Vec<u8> = Vec::with_capacity(cap * 2);
    let args = decode_args_at(settings, start, w, h, frames);
    run_ffmpeg_stream_cancel(
        &tools.ffmpeg,
        &args,
        cap,
        settings.cancel.as_deref(),
        |chunk| {
            pending.extend_from_slice(chunk);
            // Write out only whole frames so the file stays frame-aligned.
            while pending.len() >= cap {
                out.write_all(&pending[..cap])?;
                pending.drain(..cap);
                *written += 1;
                progress.step();
                if settings.cancelled() {
                    return Err(cancelled_error());
                }
            }
            Ok(())
        },
    )
    .and_then(|_| {
        if pending.is_empty() {
            Ok(())
        } else {
            Err(Error::BadMedia("truncated decoder frame".into()))
        }
    })
}

/// A sequential reader over a frame-aligned raw file.
struct RawReader {
    file: std::fs::File,
    cap: usize,
}

impl RawReader {
    fn open(path: &Path, cap: usize) -> Result<RawReader> {
        Ok(RawReader {
            file: std::fs::File::open(path)?,
            cap,
        })
    }

    /// Load the next whole frame; `Ok(false)` at a clean end of data.
    fn next(&mut self, buf: &mut Vec<u8>) -> Result<bool> {
        use std::io::Read;
        buf.clear();
        buf.resize(self.cap, 0);
        let mut got = 0usize;
        while got < self.cap {
            match self.file.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e.to_string())),
            }
        }
        if got == 0 {
            buf.clear();
            return Ok(false);
        }
        if got < self.cap {
            return Err(Error::BadMedia(format!(
                "truncated raw frame: {got} of {} bytes",
                self.cap
            )));
        }
        Ok(true)
    }
}

/// Run an export; expected input, tool, cancellation and I/O failures return [`Error`].
pub fn export(tools: &MediaTools, settings: &Settings, progress: &Progress) -> Result<Outcome> {
    let started = std::time::Instant::now();

    if settings.output.as_os_str().is_empty() {
        return Err(Error::Invalid("no output file was chosen".into()));
    }
    validate_settings(settings)?;
    check_output(&settings.output)?;
    if same_file(&settings.input, &settings.output)? {
        return Err(Error::Invalid(
            "input and output must be different files".into(),
        ));
    }
    if settings.output.exists() && !settings.overwrite {
        return Err(Error::Output("the output already exists".into()));
    }
    let info = probe_with_cancel(tools, &settings.input, settings.cancel.as_deref())?;
    let start = info.clamp_time(settings.start);
    let end = info.clamp_time(settings.end);
    if end <= start {
        return Err(Error::Invalid(
            "the selected range is empty after clamping to the video".into(),
        ));
    }

    // Resample when necessary to respect the compatible 2 cs minimum without slow motion.
    let rate_limited = settings.fps * settings.speed > 50.0;
    let effective_fps = settings.fps.min(50.0 / settings.speed);
    let frames = planned_frames(start, end, effective_fps);
    let (w, h) = output_size(&info, settings.max_width);

    let mut settings = settings.clone();
    settings.start = start;
    settings.end = end;
    settings.fps = effective_fps;
    settings.video_stream = info.video_stream;
    settings.seek_offset = info.start_time - info.format_start_time;

    // Pass 1: decode the selection into a frame-aligned raw file.
    // The worker count is settled up front: both the palette search and the
    // frame mapping use it.
    let threads = resolve_threads(settings.max_threads);
    progress.begin(PHASE_DECODING, frames.min(u32::MAX as u64) as u32);
    let raw = decode_all(tools, &settings, start, w, h, frames, progress)?;
    let frame_count = raw.count;
    progress.finish(PHASE_DECODING);

    // Pass 2: learn a palette from a representative sample, then index every
    // frame with it.
    progress.begin(PHASE_ANALYSING, frame_count.min(u32::MAX as u64) as u32);
    let mut reader = RawReader::open(raw.temp.path(), w as usize * h as usize * 3)?;

    // The inter-frame optimisation needs a palette index of its own, so one
    // slot is held back -- but only from the palette the *mapper* sees. The
    // writer gets a copy that includes the reserved slot, which is what makes
    // the GIF colour table wide enough to contain it.
    let optimize = settings.optimize;
    let requested = settings.colors.clamp(2, 256);
    let map_colors = if optimize {
        (requested - 1).max(1) as usize
    } else {
        requested as usize
    };
    // The palette is learned single-threaded for reproducibility, but the cube
    // searches inside k-means — the bulk of this phase — are spread out.
    let mut palette = build_palette(
        &mut reader,
        frame_count,
        w,
        h,
        map_colors,
        threads,
        &settings,
        progress,
    )?;
    if palette.colors.is_empty() {
        palette.colors = vec![[0, 0, 0]];
    }
    progress.finish(PHASE_ANALYSING);

    // Pass 3: index frames in parallel and stream them into the GIF.
    progress.begin(PHASE_ENCODING, frame_count.min(u32::MAX as u64) as u32);
    // One colour map for the whole export. It costs 64^3 nearest-colour
    // searches, so rebuilding it per frame used to dwarf the mapping work
    // itself. It is read-only once built, so every worker shares it.
    let colour_map = ColorMap::build_cancellable(&palette, threads, settings.cancel.as_deref())?;
    let gif_palette = if optimize {
        palette.with_reserved_slot()
    } else {
        palette.clone()
    };
    let stats = encode_frames(
        &settings,
        raw.temp.path(),
        &palette,
        &gif_palette,
        &colour_map,
        w,
        h,
        frame_count,
        threads,
        progress,
    )?;
    if stats.written == 0 {
        return Err(Error::BadMedia("no frames were decoded".into()));
    }
    progress.finish(PHASE_DONE);

    let bytes = std::fs::metadata(&settings.output)
        .map(|m| m.len())
        .unwrap_or(0);

    let mut warning = if rate_limited {
        Some("sampling rate was reduced to preserve playback speed at the 2 cs delay limit".into())
    } else {
        None
    };
    if frame_count < frames {
        warning = Some(format!(
            "the video only supplied {frame_count} of the {frames} frames requested"
        ));
    }
    if bytes > 40 * 1024 * 1024 {
        warning = Some(match warning {
            Some(w) => {
                format!("{w}; the GIF is very large, try fewer colours or a lower frame rate")
            }
            None => "the GIF is very large, try fewer colours or a lower frame rate".to_string(),
        });
    }

    Ok(Outcome {
        output: settings.output.clone(),
        frames: stats.written,
        bytes,
        width: w,
        height: h,
        fps: stats.written as f64 / (stats.total_cs as f64 / 100.0),
        duration: stats.total_cs as f64 / 100.0,
        palette_colors: palette.colors.len() as u32,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warning,
    })
}

/// What the size estimator concluded.
#[derive(Debug, Clone, Copy)]
pub struct Estimate {
    /// Projected size of the finished GIF, in bytes.
    pub bytes: u64,
    /// Sampled output frames before optional deduplication.
    pub frames: u64,
    /// Frames that were really measured.
    pub sampled: u64,
    /// How many separate moments of the selection were read.
    pub segments: u64,
    pub width: u32,
    pub height: u32,
    /// True when every requested frame was measured, making the number exact.
    pub exact: bool,
}

impl Estimate {
    pub fn summary(&self) -> String {
        if self.exact {
            format!("about {} · {} frames", human_bytes(self.bytes), self.frames)
        } else {
            format!(
                "about {} · measured {} of {} frames",
                human_bytes(self.bytes),
                self.sampled,
                self.frames
            )
        }
    }
}

/// How many frames the estimator measures before extrapolating.
pub const ESTIMATE_SAMPLE_FRAMES: u64 = 16;

/// Frames decoded per sampled segment. Two adjacent frames are enough to show
/// whether the clip repeats itself, while short segments buy more distinct
/// moments for the same budget — and it is the number of moments, not the frames
/// per moment, that decides how well the sample describes a clip whose content
/// changes across it. (Longer runs would describe 「复用重复帧」 and
/// 「帧间透明优化」 a little better; both are approximations either way.)
pub const ESTIMATE_SEGMENT_FRAMES: u64 = 2;

/// The moments the estimator reads, as `(start time, frame count)` runs that
/// can be decoded back to back into one raw file.
///
/// The selection is sampled in several short segments spread evenly across it —
/// the first flush with the start, the last ending exactly on the last frame —
/// instead of in one contiguous window, which reads the whole clip from a single
/// moment and is wrong as soon as the content changes across the selection.
/// Random offsets were rejected: the number would jump on every settings tweak
/// and no two runs would agree.
///
/// Nothing is extrapolated while the selection is no longer than the sample:
/// then one segment covers all of it and the answer is exact.
fn estimate_segments(start: f64, end: f64, fps: f64, frames: u64) -> Vec<(f64, u64)> {
    let sample = frames.min(ESTIMATE_SAMPLE_FRAMES).max(1);
    if frames <= sample || frames <= ESTIMATE_SEGMENT_FRAMES {
        return vec![(start, frames.max(1))];
    }

    let per = ESTIMATE_SEGMENT_FRAMES.min(sample).max(1);
    let count = (sample / per).max(1);
    if count <= 1 {
        return vec![(start, sample)];
    }

    // Evenly spaced first frames, the last segment ending on the last frame.
    // `frames > count * per`, so the spacing is always wider than a segment and
    // the segments can never overlap.
    let last = frames - per;
    let step = last as f64 / (count - 1) as f64;
    let mut runs = Vec::with_capacity(count as usize);
    for i in 0..count {
        let index = ((i as f64 * step).round() as u64).min(last);
        let time = (start + index as f64 / fps).min(end);
        runs.push((time, per));
    }
    runs
}

/// A sink that only remembers how much was written to it.
#[derive(Default)]
struct CountingSink {
    bytes: u64,
}

impl std::io::Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Size of everything a GIF contains apart from its frames: the header, the
/// colour table and the loop block.
fn header_bytes(palette: &Palette, w: u32, h: u32, loop_forever: bool) -> Result<u64> {
    let writer = GifWriter::new(
        CountingSink::default(),
        w as u16,
        h as u16,
        Some(palette),
        loop_forever,
    )?;
    Ok(writer.finish()?.bytes)
}

/// Project the size of an export by running the real pipeline over a few short
/// segments of the selection and scaling the result up.
///
/// The segments are spread over the whole selection (see [`estimate_segments`]),
/// and each one is encoded by exactly the code [`export`] uses — same palette
/// search, same mapping, same writer — into a sink that only counts bytes.
/// Anything cheaper (a bytes-per-pixel heuristic) would be wrong by a factor
/// whenever dithering, the inter-frame diff, or the lossy pre-pass is on, which
/// is precisely when the number is worth having.
pub fn estimate(tools: &MediaTools, settings: &Settings, progress: &Progress) -> Result<Estimate> {
    validate_settings(settings)?;
    let info = probe_with_cancel(tools, &settings.input, settings.cancel.as_deref())?;
    let start = info.clamp_time(settings.start);
    let end = info.clamp_time(settings.end);
    if end <= start {
        return Err(Error::Invalid(
            "the selected range is empty after clamping to the video".into(),
        ));
    }

    let effective_fps = settings.fps.min(50.0 / settings.speed);
    let frames = planned_frames(start, end, effective_fps);
    let (w, h) = output_size(&info, settings.max_width);

    let mut settings = settings.clone();
    settings.start = start;
    settings.end = end;
    settings.fps = effective_fps;
    settings.video_stream = info.video_stream;
    settings.seek_offset = info.start_time - info.format_start_time;

    let plan = estimate_segments(start, end, effective_fps, frames);
    let planned_sample: u64 = plan.iter().map(|(_, n)| *n).sum();

    let threads = resolve_threads(settings.max_threads);
    let frame_bytes = w as usize * h as usize * 3;

    progress.begin(PHASE_DECODING, planned_sample.min(u32::MAX as u64) as u32);
    let raw = decode_segments(tools, &settings, &plan, w, h, progress)?;
    let measured_frames = raw.count;
    progress.finish(PHASE_DECODING);

    progress.begin(PHASE_ANALYSING, measured_frames.min(u32::MAX as u64) as u32);
    let mut reader = RawReader::open(raw.temp.path(), frame_bytes)?;
    let optimize = settings.optimize;
    let requested = settings.colors.clamp(2, 256);
    let map_colors = if optimize {
        (requested - 1).max(1) as usize
    } else {
        requested as usize
    };
    let mut palette = build_palette(
        &mut reader,
        measured_frames,
        w,
        h,
        map_colors,
        threads,
        &settings,
        progress,
    )?;
    if palette.colors.is_empty() {
        palette.colors = vec![[0, 0, 0]];
    }
    progress.finish(PHASE_ANALYSING);

    progress.begin(PHASE_ENCODING, measured_frames.min(u32::MAX as u64) as u32);
    let colour_map = ColorMap::build_cancellable(&palette, threads, settings.cancel.as_deref())?;
    let gif_palette = if optimize {
        palette.with_reserved_slot()
    } else {
        palette.clone()
    };
    let (sink, stats) = stream_frames(
        CountingSink::default(),
        &settings,
        raw.temp.path(),
        &palette,
        &gif_palette,
        &colour_map,
        w,
        h,
        measured_frames,
        threads,
        frame_bytes,
        &raw.segments,
        progress,
    )?;
    progress.finish(PHASE_DONE);

    let measured = sink.bytes;
    let exact = frames <= measured_frames;
    let bytes = if exact {
        measured
    } else {
        let header = header_bytes(&gif_palette, w, h, settings.loop_forever)?;
        let key = stats.key_bytes.div_ceil(stats.segments.max(1));
        if settings.dedupe && stats.uniform {
            // An all-static sample needs delay-split frames, not N copies of its header/body.
            header
                + key
                    * timeline_cs(&settings, frames)
                        .div_ceil(u16::MAX as u64)
                        .max(1)
        } else if settings.dedupe && stats.changes == 0 {
            // Segments repeat internally but show different images: do not call the whole clip static.
            header + key * frames.div_ceil(ESTIMATE_SEGMENT_FRAMES)
        } else {
            let transitions = stats.transitions.max(1);
            let delta = stats.delta_bytes as u128 * frames.saturating_sub(1) as u128;
            header
                + key
                + (delta.div_ceil(transitions as u128))
                    .min(u64::MAX as u128 - header as u128 - key as u128) as u64
        }
    };

    Ok(Estimate {
        bytes,
        frames,
        sampled: measured_frames,
        segments: plan.len() as u64,
        width: w,
        height: h,
        exact,
    })
}

/// Prove that a GIF can actually be written to `path`.
///
/// This exists so the front end can answer "why can't I save here?" *before*
/// spending a whole export on it. It checks the three things that really stop a
/// write on Windows: the folder cannot be created, the folder is not writable,
/// or the target file is open in another program / read-only.
pub fn check_output(path: &Path) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());

    if let Some(parent) = parent {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Output(format!(
                    "cannot create the folder {}: {e}",
                    parent.display()
                ))
            })?;
        } else if !parent.is_dir() {
            // A path component that exists but is a file can never be a folder.
            // Worth naming, because the probe below would otherwise report the
            // far more confusing "the folder ... is not writable".
            return Err(Error::Output(format!(
                "{} is a file, not a folder",
                parent.display()
            )));
        }
    }

    // Probe by really creating a file next to the target: that is exactly the
    // permission the export itself needs, so a folder that only *looks* fine
    // still gets caught here.
    let dir = parent.unwrap_or_else(|| Path::new("."));
    let probe = crate::ffmpeg::process::TempFile::in_dir(dir, "write")
        .map_err(|e| Error::Output(format!("the folder {} is not writable: {e}", dir.display())))?;
    drop(probe);

    if path.exists() {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.permissions().readonly() {
                return Err(Error::Output(format!("{} is read-only", path.display())));
            }
        }
        // Opening for write is how we find out whether another program is
        // holding the file. It is also the case users hit most often: they
        // looked at the finished GIF and exported over it again.
        if let Err(e) = std::fs::OpenOptions::new().write(true).open(path) {
            return Err(Error::Output(format!(
                "cannot replace {} (it may be open in another program): {e}",
                path.display()
            )));
        }
    }

    Ok(())
}

fn same_file(input: &Path, output: &Path) -> Result<bool> {
    if !output.exists() {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Info {
            attributes: u32,
            creation: [u32; 2],
            access: [u32; 2],
            write: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(h: *mut std::ffi::c_void, out: *mut Info) -> i32;
        }
        let id = |p: &Path| -> Result<(u32, u32, u32)> {
            let file = std::fs::File::open(p)?;
            let mut info = std::mem::MaybeUninit::<Info>::uninit();
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let info = unsafe { info.assume_init() };
            Ok((info.volume, info.index_high, info.index_low))
        };
        Ok(id(input)? == id(output)?)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::metadata(input)?;
        let b = std::fs::metadata(output)?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(std::fs::canonicalize(input)? == std::fs::canonicalize(output)?)
    }
}

/// Palette-map every decoded frame and stream the result into the GIF, through
/// a sibling temp file so a failure cannot leave a half-written `.gif` behind.
///
/// Frames are handled in bounded batches: each batch is mapped across `threads`
/// workers and written immediately, so peak memory is a few frames rather than
/// the whole clip. That matters because a long export at a large canvas would
/// otherwise hold hundreds of megabytes of indices at once.
fn encode_frames(
    settings: &Settings,
    raw_path: &Path,
    palette: &Palette,
    gif_palette: &Palette,
    colour_map: &ColorMap,
    w: u32,
    h: u32,
    frame_count: u64,
    threads: u32,
    progress: &Progress,
) -> Result<EncodingStats> {
    let out_path = &settings.output;
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Output(format!(
                    "cannot create the folder {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    if out_path.exists() && !settings.overwrite {
        return Err(Error::Invalid(format!(
            "{} already exists",
            out_path.display()
        )));
    }

    let frame_bytes = w as usize * h as usize * 3;
    let part_guard = crate::ffmpeg::process::TempFile::in_dir(
        out_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
        "stage",
    )
    .map_err(|e| Error::Output(format!("cannot create export staging file: {e}")))?;
    let part = part_guard.path();

    let frames_written;
    {
        let file = std::fs::File::create(&part)
            .map_err(|e| Error::Output(format!("cannot create {}: {e}", part.display())))?;
        let sink = BufWriter::with_capacity(1 << 20, file);
        match stream_frames(
            sink,
            settings,
            raw_path,
            palette,
            gif_palette,
            colour_map,
            w,
            h,
            frame_count,
            threads,
            frame_bytes,
            &[frame_count],
            progress,
        ) {
            Ok((mut sink, written)) => {
                use std::io::Write;
                if let Err(e) = sink.flush() {
                    drop(sink);
                    let _ = std::fs::remove_file(&part);
                    return Err(e.into());
                }
                frames_written = written;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        }
    }

    // `fs::rename` replaces an existing destination on Windows: it goes through
    // MoveFileEx with MOVEFILE_REPLACE_EXISTING. So there is deliberately no
    // remove_file first. Deleting the target separately was both unnecessary and
    // worse — when the delete failed it returned early and orphaned the staged
    // `.gif.part`, and it destroyed the previous GIF before the new one was
    // known to be in place.
    check_cancel(settings)?;
    let commit = commit_output(part, out_path, settings.overwrite);
    commit.map_err(|e| {
        let _ = std::fs::remove_file(&part);
        Error::Output(format!(
            "cannot move the GIF to {}: {e} (it may be open in another program)",
            out_path.display()
        ))
    })?;
    Ok(frames_written)
}

/// Commit with an atomic no-replace operation when overwrite is disabled.
fn commit_output(part: &Path, output: &Path, overwrite: bool) -> std::io::Result<()> {
    if overwrite {
        return std::fs::rename(part, output);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
        }
        let wide = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let (a, b) = (wide(part), wide(output));
        if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::hard_link(part, output)
    }
}

#[derive(Default)]
struct EncodingStats {
    written: u64,
    total_cs: u64,
    key_bytes: u64,
    delta_bytes: u64,
    segments: u64,
    transitions: u64,
    changes: u64,
    uniform: bool,
}

fn flush_pending<W: std::io::Write>(
    writer: &mut GifWriter<W>,
    pending: &mut Option<Vec<u8>>,
    pending_delay: &mut u32,
    written: &mut u64,
) -> Result<()> {
    if let Some(mut block) = pending.take() {
        set_block_delay(&mut block, *pending_delay as u16);
        writer.add_block(&block)?;
        *written += 1;
        *pending_delay = 0;
    }
    Ok(())
}

/// Index in parallel, update the retained canvas in order, then compress in parallel.
#[allow(clippy::too_many_arguments)]
fn stream_frames<W: std::io::Write>(
    sink: W,
    settings: &Settings,
    raw_path: &Path,
    palette: &Palette,
    gif_palette: &Palette,
    colour_map: &ColorMap,
    w: u32,
    h: u32,
    frame_count: u64,
    threads: u32,
    frame_bytes: usize,
    segments: &[u64],
    progress: &Progress,
) -> Result<(W, EncodingStats)> {
    check_cancel(settings)?;
    let opts = FrameOptions::new(
        settings,
        settings.optimize.then(|| gif_palette.reserved_index()),
    );
    let mut writer = GifWriter::new(
        sink,
        w as u16,
        h as u16,
        Some(gif_palette),
        settings.loop_forever,
    )?;
    let mut reader = RawReader::open(raw_path, frame_bytes)?;
    let batch = (threads as usize * 2).clamp(1, ((24 << 20) / (frame_bytes * 3).max(1)).max(1));
    let mut stats = EncodingStats {
        uniform: true,
        ..Default::default()
    };
    let mut reference: Option<Vec<u8>> = None;
    let mut pending = None;
    let mut pending_delay = 0u32;
    let mut canvas = Vec::new();
    let mut previous = Vec::new();
    let mut read = 0u64;
    let mut segment = 0usize;
    let mut segment_end = segments.first().copied().unwrap_or(frame_count);
    let total_cs = timeline_cs(settings, frame_count);
    while read < frame_count {
        check_cancel(settings)?;
        if read == segment_end {
            flush_pending(
                &mut writer,
                &mut pending,
                &mut pending_delay,
                &mut stats.written,
            )?;
            canvas.clear();
            previous.clear();
            segment += 1;
            segment_end += segments.get(segment).copied().unwrap_or(frame_count - read);
        }
        let count = ((segment_end - read).min(frame_count - read) as usize).min(batch);
        if count == 0 {
            return Err(Error::BadMedia("empty decoded segment".into()));
        }
        let mut pixels = Vec::with_capacity(count);
        for _ in 0..count {
            let mut px = Vec::new();
            if !reader.next(&mut px)? {
                return Err(Error::BadMedia("raw frame count changed".into()));
            }
            pixels.push(px);
        }
        let mut mapped = vec![Vec::new(); count];
        parallel_each(&pixels, &mut mapped, threads, |px, out| {
            check_cancel(settings)?;
            *out = map_pixels(colour_map, palette, px, w as usize, h as usize, &opts);
            Ok(())
        })?;
        drop(pixels);
        let mut keys = Vec::with_capacity(count);
        let mut equal = Vec::with_capacity(count);
        for indices in &mut mapped {
            check_cancel(settings)?;
            let key = previous.is_empty();
            if let Some(reference) = &reference {
                stats.uniform &= reference == indices;
            } else {
                reference = Some(indices.clone());
            }
            let same = if let Some(index) = opts.transparent {
                apply_transparency(indices, &mut canvas, palette, opts.tolerance, index)
            } else {
                !key && *indices == previous
            };
            if opts.transparent.is_none() {
                previous.clone_from(indices);
            } else if key {
                previous.push(0);
            } // only the first-frame marker is needed
            keys.push(key);
            equal.push(same);
        }
        let mut blocks = vec![Vec::new(); count];
        parallel_each(&mapped, &mut blocks, threads, |indices, out| {
            check_cancel(settings)?;
            *out = frame_block(&FrameSpec {
                width: w as u16,
                height: h as u16,
                indices,
                palette: gif_palette,
                local_table: false,
                delay_cs: 0,
                disposal: Disposal::DoNotDispose,
                transparent: opts.transparent,
            })?;
            Ok(())
        })?;
        for (i, block) in blocks.into_iter().enumerate() {
            check_cancel(settings)?;
            let delay = delay_at(total_cs, frame_count, read) as u32;
            stats.total_cs += delay as u64;
            if keys[i] {
                stats.key_bytes += block.len() as u64;
                stats.segments += 1;
            } else {
                stats.transitions += 1;
                if !equal[i] {
                    stats.changes += 1;
                }
                if !opts.dedupe || !equal[i] {
                    stats.delta_bytes += block.len() as u64;
                }
            }
            if opts.dedupe
                && equal[i]
                && pending.is_some()
                && pending_delay + delay <= u16::MAX as u32
            {
                pending_delay += delay;
            } else {
                flush_pending(
                    &mut writer,
                    &mut pending,
                    &mut pending_delay,
                    &mut stats.written,
                )?;
                pending = Some(block);
                pending_delay = delay;
            }
            read += 1;
            progress.step();
        }
    }
    check_cancel(settings)?;
    flush_pending(
        &mut writer,
        &mut pending,
        &mut pending_delay,
        &mut stats.written,
    )?;
    Ok((writer.finish()?, stats))
}

fn parallel_each<F>(input: &[Vec<u8>], output: &mut [Vec<u8>], threads: u32, f: F) -> Result<()>
where
    F: Fn(&[u8], &mut Vec<u8>) -> Result<()> + Sync,
{
    let workers = (threads as usize).max(1).min(input.len().max(1));
    if workers == 1 {
        for (src, dst) in input.iter().zip(output) {
            f(src, dst)?;
        }
        return Ok(());
    }
    let chunk = input.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (src, dst) in input.chunks(chunk).zip(output.chunks_mut(chunk)) {
            let f = &f;
            handles.push(scope.spawn(move || -> Result<()> {
                for (src, dst) in src.iter().zip(dst) {
                    f(src, dst)?;
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| Error::Encode("encoding worker panicked".into()))??;
        }
        Ok(())
    })
}

fn map_pixels(
    colour_map: &ColorMap,
    palette: &Palette,
    px: &[u8],
    w: usize,
    h: usize,
    opts: &FrameOptions,
) -> Vec<u8> {
    let processed;
    let source = if opts.posterize_levels >= 2 {
        let mut copy = px.to_vec();
        posterize_in_place(&mut copy, opts.posterize_levels);
        processed = copy;
        processed.as_slice()
    } else {
        px
    };
    let mut indices = Vec::with_capacity(w * h);
    match opts.dither {
        Dither::None => map_frame_with(colour_map, source, &mut indices),
        Dither::Ordered => {
            map_frame_ordered_with(colour_map, source, w, opts.strength, &mut indices)
        }
        Dither::Floyd => map_frame_dithered_with(
            colour_map,
            palette,
            &source,
            w,
            h,
            opts.strength,
            &mut indices,
        ),
    }
    indices
}

/// Sample frames spread over the selection and quantise the union of them.
#[allow(clippy::too_many_arguments)]
fn build_palette(
    reader: &mut RawReader,
    frame_count: u64,
    w: u32,
    h: u32,
    max_colors: usize,
    threads: u32,
    settings: &Settings,
    progress: &Progress,
) -> Result<Palette> {
    const MAX_SAMPLES: usize = 40;
    const SAMPLE_PIXEL_BUDGET: usize = 1_500_000;

    use std::io::{Seek, SeekFrom};
    let pixels_per_frame = w as usize * h as usize;
    let target_frames = frame_count.min(MAX_SAMPLES as u64).max(1) as usize;
    progress.begin(PHASE_ANALYSING, (target_frames + 1) as u32);
    let pixel_stride = (pixels_per_frame * target_frames)
        .div_ceil(SAMPLE_PIXEL_BUDGET)
        .max(1);
    let mut sample = Vec::with_capacity(SAMPLE_PIXEL_BUDGET * 3);
    let mut buf = Vec::new();
    for index in sample_indices(frame_count, target_frames) {
        check_cancel(settings)?;
        let offset = index
            .checked_mul(reader.cap as u64)
            .ok_or_else(|| Error::Invalid("raw offset overflow".into()))?;
        reader.file.seek(SeekFrom::Start(offset))?;
        if !reader.next(&mut buf)? {
            return Err(Error::BadMedia("missing palette sample".into()));
        }
        posterize_in_place(&mut buf, posterize_levels(settings.lossy));
        for px in buf.chunks_exact(3).step_by(pixel_stride) {
            sample.extend_from_slice(px);
        }
        progress.step();
    }
    if sample.len() < 3 {
        return Err(Error::BadMedia("no palette samples".into()));
    }
    let palette = quantize_cancellable(
        &sample,
        max_colors,
        threads,
        quantize_iterations(settings.quality),
        settings.cancel.as_deref(),
    )?;
    progress.step();
    Ok(palette)
}

fn sample_indices(frames: u64, count: usize) -> Vec<u64> {
    (0..count)
        .map(|i| {
            if count <= 1 {
                0
            } else {
                ((i as u128 * (frames - 1) as u128 + (count - 1) as u128 / 2) / (count - 1) as u128)
                    as u64
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(w: u32, h: u32) -> MediaInfo {
        MediaInfo {
            width: w,
            height: h,
            fps: 30.0,
            fps_display: 30.0,
            duration: 10.0,
            ..Default::default()
        }
    }

    #[test]
    fn output_size_preserves_aspect_and_parity() {
        let (w, h) = output_size(&media(1920, 1080), 480);
        assert_eq!(w, 480);
        assert_eq!(h, 270);
        let (w2, h2) = output_size(&media(640, 480), 480);
        assert_eq!((w2, h2), (480, 360));
        // Never upscale.
        let (w3, h3) = output_size(&media(320, 240), 480);
        assert_eq!((w3, h3), (320, 240));

        for (sw, sh) in [(100, 33), (33, 100), (1919, 1079), (7, 3)] {
            let (w, h) = output_size(&media(sw, sh), 480);
            assert_eq!(w % 2, 0, "{sw}x{sh} -> {w}x{h}");
            assert_eq!(h % 2, 0, "{sw}x{sh} -> {w}x{h}");
            assert!(w >= 1 && h >= 1);
        }
    }

    #[test]
    fn planned_frame_counts() {
        assert_eq!(planned_frames(0.0, 5.0, 12.0), 60);
        assert_eq!(planned_frames(1.0, 1.0, 12.0), 1);
        assert_eq!(planned_frames(0.0, 5.0, 0.0), 1);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
    }

    #[test]
    fn progress_reports_fractions() {
        let p = Progress::new();
        assert_eq!(p.fraction(), 0.0);
        p.begin(PHASE_ENCODING, 4);
        p.step();
        assert!((p.fraction() - 0.7).abs() < 1e-9);
        p.finish(PHASE_DONE);
        assert_eq!(p.fraction(), 1.0);
        assert_eq!(p.phase_name(), "Done");
    }

    #[test]
    fn thread_resolution_follows_the_machine() {
        let cores = cpu_count();
        assert!(cores >= 1);

        // 0 means "decide for me", and the answer is half the cores rounded
        // down, but never zero.
        assert_eq!(default_threads(), (cores / 2).max(1));
        assert_eq!(resolve_threads(0), default_threads());

        // An explicit request is honoured, and something silly is clamped to
        // the machine rather than spawning a thousand threads.
        assert_eq!(resolve_threads(1), 1);
        assert_eq!(resolve_threads(cores), cores);
        assert_eq!(resolve_threads(u32::MAX), cores);
    }

    /// The sample has to describe the whole selection, not one moment of it:
    /// otherwise a clip that starts slow and ends in a flurry is estimated from
    /// whichever half the window happened to land in.
    #[test]
    fn the_estimate_sample_spreads_over_the_whole_selection() {
        // 12 fps over 5 s is 60 frames, sampled as eight runs of two.
        let runs = estimate_segments(0.0, 5.0, 12.0, 60);
        assert_eq!(runs.len(), 8);
        assert!(runs.iter().all(|(_, n)| *n == ESTIMATE_SEGMENT_FRAMES));

        let indices: Vec<u64> = runs
            .iter()
            .map(|(t, _)| (t * 12.0).round() as u64)
            .collect();
        // First run flush with the start, last run ending on the last frame.
        assert_eq!(indices, vec![0, 8, 17, 25, 33, 41, 50, 58]);
        assert_eq!(
            indices.last().copied().unwrap() + ESTIMATE_SEGMENT_FRAMES,
            60
        );
    }

    #[test]
    fn a_selection_shorter_than_the_sample_is_measured_exactly() {
        assert_eq!(estimate_segments(1.5, 2.5, 12.0, 8), vec![(1.5, 8)]);
        assert_eq!(estimate_segments(0.0, 0.1, 12.0, 1), vec![(0.0, 1)]);
        assert_eq!(estimate_segments(0.0, 1.0, 12.0, 16), vec![(0.0, 16)]);
    }

    #[test]
    fn estimate_segments_cover_the_sample_without_overlapping() {
        for frames in (ESTIMATE_SAMPLE_FRAMES + 1)..400u64 {
            let runs = estimate_segments(0.0, frames as f64 / 12.0, 12.0, frames);
            let total: u64 = runs.iter().map(|(_, n)| *n).sum();
            assert_eq!(total, ESTIMATE_SAMPLE_FRAMES, "frames={frames}");

            let indices: Vec<u64> = runs
                .iter()
                .map(|(t, _)| (t * 12.0).round() as u64)
                .collect();
            for (i, pair) in indices.windows(2).enumerate() {
                assert!(
                    pair[1] >= pair[0] + ESTIMATE_SEGMENT_FRAMES,
                    "frames={frames} segment {i} overlaps: {indices:?}"
                );
            }
            // No run may read past the end of the selection.
            let (last_start, last_count) = *runs.last().unwrap();
            assert!(
                last_start * 12.0 + last_count as f64 <= frames as f64 + 0.5,
                "frames={frames} runs past the end"
            );
        }
    }

    /// The parallel path must be a pure speed-up: same frames, same order, same
    /// bytes. Comparing whole compressed blocks checks the mapping, the
    /// dithering and the LZW pass together.
    #[test]
    fn parallel_encoding_matches_serial_for_every_dither() -> Result<()> {
        let w = 37usize;
        let h = 29usize;
        let palette = Palette {
            colors: vec![[0, 0, 0], [255, 255, 255], [200, 30, 40], [20, 190, 90]],
        };
        let cm = ColorMap::build(&palette);

        // Seven frames: few enough to be quick, not a multiple of any thread
        // count we try, so a wrong split cannot line up by luck.
        let frames: Vec<Vec<u8>> = (0..7u32)
            .map(|k| {
                (0..w * h * 3)
                    .map(|i| ((i * 31 + k as usize * 97) % 256) as u8)
                    .collect()
            })
            .collect();

        for dither in [Dither::None, Dither::Ordered, Dither::Floyd] {
            // Plain settings: no posterising, no transparency, no dedupe, so the
            // batch is a pure mapping/LZW pass at the historic dither strength.
            let opts = FrameOptions {
                dither,
                strength: if dither == Dither::Ordered { 0.6 } else { 1.0 },
                posterize_levels: 1,
                transparent: None,
                tolerance: 0,
                dedupe: false,
            };
            let mut serial: Vec<Vec<u8>> = vec![Vec::new(); frames.len()];
            parallel_each(&frames, &mut serial, 1, |px, out| {
                *out = map_pixels(&cm, &palette, px, w, h, &opts);
                Ok(())
            })?;

            for threads in [2u32, 3, 4, 8, 16] {
                let mut got: Vec<Vec<u8>> = vec![Vec::new(); frames.len()];
                parallel_each(&frames, &mut got, threads, |px, out| {
                    *out = map_pixels(&cm, &palette, px, w, h, &opts);
                    Ok(())
                })?;
                assert_eq!(
                    got, serial,
                    "{dither:?} with {threads} threads diverged from the serial result"
                );
            }

            for b in &serial {
                assert!(!b.is_empty(), "an empty frame block was produced");
            }
        }
        Ok(())
    }

    /// The cube build is integer arithmetic, so splitting it across workers must
    /// not change a single entry — that is what keeps the palette, and
    /// therefore the whole file, reproducible at any thread count.
    #[test]
    fn the_colour_map_is_identical_at_every_thread_count() {
        let palette = Palette {
            colors: vec![
                [0, 0, 0],
                [255, 255, 255],
                [200, 30, 40],
                [20, 190, 90],
                [17, 17, 17],
                [240, 200, 10],
            ],
        };
        let serial = ColorMap::build(&palette);
        for threads in [1u32, 2, 3, 5, 8, 16, 64] {
            let threaded = ColorMap::build_with_threads(&palette, threads);
            // `index` is the only public way in, so probe every 8-bit triple
            // stepped coarsely; a diverged cell would show up here.
            for r in (0..=255u16).step_by(7) {
                for g in (0..=255u16).step_by(11) {
                    for b in (0..=255u16).step_by(13) {
                        let (r, g, b) = (r as u8, g as u8, b as u8);
                        assert_eq!(
                            serial.index(r, g, b),
                            threaded.index(r, g, b),
                            "cell for ({r},{g},{b}) differs with {threads} threads"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn palette_sampling_covers_the_tail_and_respects_preprocessing() -> Result<()> {
        let temp = crate::ffmpeg::process::TempFile::new("palette-regression")?;
        // Only the last of 79 frames is red. Old stride=1 stopped at frame 39.
        let mut raw = vec![0u8; 79 * 2 * 2 * 3];
        for px in raw[78 * 12..].chunks_exact_mut(3) {
            px.copy_from_slice(&[255, 0, 0]);
        }
        std::fs::write(temp.path(), &raw)?;
        let mut reader = RawReader::open(temp.path(), 12)?;
        let p = build_palette(
            &mut reader,
            79,
            2,
            2,
            8,
            1,
            &Settings::default(),
            &Progress::new(),
        )?;
        assert!(p.colors.contains(&[255, 0, 0]));
        assert_eq!(sample_indices(79, 40).last(), Some(&78));
        let mut reader = RawReader::open(temp.path(), 12)?;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(matches!(
            build_palette(
                &mut reader,
                79,
                2,
                2,
                8,
                1,
                &Settings {
                    cancel: Some(cancel),
                    ..Settings::default()
                },
                &Progress::new()
            ),
            Err(Error::Cancelled)
        ));
        Ok(())
    }

    #[test]
    fn dimensions_stay_bounded_without_upscaling_extreme_aspects() {
        for (w, h) in [
            (100000, 2),
            (2, 100000),
            (1, 1),
            (u32::MAX, 1),
            (1, u32::MAX),
        ] {
            for cap in [2, 17, 480, 4096] {
                let (ow, oh) = output_size(&media(w, h), cap);
                assert!((1..=cap).contains(&ow));
                assert!((1..=4096).contains(&oh));
            }
        }
    }

    #[test]
    fn delay_rounding_distributes_error_instead_of_accumulating() {
        for n in [1, 3, 30, 50, 1000] {
            let total = 100u64.max(2 * n);
            assert_eq!(
                (0..n).map(|i| delay_at(total, n, i) as u64).sum::<u64>(),
                total
            );
        }
    }

    #[test]
    fn progress_does_not_drop_between_phases_or_after_dedupe() {
        let p = Progress::new();
        let mut previous = 0.0;
        for phase in [PHASE_DECODING, PHASE_ANALYSING, PHASE_ENCODING] {
            p.begin(phase, 10);
            for _ in 0..10 {
                p.step();
                let now = p.fraction();
                assert!(now >= previous);
                previous = now;
            }
        }
    }

    #[test]
    fn final_no_replace_commit_refuses_a_target_created_after_preflight() -> Result<()> {
        let stage = crate::ffmpeg::process::TempFile::new("stage-race")?;
        let target = crate::ffmpeg::process::TempFile::new("target-race")?;
        std::fs::write(stage.path(), b"new")?;
        std::fs::write(target.path(), b"created by another writer")?;
        assert!(commit_output(stage.path(), target.path(), false).is_err());
        assert_eq!(std::fs::read(target.path())?, b"created by another writer");
        Ok(())
    }

    #[test]
    fn dedupe_splits_long_delays_without_losing_playback_time() -> Result<()> {
        let temp = crate::ffmpeg::process::TempFile::new("delay-split")?;
        std::fs::write(temp.path(), vec![0u8; 700 * 12])?;
        let palette = Palette {
            colors: vec![[0, 0, 0]],
        };
        let map = ColorMap::build(&palette);
        let settings = Settings {
            end: 7000.0,
            fps: 0.1,
            dedupe: true,
            ..Settings::default()
        };
        let (_, stats) = stream_frames(
            CountingSink::default(),
            &settings,
            temp.path(),
            &palette,
            &palette,
            &map,
            2,
            2,
            700,
            1,
            12,
            &[700],
            &Progress::new(),
        )?;
        assert_eq!(stats.total_cs, 700000);
        assert_eq!(stats.written, 11);
        Ok(())
    }
}
