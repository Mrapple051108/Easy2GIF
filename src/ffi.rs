//! The C ABI exposed to the C++ / QML interaction layer.
//!
//! Design notes:
//!   * One opaque handle owns all state. The front end never sees Rust types.
//!   * Bulk pixel data is published into buffers owned by the handle and handed
//!     out as (pointer, length) pairs. The front end copies them immediately.
//!   * Progress is polled from the UI thread while an export runs on a worker
//!     thread, so it lives behind atomics.
//!   * Errors are thread-local strings; [`gif_last_error`] stays valid until the
//!     next call that sets an error on the same thread.
//!   * Status-returning calls answer `GIF_OK` (zero) on success and a NEGATIVE
//!     `GIF_ERR_*` code on failure, so a success can never be mistaken for an
//!     error. Purely boolean queries (`gif_has_media`, `gif_is_exporting`)
//!     answer 1/0 instead.

use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::Error;
use crate::export::{self, cpu_count, default_threads, resolve_threads, Progress, Settings};
use crate::ffmpeg::frames;
use crate::ffmpeg::probe::{self, MediaInfo};
use crate::ffmpeg::process::MediaTools;
use crate::gif::Dither;

/// ABI revision. Bump whenever a published struct changes layout or an
/// exported function's signature changes.
pub const ABI_VERSION: u32 = 6;

/// Status codes shared with the header.
pub const GIF_OK: c_int = 0;
pub const GIF_ERR_INVALID: c_int = -1;
pub const GIF_ERR_IO: c_int = -2;
pub const GIF_ERR_MEDIA: c_int = -3;
pub const GIF_ERR_TOOL: c_int = -4;
pub const GIF_ERR_ENCODE: c_int = -5;
pub const GIF_ERR_UNKNOWN: c_int = -6;
/// The GIF could not be written to the requested location: no permission, the
/// folder cannot be created, or the target file is in use by another program.
pub const GIF_ERR_OUTPUT: c_int = -7;

// ---------------------------------------------------------------------------
// Published structures
// ---------------------------------------------------------------------------

/// A borrowed byte buffer. The pointer is valid until the next call on the same
/// handle, and must be copied before then.
#[repr(C)]
pub struct GifCoreBuffer {
    pub data: *const u8,
    pub len: usize,
}

/// One thumbnail of the timeline filmstrip.
#[repr(C)]
pub struct GifCoreThumb {
    /// Tightly packed RGB24, `width * height * 3` bytes.
    pub data: *const u8,
    pub len: usize,
    pub width: u32,
    pub height: u32,
    /// Source timestamp of this thumbnail, in seconds.
    pub time: f64,
}

/// A filmstrip: `count` thumbnails plus the source video's frame total.
#[repr(C)]
pub struct GifCoreFilmstrip {
    pub thumbs: *const GifCoreThumb,
    pub count: u32,
    /// Total number of frames in the source video.
    pub source_frames: u64,
}

/// Results of an export.
#[repr(C)]
pub struct GifCoreExportResult {
    pub frames: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub palette_colors: u32,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Error plumbing
// ---------------------------------------------------------------------------

thread_local! {
    static LAST_ERROR: std::cell::RefCell<std::ffi::CString> =
        std::cell::RefCell::new(std::ffi::CString::new("").unwrap());
}

fn set_error(msg: &str) {
    let cleaned = msg.replace('\0', " ");
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = std::ffi::CString::new(cleaned)
            .unwrap_or_else(|_| std::ffi::CString::new("error").unwrap());
    });
}

fn clear_error() {
    set_error("");
}

fn code_of(e: &Error) -> c_int {
    match e {
        Error::Invalid(_) => GIF_ERR_INVALID,
        Error::Io(_) => GIF_ERR_IO,
        Error::Output(_) => GIF_ERR_OUTPUT,
        Error::BadMedia(_) => GIF_ERR_MEDIA,
        Error::ToolMissing(_) | Error::ToolFailed { .. } => GIF_ERR_TOOL,
        Error::Encode(_) => GIF_ERR_ENCODE,
    }
}

/// Run `f`, reporting failures through the thread-local error slot.
fn guard<F: FnOnce() -> Result<c_int, Error>>(f: F) -> c_int {
    match f() {
        Ok(code) => {
            clear_error();
            code
        }
        Err(e) => {
            set_error(&e.to_string());
            code_of(&e)
        }
    }
}

// ---------------------------------------------------------------------------
// Handle state
// ---------------------------------------------------------------------------

struct Preview {
    /// Poster frame at the current start time.
    poster: Option<frames::Frame>,
    /// Which source frame `poster` holds, so a second request for the same
    /// frame can be answered without spawning ffmpeg again.
    poster_frame: Option<i64>,
    /// Timeline thumbnails.
    strip: Vec<frames::Frame>,
    /// Source time of each thumbnail.
    strip_times: Vec<f64>,
    /// C-compatible thumb descriptors, rebuilt whenever the strip changes.
    thumbs: Vec<GifCoreThumb>,
    source_frames: u64,
}

