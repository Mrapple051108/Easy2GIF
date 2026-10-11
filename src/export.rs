//! The export pipeline: decode a time range, quantise, and write a GIF.
//!
//! Progress is published through [`Progress`], an atomic meter that the C ABI
//! polls from the UI thread. Keeping it poll-based (rather than a callback into
//! C++) means the core needs no FFI knowledge of the front end.

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::{Error, Result};
use crate::ffmpeg::process::{fmt_time, run_ffmpeg_stream, MediaTools};
use crate::ffmpeg::probe::{probe, MediaInfo};
use crate::gif::encode::{frame_block, set_block_delay, Disposal, FrameSpec, GifWriter};
use crate::gif::quantize::{
    map_frame_dithered_with, map_frame_with, posterize_in_place, posterize_levels,
    quantize_iterations, quantize_with_options, ColorMap, Palette,
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
            _ if total == 0 => 0.0,
            _ => (self.done() as f64 / total as f64).clamp(0.0, 1.0),
        }
    }

    /// Begin a phase with `total` work items.
    pub fn begin(&self, code: u32, total: u32) {
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
    // ---- advanced settings (all of them default to the old behaviour) ----
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

/// Quality used when the caller has no opinion; it reproduces the historical
/// three k-means iterations, so untouched exports stay byte-identical.
pub const DEFAULT_QUALITY: u32 = 5;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            input: PathBuf::new(),
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
    Error::Invalid("export cancelled".into())
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
        // Ordered dithering has always run at a lower amplitude than
        // Floyd–Steinberg; keep that as the 100% reference point so a full
        // slider reproduces the previous output exactly.
        let base = if settings.dither == Dither::Ordered {
            0.6
        } else {
            1.0
        };
        FrameOptions {
            dither: settings.dither,
            strength: base * (settings.dither_strength.min(100) as f64 / 100.0),
            posterize_levels: posterize_levels(settings.lossy),
            transparent,
            tolerance: settings.tolerance.min(64),
            dedupe: settings.dedupe,
        }
    }
}

/// Overwrite the indices of every pixel that has barely changed since `prev`
/// with the transparent index, so the decoder reuses the previous frame there.
fn apply_transparency(
    indices: &mut [u8],
    px: &[u8],
    prev: &[u8],
    tolerance: u32,
    transparent: u8,
) {
    // Three channels, so the per-channel slack is one third of the budget.
    let slack = tolerance.saturating_mul(3);
    let usable = (px.len().min(prev.len()) / 3).min(indices.len());
    for i in 0..usable {
        let o = i * 3;
        let delta = (px[o] as i32 - prev[o] as i32).unsigned_abs()
            + (px[o + 1] as i32 - prev[o + 1] as i32).unsigned_abs()
            + (px[o + 2] as i32 - prev[o + 2] as i32).unsigned_abs();
        if delta <= slack {
            indices[i] = transparent;
        }
    }
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
    let max_width = max_width.clamp(16, 4096);
    let aspect = info.aspect();
    let mut w = info.width.min(max_width);
    let mut h = (w as f64 / aspect).round() as u32;
    // Guard against degenerate results on extreme aspect ratios.
    if h < 2 {
        h = 2;
        w = ((h as f64) * aspect).round() as u32;
    }
    if w % 2 != 0 {
        w += 1;
    }
    if h % 2 != 0 {
        h += 1;
    }
    (w.max(2), h.max(2))
}

