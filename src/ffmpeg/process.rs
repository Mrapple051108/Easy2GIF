//! Locating and driving the bundled external media tools (`ffmpeg` / `ffprobe`).
//!
//! The application never links against libav* — it *drives* the command line
//! tools that ship next to the executable. That keeps the core free of C
//! dependencies while still giving us the decoder.
//!
//! Argument lists are built with the [`ffargs!`] macro into `OsString`s, so a
//! path is never lossily re-encoded on the way to the tool.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Build an argument list, converting each element to an `OsString`.
macro_rules! ffargs {
    ($($arg:expr),* $(,)?) => {{
        let mut v: Vec<::std::ffi::OsString> = Vec::new();
        $(
            v.push(::std::ffi::OsString::from($arg));
        )*
        v
    }};
}

pub(crate) use ffargs;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Where scratch files go, memoised after the first successful resolution.
static SCRATCH_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Candidate scratch directories, most desirable first.
///
/// The kernel must not assume `%TEMP%` is usable. A locked-down machine, a
/// launcher that strips the environment, or a redirected temp directory can
/// all leave it missing or unwritable — and `create_dir_all` then fails with a
/// bare "access denied" that says nothing about what actually went wrong. That
/// is precisely the `打开视频失败：拒绝访问。 (os error 5)` a user sees when the
/// only thing broken is the temp directory, so we keep a list of places that
/// are progressively more likely to be writable.
fn scratch_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    // 1. An explicit override always wins.
    if let Some(dir) = std::env::var_os("GIFCORE_TEMP") {
        if !dir.is_empty() {
            out.push(PathBuf::from(dir));
        }
    }
    // 2. The conventional location.
    out.push(std::env::temp_dir().join("gifcore-tmp"));
    // 3. Next to the executable: whoever can run the app can normally write
    //    beside it, which covers a machine whose temp directory is locked down.
    if let Some(dir) = exe_dir() {
        out.push(dir.join("gifcore-tmp"));
    }
    // 4. Per-user application data.
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        if !base.is_empty() {
            out.push(PathBuf::from(base).join("easy2gif").join("tmp"));
        }
    }
    // 5. Wherever we happen to have been started from.
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("gifcore-tmp"));
    }
    out
}

/// Prove a directory really accepts a file.
///
/// Merely existing is not enough — it has to be writable — so every candidate
/// is probed by creating and then removing a file.
fn probe_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = TempFile::in_dir(dir, "probe")?;
    drop(probe);
    Ok(())
}

/// The first candidate that accepts a real file, or every failure.
fn pick_writable(candidates: &[PathBuf]) -> std::result::Result<PathBuf, Vec<(PathBuf, String)>> {
    let mut failures = Vec::new();
    for dir in candidates {
        match probe_writable(dir) {
            Ok(()) => return Ok(dir.clone()),
            Err(e) => failures.push((dir.clone(), e.to_string())),
        }
    }
    Err(failures)
}

fn describe_failures(failures: &[(PathBuf, String)]) -> String {
    let mut msg = String::from(
        "no writable directory for temporary files; set GIFCORE_TEMP to a writable path",
    );
    for (dir, why) in failures {
        msg.push_str(" | ");
        msg.push_str(&dir.display().to_string());
        msg.push_str(": ");
        msg.push_str(why);
    }
    msg
}

/// The scratch directory, resolved on first success and reused after that.
///
/// Failures are deliberately *not* memoised, so a user who frees up a
/// directory can simply retry instead of having to restart the application.
pub fn scratch_dir() -> Result<&'static Path> {
    if let Some(dir) = SCRATCH_DIR.get() {
        return Ok(dir.as_path());
    }
    match pick_writable(&scratch_candidates()) {
        Ok(dir) => Ok(SCRATCH_DIR.get_or_init(|| dir).as_path()),
        Err(failures) => Err(Error::Io(describe_failures(&failures))),
    }
}