impl Default for Preview {
    fn default() -> Self {
        Preview {
            poster: None,
            poster_frame: None,
            strip: Vec::new(),
            strip_times: Vec::new(),
            thumbs: Vec::new(),
            source_frames: 0,
        }
    }
}

struct State {
    tools: MediaTools,
    info: Option<MediaInfo>,
    /// Selection in seconds.
    start: f64,
    end: f64,
    cached_error: std::ffi::CString,
    preview: Preview,
    strip_pixels: Vec<u8>,
}

/// The handle handed to C++.
pub struct GifCore {
    state: Mutex<State>,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    running: AtomicBool,
    last_result: Mutex<Option<export::Outcome>>,
    export_error: Mutex<Option<String>>,
}

/// Check a pointer handed in from C++.
unsafe fn as_handle<'a>(h: *mut GifCore) -> Result<&'a GifCore, Error> {
    if h.is_null() {
        return Err(Error::Invalid("null handle".into()));
    }
    Ok(unsafe { &*h })
}

fn to_str(p: *const c_char) -> Result<String, Error> {
    if p.is_null() {
        return Err(Error::Invalid("null string argument".into()));
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// The ABI revision this library was built with.
#[unsafe(no_mangle)]
pub extern "C" fn gif_abi_version() -> u32 {
    ABI_VERSION
}

/// Core version string, e.g. `gifcore 1.0.0`. Static, never freed.
#[unsafe(no_mangle)]
pub extern "C" fn gif_version() -> *const c_char {
    static VER: &str = concat!("gifcore ", env!("CARGO_PKG_VERSION"), "\0");
    VER.as_ptr() as *const c_char
}

/// Integrity level of *this* process: 0 unknown, 1 low, 2 medium, 3 high,
/// 4 system.
///
/// The front end needs this to explain a refused write honestly. Windows will
/// not let a Low-integrity process write to any object that is not itself
/// Low-labelled, so a sandboxed launcher makes *every* ordinary folder answer
/// `拒绝访问` — `C:\Users\…\Videos`, `D:\CODE`, the desktop — however permissive
/// its ACL is. Without this number that symptom is indistinguishable from "this
/// folder has no write permission", which sends the user hunting for a
/// permission problem that does not exist.
#[unsafe(no_mangle)]
pub extern "C" fn gif_integrity_level() -> c_int {
    crate::platform::integrity().code()
}

/// Logical CPUs available to this process, read at call time.
///
/// Exposed so the front end can present the thread choice without guessing:
/// the number is a property of the machine, not of the UI.
#[unsafe(no_mangle)]
pub extern "C" fn gif_cpu_count() -> u32 {
    cpu_count()
}

/// Worker count used when [`gif_export`] is passed `threads = 0`: half the CPUs.
#[unsafe(no_mangle)]
pub extern "C" fn gif_default_threads() -> u32 {
    default_threads()
}

/// Clamp a requested worker count the same way [`gif_export`] does, so the UI
/// can show what will actually be used.
#[unsafe(no_mangle)]
pub extern "C" fn gif_resolve_threads(requested: u32) -> u32 {
    resolve_threads(requested)
}

/// Create a handle. Returns null only if allocation fails.
#[unsafe(no_mangle)]
pub extern "C" fn gif_create() -> *mut GifCore {
    let core = Box::new(GifCore {
        state: Mutex::new(State {
            tools: MediaTools::discover(),
            info: None,
            start: 0.0,
            end: 0.0,
            cached_error: std::ffi::CString::new("").unwrap(),
            preview: Preview::default(),
            strip_pixels: Vec::new(),
        }),
        progress: Arc::new(Progress::new()),
        cancel: Arc::new(AtomicBool::new(false)),
        running: AtomicBool::new(false),
        last_result: Mutex::new(None),
        export_error: Mutex::new(None),
    });
    Box::into_raw(core)
}

/// Destroy a handle. Safe to call with null.
#[unsafe(no_mangle)]
pub extern "C" fn gif_destroy(h: *mut GifCore) {
    if h.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(h));
    }
}

/// Message from the most recent failing call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn gif_last_error(h: *mut GifCore) -> *const c_char {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return std::ptr::null();
    };
    let mut st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    LAST_ERROR.with(|e| {
        let msg = e.borrow().clone();
        st.cached_error = msg;
    });
    st.cached_error.as_ptr()
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Override the ffmpeg location (used by tests and advanced setups).
#[unsafe(no_mangle)]
pub extern "C" fn gif_set_ffmpeg_path(h: *mut GifCore, path: *const c_char) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let p = to_str(path)?;
        let mut st = core.state.lock().map_err(lock_err)?;
        if !p.is_empty() {
            st.tools.ffmpeg = std::ffi::OsString::from(p);
        }
        Ok(GIF_OK)
    })
}