/// How many frames the request will produce.
pub fn planned_frames(start: f64, end: f64, fps: f64) -> u64 {
    let dur = (end - start).max(0.0);
    if fps <= 0.0 || dur <= 0.0 {
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
        "fps={},trim=end_frame={},scale={}:{}:flags=lanczos",
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
        fmt_time(start).into(),
        "-i".into(),
        settings.input.as_os_str().to_os_string(),
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

    for (start, frames) in runs {
        let io_result = decode_run_into(
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
        );
        if let Err(e) = io_result {
            // A cancelled or partially broken decode may still have produced
            // usable frames, so only fail hard when we got nothing at all.
            if written == 0 {
                return Err(e);
            }
        }
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
    run_ffmpeg_stream(&tools.ffmpeg, &args, cap, |chunk| {
        pending.extend_from_slice(chunk);
        // Write out only whole frames so the file stays frame-aligned.
        while pending.len() >= cap {
            out.write_all(&pending[..cap])?;
            pending.drain(..cap);
            *written += 1;
            progress.step();
            if *written % 4 == 0 && settings.cancelled() {
                return Err(cancelled_error());
            }
        }
        Ok(())
    })
    .map(|_| ())
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

/// Run an export. Never panics; all failures come back as [`Error`].
pub fn export(
    tools: &MediaTools,
    settings: &Settings,
    progress: &Progress,
) -> Result<Outcome> {
    let started = std::time::Instant::now();

    if settings.output.as_os_str().is_empty() {
        return Err(Error::Invalid("no output file was chosen".into()));
    }
    if settings.fps <= 0.0 || settings.fps > 120.0 {
        return Err(Error::Invalid(format!(
            "frame rate {} is outside the supported range 0.1 - 120",
            settings.fps
        )));
    }
    if settings.end <= settings.start {
        return Err(Error::Invalid(
            "the end of the selection must come after the start".into(),
        ));
    }

    let info = probe(tools, &settings.input)?;
    let start = info.clamp_time(settings.start);
    let end = info.clamp_time(settings.end);
    if end <= start {
        return Err(Error::Invalid(
            "the selected range is empty after clamping to the video".into(),
        ));
    }

    // Sample at the requested rate regardless of the speed multiplier, and let
    // the speed ride entirely on the per-frame delay. Folding speed into the
    // sampling rate instead would keep the playback duration unchanged while
    // dropping frames, i.e. a choppier clip rather than a faster one.
    let speed = settings.speed.clamp(0.1, 10.0);
    let effective_fps = settings.fps.clamp(0.1, 120.0);
    // A GIF delay is stored in centiseconds with a floor of 2, so 50 fps is the
    // fastest rate the container can actually express.
    let play_fps = (effective_fps * speed).clamp(0.1, 50.0);
    let frames = planned_frames(start, end, effective_fps);
    let (w, h) = output_size(&info, settings.max_width);

    let mut settings = settings.clone();
    settings.start = start;
    settings.end = end;
    settings.fps = effective_fps;

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
        (requested - 1).max(2) as usize
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
        settings.quality,
        progress,
    )?;
    if palette.colors.is_empty() {
        palette.colors = vec![[0, 0, 0]];
    }
    progress.finish(PHASE_ANALYSING);

    // Pass 3: index frames in parallel and stream them into the GIF.
    progress.begin(PHASE_ENCODING, frame_count.min(u32::MAX as u64) as u32);
    let delay_cs = ((100.0 / play_fps).round() as u32).clamp(2, 1000) as u16;
    // One colour map for the whole export. It costs 64^3 nearest-colour
    // searches, so rebuilding it per frame used to dwarf the mapping work
    // itself. It is read-only once built, so every worker shares it.
    let colour_map = ColorMap::build_with_threads(&palette, threads);
    let gif_palette = if optimize {
        palette.with_reserved_slot()
    } else {
        palette.clone()
    };
    let frames_written = encode_frames(
        &settings,
        raw.temp.path(),
        &palette,
        &gif_palette,
        &colour_map,
        w,
        h,
        delay_cs,
        frame_count,
        threads,
        progress,
    )?;
    if frames_written == 0 {
        return Err(Error::BadMedia("no frames were decoded".into()));
    }
    progress.finish(PHASE_DONE);

    let bytes = std::fs::metadata(&settings.output)
        .map(|m| m.len())
        .unwrap_or(0);

    let mut warning = None;
    if frame_count < frames {
        warning = Some(format!(
            "the video only supplied {frame_count} of the {frames} frames requested"
        ));
    }
    if bytes > 40 * 1024 * 1024 {
        warning = Some(match warning {
            Some(w) => format!("{w}; the GIF is very large, try fewer colours or a lower frame rate"),
            None => "the GIF is very large, try fewer colours or a lower frame rate".to_string(),
        });
    }

    Ok(Outcome {
        output: settings.output.clone(),
        frames: frames_written,
        bytes,
        width: w,
        height: h,
        fps: play_fps,
        duration: end - start,
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
    /// Frames the finished GIF would contain.
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
    if settings.fps <= 0.0 || settings.fps > 120.0 {
        return Err(Error::Invalid(format!(
            "frame rate {} is outside the supported range 0.1 - 120",
            settings.fps
        )));
    }
    if settings.end <= settings.start {
        return Err(Error::Invalid(
            "the end of the selection must come after the start".into(),
        ));
    }

    let info = probe(tools, &settings.input)?;
    let start = info.clamp_time(settings.start);
    let end = info.clamp_time(settings.end);
    if end <= start {
        return Err(Error::Invalid(
            "the selected range is empty after clamping to the video".into(),
        ));
    }

    let speed = settings.speed.clamp(0.1, 10.0);
    let effective_fps = settings.fps.clamp(0.1, 120.0);
    let play_fps = (effective_fps * speed).clamp(0.1, 50.0);
    let frames = planned_frames(start, end, effective_fps);
    let (w, h) = output_size(&info, settings.max_width);

    let mut settings = settings.clone();
    settings.start = start;
    settings.end = end;
    settings.fps = effective_fps;

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
        (requested - 1).max(2) as usize
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
        settings.quality,
        progress,
    )?;
    if palette.colors.is_empty() {
        palette.colors = vec![[0, 0, 0]];
    }
    progress.finish(PHASE_ANALYSING);

    progress.begin(PHASE_ENCODING, measured_frames.min(u32::MAX as u64) as u32);
    let delay_cs = ((100.0 / play_fps).round() as u32).clamp(2, 1000) as u16;
    let colour_map = ColorMap::build_with_threads(&palette, threads);
    let gif_palette = if optimize {
        palette.with_reserved_slot()
    } else {
        palette.clone()
    };
    let (sink, _) = stream_frames(
        CountingSink::default(),
        &settings,
        raw.temp.path(),
        &palette,
        &gif_palette,
        &colour_map,
        w,
        h,
        delay_cs,
        measured_frames,
        threads,
        frame_bytes,
        progress,
    )?;
    progress.finish(PHASE_DONE);

    let measured = sink.bytes;
    let exact = frames <= measured_frames;
    let bytes = if exact {
        measured
    } else {
        // Only the per-frame bytes scale; the header is a fixed cost.
        let header = header_bytes(&gif_palette, w, h, settings.loop_forever)?;
        let body = measured.saturating_sub(header);
        header + (body * frames).div_ceil(measured_frames.max(1))
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
                Error::Output(format!("cannot create the folder {}: {e}", parent.display()))
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
    let probe = dir.join(format!(".gifcore-write-{}.tmp", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) => {
            return Err(Error::Output(format!(
                "the folder {} is not writable: {e}",
                dir.display()
            )));
        }
    }

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
    delay_cs: u16,
    frame_count: u64,
    threads: u32,
    progress: &Progress,
) -> Result<u64> {
    let out_path = &settings.output;
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Output(format!("cannot create the folder {}: {e}", parent.display()))
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
    let part = out_path.with_extension("gif.part");

    let frames_written;
    {
        let file = std::fs::File::create(&part).map_err(|e| {
            Error::Output(format!("cannot create {}: {e}", part.display()))
        })?;
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
            delay_cs,
            frame_count,
            threads,
            frame_bytes,
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
    std::fs::rename(&part, out_path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        Error::Output(format!(
            "cannot move the GIF to {}: {e} (it may be open in another program)",
            out_path.display()
        ))
    })?;
    Ok(frames_written)
}

/// Hand the block being held back to the writer, patching in the delay it has
/// accumulated while identical frames were folded into it.
fn flush_pending<W: std::io::Write>(
    writer: &mut GifWriter<W>,
    pending: &mut Option<Vec<u8>>,
    pending_delay: &mut u32,
    written: &mut u64,
    progress: &Progress,
) -> Result<()> {
    if let Some(mut block) = pending.take() {
        let delay = (*pending_delay).clamp(1, u16::MAX as u32) as u16;
        set_block_delay(&mut block, delay);
        writer.add_block(&block)?;
        *written += 1;
        *pending_delay = 0;
        progress.step();
    }
    Ok(())
}

/// Read, palette-map and write frames in bounded batches.
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
    delay_cs: u16,
    frame_count: u64,
    threads: u32,
    frame_bytes: usize,
    progress: &Progress,
) -> Result<(W, u64)> {
    let opts = FrameOptions::new(
        settings,
        if settings.optimize {
            Some(gif_palette.reserved_index())
        } else {
            None
        },
    );
    let mut writer = GifWriter::new(
        sink,
        w as u16,
        h as u16,
        Some(gif_palette),
        settings.loop_forever,
    )?;
    let mut reader = RawReader::open(raw_path, frame_bytes)?;

    // Cap a batch by bytes rather than by frame count: a 4K clip and a 320x240
    // clip must not reserve the same amount of memory. Each in-flight frame
    // costs one RGB buffer plus one index buffer.
    const BATCH_BUDGET: usize = 24 << 20;
    let per_frame = (frame_bytes * 2).max(1);
    let by_budget = (BATCH_BUDGET / per_frame).max(1);
    let batch = ((threads as usize) * 2).clamp(1, by_budget);

    let mut pixels: Vec<Vec<u8>> = Vec::with_capacity(batch);
    let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(batch);
    let mut read_total = 0u64;
    let mut written = 0u64;

    // 「复用重复帧」 holds one block back so that a run of identical blocks can
    // become a single, longer-lasting frame. With the setting off the block is
    // handed straight on again, so nothing about the output changes.
    let mut pending: Option<Vec<u8>> = None;
    let mut pending_delay: u32 = 0;
    // 「帧间透明优化」 needs the previous frame's pixels, which may live in the
    // previous batch.
    let mut carry: Option<Vec<u8>> = None;

    while read_total < frame_count {
        let want = ((frame_count - read_total) as usize).min(batch);
        pixels.clear();
        for _ in 0..want {
            let mut buf = Vec::with_capacity(frame_bytes);
            if !reader.next(&mut buf)? {
                break;
            }
            pixels.push(buf);
        }
        if pixels.is_empty() {
            break;
        }
        read_total += pixels.len() as u64;

        // Cancellation is checked once per batch. A batch is small, so this
        // stays responsive without touching an atomic for every frame.
        if settings.cancelled() {
            return Err(cancelled_error());
        }

        // Map and compress the whole batch in parallel, then write the blocks
        // in order. Only this final write loop has to be serial.
        blocks.clear();
        blocks.resize_with(pixels.len(), Vec::new);
        encode_batch(
            palette,
            gif_palette,
            colour_map,
            &pixels,
            &mut blocks,
            w as usize,
            h as usize,
            delay_cs,
            &opts,
            carry.as_deref(),
            threads,
        )?;

        if opts.transparent.is_some() {
            let last = pixels.len() - 1;
            carry = Some(pixels[last].clone());
        }

        for block in &blocks {
            if opts.dedupe {
                if let Some(held) = pending.as_mut() {
                    if held == block && pending_delay + delay_cs as u32 <= u16::MAX as u32 {
                        // The rendered frame is identical, so show the previous
                        // one for longer instead of paying for the bytes twice.
                        pending_delay += delay_cs as u32;
                        continue;
                    }
                }
            }
            flush_pending(
                &mut writer,
                &mut pending,
                &mut pending_delay,
                &mut written,
                progress,
            )?;
            pending = Some(block.clone());
            pending_delay = delay_cs as u32;
        }
    }

    flush_pending(
        &mut writer,
        &mut pending,
        &mut pending_delay,
        &mut written,
        progress,
    )?;
    let sink = writer.finish()?;
    Ok((sink, written))
}

/// Palette-map and LZW-compress a batch of frames across up to `threads`
/// workers, producing one ready-to-write block per frame.
///
/// Frames are independent — dithering diffuses error only within its own frame,
/// and each frame carries its own LZW stream — so this is a straight
/// data-parallel split over contiguous chunks. Doing the compression here
/// rather than on the writing thread matters: it is the second-largest
/// per-frame cost after the mapping itself. The chunks are disjoint, which is
/// what lets every worker hold a `&mut` into the same output slice at once.
#[allow(clippy::too_many_arguments)]
fn encode_batch(
    palette: &Palette,
    gif_palette: &Palette,
    colour_map: &ColorMap,
    pixels: &[Vec<u8>],
    blocks: &mut [Vec<u8>],
    w: usize,
    h: usize,
    delay_cs: u16,
    opts: &FrameOptions,
    carry: Option<&[u8]>,
    threads: u32,
) -> Result<()> {
    let n = pixels.len();
    let workers = (threads as usize).max(1).min(n.max(1));

    /// The frame a delta is taken against: the batch's previous frame, or the
    /// last frame of the previous batch for the first one.
    fn previous<'a>(
        i: usize,
        pixels: &'a [Vec<u8>],
        carry: Option<&'a [u8]>,
    ) -> Option<&'a [u8]> {
        if i == 0 {
            carry
        } else {
            Some(pixels[i - 1].as_slice())
        }
    }

    if workers <= 1 {
        for (i, (px, block)) in pixels.iter().zip(blocks.iter_mut()).enumerate() {
            *block = map_and_compress(
                colour_map,
                palette,
                gif_palette,
                px,
                previous(i, pixels, carry),
                w,
                h,
                delay_cs,
                opts,
            )?;
        }
        return Ok(());
    }

    let chunk = n.div_ceil(workers);
    let failure: std::sync::Mutex<Option<Error>> = std::sync::Mutex::new(None);

    // Scoped threads let the workers borrow the batch directly, so no frame is
    // copied to hand it over and nothing has to outlive the call.
    std::thread::scope(|scope| {
        for (ci, (blk_chunk, px_chunk)) in blocks
            .chunks_mut(chunk)
            .zip(pixels.chunks(chunk))
            .enumerate()
        {
            let failure = &failure;
            let base = ci * chunk;
            scope.spawn(move || {
                for (j, (block, px)) in blk_chunk.iter_mut().zip(px_chunk.iter()).enumerate() {
                    let i = base + j;
                    match map_and_compress(
                        colour_map,
                        palette,
                        gif_palette,
                        px,
                        previous(i, pixels, carry),
                        w,
                        h,
                        delay_cs,
                        opts,
                    ) {
                        Ok(bytes) => *block = bytes,
                        Err(e) => {
                            // Keep the first error and stop this worker; the
                            // others finish their own chunks harmlessly.
                            if let Ok(mut slot) = failure.lock() {
                                if slot.is_none() {
                                    *slot = Some(e);
                                }
                            }
                            return;
                        }
                    }
                }
            });
        }
    });

    match failure.into_inner() {
        Ok(Some(e)) => Err(e),
        _ => Ok(()),
    }
}

/// Map one frame's pixels and compress it into a write-ready GIF block.
#[allow(clippy::too_many_arguments)]
fn map_and_compress(
    colour_map: &ColorMap,
    palette: &Palette,
    gif_palette: &Palette,
    px: &[u8],
    prev: Option<&[u8]>,
    w: usize,
    h: usize,
    delay_cs: u16,
    opts: &FrameOptions,
) -> Result<Vec<u8>> {
    // 「有损压缩」 runs before anything else looks at the pixels, so the palette,
    // the indices and the inter-frame diff all agree on the same colours.
    let posterized;
    let source: &[u8] = if opts.posterize_levels >= 2 {
        let mut copy = px.to_vec();
        posterize_in_place(&mut copy, opts.posterize_levels);
        posterized = copy;
        &posterized
    } else {
        px
    };

    let mut indices: Vec<u8> = Vec::with_capacity(w * h);
    match opts.dither {
        Dither::None => map_frame_with(colour_map, source, &mut indices),
        Dither::Ordered | Dither::Floyd => {
            map_frame_dithered_with(colour_map, palette, source, w, h, opts.strength, &mut indices)
        }
    }

    if let (Some(index), Some(prev)) = (opts.transparent, prev) {
        // Compare the raw pixels on both sides: "the source frame did not
        // change here" is a statement about the video, not about the palette.
        apply_transparency(&mut indices, px, prev, opts.tolerance, index);
    }

    frame_block(&FrameSpec {
        width: w as u16,
        height: h as u16,
        indices: &indices,
        palette: gif_palette,
        local_table: false,
        delay_cs,
        disposal: if opts.transparent.is_some() {
            Disposal::DoNotDispose
        } else {
            Disposal::Keep
        },
        transparent: opts.transparent,
    })
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
    quality: u32,
    progress: &Progress,
) -> Result<Palette> {
    const MAX_SAMPLES: usize = 40;
    const SAMPLE_PIXEL_BUDGET: usize = 1_500_000;

    let pixels_per_frame = (w as usize) * (h as usize);

    // How many frames to sample, and how densely to subsample each of them.
    let target_frames = (frame_count as usize).min(MAX_SAMPLES).max(1);
    let stride = (frame_count as usize / target_frames).max(1);
    let pixel_stride = (pixels_per_frame * target_frames / SAMPLE_PIXEL_BUDGET).max(1);

    let mut sample: Vec<u8> = Vec::with_capacity(SAMPLE_PIXEL_BUDGET * 3);
    let mut buf: Vec<u8> = Vec::new();
    let mut seen = 0usize;
    let mut taken = 0usize;

    while reader.next(&mut buf)? {
        if seen % stride == 0 {
            if pixel_stride <= 1 {
                sample.extend_from_slice(&buf);
            } else {
                let npx = buf.len() / 3;
                let mut p = 0usize;
                while p < npx {
                    let o = p * 3;
                    sample.push(buf[o]);
                    sample.push(buf[o + 1]);
                    sample.push(buf[o + 2]);
                    p += pixel_stride;
                }
            }
            taken += 1;
            // Report coarse progress: this phase is short next to decoding.
            if taken % 8 == 0 {
                progress.step();
            }
            if taken >= target_frames {
                break;
            }
        }
        seen += 1;
    }

    if sample.len() < 3 {
        return Err(Error::BadMedia("no pixel data available to build a palette".into()));
    }

    Ok(quantize_with_options(
        &sample,
        max_colors,
        threads,
        quantize_iterations(quality),
    ))
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
            assert!(w >= 2 && h >= 2);
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
        assert!((p.fraction() - 0.25).abs() < 1e-9);
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

        let indices: Vec<u64> = runs.iter().map(|(t, _)| (t * 12.0).round() as u64).collect();
        // First run flush with the start, last run ending on the last frame.
        assert_eq!(indices, vec![0, 8, 17, 25, 33, 41, 50, 58]);
        assert_eq!(indices.last().copied().unwrap() + ESTIMATE_SEGMENT_FRAMES, 60);
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

            let indices: Vec<u64> = runs.iter().map(|(t, _)| (t * 12.0).round() as u64).collect();
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
            encode_batch(
                &palette, &palette, &cm, &frames, &mut serial, w, h, 8, &opts, None, 1,
            )?;

            for threads in [2u32, 3, 4, 8, 16] {
                let mut got: Vec<Vec<u8>> = vec![Vec::new(); frames.len()];
                encode_batch(
                    &palette, &palette, &cm, &frames, &mut got, w, h, 8, &opts, None, threads,
                )?;
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
}
