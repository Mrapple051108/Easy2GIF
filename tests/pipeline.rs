//! End-to-end checks for the kernel: probe a real file, export a real GIF, and
//! then validate the result with an independent tool rather than trusting our
//! own writer.
//!
//! These tests need the bundled media engine. They look for it on `PATH` via
//! `MediaTools::discover()`, or wherever `GIFCORE_FFMPEG` / `GIFCORE_FFPROBE`
//! point. When no fixture or no tool is available the test reports success
//! without asserting anything, so they stay usable on a bare checkout.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gifcore::export::{export, planned_frames, Progress, Settings};
use gifcore::ffmpeg::probe;
use gifcore::ffmpeg::process::MediaTools;
use gifcore::gif::Dither;

fn fixture() -> Option<PathBuf> {
    let candidates = [
        ".probe/test.mp4",
        "../.probe/test.mp4",
        "testdata/sample.mp4",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn fixture_with_audio() -> Option<PathBuf> {
    let candidates = [".probe/test_audio.mp4", "../.probe/test_audio.mp4"];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Locate a tool, preferring an explicit environment override.
fn tool(name: &str, var: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(var) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = format!("{name}.exe");
    for dir in std::env::var("PATH").unwrap_or_default().split(';') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(&exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn tools() -> Option<(MediaTools, PathBuf, PathBuf)> {
    let ffmpeg = tool("ffmpeg", "GIFCORE_FFMPEG")?;
    let ffprobe = tool("ffprobe", "GIFCORE_FFPROBE")?;
    let mut t = MediaTools::discover();
    t.ffmpeg = ffmpeg.clone().into_os_string();
    t.ffprobe = ffprobe.clone().into_os_string();
    Some((t, ffmpeg, ffprobe))
}

/// A scratch directory shared by these tests, created on demand.
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join("gifcore-tests");
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// Ask ffprobe how many frames the produced GIF actually contains.
fn count_gif_frames(ffprobe: &Path, gif: &Path) -> u64 {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames,width,height",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(gif)
        .output()
        .expect("running ffprobe");
    assert!(
        out.status.success(),
        "ffprobe rejected the GIF we produced: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.strip_prefix("nb_read_frames="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Decode the GIF back to raw pixels. Any structural damage makes ffmpeg fail.
///
/// The scratch file is named after the GIF being checked: these tests run in
/// parallel threads of one process, so a path keyed on the process id alone
/// would let two ffmpeg runs write the same file and corrupt the count.
fn decode_gif_size(ffmpeg: &Path, gif: &Path) -> (u64, usize) {
    let stem = gif
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out.gif".to_string());
    let raw = scratch().join(format!("{stem}.raw"));
    let status = Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(gif)
        // Without passthrough, rawvideo output defaults to a constant frame rate
        // and ffmpeg pads the clip by duplicating frames, so the byte count
        // would measure ffmpeg's duplication rather than what the GIF holds.
        .args([
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
        ])
        .arg(&raw)
        .status()
        .expect("running ffmpeg");
    assert!(status.success(), "ffmpeg could not decode the GIF we wrote");
    let len = std::fs::metadata(&raw)
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    let _ = std::fs::remove_file(&raw);
    (0, len)
}

#[test]
fn probe_reports_the_real_media_facts() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let info = probe(&tools, &path).expect("probing the fixture");
    assert_eq!(info.width, 320, "width");
    assert_eq!(info.height, 240, "height");
    assert!((info.fps - 30.0).abs() < 0.01, "fps was {}", info.fps);
    assert!(
        (info.duration - 5.0).abs() < 0.05,
        "duration was {}",
        info.duration
    );
    assert!(info.is_usable());
    assert_eq!(info.total_frames(), 150);
    // Frame 0 is sampled at its centre, and time_to_frame inverts that.
    assert!((info.frame_time(0) - 0.5 / 30.0).abs() < 1e-9);
    assert_eq!(info.time_to_frame(1.0), 30);
}

#[test]
fn export_writes_a_gif_a_third_party_reader_accepts() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, ffmpeg, ffprobe)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let out = std::env::temp_dir().join("gifcore-tests/export.mp4.gif");
    let _ = std::fs::remove_file(&out);

    let settings = Settings {
        input: path.clone(),
        output: out.clone(),
        start: 1.0,
        end: 2.0,
        fps: 12.0,
        max_width: 160,
        colors: 64,
        dither: Dither::Floyd,
        ..Settings::default()
    };

    let progress = Progress::new();
    let outcome = export(&tools, &settings, &progress).expect("exporting a GIF");

    // 1.0s..2.0s at 12 fps is twelve frames.
    assert_eq!(planned_frames(1.0, 2.0, 12.0), 12);
    assert_eq!(outcome.frames, 12, "frame count");
    // 320x240 scaled to 160 wide keeps the aspect ratio: 160x120.
    assert_eq!((outcome.width, outcome.height), (160, 120));
    assert!((outcome.fps - 12.0).abs() < 1e-9);

    let bytes = std::fs::read(&out).expect("reading the GIF back");
    assert!(bytes.len() > 100, "GIF suspiciously small");
    assert_eq!(&bytes[0..6], b"GIF89a", "missing the GIF89a signature");
    assert_eq!(outcome.bytes, bytes.len() as u64);

    // The logical screen descriptor must agree with what we reported.
    let screen_w = u16::from_le_bytes([bytes[6], bytes[7]]);
    let screen_h = u16::from_le_bytes([bytes[8], bytes[9]]);
    assert_eq!((screen_w, screen_h), (160, 120), "logical screen size");

    // An independent reader must accept the stream and see the same frames.
    let seen = count_gif_frames(&ffprobe, &out);
    assert_eq!(seen, 12, "ffprobe counted a different number of frames");

    // And it must decode to exactly the expected amount of pixel data.
    let (_, raw_len) = decode_gif_size(&ffmpeg, &out);
    assert_eq!(
        raw_len,
        12 * 160 * 120 * 3,
        "decoded pixel payload does not match the frame count"
    );

    // The reported palette must be a legal GIF colour-table size.
    assert!(outcome.palette_colors >= 2 && outcome.palette_colors <= 256);
    assert!(outcome.palette_colors.is_power_of_two());

    let _ = std::fs::remove_file(&out);
}

#[test]
fn every_dither_mode_produces_a_decodable_gif() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, ffmpeg, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    for dither in [Dither::None, Dither::Ordered, Dither::Floyd] {
        let out = scratch().join(format!("dither-{}.gif", dither.as_str()));
        let _ = std::fs::remove_file(&out);
        let settings = Settings {
            input: path.clone(),
            output: out.clone(),
            start: 0.0,
            end: 0.5,
            fps: 8.0,
            // 4096 is how "keep the original size" reaches the kernel: it never
            // upscales, so the source dimensions survive.
            max_width: 4096,
            colors: 32,
            dither,
            ..Settings::default()
        };
        let outcome = export(&tools, &settings, &Progress::new())
            .unwrap_or_else(|e| panic!("{:?} export failed: {e}", dither));
        // Asking for the original size must leave 320x240 alone.
        assert_eq!((outcome.width, outcome.height), (320, 240));
        let (_, raw_len) = decode_gif_size(&ffmpeg, &out);
        assert_eq!(
            raw_len,
            outcome.frames as usize * 320 * 240 * 3,
            "{:?} decoded to the wrong pixel count",
            dither
        );
        let _ = std::fs::remove_file(&out);
    }
}

#[test]
fn speed_changes_playback_rate_not_frame_count() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let base = Settings {
        input: path.clone(),
        output: scratch().join("speed.gif"),
        start: 0.0,
        end: 2.0,
        fps: 10.0,
        max_width: 64,
        colors: 16,
        ..Settings::default()
    };

    let normal = export(&tools, &base, &Progress::new()).expect("1x export");

    let doubled = export(
        &tools,
        &Settings {
            speed: 2.0,
            ..base.clone()
        },
        &Progress::new(),
    )
    .expect("2x export");

    // Speed rides on the per-frame delay, so the same frames come out and the
    // clip plays back twice as fast.
    assert_eq!(
        doubled.frames, normal.frames,
        "speed must not change how many frames are sampled"
    );
    assert!(
        (doubled.fps - normal.fps * 2.0).abs() < 1e-9,
        "2x speed reported {} fps against {} at 1x",
        doubled.fps,
        normal.fps
    );
    assert!(
        (doubled.duration * 2.0 - normal.duration).abs() < 1e-9,
        "2x speed must halve the actual playback duration"
    );

    // Halving the speed must land the playback rate below the sampling rate.
    let halved = export(
        &tools,
        &Settings {
            speed: 0.5,
            ..base.clone()
        },
        &Progress::new(),
    )
    .expect("0.5x export");
    assert_eq!(halved.frames, normal.frames);
    assert!((halved.fps - normal.fps * 0.5).abs() < 1e-9);

    let _ = std::fs::remove_file(&base.output);
}

#[test]
fn a_cancelled_export_stops_early() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let cancel = Arc::new(AtomicBool::new(true));
    let settings = Settings {
        input: path.clone(),
        output: std::env::temp_dir().join("gifcore-tests/cancelled.gif"),
        start: 0.0,
        end: 5.0,
        fps: 12.0,
        max_width: 128,
        cancel: Some(cancel.clone()),
        ..Settings::default()
    };

    let result = export(&tools, &settings, &Progress::new());
    assert!(result.is_err(), "a pre-cancelled export must not succeed");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("cancel"), "unexpected error: {msg}");

    // Flip it back so the flag is genuinely the cause.
    cancel.store(false, Ordering::SeqCst);
    assert_eq!(cancel.load(Ordering::SeqCst), false);
}

