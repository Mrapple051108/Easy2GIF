<#
.SYNOPSIS
    Makes the built executables able to write outside this project.

.DESCRIPTION
    The cause is the *integrity level of the executable file itself*. Windows
    takes a process's integrity level from the label on the image it is created
    from, and its mandatory policy then lets a Low process write only where a
    matching Low label is present. Measured, changing one thing at a time:

        b\bundle\forge.exe, labelled Low  -> 完整性 low,    export to D:\CODE refused (os error 5)
        the same file, relabelled Medium  -> 完整性 medium, export ok
        System32\whoami.exe copied into a Low folder -> Low
        that copy, relabelled Medium                 -> Medium

    What matters just as much is what a *folder* is labelled, because a new file
    inherits the label of the directory it is created in. A Low-labelled build
    tree therefore produces Low-labelled binaries -- and a Low process may only
    write where a matching Low label is present. That is exactly how the CLion
    Run path broke: `forge.exe` was born Low inside `b`, so it ran Low, and even
    though `easy2gif.exe` itself was labelled Medium the export was still
    refused, because a Low *parent* cannot create a Medium child and the shell it
    started was Low too.

    No program can repair that for itself: raising a file's integrity label needs
    the very privilege a sandbox withholds, so a Low process stays Low.

    So the build trees must NOT be low. `b`, `target`, `.rustup_local` and the
    deployment folder are all left at ordinary (Medium) integrity:

        icacls b /setintegritylevel "(OI)(CI)M" /T /C

    Copying or moving a folder loses its label, which is the case this script
    exists to repair. Builds then have to run from a normal (Medium) context --
    a confined shell flattens the whole toolchain to Low, and a Low toolchain
    cannot write into these Medium directories at all.

    The script also puts the deployment folder outside the project behind a
    junction, so the bundle can be copied and shipped without dragging the build
    tree along. The exe keeps its project-relative path, so CLion's Run button
    and `cmake --build` are unchanged.

    What is *not* needed: the front end itself works from anywhere. Started from
    Explorer or the Start menu it is Medium, and a Medium process may write to
    ordinary unlabelled folders. A label restricts who may write *into* a folder;
    it does not travel with the executable.

    What is *not* the cause, each tested and ruled out:

      * the destination's permissions - Videos\视频素材 grants this account
        FullControl, and PowerShell/cmd write there without complaint;
      * the executable's own ACL - cutting its inherited ACEs and leaving only
        FullControl changed nothing;
      * the capability ACE the project root carries
        (S-1-4-834417400-1017309768, "Write, Delete") - folders created inside
        the project really do inherit it, but it is a *grant*, not a denial;
        removing it looked like a fix only because that run recreated the folder
        and so dropped its integrity label as well.

    The one thing that *does* matter is the label on the image file being run:
    that, and not the shell, decides the integrity level of the process, while
    the shell only sets the ceiling. So "run it from Explorer" cures a program
    that was started from a confined terminal, and a label is what cures an
    executable that was *born* Low inside a Low build tree.

    `target\release` deliberately stays a real folder. Cargo has to create its
    own lock file there (`.cargo-build-lock`) and refuses when that path goes
    through a junction; the Rust launcher is instead copied into the junctioned
    bundle by the build, where it sits next to the front end and inherits the
    same freedom.

    The script is idempotent. Run it once; run it again after moving the project.
    It is not a CLion setting and CLion needs no configuration.

.PARAMETER Store
    Where the real folders live. Defaults to %LOCALAPPDATA%\easy2gif-build.
    Must be outside the project.

.PARAMETER Root
    The project folder. Defaults to the folder this script sits in.

.PARAMETER Force
    Recreate the junctions even when they already point at the right place.

.EXAMPLE
    pwsh -File .\relocate.ps1
    pwsh -File .\relocate.ps1 -Store D:\easy2gif-build

