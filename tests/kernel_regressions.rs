//! Self-generated, lossless fixtures; missing tools are a failure, never a silent pass.
use gifcore::ffmpeg::{
    frames::{decode_frame, filmstrip},
    probe::probe,
};
use gifcore::{export, Dither, Error, MediaTools, Progress, Settings};
use std::{
    ffi::{CString, OsString},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::{Duration, Instant},
};

struct Fixtures {
    dir: PathBuf,
    tools: MediaTools,
    broken: PathBuf,
}

fn run(program: &OsString, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .output()
        .expect("start media tool");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("gifcore-regression-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tools = MediaTools::discover();
        run(&tools.ffmpeg, &["-version"]);
        run(&tools.ffprobe, &["-version"]);
        let make = |name: &str,
                    w: usize,
                    h: usize,
                    n: usize,
                    pixel: &dyn Fn(usize, usize, usize) -> u8| {
            let bytes: Vec<u8> = (0..n)
                .flat_map(|i| {
                    (0..h).flat_map(move |y| (0..w).flat_map(move |x| [pixel(i, x, y); 3]))
                })
                .collect();
            let raw = dir.join(format!("{name}.rgb"));
            let video = dir.join(format!("{name}.mkv"));
            std::fs::write(&raw, bytes).unwrap();
            run(
                &tools.ffmpeg,
                &[
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgb24",
                    "-video_size",
                    &format!("{w}x{h}"),
                    "-framerate",
                    "10",
                    "-i",
                    raw.to_str().unwrap(),
                    "-c:v",
                    "ffv1",
                    video.to_str().unwrap(),
                ],
            );
        };
        make("fade", 32, 32, 32, &|i, _, _| (i * 4) as u8);
        make("dither", 32, 32, 8, &|i, x, y| {
            if x == 0 {
                (i * 30) as u8
            } else {
                ((x * 7 + y * 3) % 256) as u8
            }
        });
        make("odd", 320, 181, 12, &|i, _, _| (i * 16) as u8);
        make("constant", 32, 32, 100, &|_, _, _| 40);
        make("sparse", 64, 48, 24, &|i, x, y| {
            let (left, top) = [(0, 0), (60, 0), (60, 44), (0, 44), (30, 20), (0, 0)][i / 4];
            if (left..left + 4).contains(&x) && (top..top + 4).contains(&y) {
                200
            } else {
                40
            }
        });
        run(
            &tools.ffmpeg,
            &[
                "-v",
                "error",
                "-y",
                "-i",
                dir.join("fade.mkv").to_str().unwrap(),
                "-vf",
                "setpts=PTS+3/TB",
                "-c:v",
                "ffv1",
                dir.join("shifted.mkv").to_str().unwrap(),
            ],
        );
        run(
            &tools.ffmpeg,
            &[
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=32x32:r=10:d=1",
                "-f",
                "lavfi",
                "-i",
                "color=c=green:s=64x64:r=10:d=1",
                "-map",
                "0:v",
                "-map",
                "1:v",
                "-c:v",
                "ffv1",
                "-disposition:v:0",
                "0",
                "-disposition:v:1",
                "0",
                dir.join("multi.mkv").to_str().unwrap(),
            ],
        );
        run(
            &tools.ffmpeg,
            &[
                "-v",
                "error",
                "-y",
                "-i",
                dir.join("fade.mkv").to_str().unwrap(),
                "-vf",
                "setpts=PTS+floor(N/4)*0.1/TB",
                "-fps_mode",
                "vfr",
                "-c:v",
                "ffv1",
                dir.join("vfr.mkv").to_str().unwrap(),
            ],
        );
        let broken = dir.join(format!("broken{}", std::env::consts::EXE_SUFFIX));
        let compiled = Command::new("rustc")
            .args(["--edition", "2021", "tests/support/media_tool.rs", "-o"])
            .arg(&broken)
            .output()
            .expect("compile decoder fixture");
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        Fixtures { dir, tools, broken }
    })
}

fn settings(name: &str) -> Settings {
    let f = fixtures();
    Settings {
        input: f.dir.join("fade.mkv"),
        output: f.dir.join(format!("{name}.gif")),
        end: 3.2,
        fps: 10.0,
        max_width: 32,
        colors: 255,
        dither: Dither::None,
        max_threads: 1,
        ..Settings::default()
    }
}