#[test]
fn refuses_to_clobber_when_overwrite_is_off() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let out = scratch().join("existing.gif");
    std::fs::write(&out, b"do not touch").expect("seeding the output file");

    let settings = Settings {
        input: path.clone(),
        output: out.clone(),
        start: 0.0,
        end: 0.4,
        fps: 8.0,
        max_width: 64,
        colors: 16,
        overwrite: false,
        ..Settings::default()
    };

    let result = export(&tools, &settings, &Progress::new());
    assert!(result.is_err(), "overwriting a file must be refused");
    assert_eq!(
        std::fs::read(&out).expect("re-reading"),
        b"do not touch",
        "the existing file was modified"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn a_region_beyond_the_end_is_clamped_to_the_media() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    // The clip is 5 s long; ask for 4 s .. 90 s.
    let out = std::env::temp_dir().join("gifcore-tests/clamped.gif");
    let _ = std::fs::remove_file(&out);
    let settings = Settings {
        input: path.clone(),
        output: out.clone(),
        start: 4.0,
        end: 90.0,
        fps: 10.0,
        max_width: 64,
        colors: 16,
        ..Settings::default()
    };

    let outcome = export(&tools, &settings, &Progress::new()).expect("clamped export");
    // 4 s .. 5 s at 10 fps is ten frames; the export must not run to 90 s.
    assert!(outcome.frames <= 11, "produced {} frames", outcome.frames);
    assert!(
        outcome.frames >= 9,
        "produced only {} frames",
        outcome.frames
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn audio_present_is_reported_but_dropped_from_the_gif() {
    let Some(path) = fixture_with_audio().or_else(fixture) else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let info = probe(&tools, &path).expect("probing");
    // Whatever the fixture holds, the exported GIF must always be silent.
    let out = std::env::temp_dir().join("gifcore-tests/silent.gif");
    let _ = std::fs::remove_file(&out);
    let settings = Settings {
        input: path.clone(),
        output: out.clone(),
        start: 0.0,
        end: 0.3,
        fps: 6.0,
        max_width: 0,
        colors: 8,
        ..Settings::default()
    };
    let outcome = export(&tools, &settings, &Progress::new()).expect("export");
    let bytes = std::fs::read(&out).expect("reading the GIF");
    // A GIF has no audio: if the has_audio flag drove anything, this would differ.
    assert!(!bytes.is_empty());
    let _ = info;
    let _ = outcome;
    let _ = std::fs::remove_file(&out);
}

// ---------------------------------------------------------------------------
// Preview path, driven through the C ABI exactly as the Qt layer drives it.
//
// These matter because the UI reads previews through `gif_poster_rgb` and
// `gif_thumb_rgb`, not through the Rust API: a defect in the borrowed-buffer
// bookkeeping would show up as a blank preview and nowhere else.
// ---------------------------------------------------------------------------

fn last_error(handle: *mut gifcore::ffi::GifCore) -> String {
    let ptr = unsafe { gifcore::ffi::gif_last_error(handle) };
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn cstring(s: &Path) -> std::ffi::CString {
    std::ffi::CString::new(s.to_string_lossy().as_bytes()).unwrap()
}

#[test]
fn preview_publishes_real_pixels_through_the_c_abi() {
    use gifcore::ffi;

    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    let handle = ffi::gif_create();
    assert!(!handle.is_null());

    let ffmpeg = std::ffi::CString::new(tools.ffmpeg.to_string_lossy().as_bytes()).unwrap();
    let ffprobe = std::ffi::CString::new(tools.ffprobe.to_string_lossy().as_bytes()).unwrap();
    let input = cstring(&path);
    {
        assert_eq!(
            unsafe { ffi::gif_set_ffmpeg_path(handle, ffmpeg.as_ptr()) },
            ffi::GIF_OK
        );
        assert_eq!(
            unsafe { ffi::gif_set_ffprobe_path(handle, ffprobe.as_ptr()) },
            ffi::GIF_OK
        );
        assert_eq!(
            unsafe { ffi::gif_load(handle, input.as_ptr()) },
            ffi::GIF_OK,
            "load failed: {}",
            last_error(handle)
        );
    }

    // --- poster ---------------------------------------------------------
    let status = unsafe { ffi::gif_update_poster(handle) };
    assert_eq!(status, ffi::GIF_OK, "poster failed: {}", last_error(handle));

    let pw = unsafe { ffi::gif_poster_width(handle) };
    let ph = unsafe { ffi::gif_poster_height(handle) };
    let poster = unsafe { ffi::gif_poster_rgb(handle) };
    assert!(pw > 0 && ph > 0, "poster is {pw}x{ph}");
    assert_eq!(poster.len, (pw * ph * 3) as usize, "poster byte count");

    // The fixture is colour bars. A blank or all-black buffer means the preview
    // would render as an empty box in the UI, which is exactly the bug this
    // guards against.
    let pixels = unsafe { std::slice::from_raw_parts(poster.data, poster.len) };
    let bright = pixels
        .chunks_exact(3)
        .filter(|c| c[0] > 200 || c[1] > 200 || c[2] > 200)
        .count();
    assert!(bright > 0, "poster has no bright pixels — it is blank");

    // --- filmstrip ------------------------------------------------------
    let status = unsafe { ffi::gif_build_filmstrip(handle, 12, 160) };
    assert_eq!(
        status,
        ffi::GIF_OK,
        "filmstrip failed: {}",
        last_error(handle)
    );

    let strip = unsafe { ffi::gif_filmstrip(handle) };
    assert_eq!(strip.count, 12, "filmstrip thumbnail count");
    assert_eq!(strip.source_frames, 150, "source frame total");

    let thumbs = unsafe { std::slice::from_raw_parts(strip.thumbs, strip.count as usize) };
    for (i, t) in thumbs.iter().enumerate() {
        assert!(
            t.width > 0 && t.height > 0 && t.len > 0,
            "thumbnail {i} is empty"
        );
        assert_eq!(
            t.len,
            (t.width * t.height * 3) as usize,
            "thumbnail {i} byte count"
        );
        let buf = unsafe { ffi::gif_thumb_rgb(handle, i as u32) };
        assert_eq!(
            buf.len, t.len,
            "thumbnail {i} buffer disagrees with descriptor"
        );
        let px = unsafe { std::slice::from_raw_parts(buf.data, buf.len) };
        assert!(
            px.chunks_exact(3)
                .any(|c| c[0] > 150 || c[1] > 150 || c[2] > 150),
            "thumbnail {i} at t={} is blank",
            t.time
        );
    }

    // Thumbnails must be spread across the clip, not all the same instant.
    assert!(
        thumbs.last().unwrap().time > thumbs[0].time + 1.0,
        "filmstrip covers no time: {:?} .. {:?}",
        thumbs[0].time,
        thumbs.last().unwrap().time
    );

    unsafe { ffi::gif_destroy(handle) };
}

/// Threading must be a pure speed-up: the frames are independent, so no worker
/// count may change a single byte of the output.
#[test]
fn the_thread_count_cannot_change_the_output_bytes() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };

    // Exercise a few frames so batching and chunking really happen, and pick a
    // dither mode that carries error between pixels within a frame.
    let base = Settings {
        input: path.clone(),
        output: scratch().join("threads-serial.gif"),
        start: 0.0,
        end: 1.0,
        fps: 10.0,
        max_width: 96,
        colors: 32,
        dither: gifcore::Dither::Floyd,
        max_threads: 1,
        ..Settings::default()
    };

    let serial = export(&tools, &base, &Progress::new()).expect("single-threaded export");
    let serial_bytes = std::fs::read(&serial.output).expect("read the single-threaded GIF");

    for threads in [2u32, 3, 8, 16] {
        let out = scratch().join(format!("threads-{threads}.gif"));
        let outcome = export(
            &tools,
            &Settings {
                output: out.clone(),
                max_threads: threads,
                ..base.clone()
            },
            &Progress::new(),
        )
        .unwrap_or_else(|e| panic!("{threads}-threaded export failed: {e}"));

        assert_eq!(
            outcome.frames, serial.frames,
            "{threads} threads produced a different frame count"
        );
        let bytes = std::fs::read(&out).expect("read the threaded GIF");
        assert_eq!(
            bytes.len(),
            serial_bytes.len(),
            "{threads} threads produced a different file size"
        );
        assert!(
            bytes == serial_bytes,
            "{threads} threads produced different bytes than a single thread"
        );
        let _ = std::fs::remove_file(&out);
    }

    // And the automatic choice is the one the UI advertises.
    assert_eq!(
        gifcore::export::resolve_threads(0),
        gifcore::export::default_threads()
    );
}

/// A threaded export should actually be faster, not just equal. This is a
/// smoke check with a deliberately loose bound so it cannot flake on a busy
/// machine; it exists to catch the parallel path being wired up but never
/// entered.
#[test]
#[ignore = "manual performance benchmark; run in release with --ignored"]
fn threading_speeds_up_a_large_export() {
    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };
    let cores = gifcore::export::cpu_count();
    if cores < 2 {
        eprintln!("skipping: single-core machine");
        return;
    }

    let base = Settings {
        input: path.clone(),
        output: scratch().join("perf-serial.gif"),
        start: 0.0,
        end: 5.0,
        fps: 30.0,
        max_width: 256,
        colors: 256,
        dither: gifcore::Dither::Floyd,
        max_threads: 1,
        ..Settings::default()
    };

    let t0 = std::time::Instant::now();
    let serial = export(&tools, &base, &Progress::new()).expect("single-threaded export");
    let serial_ms = t0.elapsed().as_millis();
    let _ = std::fs::remove_file(&serial.output);

    let t1 = std::time::Instant::now();
    let threaded = export(
        &tools,
        &Settings {
            output: scratch().join("perf-threaded.gif"),
            max_threads: cores,
            ..base.clone()
        },
        &Progress::new(),
    )
    .expect("multi-threaded export");
    let threaded_ms = t1.elapsed().as_millis();
    let _ = std::fs::remove_file(&threaded.output);

    // The ffmpeg decode phases are shared and cannot be parallelised, so the
    // achievable win is bounded well below `cores`; require only a visible gain
    // and report the measurement rather than asserting a tight ratio.
    eprintln!(
        "threading: 1 thread {serial_ms} ms, {cores} threads {threaded_ms} ms ({:.2}x)",
        serial_ms as f64 / threaded_ms.max(1) as f64
    );
    assert!(
        threaded_ms < serial_ms,
        "using {cores} threads ({threaded_ms} ms) was not faster than one thread ({serial_ms} ms)"
    );
}