/// A file in the scratch directory that removes itself on drop.
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// Atomically reserve a unique temporary file.
    pub fn new(tag: &str) -> Result<TempFile> {
        Ok(Self::in_dir(scratch_dir()?, tag)?)
    }

    pub fn in_dir(dir: &Path, tag: &str) -> std::io::Result<TempFile> {
        for _ in 0..128 {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(".gifcore-{tag}-{}-{seq}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(TempFile { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "cannot reserve a temporary file",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn path_string(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    pub fn len(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    /// Slurp the whole file.
    pub fn read_all(&self) -> Result<Vec<u8>> {
        let mut f = std::fs::File::open(&self.path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Absolute locations of the external tools, or bare names to be found on PATH.
#[derive(Clone, Debug)]
pub struct MediaTools {
    pub ffmpeg: OsString,
    pub ffprobe: OsString,
}

impl MediaTools {
    /// Resolve the tools.
    ///
    /// Search order:
    ///   1. `GIFCORE_FFMPEG` / `GIFCORE_FFPROBE` environment overrides.
    ///   2. A `bin` directory next to the running executable (the shipped bundle).
    ///   3. The directory holding the running executable itself.
    ///   4. Bare `ffmpeg` / `ffprobe`, i.e. whatever is on PATH.
    pub fn discover() -> MediaTools {
        MediaTools {
            ffmpeg: resolve_tool("GIFCORE_FFMPEG", "ffmpeg"),
            ffprobe: resolve_tool("GIFCORE_FFPROBE", "ffprobe"),
        }
    }

    /// Run `ffprobe -show_format -show_streams -of json` and return the JSON.
    pub fn probe_json(&self, input: &Path) -> Result<String> {
        self.probe_json_cancel(input, None)
    }

    pub fn probe_json_cancel(&self, input: &Path, cancel: Option<&AtomicBool>) -> Result<String> {
        let args = ffargs![
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
            input.as_os_str()
        ];
        capture_cancel(&self.ffprobe, &args, cancel)
    }

    pub fn frame_times(&self, input: &Path, stream: u32, start: f64, end: f64) -> Result<String> {
        capture_cancel(
            &self.ffprobe,
            &ffargs![
                "-v",
                "error",
                "-select_streams",
                stream.to_string(),
                "-read_intervals",
                format!("{start:.9}%{end:.9}"),
                "-show_frames",
                "-show_entries",
                "frame=best_effort_timestamp_time",
                "-of",
                "json",
                input.as_os_str()
            ],
            None,
        )
    }
}

fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.parent().map(|p| p.to_path_buf())
}

fn resolve_tool(env_key: &str, base: &str) -> OsString {
    if let Some(v) = std::env::var_os(env_key) {
        if !v.is_empty() {
            return v;
        }
    }
    let exe_name = format!("{base}.exe");
    if let Some(dir) = exe_dir() {
        // 1. <exe dir>/bin/ffmpeg.exe   (bundle layout)
        let bundled = dir.join("bin").join(&exe_name);
        if bundled.is_file() {
            return bundled.into_os_string();
        }
        // 2. <exe dir>/ffmpeg.exe
        let sibling = dir.join(&exe_name);
        if sibling.is_file() {
            return sibling.into_os_string();
        }
    }
    // 3. rely on PATH
    OsString::from(base)
}

/// Windows flag that stops a console child from allocating a console window.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a command that runs without flashing a console window.
///
/// The shell is a GUI-subsystem executable, so when it starts a *console*
/// program such as `ffmpeg` Windows allocates a brand new console for it — and
/// that shows up as a window that pops up and vanishes. Dragging a selection
/// handle re-decodes the poster, so without this flag the user gets one flash
/// per decode, which looks like the app is spawning processes in a loop.
pub(crate) fn quiet_command(program: &OsString) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn capture_cancel(
    program: &OsString,
    args: &[OsString],
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    let mut bytes = Vec::new();
    run_ffmpeg_stream_cancel(program, args, 16384, cancel, |part| {
        if bytes.len() + part.len() > 32 << 20 {
            return Err(Error::BadMedia("media metadata exceeds 32 MiB".into()));
        }
        bytes.extend_from_slice(part);
        Ok(())
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Human readable command line for error messages.
pub fn describe(program: &OsString, args: &[OsString]) -> String {
    let mut s = program.to_string_lossy().into_owned();
    for a in args {
        s.push(' ');
        let a = a.to_string_lossy();
        if a.contains(' ') {
            s.push('"');
            s.push_str(&a);
            s.push('"');
        } else {
            s.push_str(&a);
        }
    }
    s
}

/// Run ffmpeg with `-v info -vf ...showinfo`, expecting stdout redirected into
/// `stdout_path`, and return the captured stderr text plus success flag.
pub struct FfmpegRun {
    pub success: bool,
    pub stderr: String,
    pub stdout_path: PathBuf,
    _stdout: TempFile,
    _stderr: TempFile,
}

impl FfmpegRun {
    /// Read the bytes ffmpeg wrote to stdout.
    pub fn read_all(&self) -> Result<Vec<u8>> {
        std::fs::read(&self.stdout_path).map_err(|e| {
            Error::Io(format!(
                "cannot read tool output {}: {e}",
                self.stdout_path.display()
            ))
        })
    }
}

/// Run ffmpeg writing raw video to a temp file and returning captured stderr.
pub fn run_ffmpeg_to_temp(program: &OsString, args: &[OsString], tag: &str) -> Result<FfmpegRun> {
    use std::io::Write;
    let stdout = TempFile::new(tag)?;
    let stderr = TempFile::new("err")?;
    let mut file = std::fs::File::create(stdout.path())?;
    let err_text = run_ffmpeg_stream(program, args, 65536, |part| {
        file.write_all(part)?;
        Ok(())
    })?;
    Ok(FfmpegRun {
        success: true,
        stderr: err_text,
        stdout_path: stdout.path().to_path_buf(),
        _stdout: stdout,
        _stderr: stderr,
    })
}

/// Run ffmpeg with no captured output at all, only caring about success.
pub fn run_ffmpeg_quiet(program: &OsString, args: &[OsString]) -> Result<()> {
    run_ffmpeg_stream(program, args, 16384, |_| Ok(())).map(|_| ())
}

/// Run ffmpeg, handing each stdout chunk to `sink` as it arrives.
///
/// `chunk` is the natural read size (one raw frame), so the sink sees
/// frame-aligned data. stderr goes to a temp file so a chatty encoder can never
/// deadlock against an unread pipe.
pub fn run_ffmpeg_stream<F>(
    program: &OsString,
    args: &[OsString],
    chunk: usize,
    sink: F,
) -> Result<String>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    run_ffmpeg_stream_cancel(program, args, chunk, None, sink)
}

pub fn run_ffmpeg_stream_cancel<F>(
    program: &OsString,
    args: &[OsString],
    chunk: usize,
    cancel: Option<&AtomicBool>,
    mut sink: F,
) -> Result<String>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(Error::Cancelled);
    }
    let err = TempFile::new("err")?;
    let mut child = quiet_command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(err.path())?)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                Error::ToolMissing(program.to_string_lossy().into_owned())
            }
            _ => Error::Io(e.to_string()),
        })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Io("no stdout pipe".into()))?;
    let child = std::sync::Mutex::new(child);
    let finished = AtomicBool::new(false);
    let timed_out = AtomicBool::new(false);
    let activity = std::sync::Mutex::new(std::time::Instant::now());
    let read_result = std::thread::scope(|scope| {
        // A blocked read must be interruptible even when the child produces no frames.
        scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed))
                    || activity.lock().unwrap().elapsed().as_secs() >= 120
                {
                    timed_out.store(
                        !cancel.is_some_and(|c| c.load(Ordering::Relaxed)),
                        Ordering::Relaxed,
                    );
                    let _ = child.lock().unwrap().kill();
                    break;
                }
                std::thread::park_timeout(std::time::Duration::from_millis(20));
            }
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut buf = vec![0u8; chunk.max(1)];
            loop {
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return Err(Error::Cancelled);
                }
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        *activity.lock().unwrap() = std::time::Instant::now();
                        sink(&buf[..n])?;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::Io(e.to_string())),
                }
            }
            // Waiting after EOF is also interruptible; never hold the child mutex while waiting.
            loop {
                if child.lock().unwrap().try_wait()?.is_some() {
                    break;
                }
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return Err(Error::Cancelled);
                }
                std::thread::park_timeout(std::time::Duration::from_millis(10));
            }
            Ok(())
        }))
        .unwrap_or_else(|_| Err(Error::Io("media output consumer panicked".into())));
        if result.is_err() {
            let _ = child.lock().unwrap().kill();
        }
        finished.store(true, Ordering::Release);
        result
    });
    let status = child.into_inner().unwrap().wait()?;
    let err_text = String::from_utf8_lossy(&err.read_all()?).into_owned();
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(Error::Cancelled);
    }
    if timed_out.load(Ordering::Relaxed) {
        return Err(Error::ToolFailed {
            cmd: describe(program, args),
            detail: "no output for 120 seconds".into(),
        });
    }
    read_result?;

    if !status.success() {
        let detail = err_text.trim();
        let detail: String = detail
            .chars()
            .rev()
            .take(800)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return Err(Error::ToolFailed {
            cmd: describe(program, args),
            detail,
        });
    }

    Ok(err_text)
}

