//! # `forge` — the Rust entry point you press **Run** on
//!
//! Two jobs, in order:
//!
//! 1. **Start the kernel.** `gifcore` is brought up the way the shipped
//!    application brings it up: the media tools are resolved, a kernel handle is
//!    created through the C ABI declared in `include/gifcore.h`, and that handle
//!    is asked to verify its tools and report its thread budget. With
//!    `--probe <video>` a real probe additionally runs through the kernel.
//! 2. **Find the shell.** Not a trivial step in this environment, because of
//!    Windows' integrity levels: a process takes its integrity level from the
//!    label on the **image file** it is created from, and a Low process may then
//!    write only to locations that carry the matching
//!    `Mandatory Label\Low Mandatory Level` label. Ordinary folders count as
//!    Medium and answer `拒绝访问。 (os error 5)`. Measured here, changing one
//!    thing at a time — the application itself was never at fault:
//!
//!    ```text
//!    b\bundle\forge.exe, labelled Low  -> 完整性 low,    export to D:\CODE refused
//!    the same file, relabelled Medium  -> 完整性 medium, export ok
//!    System32\whoami.exe copied into a Low folder -> Low
//!    that copy, relabelled Medium                 -> Medium
//!    ```
//!
//!    The launching shell only sets a ceiling; it does not change the file's own
//!    label. The failure this project actually hit was exactly that ceiling, in
//!    the shape CLion's Run button builds: a Low *parent* cannot create a Medium
//!    child, so once `forge` came out Low, the shell it started was Low too, no
//!    matter that `gifforge.exe` itself was labelled Medium.
//!
//!    The fix is to keep every build directory at ordinary (Medium) integrity, so
//!    nothing is ever *born* Low:
//!
//!    * `b`, `target`, `.rustup_local` and the deployment folder carry no Low
//!      label. New files inherit the label of the directory holding them, which is
//!      why a Low build tree produced Low binaries.
//!    * The build's final `icacls` step re-stamps `gifforge.exe`, `forge.exe` and
//!      the bundled media tools with `M` as a second line of defence.
//!
//!    A Low process cannot raise its own label — the privilege that would take is
//!    precisely what a sandbox withholds — so a program that somehow starts Low
//!    stays Low; the UI says so with the amber 「沙箱运行 · 导出受限」 chip and the
//!    only way out is to start it from Explorer, the Start menu, or a normal
//!    terminal. `relocate.ps1` repairs labels that were lost by copying or moving.
//!
//!    This is not a CLion setting and CLion needs no configuration.
//! 3. **Pull up the shell.** `gifforge.exe` — the C++/QML front end — is located,
//!    started, and waited for; its exit code becomes ours.
//!
//! These are one program at build time: the shell links this very kernel as a
//! static library. So the launcher is a convenient, self-checking way in, not a
//! broker — the shell runs its own kernel instance and never talks back to this
//! process.
//!
//! ```text
//! forge [--shell <gifforge.exe>] [--dest <dir>] [--probe <video>]
//!       [--no-shell] [--publish] [--publish-only] [--help]
//!       [-- <arguments for the shell>]
//! ```
//!
//! Anything the launcher does not recognise is forwarded to the shell, so
//! `forge --open clip.mp4 --export out.gif --threads 8` works as you would
//! expect.