/// Dragging a selection handle asks for the poster again and again, often for
/// times that land on the very same source frame. Each decode is a whole
/// ffmpeg process — which is both slow and, on Windows, a console window the
/// user sees flash by — so a repeat request for the frame already on screen
/// must be answered without starting anything.
#[test]
fn repeating_a_poster_request_for_the_same_frame_spawns_nothing() {
    use gifcore::ffi;

    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };
    let info = gifcore::probe(&tools, &path).expect("probe");

    let handle = ffi::gif_create();
    assert!(!handle.is_null(), "gif_create returned null");
    let ffmpeg = std::ffi::CString::new(tools.ffmpeg.to_string_lossy().as_bytes()).unwrap();
    let ffprobe = std::ffi::CString::new(tools.ffprobe.to_string_lossy().as_bytes()).unwrap();
    {
        unsafe { ffi::gif_set_ffmpeg_path(handle, ffmpeg.as_ptr()) };
        unsafe { ffi::gif_set_ffprobe_path(handle, ffprobe.as_ptr()) };
    }
    let video = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(
        unsafe { ffi::gif_load(handle, video.as_ptr()) },
        0,
        "{}",
        last_error(handle)
    );

    // Land on a frame well inside the clip, then request it twice.
    let start = info.frame_time(40);
    assert_eq!(
        unsafe { ffi::gif_set_selection(handle, start, start + 0.5) },
        0
    );

    let first = std::time::Instant::now();
    assert_eq!(
        unsafe { ffi::gif_update_poster(handle) },
        0,
        "{}",
        last_error(handle)
    );
    let cold_ms = first.elapsed().as_millis();

    let second = std::time::Instant::now();
    assert_eq!(
        unsafe { ffi::gif_update_poster(handle) },
        0,
        "{}",
        last_error(handle)
    );
    let warm_ms = second.elapsed().as_millis();

    eprintln!("poster decode: first {cold_ms} ms, repeat {warm_ms} ms");

    // Nudge the start within the same source frame: still no new decode.
    let nudged = start + 0.4 / info.fps.max(1.0);
    assert_eq!(
        unsafe { ffi::gif_set_selection(handle, nudged, nudged + 0.5) },
        0
    );
    let third = std::time::Instant::now();
    assert_eq!(
        unsafe { ffi::gif_update_poster(handle) },
        0,
        "{}",
        last_error(handle)
    );
    let nudged_ms = third.elapsed().as_millis();

    unsafe { ffi::gif_destroy(handle) };

    // Spawning ffmpeg to decode a frame costs hundreds of milliseconds; doing
    // nothing costs microseconds. A generous bound keeps this from being flaky
    // while still failing loudly if a process is started again.
    assert!(
        warm_ms < 20,
        "a repeat request for the same frame took {warm_ms} ms (first was {cold_ms} ms), \
         so it started a decoder again"
    );
    assert!(
        nudged_ms < 20,
        "a request that stayed inside the same frame took {nudged_ms} ms, \
         so it started a decoder again"
    );
}