#[test]
fn shifted_stream_uses_its_own_duration_and_relative_selection() {
    let f = fixtures();
    let input = f.dir.join("shifted.mkv");
    let info = probe(&f.tools, &input).unwrap();
    assert!((info.duration - 3.2).abs() < 1e-6);
    for (time, grey) in [(0.0, 0), (1.0, 40), (3.2, 124)] {
        let frame = decode_frame(&f.tools, &input, &info, time).unwrap();
        assert_eq!(frame.rgb[0], grey);
    }
    let s = Settings {
        input,
        start: 1.0,
        end: 1.4,
        ..settings("shifted-selection")
    };
    let result = export(&f.tools, &s, &Progress::new()).unwrap();
    assert_eq!(result.frames, 4);
    assert_eq!(result.duration, 0.4);
    let rgb = decode(&s.output);
    for (i, frame) in rgb.chunks_exact(32 * 32 * 3).enumerate() {
        assert_eq!(frame[0], ((10 + i) * 4) as u8);
    }
}

fn decode(path: &Path) -> Vec<u8> {
    let out = Command::new(&fixtures().tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Parse GIF blocks structurally; do not mistake LZW payload bytes for GCEs.
fn delays(path: &Path) -> Vec<u16> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..6], b"GIF89a");
    let mut p = 13;
    if bytes[10] & 128 != 0 {
        p += 3 * (1 << ((bytes[10] & 7) + 1));
    }
    let mut delays = Vec::new();
    loop {
        let tag = bytes[p];
        p += 1;
        match tag {
            0x3b => break,
            0x21 => {
                let ext = bytes[p];
                p += 1;
                if ext == 0xf9 {
                    delays.push(u16::from_le_bytes([bytes[p + 2], bytes[p + 3]]));
                }
            }
            0x2c => {
                let packed = bytes[p + 8];
                p += 9;
                if packed & 128 != 0 {
                    p += 3 * (1 << ((packed & 7) + 1));
                }
                p += 1;
            }
            _ => panic!("unexpected GIF block {tag}"),
        }
        while bytes[p] != 0 {
            p += bytes[p] as usize + 1;
        }
        p += 1;
    }
    delays
}

fn image_regions(path: &Path) -> Vec<(u16, u16, u16, u16)> {
    let bytes = std::fs::read(path).unwrap();
    let mut p = 13;
    if bytes[10] & 128 != 0 {
        p += 3 * (1 << ((bytes[10] & 7) + 1));
    }
    let mut regions = Vec::new();
    loop {
        let tag = bytes[p];
        p += 1;
        match tag {
            0x3b => break,
            0x21 => {
                p += 1;
            }
            0x2c => {
                let field = |offset| u16::from_le_bytes([bytes[p + offset], bytes[p + offset + 1]]);
                regions.push((field(0), field(2), field(4), field(6)));
                let packed = bytes[p + 8];
                p += 9;
                if packed & 128 != 0 {
                    p += 3 * (1 << ((packed & 7) + 1));
                }
                p += 1;
            }
            _ => panic!("unexpected GIF block {tag}"),
        }
        while bytes[p] != 0 {
            p += bytes[p] as usize + 1;
        }
        p += 1;
    }
    regions
}