/// Override the ffprobe location.
#[unsafe(no_mangle)]
pub extern "C" fn gif_set_ffprobe_path(h: *mut GifCore, path: *const c_char) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let p = to_str(path)?;
        let mut st = core.state.lock().map_err(lock_err)?;
        if !p.is_empty() {
            st.tools.ffprobe = std::ffi::OsString::from(p);
        }
        Ok(GIF_OK)
    })
}

/// Probe the tools and return [`GIF_OK`] when both are runnable.
#[unsafe(no_mangle)]
pub extern "C" fn gif_check_tools(h: *mut GifCore) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let st = core.state.lock().map_err(lock_err)?;
        check_binary(&st.tools.ffprobe)?;
        check_binary(&st.tools.ffmpeg)?;
        Ok(GIF_OK)
    })
}

/// Run `<tool> -version` to prove the binary exists and executes.
fn check_binary(tool: &std::ffi::OsString) -> Result<(), Error> {
    let out = crate::ffmpeg::process::quiet_command(tool)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match out {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Error::ToolFailed {
            cmd: format!("{} -version", tool.to_string_lossy()),
            detail: format!("exit status {s}"),
        }),
        Err(e) => Err(match e.kind() {
            std::io::ErrorKind::NotFound => Error::ToolMissing(tool.to_string_lossy().into_owned()),
            _ => Error::Io(e.to_string()),
        }),
    }
}

fn lock_err<T>(_: T) -> Error {
    Error::Invalid("internal lock was poisoned by an earlier panic".into())
}

// ---------------------------------------------------------------------------
// Loading media
// ---------------------------------------------------------------------------

/// Open a video, probe it, and reset the selection to the whole clip.
///
/// Returns [`GIF_OK`] on success; on failure the previous media stays loaded and
/// [`gif_last_error`] explains why.
#[unsafe(no_mangle)]
pub extern "C" fn gif_load(h: *mut GifCore, path: *const c_char) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let p = to_str(path)?;
        if p.trim().is_empty() {
            return Err(Error::Invalid("no file path was given".into()));
        }
        let path = PathBuf::from(&p);
        let st_tools = {
            let st = core.state.lock().map_err(lock_err)?;
            st.tools.clone()
        };
        let info = probe::probe(&st_tools, &path)?;
        let mut st = core.state.lock().map_err(lock_err)?;
        st.start = 0.0;
        st.end = info.duration;
        st.preview = Preview::default();
        st.strip_pixels.clear();
        st.info = Some(info);
        Ok(GIF_OK)
    })
}

/// Forget the loaded media.
#[unsafe(no_mangle)]
pub extern "C" fn gif_close(h: *mut GifCore) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let mut st = core.state.lock().map_err(lock_err)?;
        st.info = None;
        st.start = 0.0;
        st.end = 0.0;
        st.preview = Preview::default();
        st.strip_pixels.clear();
        Ok(GIF_OK)
    })
}

/// Whether media is loaded.
#[unsafe(no_mangle)]
pub extern "C" fn gif_has_media(h: *mut GifCore) -> c_int {
    match unsafe { as_handle(h) } {
        Ok(core) => match core.state.lock() {
            Ok(st) => st.info.is_some() as c_int,
            Err(p) => p.into_inner().info.is_some() as c_int,
        },
        Err(_) => 0,
    }
}

macro_rules! info_getter {
    ($name:ident, $ty:ty, $field:ident, $default:expr) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(h: *mut GifCore) -> $ty {
            let Ok(core) = (unsafe { as_handle(h) }) else {
                return $default;
            };
            let st = match core.state.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            match &st.info {
                Some(i) => i.$field as $ty,
                None => $default,
            }
        }
    };
}

info_getter!(gif_width, u32, width, 0);
info_getter!(gif_height, u32, height, 0);
info_getter!(gif_fps, f64, fps, 0.0);
info_getter!(gif_fps_display, f64, fps_display, 0.0);
info_getter!(gif_duration, f64, duration, 0.0);
info_getter!(gif_frame_count, u64, frame_count, 0);
info_getter!(gif_has_audio, c_int, has_audio, 0);

/// Total frames derived from duration and rate when the container is silent.
#[unsafe(no_mangle)]
pub extern "C" fn gif_total_frames(h: *mut GifCore) -> u64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    st.info.as_ref().map(|i| i.total_frames()).unwrap_or(0)
}