.NOTES
    Run from a normal terminal (or CLion's Terminal). A shell that is itself
    confined to the project cannot create the store, and the script says so.
#>
[CmdletBinding()]
param(
    [string]$Root = $PSScriptRoot,
    [string]$Store,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

if (-not $Root) { throw 'Run this with -File, or pass -Root <project folder>.' }
$Root = (Resolve-Path -LiteralPath $Root).Path
if (-not $Store) { $Store = Join-Path $env:LOCALAPPDATA 'easy2gif-build' }
$Store = [IO.Path]::GetFullPath($Store)

if ($Store.StartsWith($Root + [IO.Path]::DirectorySeparatorChar, 'OrdinalIgnoreCase')) {
    throw "The store must be outside the project, but $Store is inside $Root."
}

# These are the deployment folders: the build writes the front end, the Qt DLLs
# and `forge.exe` straight into them, so they have to sit where the (Low
# integrity) toolchain is allowed to write. `target\release` is deliberately NOT
# one: cargo has to create its own lock file there (.cargo-build-lock) and
# refuses when that path goes through a junction. The Rust launcher is copied
# into the junctioned bundle by the build instead, so it sits next to the front
# end.
$Links = @(
    @{ Link = 'b\bundle'; Store = 'bundle' }   # easy2gif.exe, forge.exe, Qt DLLs, bin\ffmpeg.exe
)

Write-Host "项目    $Root"
Write-Host "实体目录 $Store"
Write-Host ''

if (-not (Test-Path -LiteralPath $Store)) {
    New-Item -ItemType Directory -Force -Path $Store | Out-Null
}
$probe = Join-Path $Store ('.relocate-probe-{0}' -f $PID)
try {
    Set-Content -LiteralPath $probe -Value 'ok' -ErrorAction Stop
    Remove-Item -LiteralPath $probe -Force
}
catch {
    throw "写不进 $Store：$($_.Exception.Message)。请从普通终端（或 CLion 的 Terminal）运行本脚本。"
}

function Get-LinkTarget([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $item) { return $null }              # missing
    if ($item.LinkType -ne 'Junction') { return '' }  # a real folder
    $target = @($item.Target)[0]
    if (-not $target) { return '' }
    return [IO.Path]::GetFullPath($target)
}

# Copying or moving a folder loses its integrity label, and a *Low* build folder
# is what turns its contents Low: a new file inherits the label of the directory
# holding it, so a Low tree yields Low executables, and a Low process is refused
# every ordinary folder with "拒绝访问。 (os error 5)". Every folder the build
# writes into is therefore put back to ordinary (Medium), which is also the only
# state in which nothing gets mislabelled by accident.
function Set-Integrity([string]$Path, [string]$Level) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    icacls $Path /setintegritylevel $Level /T /C 2>&1 | Out-Null
    Write-Host ("{0,-15} 完整性标签 {1}（普通目录）" -f (Split-Path $Path -Leaf), $Level)
}

# The deployment folder first: the store sits outside the project, and both it
# and the junctioned build folders have to be Medium.
Set-Integrity $Store '(OI)(CI)M'
# The build trees, including the toolchain directory: rustc.exe and cargo.exe
# live inside .rustup_local, which is inside this project, so if that folder is
# Low they run Low and cannot write into a Medium build tree.
foreach ($build in @('b', 'target', '.rustup_local')) { Set-Integrity (Join-Path $Root $build) '(OI)(CI)M' }

# ...and the *programs* are stamped with M explicitly as a second line of
# defence. The build does this by itself (see the integrity section of
# CMakeLists.txt); this is the repair for copies, and for what was written
# before that existed.
foreach ($exe in @('b\bundle\easy2gif.exe', 'b\bundle\forge.exe',
                   'b\bundle\bin\ffmpeg.exe', 'b\bundle\bin\ffprobe.exe')) {
    $path = Join-Path $Root $exe
    if (Test-Path -LiteralPath $path) {
        icacls $path /setintegritylevel M /C 2>&1 | Out-Null
        Write-Host ("{0,-15} 完整性标签 M（可写普通目录）" -f (Split-Path $path -Leaf))
    }
}

foreach ($entry in $Links) {
    $link = Join-Path $Root $entry.Link
    $real = Join-Path $Store $entry.Store

    if (-not $Force -and (Get-LinkTarget $link) -eq $real) {
        Write-Host ("{0,-15} 已经指向 {1}，跳过" -f $entry.Link, $real)
        continue
    }
    if (-not (Test-Path -LiteralPath $real)) {
        New-Item -ItemType Directory -Force -Path $real | Out-Null
    }

    $existing = Get-LinkTarget $link
    if ($existing -and $existing -ne $real) {
        Write-Host ("{0,-15} 删除指向 {1} 的旧 junction" -f $entry.Link, $existing)
        cmd /c "rmdir `"$link`"" | Out-Null
    }
    elseif ($existing -eq '') {
        Write-Host ("{0,-15} 是普通目录，把内容搬进 {1}" -f $entry.Link, $real)
        # /E /MOVE merges into the store and empties the source, which
        # Move-Item cannot do when the store already holds something.
        robocopy $link $real /E /MOVE /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "robocopy $link -> $real 失败，退出码 $LASTEXITCODE" }
        if (Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link -Recurse -Force }
    }
    elseif ($existing -eq $real -and $Force) {
        Write-Host ("{0,-15} 按 -Force 重建 junction" -f $entry.Link)
        cmd /c "rmdir `"$link`"" | Out-Null
    }

    cmd /c "mklink /J `"$link`" `"$real`"" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "mklink /J $link $real 失败，退出码 $LASTEXITCODE" }
    Write-Host ("{0,-15} {1}  ->  {2}" -f $entry.Link, $link, $real)
}

Write-Host ''
$ready = 0
foreach ($exe in @('b\bundle\easy2gif.exe', 'target\release\forge.exe')) {
    $full = Join-Path $Root $exe
    if (Test-Path -LiteralPath $full) {
        $parent = Split-Path $exe -Parent
        $target = Get-LinkTarget (Join-Path $Root $parent)
        Write-Host ("{0,-28} 就绪  父目录 -> {1}" -f $exe, $target)
        $ready++
    }
    else {
        Write-Host ("{0,-28} 还没构建" -f $exe)
    }
}
Write-Host ''
if ($ready -gt 0) {
    Write-Host '完成。构建目录、工具链目录和部署目录都是普通（中）完整性，新生成的程序出生就是中完整性。'
    Write-Host '        若程序仍提示「沙箱运行 · 导出受限」，说明它是从受限终端/沙箱启动的 —— 请从资源管理器启动。'
}
else {
    Write-Host '完成。先构建一次（cmake --build b --target bundle），再运行程序。'
}