#[test]
fn sparse_rectangles_preserve_pixels_timing_edges_and_loop_restart() {
    let f = fixtures();
    let plain = Settings {
        input: f.dir.join("sparse.mkv"),
        end: 2.4,
        max_width: 64,
        ..settings("sparse-plain")
    };
    let full = export(&f.tools, &plain, &Progress::new()).unwrap();
    let expected = decode(&plain.output);
    for dedupe in [false, true] {
        let mut serial = None;
        for threads in [1, 4] {
            let s = Settings {
                output: f.dir.join(format!("sparse-{dedupe}-{threads}.gif")),
                optimize: true,
                dedupe,
                tolerance: 0,
                max_threads: threads,
                ..plain.clone()
            };
            let actual = export(&f.tools, &s, &Progress::new()).unwrap();
            assert_eq!(actual.duration, full.duration);
            assert!(actual.bytes < full.bytes);
            let regions = image_regions(&s.output);
            assert_eq!(regions[0], (0, 0, 64, 48));
            assert!(regions
                .iter()
                .skip(1)
                .any(|&(x, y, w, h)| x > 0 || y > 0 || w < 64 || h < 48));
            if !dedupe {
                assert!(regions.contains(&(0, 0, 1, 1)));
            }
            let rgb = decode(&s.output);
            let delays = delays(&s.output);
            let pixels = 64 * 48 * 3;
            let mut expanded = Vec::new();
            for (frame, delay) in rgb.chunks_exact(pixels).zip(&delays) {
                assert_eq!(delay % 10, 0);
                for _ in 0..delay / 10 {
                    expanded.extend_from_slice(frame);
                }
            }
            assert_eq!(expanded, expected);
            // Decode two actual playback loops; a cropped first frame would leave stale pixels.
            let loops = Command::new(&f.tools.ffmpeg)
                .args(["-v", "error", "-ignore_loop", "0", "-i"])
                .arg(&s.output)
                .args([
                    "-frames:v",
                    &(actual.frames * 2).to_string(),
                    "-fps_mode",
                    "passthrough",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(
                loops.status.success(),
                "{}",
                String::from_utf8_lossy(&loops.stderr)
            );
            assert_eq!(loops.stdout, rgb.repeat(2));
            let bytes = std::fs::read(&s.output).unwrap();
            if let Some(serial) = &serial {
                assert_eq!(&bytes, serial);
            } else {
                serial = Some(bytes);
            }
        }
    }
    let short = Settings {
        output: f.dir.join("sparse-exact-estimate.gif"),
        end: 0.8,
        optimize: true,
        dedupe: true,
        ..plain
    };
    let estimate = gifcore::export::estimate(&f.tools, &short, &Progress::new()).unwrap();
    let actual = export(&f.tools, &short, &Progress::new()).unwrap();
    assert!(estimate.exact);
    assert_eq!(estimate.bytes, actual.bytes);
}

#[test]
fn transparency_tracks_the_retained_canvas_and_bounds_cumulative_error() {
    let f = fixtures();
    let plain = settings("fade-plain");
    export(&f.tools, &plain, &Progress::new()).unwrap();
    let expected = decode(&plain.output);
    for dedupe in [false, true] {
        let optimized = Settings {
            output: f.dir.join(format!("fade-opt-{dedupe}.gif")),
            colors: 256,
            optimize: true,
            tolerance: 4,
            dedupe,
            ..plain.clone()
        };
        export(&f.tools, &optimized, &Progress::new()).unwrap();
        let raw = decode(&optimized.output);
        if !dedupe {
            assert_eq!(raw.len(), expected.len());
            assert!(raw.iter().zip(&expected).all(|(a, b)| a.abs_diff(*b) <= 4));
        }
        assert!(
            raw[raw.len() - 3] > 110,
            "gradual changes must not freeze at the first frame"
        );
        assert_eq!(
            delays(&optimized.output)
                .into_iter()
                .map(u64::from)
                .sum::<u64>(),
            320
        );
    }
}

#[test]
fn zero_tolerance_transparency_preserves_diffused_pixels_and_thread_determinism() {
    let f = fixtures();
    let plain = Settings {
        input: f.dir.join("dither.mkv"),
        end: 0.8,
        colors: 8,
        dither: Dither::Floyd,
        ..settings("dither-plain")
    };
    export(&f.tools, &plain, &Progress::new()).unwrap();
    let expected = decode(&plain.output);
    let mut serial = None;
    for threads in [1, 3] {
        let optimized = Settings {
            output: f.dir.join(format!("dither-opt-{threads}.gif")),
            colors: 9,
            optimize: true,
            max_threads: threads,
            ..plain.clone()
        };
        export(&f.tools, &optimized, &Progress::new()).unwrap();
        assert_eq!(decode(&optimized.output), expected);
        let bytes = std::fs::read(&optimized.output).unwrap();
        if let Some(serial) = &serial {
            assert_eq!(&bytes, serial);
        } else {
            serial = Some(bytes);
        }
    }
}

#[test]
fn all_decoding_paths_use_the_probed_stream() {
    let f = fixtures();
    let input = f.dir.join("multi.mkv");
    let info = probe(&f.tools, &input).unwrap();
    assert_eq!((info.width, info.height), (32, 32));
    let red = |rgb: &[u8]| {
        assert!(
            rgb[0] > 240 && rgb[1] < 10,
            "expected first red video stream: {:?}",
            &rgb[..3]
        );
    };
    red(&decode_frame(&f.tools, &input, &info, 0.0).unwrap().rgb);
    red(&filmstrip(&f.tools, &input, &info, 4, 32).unwrap().thumbs[0].rgb);
    let s = Settings {
        input,
        end: 1.0,
        ..settings("multi-out")
    };
    export(&f.tools, &s, &Progress::new()).unwrap();
    red(&decode(&s.output));
}

#[test]
fn odd_source_dimensions_do_not_corrupt_thumbnail_boundaries() {
    let f = fixtures();
    let input = f.dir.join("odd.mkv");
    let info = probe(&f.tools, &input).unwrap();
    let strip = filmstrip(&f.tools, &input, &info, 12, 160).unwrap();
    assert_eq!(strip.thumbs.len(), 12);
    assert_eq!((strip.thumbs[0].w, strip.thumbs[0].h), (160, 90));
    for (i, frame) in strip.thumbs.iter().enumerate() {
        assert_eq!(frame.rgb.len(), 160 * 90 * 3);
        assert!(frame.rgb.iter().all(|c| c.abs_diff((i * 16) as u8) <= 1));
        assert!(strip.times[i] < info.duration);
    }
}

#[test]
fn preview_resolves_actual_display_intervals_including_the_final_frame() {
    let f = fixtures();
    let input = f.dir.join("fade.mkv");
    let info = probe(&f.tools, &input).unwrap();
    for (time, value) in [
        (0.0, 0),
        (0.051, 0),
        (0.099, 0),
        (0.1, 4),
        (3.19, 124),
        (3.2, 124),
    ] {
        let frame = decode_frame(&f.tools, &input, &info, time).unwrap();
        assert_eq!(&frame.rgb[..3], &[value; 3], "time={time}");
    }
}

#[test]
fn gif_timing_preserves_requested_speed_and_reports_actual_duration() {
    let f = fixtures();
    for (fps, speed) in [
        (30.0, 1.0),
        (60.0, 1.0),
        (120.0, 1.0),
        (30.0, 2.0),
        (10.0, 0.5),
    ] {
        let s = Settings {
            end: 1.0,
            fps,
            speed,
            ..settings(&format!("timing-{fps}-{speed}"))
        };
        let result = export(&f.tools, &s, &Progress::new()).unwrap();
        let ticks = delays(&s.output);
        assert!(ticks.iter().all(|d| *d >= 2));
        let actual = ticks.iter().map(|d| *d as u64).sum::<u64>() as f64 / 100.0;
        assert!(
            (actual - 1.0 / speed).abs() <= 0.01,
            "{fps} fps at {speed}x: {actual}"
        );
        assert_eq!(actual, result.duration);
        assert!((result.fps - result.frames as f64 / actual).abs() < 1e-9);
        if fps * speed > 50.0 {
            assert!(result.warning.is_some());
        }
    }
}

#[test]
fn partial_decoder_failure_preserves_target_and_leaves_no_staging_file() {
    let f = fixtures();
    let s = settings("failure");
    std::fs::write(&s.output, b"previous GIF").unwrap();
    let tools = MediaTools {
        ffmpeg: f.broken.clone().into_os_string(),
        ..f.tools.clone()
    };
    let err = export(&tools, &s, &Progress::new()).unwrap_err();
    assert!(matches!(err, Error::ToolFailed { .. }), "{err}");
    assert!(err.to_string().contains("intentional decoder failure"));
    assert_eq!(std::fs::read(&s.output).unwrap(), b"previous GIF");
}

#[test]
fn stalled_children_are_cancelled_and_sink_errors_do_not_wait_for_exit() {
    use gifcore::ffmpeg::process::run_ffmpeg_stream_cancel;
    let f = fixtures();
    let program = f.broken.clone().into_os_string();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(150));
            cancel.store(true, Ordering::SeqCst);
        });
        run_ffmpeg_stream_cancel(&program, &["--stall".into()], 3072, Some(&cancel), |_| {
            Ok(())
        })
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(5));
    let started = Instant::now();
    let result = run_ffmpeg_stream_cancel(
        &program,
        &["--stall-after-frame".into()],
        3072,
        None,
        |_| Err(Error::Io("sink failed".into())),
    );
    assert!(result.unwrap_err().to_string().contains("sink failed"));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn static_size_estimates_do_not_scale_a_single_frame_by_source_frame_count() {
    let f = fixtures();
    for optimize in [false, true] {
        let s = Settings {
            input: f.dir.join("constant.mkv"),
            output: f.dir.join(format!("constant-{optimize}.gif")),
            end: 10.0,
            dedupe: true,
            optimize,
            ..settings("unused")
        };
        let estimate = gifcore::export::estimate(&f.tools, &s, &Progress::new()).unwrap();
        let actual = export(&f.tools, &s, &Progress::new()).unwrap();
        assert_eq!(estimate.bytes, actual.bytes);
    }
}

