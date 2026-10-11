#requires -Version 5
<#
.SYNOPSIS
    Builds Easy2GIF: the Rust core library, the C++/QML application, and the
    self-contained bundle.

.DESCRIPTION
    The build has three moving parts that all need agreeing on a toolchain:

      * the Rust core is built for the GNU target so its staticlib can be linked
        by MinGW g++ (this machine has no MSVC compiler at all);
      * Qt here is a shared build, so the app is finished by windeployqt;
      * cargo and moc both need an environment the sandbox may not provide.

    This script sets all of that up in one shot.

    The generator is deliberately "MinGW Makefiles": Ninja hangs indefinitely
    in this environment.

.EXAMPLE
    .\build.ps1
    .\build.ps1 -FfmpegExe C:\tools\ffmpeg.exe -FfprobeExe C:\tools\ffprobe.exe
    .\build.ps1 -SkipBundle          # just the executable, no deployment
#>
[CmdletBinding()]
param(
    [string] $QtRoot     = 'D:\CODE\QT\6.10.2\mingw_64',
    [string] $MingwBin   = 'D:\CODE\QT\Tools\mingw1310_64\bin',
    [string] $CMakeBin   = 'D:\CODE\QT\Tools\CMake_64\bin',
    [string] $BuildDir   = 'b',
    [string] $FfmpegExe  = '',
    [string] $FfprobeExe = '',
    [switch] $SkipBundle,
    [switch] $Console
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
Set-Location $root

function Require-Path([string] $Path, [string] $What) {
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$What not found at '$Path'. Pass the right location as a script parameter."
    }
    return $Path
}

$cmake = Require-Path (Join-Path $CMakeBin 'cmake.exe') 'cmake'
Require-Path $MingwBin 'The MinGW toolchain directory'
Require-Path $QtRoot   'The Qt kit'

# g++ must be reachable for both cargo's link step and the C++ compile.
$env:PATH = "$MingwBin;$CMakeBin;$(Join-Path $QtRoot 'bin');$env:PATH"

# The Rust toolchain lives inside the repo because the user profile is not
# writable here. CMakeLists.txt picks these up via GIFCORE_RUSTUP_HOME.
$rustupHome = Join-Path $root '.rustup_local'
$env:RUSTUP_HOME = $rustupHome
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnu'

if (-not (Test-Path -LiteralPath (Join-Path $rustupHome 'toolchains'))) {
    Write-Warning "No Rust toolchain at $rustupHome. Install one with:"
    Write-Warning "  `$env:RUSTUP_HOME='$rustupHome'; rustup toolchain install stable-x86_64-pc-windows-gnu"
}

# Discover the media engine unless the caller pinned it. It is only ever
# invoked as a subprocess, so no headers or import libraries are needed.
# A launcher shim (the chocolatey one, for instance) is a few hundred KiB and
# resolves its real target *relative to its own location*. Copying one into the
# bundle therefore yields an engine that only works on the machine that built
# it. A genuinely self-contained engine is a static build, tens of megabytes.
$minEngineBytes = 5MB

function Resolve-MediaTool([string] $Name, [string] $Explicit) {
    if ($Explicit) {
        if (-not (Test-Path -LiteralPath $Explicit)) { throw "$Name not found at '$Explicit'." }
        return Get-Item -LiteralPath $Explicit
    }

    $candidates = @()
    $onPath = Get-Command "$Name.exe" -ErrorAction SilentlyContinue
    if ($onPath) { $candidates += $onPath.Source }
    if (Test-Path 'C:\ProgramData\chocolatey\lib') {
        $candidates += Get-ChildItem 'C:\ProgramData\chocolatey\lib' -Recurse `
            -Filter "$Name.exe" -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty FullName
    }

    foreach ($candidate in $candidates) {
        $item = Get-Item -LiteralPath $candidate -ErrorAction SilentlyContinue
        if ($item -and $item.Length -ge $minEngineBytes) { return $item }
    }

    $first = $candidates | Select-Object -First 1
    if ($first) {
        Write-Warning ("$Name at $first is only {0:N0} bytes - a shim, not a real build." -f `
                (Get-Item -LiteralPath $first).Length)
    }
    return $null
}

$ffmpegItem  = Resolve-MediaTool 'ffmpeg'  $FfmpegExe
$ffprobeItem = Resolve-MediaTool 'ffprobe' $FfprobeExe
if ($ffmpegItem -and $ffprobeItem) {
    $FfmpegExe  = $ffmpegItem.FullName
    $FfprobeExe = $ffprobeItem.FullName
    Write-Host ("Media engine: {0} ({1:N1} MiB)" -f $FfmpegExe, ($ffmpegItem.Length / 1MB)) `
        -ForegroundColor DarkGray
} else {
    Write-Warning 'No self-contained ffmpeg/ffprobe found; the bundle would not run.'
    Write-Warning 'Pass -FfmpegExe/-FfprobeExe pointing at a static build.'
}

$configureArgs = @(
    '-S', $root
    '-B', (Join-Path $root $BuildDir)
    '-G', 'MinGW Makefiles'
    '-DCMAKE_BUILD_TYPE=Release'
    "-DCMAKE_PREFIX_PATH=$QtRoot"
)
if ($FfmpegExe)  { $configureArgs += "-DGIFCORE_FFMPEG_EXE=$FfmpegExe" }
if ($FfprobeExe) { $configureArgs += "-DGIFCORE_FFPROBE_EXE=$FfprobeExe" }
if ($Console)    { $configureArgs += '-DGIFCORE_CONSOLE=ON' }

Write-Host '--- configure ---' -ForegroundColor Cyan
& $cmake @configureArgs
if ($LASTEXITCODE -ne 0) { throw 'Configure failed.' }

Write-Host '--- build ---' -ForegroundColor Cyan
& $cmake --build (Join-Path $root $BuildDir)
if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }

if (-not $SkipBundle) {
    Write-Host '--- bundle ---' -ForegroundColor Cyan
    & $cmake --build (Join-Path $root $BuildDir) --target bundle
    if ($LASTEXITCODE -ne 0) { throw 'Bundling failed.' }

    $bundle = Join-Path $root "$BuildDir\bundle"
    $size = (Get-ChildItem $bundle -Recurse -File | Measure-Object -Property Length -Sum).Sum / 1MB
    Write-Host ''
    Write-Host ("Bundle ready: {0}  ({1:N1} MiB)" -f $bundle, $size) -ForegroundColor Green
    Write-Host 'It is self-contained: copy the folder anywhere and run gifforge.exe.'
} else {
    Write-Host ''
    Write-Host ("Executable: {0}" -f (Join-Path $root "$BuildDir\gifforge.exe")) -ForegroundColor Green
}