/// Video codec name. Empty when nothing is loaded.
#[unsafe(no_mangle)]
pub extern "C" fn gif_video_codec(h: *mut GifCore) -> *const c_char {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return std::ptr::null();
    };
    let mut st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    let s = st
        .info
        .as_ref()
        .map(|i| i.video_codec.clone())
        .unwrap_or_default();
    st.cached_error = std::ffi::CString::new(s).unwrap_or_default();
    st.cached_error.as_ptr()
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Set the selection, in seconds. Values are clamped and ordered.
#[unsafe(no_mangle)]
pub extern "C" fn gif_set_selection(h: *mut GifCore, start: f64, end: f64) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let mut st = core.state.lock().map_err(lock_err)?;
        if st.info.is_none() {
            return Err(Error::Invalid("no video is loaded".into()));
        }
        let dur = st.info.as_ref().unwrap().duration;
        let (mut a, mut b) = (start.clamp(0.0, dur), end.clamp(0.0, dur));
        if !a.is_finite() {
            a = 0.0;
        }
        if !b.is_finite() {
            b = dur;
        }
        if b < a {
            std::mem::swap(&mut a, &mut b);
        }
        st.start = a;
        st.end = b;
        Ok(GIF_OK)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn gif_selection_start(h: *mut GifCore) -> f64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0.0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    st.start
}

#[unsafe(no_mangle)]
pub extern "C" fn gif_selection_end(h: *mut GifCore) -> f64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0.0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    st.end
}

/// Selection length in seconds.
#[unsafe(no_mangle)]
pub extern "C" fn gif_selection_duration(h: *mut GifCore) -> f64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0.0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    (st.end - st.start).max(0.0)
}

/// Frame index of the current selection start.
#[unsafe(no_mangle)]
pub extern "C" fn gif_selection_start_frame(h: *mut GifCore) -> u64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    match &st.info {
        Some(i) => i.time_to_frame(st.start) as u64,
        None => 0,
    }
}

/// Frame index of the current selection end.
#[unsafe(no_mangle)]
pub extern "C" fn gif_selection_end_frame(h: *mut GifCore) -> u64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    match &st.info {
        Some(i) => i.time_to_frame(st.end) as u64,
        None => 0,
    }
}

/// How many frames an export with `fps` would write. Never zero once loaded.
#[unsafe(no_mangle)]
pub extern "C" fn gif_estimate_frames(h: *mut GifCore, fps: f64) -> u64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    if fps <= 0.0 {
        return 0;
    }
    export::planned_frames(st.start, st.end, fps)
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// Decode the frame shown at the selection start and publish it as the poster.
#[unsafe(no_mangle)]
pub extern "C" fn gif_update_poster(h: *mut GifCore) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let (tools, info, start, wanted, unchanged) = {
            let st = core.state.lock().map_err(lock_err)?;
            let info = st
                .info
                .clone()
                .ok_or_else(|| Error::Invalid("no video is loaded".into()))?;
            let start = st.start;
            let wanted = info.time_to_frame(start);
            let unchanged = st.preview.poster.is_some() && st.preview.poster_frame == Some(wanted);
            (st.tools.clone(), info, start, wanted, unchanged)
        };
        // Dragging a handle asks for this over and over with times that land on
        // the same frame. Every decode is a whole ffmpeg process, so when the
        // picture would not change, do nothing at all.
        if unchanged {
            return Ok(GIF_OK);
        }
        let frame = frames::decode_frame(&tools, PathBuf::from(&info.path).as_path(), &info, start)?;
        let mut st = core.state.lock().map_err(lock_err)?;
        st.preview.poster = Some(frame);
        st.preview.poster_frame = Some(wanted);
        Ok(GIF_OK)
    })
}