#[test]
fn abi_strength_zero_disables_dithering_and_ordered_is_independent() {
    use gifcore::ffi;
    let f = fixtures();
    let input = CString::new(f.dir.join("dither.mkv").to_str().unwrap()).unwrap();
    let handle = ffi::gif_create();
    unsafe {
        let ffmpeg = CString::new(f.tools.ffmpeg.to_string_lossy().as_bytes()).unwrap();
        let ffprobe = CString::new(f.tools.ffprobe.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(ffi::gif_set_ffmpeg_path(handle, ffmpeg.as_ptr()), 0);
        assert_eq!(ffi::gif_set_ffprobe_path(handle, ffprobe.as_ptr()), 0);
        assert_eq!(ffi::gif_load(handle, input.as_ptr()), 0);
        for (name, mode, strength) in [
            ("none", "none", 100),
            ("zero", "floyd", 0),
            ("full", "floyd", 100),
            ("ordered", "ordered", 100),
        ] {
            let mode = CString::new(mode).unwrap();
            let path =
                CString::new(f.dir.join(format!("abi-{name}.gif")).to_str().unwrap()).unwrap();
            let opts = ffi::GifCoreExportOptions {
                colors: 8,
                max_width: 32,
                threads: 1,
                fps: 10.0,
                dither: mode.as_ptr(),
                dither_strength: strength,
                ..Default::default()
            };
            assert_eq!(ffi::gif_export_ex(handle, path.as_ptr(), &opts), 0);
        }
        ffi::gif_destroy(handle);
    }
    let read = |name| std::fs::read(f.dir.join(format!("abi-{name}.gif"))).unwrap();
    assert_eq!(read("none"), read("zero"));
    assert_ne!(read("zero"), read("full"));
    assert_ne!(read("ordered"), read("full"));
}

#[test]
fn input_aliases_and_nonfinite_parameters_are_rejected() {
    let f = fixtures();
    let alias = f.dir.join("input-alias.gif");
    std::fs::hard_link(f.dir.join("fade.mkv"), &alias).unwrap();
    let err = export(
        &f.tools,
        &Settings {
            output: alias,
            ..settings("unused")
        },
        &Progress::new(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("different files"));
    for (fps, speed) in [(f64::NAN, 1.0), (10.0, f64::INFINITY)] {
        assert!(matches!(
            export(
                &f.tools,
                &Settings {
                    fps,
                    speed,
                    ..settings("bad-param")
                },
                &Progress::new()
            ),
            Err(Error::Invalid(_))
        ));
    }
}

#[test]
fn preview_uses_pts_for_variable_frame_rate() {
    let f = fixtures();
    let input = f.dir.join("vfr.mkv");
    let info = probe(&f.tools, &input).unwrap();
    for (time, value) in [(0.39, 12), (0.49, 12), (0.5, 16), (0.99, 28)] {
        let frame = decode_frame(&f.tools, &input, &info, time).unwrap();
        assert_eq!(&frame.rgb[..3], &[value; 3], "VFR time={time}");
    }
}

#[test]
fn a_short_estimate_measures_the_exact_same_pipeline_as_export() {
    let f = fixtures();
    for optimize in [false, true] {
        let s = Settings {
            end: 0.8,
            optimize,
            dedupe: true,
            lossy: 35,
            dither: Dither::Ordered,
            output: f.dir.join(format!("estimate-exact-{optimize}.gif")),
            ..settings("unused")
        };
        let estimate = gifcore::export::estimate(&f.tools, &s, &Progress::new()).unwrap();
        let actual = export(&f.tools, &s, &Progress::new()).unwrap();
        assert!(estimate.exact);
        assert_eq!(estimate.bytes, actual.bytes);
    }
}