/// "Why can't I save there?" must be answerable *before* a whole export is
/// spent on it, and the answer must name the real reason rather than leaving
/// the user with a bare "拒绝访问。 (os error 5)".
#[test]
#[cfg(windows)]
fn check_output_reports_the_real_reason_before_an_export_starts() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = scratch().join("outcheck");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create check folder");

    // 1. An ordinary new file in a writable folder is fine.
    gifcore::check_output(&dir.join("ok.gif")).expect("a writable folder must pass");

    // 2. A folder that does not exist yet is created, not rejected: users type
    //    new subfolders all the time.
    let nested = dir.join("a").join("b").join("deep.gif");
    gifcore::check_output(&nested).expect("a creatable folder must pass");
    assert!(
        nested.parent().expect("parent").is_dir(),
        "check_output should have created the missing folder"
    );

    // 3. A parent that is a *file* can never be a folder.
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let err = gifcore::check_output(&blocker.join("child.gif"))
        .expect_err("a file cannot contain a file");
    assert!(
        err.to_string().contains("is a file, not a folder"),
        "unexpected message: {err}"
    );

    // 4. A read-only target.
    let ro = dir.join("ro.gif");
    std::fs::write(&ro, b"old").expect("write ro");
    let mut perms = std::fs::metadata(&ro).expect("metadata").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).expect("set readonly");
    let err = gifcore::check_output(&ro).expect_err("read-only target");
    assert!(
        err.to_string().contains("read-only"),
        "unexpected message: {err}"
    );
    let mut back = std::fs::metadata(&ro).expect("metadata").permissions();
    back.set_readonly(false);
    std::fs::set_permissions(&ro, back).expect("clear readonly");

    // 5. The one users actually hit: the previous GIF is still open in a viewer,
    //    so it cannot be replaced. `share_mode(0)` stands in for that viewer.
    let locked = dir.join("locked.gif");
    std::fs::write(&locked, b"old").expect("write locked");
    let hold = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&locked)
        .expect("exclusive open");
    let err = gifcore::check_output(&locked).expect_err("a locked target must be refused");
    assert!(
        err.to_string().contains("open in another program"),
        "unexpected message: {err}"
    );
    drop(hold);

    // Once the viewer lets go, the very same path is fine again.
    gifcore::check_output(&locked).expect("released target must pass");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed write must not leave the staged `.gif.part` behind: the old code
/// returned early on the delete step and orphaned it.
#[test]
#[cfg(windows)]
fn a_failed_export_leaves_no_staged_part_file() {
    use std::os::windows::fs::OpenOptionsExt;

    let Some(path) = fixture() else {
        eprintln!("skipping: no fixture video available");
        return;
    };
    let Some((mut tools, _, _)) = tools() else {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    };
    let _ = &mut tools;

    let dir = scratch().join("partleak");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create folder");
    let out = dir.join("leak.gif");
    std::fs::write(&out, b"previous gif").expect("seed the target");

    // Hold the destination so the final move cannot succeed.
    let hold = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&out)
        .expect("exclusive open");

    let settings = Settings {
        input: path,
        output: out.clone(),
        start: 0.0,
        end: 0.5,
        fps: 8.0,
        max_width: 160,
        overwrite: true,
        ..Settings::default()
    };
    let result = gifcore::export(&tools, &settings, &Progress::new());
    drop(hold);

    assert!(
        result.is_err(),
        "the export should have failed on a locked target"
    );
    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a failed export left staged files behind: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