/// Poster frame pixels. Returns an empty buffer when there is no poster.
#[unsafe(no_mangle)]
pub extern "C" fn gif_poster_rgb(h: *mut GifCore) -> GifCoreBuffer {
    let empty = GifCoreBuffer {
        data: std::ptr::null(),
        len: 0,
    };
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return empty;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    match &st.preview.poster {
        Some(f) => GifCoreBuffer {
            data: f.rgb.as_ptr(),
            len: f.rgb.len(),
        },
        None => empty,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn gif_poster_width(h: *mut GifCore) -> u32 {
    poster_dim(h, true)
}

#[unsafe(no_mangle)]
pub extern "C" fn gif_poster_height(h: *mut GifCore) -> u32 {
    poster_dim(h, false)
}

fn poster_dim(h: *mut GifCore, width: bool) -> u32 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    match &st.preview.poster {
        Some(f) => {
            if width {
                f.w
            } else {
                f.h
            }
        }
        None => 0,
    }
}

/// Decode `count` timeline thumbnails, each at most `thumb_w` pixels wide.
#[unsafe(no_mangle)]
pub extern "C" fn gif_build_filmstrip(h: *mut GifCore, count: u32, thumb_w: u32) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let (tools, info) = {
            let st = core.state.lock().map_err(lock_err)?;
            let info = st
                .info
                .clone()
                .ok_or_else(|| Error::Invalid("no video is loaded".into()))?;
            (st.tools.clone(), info)
        };
        let strip = frames::filmstrip(
            &tools,
            PathBuf::from(&info.path).as_path(),
            &info,
            count,
            thumb_w,
        )?;

        let mut st = core.state.lock().map_err(lock_err)?;
        // Keep the pixel bytes alive in one contiguous allocation so the thumb
        // descriptors stay valid until the next rebuild.
        st.strip_pixels.clear();
        let total: usize = strip.thumbs.iter().map(|f| f.rgb.len()).sum();
        st.strip_pixels.reserve(total);
        let mut offsets = Vec::with_capacity(strip.thumbs.len());
        for f in &strip.thumbs {
            offsets.push(st.strip_pixels.len());
            st.strip_pixels.extend_from_slice(&f.rgb);
        }
        let base = st.strip_pixels.as_ptr();
        let thumbs: Vec<GifCoreThumb> = strip
            .thumbs
            .iter()
            .enumerate()
            .map(|(i, f)| GifCoreThumb {
                data: unsafe { base.add(offsets[i]) },
                len: f.rgb.len(),
                width: f.w,
                height: f.h,
                time: strip.times.get(i).copied().unwrap_or(0.0),
            })
            .collect();

        st.preview.source_frames = info.total_frames();
        st.preview.strip_times = strip.times.clone();
        st.preview.thumbs = thumbs;
        st.preview.strip = strip.thumbs;
        Ok(GIF_OK)
    })
}

/// The filmstrip built by [`gif_build_filmstrip`].
#[unsafe(no_mangle)]
pub extern "C" fn gif_filmstrip(h: *mut GifCore) -> GifCoreFilmstrip {
    let empty = GifCoreFilmstrip {
        thumbs: std::ptr::null(),
        count: 0,
        source_frames: 0,
    };
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return empty;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    GifCoreFilmstrip {
        thumbs: st.preview.thumbs.as_ptr(),
        count: st.preview.thumbs.len() as u32,
        source_frames: st.preview.source_frames,
    }
}

/// Pixels of thumbnail `i`, as a detached copy.
#[unsafe(no_mangle)]
pub extern "C" fn gif_thumb_rgb(h: *mut GifCore, index: u32) -> GifCoreBuffer {
    let empty = GifCoreBuffer {
        data: std::ptr::null(),
        len: 0,
    };
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return empty;
    };
    let st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    match st.preview.strip.get(index as usize) {
        Some(f) => GifCoreBuffer {
            data: f.rgb.as_ptr(),
            len: f.rgb.len(),
        },
        None => empty,
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Check that a GIF could actually be written to `out_path`.
///
/// The front end calls this while the user is choosing where to save, so the
/// answer to "why can't I save there?" arrives immediately instead of after a
/// full export. Returns [`GIF_OK`] when the location is usable, otherwise
/// [`GIF_ERR_OUTPUT`] with the reason in [`gif_last_error`].
///
/// Needs no loaded media: it only looks at the filesystem.
#[unsafe(no_mangle)]
pub extern "C" fn gif_check_output(h: *mut GifCore, out_path: *const c_char) -> c_int {
    guard(|| {
        let _core = unsafe { as_handle(h) }?;
        let text = to_str(out_path)?;
        if text.is_empty() {
            return Err(Error::Output(
                "a save folder and file name are both required".into(),
            ));
        }
        export::check_output(std::path::Path::new(&text))?;
        clear_error();
        Ok(GIF_OK)
    })
}

/// Run an export synchronously on the calling thread.
///
/// The UI normally calls this from a worker thread and polls
/// [`gif_progress`] while it runs. Returns [`GIF_OK`] on success.
///
/// `threads` is the worker count for the palette-mapping pass; `0` means "use
/// [`gif_default_threads`]" and any value above the machine's CPU count is
/// clamped down to it.
/// Every knob of an export, so the argument list stops growing.
///
/// Zero means "use the documented default", which lets a caller memset the
/// struct and set only what it cares about.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GifCoreExportOptions {
    /// Output frame rate; `0` means 12.
    pub fps: f64,
    /// Maximum output width; `0` means 480.
    pub max_width: u32,
    /// Palette size, 2..=256; `0` means 256.
    pub colors: u32,
    /// `"none"`, `"ordered"` or `"floyd"`; `NULL` means floyd.
    pub dither: *const c_char,
    /// Playback speed multiplier; `0` means 1.0.
    pub speed: f64,
    /// Non-zero repeats the GIF forever.
    pub loop_forever: c_int,
    /// Worker threads; `0` means "half the machine".
    pub threads: u32,
    /// 「编码质量」 1..=10; `0` means 5, which is the historical behaviour.
    pub quality: u32,
    /// 「有损压缩」 0..=100: coarse colour grid before quantising.
    pub lossy: u32,
    /// 「抖动强度」 0..=100; `0` means 100.
    pub dither_strength: u32,
    /// 「复用重复帧」: fold identical consecutive frames together.
    pub dedupe: c_int,
    /// 「帧间透明优化」: leave unchanged pixels transparent.
    pub optimize: c_int,
    /// Per-channel slack, 0..=64, for the inter-frame comparison.
    pub tolerance: u32,
}