use std::env;
use std::ffi::{CStr, CString, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use gifcore::ffi;

/// The front end this launcher starts.
const SHELL_NAME: &str = "gifforge.exe";

/// Build trees to search, relative to the package root. The bundle comes first
/// because it carries its own Qt and therefore needs nothing from `PATH`.
const SHELL_DIRS: [&str; 4] = ["b/bundle", "b", "b-debug/bundle", "b-debug"];

/// A GUI-subsystem parent that starts a console child makes Windows allocate a
/// fresh console window for it, which flashes on screen. Every child this
/// launcher starts is a helper whose output nobody wants to read.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut shell_arg: Option<PathBuf> = None;
    let mut dest_arg: Option<PathBuf> = None;
    let mut probe_arg: Option<PathBuf> = None;
    let mut no_shell = false;
    let mut do_publish = false;
    let mut publish_only = false;
    let mut forwarded: Vec<String> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--shell" if i + 1 < argv.len() => {
                shell_arg = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--dest" if i + 1 < argv.len() => {
                dest_arg = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--probe" if i + 1 < argv.len() => {
                probe_arg = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--no-shell" => {
                no_shell = true;
                i += 1;
            }
            "--publish" => {
                do_publish = true;
                i += 1;
            }
            "--publish-only" => {
                do_publish = true;
                publish_only = true;
                i += 1;
            }
            "--" => {
                forwarded.extend_from_slice(&argv[i + 1..]);
                break;
            }
            other => {
                forwarded.push(other.to_string());
                i += 1;
            }
        }
    }

    println!("Easy2GIF — Rust 入口 (forge)");

    // Launching the shell the build produced is the normal path. Whether that
    // shell may write outside the project depends on the token this process was
    // started with, not on where the bytes live -- see the module docs.
    // `--publish` is only for users who want a clean copy somewhere else.
    let source = locate_shell(shell_arg.as_deref());
    let (shell, published, publish_error) = publish_shell(source, dest_arg, do_publish);
    if publish_only {
        if let Some(err) = &publish_error {
            println!("  发布      失败：{err}");
            std::process::exit(4);
        }
        if !published {
            println!("  发布      没有发布任何东西（用 --publish 才会发布）");
        }
        return;
    }

    // Locating the shell first also tells us where the media engine that ships
    // with it lives, so the self-check below reports the engine the app will
    // really use rather than whatever happens to be on PATH.
    let engine_dir = shell
        .as_ref()
        .and_then(|path| path.parent())
        .map(|dir| dir.join("bin"));

    if !bring_up_kernel(probe_arg.as_deref(), engine_dir.as_deref()) {
        println!("\n内核自检未通过，正在终止。");
        std::process::exit(2);
    }
    if no_shell {
        return;
    }

    let Some(shell) = shell else {
        println!("  外壳      未找到 {SHELL_NAME}");
        println!("             先构建一次：");
        println!("               cmake --preset mingw-release");
        println!("               cmake --build --preset mingw-release-bundle");
        println!("             也可以用 --shell <路径> 指定，或加 --no-shell 只跑内核自检。");
        std::process::exit(3);
    };

    println!(
        "  外壳      {}{}",
        shell.display(),
        if published {
            "（已发布副本）"
        } else {
            "（项目内，就地启动）"
        }
    );
    if !forwarded.is_empty() {
        println!("  参数      {}", forwarded.join(" "));
    }
    println!("  已启动，等待退出…");
    let code = launch_shell(&shell, &forwarded);
    println!("  外壳退出  代码 {code}");
    std::process::exit(code);
}

/// Copies the bundle to a clean location and returns the shell to run, whether it
/// was published, and why it failed.
///
/// The copy is handed to `robocopy.exe` rather than done with `std::fs` only so
/// that `/E` skips files whose size and timestamp already match. Do **not**
/// expect the system tool to escape the restriction: a Low integrity process may
/// only write to Low-labelled locations, and **every child it starts inherits
/// that level** — `robocopy.exe` included, even though its own image lives under
/// `System32`. Measured, not guessed: robocopy started from a Low `forge` reports
/// `错误 5 … 拒绝访问` and exits 16. It works once this file carries the `M`
/// label, which the build's final `icacls` step and `relocate.ps1` arrange; when
/// it fails, the failure is reported and we fall back to starting the in-project
/// shell in place.
fn publish_shell(
    source: Option<PathBuf>,
    dest_arg: Option<PathBuf>,
    publish_enabled: bool,
) -> (Option<PathBuf>, bool, Option<String>) {
    if !publish_enabled {
        return (source, false, None);
    }
    let Some(source) = source else {
        return (None, false, None);
    };
    let Some(source_dir) = source.parent() else {
        return (Some(source), false, None);
    };
    let Some(dest) = dest_arg.or_else(default_dest) else {
        println!("  发布      跳过（环境里没有 LOCALAPPDATA，用 --dest 指定位置）");
        return (Some(source), false, None);
    };

    match publish(source_dir, &dest) {
        Ok(()) => {
            let target = dest.join(SHELL_NAME);
            if target.is_file() {
                println!("  发布      {}", dest.display());
                (Some(target), true, None)
            } else {
                let err = format!("复制完成，但 {} 不在", target.display());
                println!("  发布      {err}");
                (Some(source), false, Some(err))
            }
        }
        Err(err) => {
            println!("  发布      失败：{err}");
            println!("            改成就地启动；导出到项目目录之外会被系统拒绝。");
            (Some(source), false, Some(err))
        }
    }
}