/// Sequentially read frame-aligned records back out of a raw file.
pub struct DataFile {
    _temp: TempFile,
    file: std::fs::File,
    frame_bytes: usize,
}

impl DataFile {
    pub fn open(temp: TempFile, frame_bytes: usize) -> Result<DataFile> {
        let file = std::fs::File::open(temp.path())?;
        Ok(DataFile {
            _temp: temp,
            file,
            frame_bytes,
        })
    }

    pub fn len(&self) -> u64 {
        self._temp.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read exactly `frame_bytes`; `Ok(false)` means a clean end of data.
    pub fn read_frame(&mut self, out: &mut [u8]) -> Result<bool> {
        let want = self.frame_bytes.min(out.len());
        let mut got = 0usize;
        while got < want {
            match self.file.read(&mut out[got..want]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e.to_string())),
            }
        }
        if got == 0 {
            return Ok(false);
        }
        if got < want {
            return Err(Error::BadMedia(format!(
                "truncated raw frame: {got} of {want} bytes"
            )));
        }
        Ok(true)
    }
}

/// Format an f64 as an ffmpeg-friendly decimal (no exponent, bounded digits).
pub fn fmt_time(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "0".to_string();
    }
    let s = format!("{:.6}", seconds.max(0.0));
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() {
        "0".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path that can never become a directory, because its parent is a file.
    /// That is a portable stand-in for a location we are not allowed to use.
    fn blocked(name: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("gifcore-proc-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_file(&root);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::write(&root, b"not a directory").expect("write the blocker file");
        (root.clone(), root.join("child"))
    }

    fn good(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("gifcore-proc-{}-{}", std::process::id(), name))
            .join("scratch")
    }

    #[test]
    fn picks_the_first_writable_candidate() {
        let dir = good("first");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(pick_writable(&[dir.clone()]).unwrap(), dir);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn skips_a_candidate_it_cannot_write() {
        // Distinct roots: `blocked` creates a *file* where `good` wants a
        // directory, so they must not share a name.
        let (root, unusable) = blocked("skip-blocked");
        let dir = good("skip-good");
        let _ = std::fs::remove_dir_all(&dir);

        let picked = pick_writable(&[unusable, dir.clone()]).expect("should fall through");
        assert_eq!(
            picked, dir,
            "an unusable candidate must be skipped, not handed back"
        );

        let _ = std::fs::remove_file(root);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn reports_every_path_it_tried_when_nothing_works() {
        let (root_a, unusable_a) = blocked("all-a");
        let (root_b, unusable_b) = blocked("all-b");

        let failures =
            pick_writable(&[unusable_a.clone(), unusable_b.clone()]).expect_err("nothing works");
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, unusable_a);
        assert_eq!(failures[1].0, unusable_b);
        for (_, why) in &failures {
            assert!(!why.is_empty(), "each failure needs a reason");
        }

        // This message is what the user sees, so it has to say what to do.
        let msg = describe_failures(&failures);
        assert!(
            msg.contains("GIFCORE_TEMP"),
            "should say how to fix it: {msg}"
        );
        assert!(msg.contains(&unusable_a.display().to_string()));
        assert!(msg.contains(&unusable_b.display().to_string()));

        let _ = std::fs::remove_file(root_a);
        let _ = std::fs::remove_file(root_b);
    }

    #[test]
    fn scratch_dir_is_writable_and_stable() {
        let dir = scratch_dir().expect("a writable scratch directory");
        assert!(dir.is_dir());
        probe_writable(dir).expect("the chosen directory really accepts a file");
        assert_eq!(dir, scratch_dir().unwrap(), "resolution is memoised");
    }

    #[test]
    fn candidates_are_absolute_and_start_with_the_conventional_location() {
        let candidates = scratch_candidates();
        assert!(!candidates.is_empty());
        assert!(candidates.iter().all(|c| c.is_absolute()));
        if std::env::var_os("GIFCORE_TEMP").is_none() {
            assert!(candidates[0].ends_with("gifcore-tmp"));
        }
    }

    #[test]
    fn temp_files_are_unique_and_removed_on_drop() {
        let a = TempFile::new("test").unwrap();
        let b = TempFile::new("test").unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(a.path().parent(), b.path().parent());
        std::fs::write(a.path(), b"payload").unwrap();
        assert_eq!(a.len(), 7);
        assert_eq!(a.read_all().unwrap(), b"payload");

        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists(), "the file removes itself on drop");
    }
}