impl Default for GifCoreExportOptions {
    fn default() -> Self {
        GifCoreExportOptions {
            fps: 0.0,
            max_width: 0,
            colors: 0,
            dither: std::ptr::null(),
            speed: 0.0,
            loop_forever: 0,
            threads: 0,
            quality: 0,
            lossy: 0,
            dither_strength: 0,
            dedupe: 0,
            optimize: 0,
            tolerance: 0,
        }
    }
}

/// Result of [`gif_estimate_size`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GifCoreEstimate {
    /// Projected size of the finished GIF, in bytes.
    pub bytes: u64,
    /// Frames the export would contain.
    pub frames: u64,
    /// Frames that were really measured.
    pub sampled: u64,
    /// How many separate moments of the selection were read.
    pub segments: u64,
    pub width: u32,
    pub height: u32,
    /// Non-zero when every frame was measured, so `bytes` is exact.
    pub exact: c_int,
}

/// Turn the C options block into kernel settings, filling in defaults.
fn settings_from(
    core: &GifCore,
    info: &crate::ffmpeg::probe::MediaInfo,
    output: PathBuf,
    start: f64,
    end: f64,
    o: &GifCoreExportOptions,
) -> Result<Settings, Error> {
    let dither_mode = if o.dither.is_null() {
        Dither::Floyd
    } else {
        Dither::from_str(&to_str(o.dither)?)
    };
    Ok(Settings {
        input: PathBuf::from(&info.path),
        output,
        start,
        end,
        fps: if o.fps > 0.0 { o.fps } else { 12.0 },
        max_width: if o.max_width > 0 { o.max_width } else { 480 },
        speed: if o.speed > 0.0 { o.speed } else { 1.0 },
        colors: if o.colors > 0 { o.colors } else { 256 },
        dither: dither_mode,
        loop_forever: o.loop_forever != 0,
        overwrite: true,
        max_threads: resolve_threads(o.threads),
        quality: if o.quality > 0 {
            o.quality
        } else {
            export::DEFAULT_QUALITY
        },
        lossy: o.lossy.min(100),
        dither_strength: if o.dither_strength == 0 {
            100
        } else {
            o.dither_strength.min(100)
        },
        dedupe: o.dedupe != 0,
        optimize: o.optimize != 0,
        tolerance: o.tolerance.min(64),
        cancel: Some(core.cancel.clone()),
    })
}

/// Copy the caller's options, tolerating a null pointer.
fn read_options(opts: *const GifCoreExportOptions) -> GifCoreExportOptions {
    if opts.is_null() {
        GifCoreExportOptions::default()
    } else {
        // A plain C struct of scalars and one borrowed string pointer; copying
        // it is the whole contract.
        unsafe { std::ptr::read(opts) }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn gif_export(
    h: *mut GifCore,
    out_path: *const c_char,
    fps: f64,
    max_width: u32,
    colors: u32,
    dither: *const c_char,
    speed: f64,
    loop_forever: c_int,
    threads: u32,
) -> c_int {
    let options = GifCoreExportOptions {
        fps,
        max_width,
        colors,
        dither,
        speed,
        loop_forever,
        threads,
        ..GifCoreExportOptions::default()
    };
    gif_export_ex(h, out_path, &options)
}

/// Clears the "busy" flag however the operation ends — including the early `?`
/// returns below and a panic caught by [`guard`]. Storing `false` by hand after
/// the work missed those paths, so one bad call left `gif_is_exporting`
/// reporting `1` for the rest of the session.
struct Running<'a>(&'a AtomicBool);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// [`gif_export`], with the advanced settings included.
#[unsafe(no_mangle)]
pub extern "C" fn gif_export_ex(
    h: *mut GifCore,
    out_path: *const c_char,
    opts: *const GifCoreExportOptions,
) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        let out = to_str(out_path)?;
        if out.trim().is_empty() {
            return Err(Error::Invalid("no output file was chosen".into()));
        }
        let options = read_options(opts);

        core.cancel.store(false, Ordering::SeqCst);
        core.running.store(true, Ordering::SeqCst);
        let _running = Running(&core.running);
        *core.export_error.lock().map_err(lock_err)? = None;

        let result = {
            let st = core.state.lock().map_err(lock_err)?;
            let info = st
                .info
                .clone()
                .ok_or_else(|| Error::Invalid("no video is loaded".into()))?;
            let settings = settings_from(
                core,
                &info,
                PathBuf::from(&out),
                st.start,
                st.end,
                &options,
            )?;
            let tools = st.tools.clone();
            drop(st);
            export::export(&tools, &settings, &core.progress)
        };

        match result {
            Ok(outcome) => {
                *core.last_result.lock().map_err(lock_err)? = Some(outcome);
                Ok(GIF_OK)
            }
            Err(e) => {
                core.progress.finish(export::PHASE_FAILED);
                *core.export_error.lock().map_err(lock_err)? = Some(e.to_string());
                Err(e)
            }
        }
    })
}