fn default_dest() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("GifForge"))
}

fn publish(source_dir: &Path, dest: &Path) -> Result<(), String> {
    if !source_dir.is_dir() {
        return Err(format!("{} 不是目录", source_dir.display()));
    }
    let tool = robocopy();
    if !tool.is_file() {
        return Err(format!("找不到 {}", tool.display()));
    }

    let mut command = Command::new(&tool);
    command
        .arg(source_dir)
        .arg(dest)
        // `/R:1 /W:1` rather than robocopy's default million retries: when the
        // destination is refused, waiting 30 s per attempt per file just makes
        // the failure slow to report.
        .args(["/E", "/R:1", "/W:1", "/NFL", "/NDL", "/NJH", "/NJS", "/NP"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // robocopy treats 0-7 as success and only 8+ as a real failure.
    let status = command
        .status()
        .map_err(|e| format!("跑 {} 失败：{e}", tool.display()))?;
    match status.code() {
        Some(code) if code < 8 => Ok(()),
        Some(code) => Err(format!("{} 退出码 {code}", tool.display())),
        None => Err(format!("{} 被中断", tool.display())),
    }
}

fn robocopy() -> PathBuf {
    let root = env::var_os("SystemRoot").unwrap_or_else(|| OsString::from("C:\\Windows"));
    PathBuf::from(root).join("System32").join("robocopy.exe")
}

/// Resolves the tools, brings up a kernel handle through the C ABI and reports
/// what it finds. Returns whether the kernel considers itself usable.
fn bring_up_kernel(probe: Option<&Path>, engine_dir: Option<&Path>) -> bool {
    let version = unsafe { CStr::from_ptr(ffi::gif_version()) };
    println!(
        "  内核      {} · ABI {}",
        version.to_string_lossy(),
        ffi::gif_abi_version()
    );

    let mut tools = gifcore::MediaTools::discover();
    tools.ffmpeg = prefer_engine(&tools.ffmpeg, engine_dir, "ffmpeg");
    tools.ffprobe = prefer_engine(&tools.ffprobe, engine_dir, "ffprobe");
    println!("  媒体引擎  {}", tools.ffmpeg.to_string_lossy());
    println!("            {}", tools.ffprobe.to_string_lossy());

    let cores = ffi::gif_cpu_count();
    let automatic = ffi::gif_default_threads();
    println!(
        "  线程      {cores} 逻辑核 · 自动 = {automatic} · 请求 8 → {}",
        ffi::gif_resolve_threads(8)
    );

    // Scratch space is the one thing the kernel cannot work without, and a
    // locked-down %TEMP% is the usual reason a load fails with "access denied",
    // so it is worth naming out loud here.
    match gifcore::scratch_dir() {
        Ok(dir) => println!("  暂存目录  {}", dir.display()),
        Err(e) => {
            println!("  暂存目录  不可用：{e}");
            return false;
        }
    }

    // A Low-integrity token — what a sandboxed or otherwise restricted launcher
    // hands its children — makes Windows refuse every write to an ordinary
    // folder, whatever that folder's ACL says, because the mandatory check runs
    // before the discretionary one. That turns every export into "拒绝访问" for a
    // reason nothing in the file system explains, so name it here. It is not
    // fatal: reading still works, and the shell can still write inside the
    // sandbox.
    match gifcore::platform::integrity() {
        gifcore::Integrity::Low => {
            println!("  完整性    低（沙箱）—— 导出到普通文件夹会被系统拒绝");
            println!("            想正常导出，请从资源管理器 / 开始菜单启动本程序");
        }
        level => println!("  完整性    {}", level.label()),
    }

    // Same entry point the shell uses, so a failure here is a failure there.
    let handle = ffi::gif_create();
    if handle.is_null() {
        println!("  句柄      创建失败");
        return false;
    }

    // Pin the handle to the paths we just resolved so the check below exercises
    // this exact pair of binaries.
    if let (Ok(ffmpeg), Ok(ffprobe)) = (
        CString::new(tools.ffmpeg.to_string_lossy().as_bytes()),
        CString::new(tools.ffprobe.to_string_lossy().as_bytes()),
    ) {
        unsafe { ffi::gif_set_ffmpeg_path(handle, ffmpeg.as_ptr()) };
        unsafe { ffi::gif_set_ffprobe_path(handle, ffprobe.as_ptr()) };
    }

    let mut healthy = true;
    match unsafe { ffi::gif_check_tools(handle) } {
        0 => println!("  工具自检  就绪"),
        _ => {
            healthy = false;
            println!("  工具自检  失败：{}", last_error(handle));
        }
    }

    if let Some(path) = probe {
        match gifcore::probe(&tools, path) {
            Ok(info) => println!(
                "  探测      {} · {}x{} · {} fps · {:.3} s · {} 帧 · {}",
                info.path,
                info.width,
                info.height,
                info.fps_display,
                info.duration,
                info.total_frames(),
                info.video_codec
            ),
            Err(err) => {
                healthy = false;
                println!("  探测      失败：{err}");
            }
        }
    }

    unsafe { ffi::gif_destroy(handle) };
    healthy
}

/// The engine shipped beside the shell is the one the app will really use, so
/// prefer it. Otherwise turn a bare program name into the absolute path that
/// `PATH` resolves to, so the report names a concrete binary.
fn prefer_engine(program: &OsString, engine_dir: Option<&Path>, name: &str) -> OsString {
    if let Some(dir) = engine_dir {
        let mut candidate = dir.join(name);
        candidate.set_extension("exe");
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }

    let path = Path::new(program);
    if path.components().count() > 1 {
        return program.clone(); // already an explicit path
    }
    let Some(dirs) = env::var_os("PATH") else {
        return program.clone();
    };
    for dir in env::split_paths(&dirs) {
        let mut candidate = dir.join(program);
        if !candidate.is_file() {
            candidate.set_extension("exe");
        }
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }
    program.clone()
}

/// Runs the front end and waits for it. The shell is a GUI-subsystem binary, so
/// without waiting the exit code would be meaningless.
fn launch_shell(shell: &Path, forwarded: &[String]) -> i32 {
    let mut command = Command::new(shell);
    command.args(forwarded);
    if let Some(path) = child_path(shell) {
        command.env("PATH", path);
    }
    match command.status() {
        Ok(status) => status.code().unwrap_or(0),
        Err(err) => {
            println!("  启动失败  {err}");
            1
        }
    }
}

/// `None` means "inherit `PATH` unchanged", which is right for the bundle.
fn child_path(shell: &Path) -> Option<OsString> {
    let dir = shell.parent()?;
    if dir.join("Qt6Core.dll").is_file() {
        return None; // self-contained: it carries its own Qt
    }

    let mut extra: Vec<PathBuf> = Vec::new();
    // GIFCORE_QT_BIN already points at a bin directory; the Qt variables point
    // at an installation root.
    if let Some(value) = env::var_os("GIFCORE_QT_BIN") {
        extra.push(PathBuf::from(value));
    }
    for var in ["QTDIR", "QT_ROOT"] {
        if let Some(value) = env::var_os(var) {
            extra.push(PathBuf::from(value).join("bin"));
        }
    }
    if let Some(bin) = qt_bin_near(shell) {
        extra.push(bin);
    }
    if extra.is_empty() {
        return None;
    }

    let mut joined = OsString::new();
    for dir in extra {
        joined.push(dir);
        joined.push(";");
    }
    joined.push(env::var_os("PATH").unwrap_or_default());
    Some(joined)
}

/// The build tree recorded where Qt was found when it was configured; that is
/// where the Qt DLLs live. Checked in the exe's own directory and one level up,
/// so `b/gifforge.exe` and `b/bundle/gifforge.exe` both resolve.
fn qt_bin_near(shell: &Path) -> Option<PathBuf> {
    let dir = shell.parent()?;
    qt_bin_from_cache(dir).or_else(|| dir.parent().and_then(qt_bin_from_cache))
}

fn qt_bin_from_cache(build_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(build_dir.join("CMakeCache.txt")).ok()?;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.starts_with("Qt6_DIR:") {
            continue;
        }
        // <prefix>/lib/cmake/Qt6  ->  <prefix>/bin
        let prefix = Path::new(value.trim()).parent()?.parent()?.parent()?;
        let bin = prefix.join("bin");
        if bin.is_dir() {
            return Some(bin);
        }
    }
    None
}

/// `--shell` wins; otherwise look next to this executable and then in the CMake
/// build trees. Those trees are anchored at compile time, so the working
/// directory does not matter.
fn locate_shell(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.is_file().then(|| path.to_path_buf());
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(SHELL_NAME));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for sub in SHELL_DIRS {
        // Pushed component by component so the result uses native separators
        // throughout instead of mixing the `/` from the literal.
        let mut dir = root.to_path_buf();
        for part in Path::new(sub).components() {
            dir.push(part);
        }
        candidates.push(dir.join(SHELL_NAME));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn last_error(handle: *mut ffi::GifCore) -> String {
    let ptr = unsafe { ffi::gif_last_error(handle) };
    if ptr.is_null() {
        return "(内核未提供错误信息)".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn print_help() {
    println!(
        "\
forge — 先跑内核自检，再拉起外壳（gifforge.exe）

用法：
  forge [选项] [外壳参数…]

选项：
  --shell <路径>   指定 gifforge.exe 的来源（默认在 b/bundle、b/ 等目录里找）
  --dest <目录>    发布到哪个目录（默认 %LOCALAPPDATA%\\GifForge）
  --publish        先把外壳复制到 --dest 再启动（默认不复制）
  --publish-only   只发布，不启动（发布失败时退出码 4）
  --probe <视频>   额外让内核探测一个视频并打印结果
  --no-shell       只跑内核自检，不启动外壳
  --               后面的参数原样传给外壳
  -h, --help       显示这段帮助

关于「拒绝访问」：这是完整性级别（integrity level）的问题。Windows 是看 **exe 文件
自己带的标签** 来决定进程身份的：带「低」标签的 exe 只会跑成低完整性进程，而低完整性
进程只能往同样带「低」标签的位置写，普通目录（视频、图片、桌面、D:\\CODE …）一律被拒，
报 os error 5。启动它的 shell 只给出上限，不会改变文件自己的标签。
所以构建目录和程序都必须是不带「低」标签的普通（中）完整性：
  * b、target、.rustup_local 和部署目录都不贴 Low，这样新生成的 exe 出生就是中完整性
    （新建的文件会继承所在目录的标签，目录带 Low 的话程序就跟着 Low）；
  * 构建的最后一步会再给 gifforge.exe、forge.exe、bin\\ffmpeg.exe、bin\\ffprobe.exe
    显式贴一次 M，多一层保险。
低完整性进程**无法**把自己的标签提上去（需要的特权正是沙箱不给的那个），所以一旦程序
以低完整性跑起来，它自己是修不好的：界面顶部会出现「沙箱运行 · 导出受限」，这时请从
资源管理器或开始菜单启动它，不要从受限的终端或沙箱里启动。
relocate.ps1 是标签丢了的修复手段：它把 b、target、.rustup_local 和部署目录改回中完整性，
再给四个程序贴一次 M。相关命令是 icacls <路径> /setintegritylevel \"(OI)(CI)M\" /T /C。
这与 CLion 的设置无关 —— 但 CLion 自己是从哪里启动的很关键。

其余参数会原样转发给外壳，例如：
  forge --open clip.mp4 --export out.gif --threads 8
"
    );
}