/// Project the size of an export without writing it.
///
/// Runs the real pipeline over a short window of the selection — so the number
/// reflects the dither, the lossy pre-pass, the inter-frame optimisation and
/// the palette size — and extrapolates. Meant to be called from a worker
/// thread: it decodes video and blocks for a fraction of a second.
#[unsafe(no_mangle)]
pub extern "C" fn gif_estimate_size(
    h: *mut GifCore,
    opts: *const GifCoreExportOptions,
    out: *mut GifCoreEstimate,
) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        if out.is_null() {
            return Err(Error::Invalid("no estimate buffer was given".into()));
        }
        let options = read_options(opts);

        // An estimate is short, so it is never cancelled from underneath; the
        // front end simply ignores a result it has already superseded.
        core.cancel.store(false, Ordering::SeqCst);
        core.running.store(true, Ordering::SeqCst);
        let _running = Running(&core.running);
        *core.export_error.lock().map_err(lock_err)? = None;

        let result = {
            let st = core.state.lock().map_err(lock_err)?;
            let info = st
                .info
                .clone()
                .ok_or_else(|| Error::Invalid("no video is loaded".into()))?;
            let settings = settings_from(
                core,
                &info,
                PathBuf::new(),
                st.start,
                st.end,
                &options,
            )?;
            let tools = st.tools.clone();
            drop(st);
            export::estimate(&tools, &settings, &core.progress)
        };

        match result {
            Ok(estimate) => {
                unsafe {
                    *out = GifCoreEstimate {
                        bytes: estimate.bytes,
                        frames: estimate.frames,
                        sampled: estimate.sampled,
                        segments: estimate.segments,
                        width: estimate.width,
                        height: estimate.height,
                        exact: estimate.exact as c_int,
                    };
                }
                Ok(GIF_OK)
            }
            Err(e) => {
                core.progress.finish(export::PHASE_FAILED);
                *core.export_error.lock().map_err(lock_err)? = Some(e.to_string());
                Err(e)
            }
        }
    })
}

/// Ask a running export to stop. It is checked between frames.
#[unsafe(no_mangle)]
pub extern "C" fn gif_cancel(h: *mut GifCore) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        core.cancel.store(true, Ordering::SeqCst);
        Ok(GIF_OK)
    })
}

/// Whether an export is currently running.
#[unsafe(no_mangle)]
pub extern "C" fn gif_is_exporting(h: *mut GifCore) -> c_int {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0;
    };
    core.running.load(Ordering::SeqCst) as c_int
}

/// Progress as a fraction in 0..=1.
#[unsafe(no_mangle)]
pub extern "C" fn gif_progress(h: *mut GifCore) -> f64 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return 0.0;
    };
    core.progress.fraction()
}

/// Current phase code. See the `GIF_PHASE_*` constants in the header.
#[unsafe(no_mangle)]
pub extern "C" fn gif_progress_phase(h: *mut GifCore) -> u32 {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return export::PHASE_IDLE;
    };
    core.progress.phase_code()
}

/// Human readable phase name. Static, never freed.
#[unsafe(no_mangle)]
pub extern "C" fn gif_progress_phase_name(h: *mut GifCore) -> *const c_char {
    static NAMES: [&str; 6] = [
        "Idle\0",
        "Decoding frames\0",
        "Analysing colours\0",
        "Encoding GIF\0",
        "Done\0",
        "Failed\0",
    ];
    let code = match unsafe { as_handle(h) } {
        Ok(core) => core.progress.phase_code() as usize,
        Err(_) => 0,
    };
    NAMES.get(code).copied().unwrap_or("Idle\0").as_ptr() as *const c_char
}

/// Results of the most recent successful export.
#[unsafe(no_mangle)]
pub extern "C" fn gif_last_result(h: *mut GifCore) -> GifCoreExportResult {
    let zero = GifCoreExportResult {
        frames: 0,
        bytes: 0,
        width: 0,
        height: 0,
        fps: 0.0,
        duration: 0.0,
        palette_colors: 0,
        elapsed_ms: 0,
    };
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return zero;
    };
    let guard = match core.last_result.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match guard.as_ref() {
        Some(o) => GifCoreExportResult {
            frames: o.frames,
            bytes: o.bytes,
            width: o.width,
            height: o.height,
            fps: o.fps,
            duration: o.duration,
            palette_colors: o.palette_colors,
            elapsed_ms: o.elapsed_ms,
        },
        None => zero,
    }
}

/// A one-line summary of the last export, e.g. `120 frames · 480x270 · …`.
#[unsafe(no_mangle)]
pub extern "C" fn gif_last_result_summary(h: *mut GifCore) -> *const c_char {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return std::ptr::null();
    };
    let text = {
        let guard = match core.last_result.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.as_ref().map(|o| o.summary()).unwrap_or_default()
    };
    let mut st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    st.cached_error = std::ffi::CString::new(text).unwrap_or_default();
    st.cached_error.as_ptr()
}

/// Warning from the last export, or an empty string.
#[unsafe(no_mangle)]
pub extern "C" fn gif_last_warning(h: *mut GifCore) -> *const c_char {
    let Ok(core) = (unsafe { as_handle(h) }) else {
        return std::ptr::null();
    };
    let text = {
        let guard = match core.last_result.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .as_ref()
            .and_then(|o| o.warning.clone())
            .unwrap_or_default()
    };
    let mut st = match core.state.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    st.cached_error = std::ffi::CString::new(text).unwrap_or_default();
    st.cached_error.as_ptr()
}

/// Reset the progress meter to idle.
#[unsafe(no_mangle)]
pub extern "C" fn gif_reset_progress(h: *mut GifCore) -> c_int {
    guard(|| {
        let core = unsafe { as_handle(h) }?;
        core.progress.finish(export::PHASE_IDLE);
        core.progress.begin(export::PHASE_IDLE, 1);
        Ok(GIF_OK)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_is_safe() {
        let h = gif_create();
        assert!(!h.is_null());
        assert_eq!(gif_abi_version(), ABI_VERSION);
        assert_eq!(gif_has_media(h), 0);
        assert_eq!(gif_width(h), 0);
        assert_eq!(gif_duration(h), 0.0);
        assert!(gif_build_filmstrip(h, 8, 160) != GIF_OK);
        // The error slot must be readable after a failure.
        let msg = unsafe { std::ffi::CStr::from_ptr(gif_last_error(h)) };
        assert!(!msg.to_bytes().is_empty());
        gif_destroy(h);
        // Destroying null is a no-op.
        gif_destroy(std::ptr::null_mut());
    }

    #[test]
    fn selection_requires_media() {
        let h = gif_create();
        assert_ne!(gif_set_selection(h, 1.0, 2.0), GIF_OK);
        assert_eq!(gif_selection_duration(h), 0.0);
        gif_destroy(h);
    }

    #[test]
    fn null_handle_is_tolerated() {
        let n = std::ptr::null_mut();
        assert_eq!(gif_has_media(n), 0);
        assert_eq!(gif_duration(n), 0.0);
        assert_eq!(gif_progress(n), 0.0);
        gif_destroy(n);
    }

    #[test]
    fn progress_helpers() {
        let h = gif_create();
        gif_reset_progress(h);
        assert_eq!(gif_progress(h), 0.0);
        let name = unsafe { std::ffi::CStr::from_ptr(gif_progress_phase_name(h)) };
        assert!(!name.to_bytes().is_empty());
        gif_destroy(h);
    }

    #[test]
    fn a_failed_call_does_not_leave_the_kernel_busy() {
        let h = gif_create();
        // Both entry points raise the busy flag before they look at the state,
        // so every early return has to lower it again. When it stayed up,
        // gif_is_exporting() reported 1 for the rest of the session.
        let out = b"out.gif\0";
        assert_ne!(
            gif_export(
                h,
                out.as_ptr() as *const c_char,
                12.0,
                480,
                256,
                std::ptr::null(),
                1.0,
                1,
                1
            ),
            GIF_OK
        );
        assert_eq!(gif_is_exporting(h), 0);

        let mut estimate = GifCoreEstimate::default();
        assert_ne!(
            gif_estimate_size(h, std::ptr::null(), &mut estimate),
            GIF_OK
        );
        assert_eq!(gif_is_exporting(h), 0);
        gif_destroy(h);
    }

    #[test]
    fn version_string_is_static() {
        let v = unsafe { std::ffi::CStr::from_ptr(gif_version()) };
        let s = v.to_str().unwrap();
        assert!(s.starts_with("gifcore "));
    }
}
