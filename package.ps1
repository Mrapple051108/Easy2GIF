#Requires -Version 5.1
[CmdletBinding()]
param(
    [switch]$SharedMedia,
    [switch]$SkipBuild,
    [switch]$SevenZip,
    [switch]$SevenZipOnly,
    [string]$QtRoot = 'D:\CODE\QT\6.10.2\mingw_64'
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$bundle = Join-Path $root 'b\bundle'
$projectLicense = Join-Path $root 'LICENSE'
if (!(Test-Path -LiteralPath $projectLicense -PathType Leaf)) { throw 'Project LICENSE is required for packaging.' }
if ($SevenZip -and $SevenZipOnly) { throw 'Use either -SevenZip or -SevenZipOnly.' }
if ($SevenZip -or $SevenZipOnly) {
    $tar = Join-Path $env:SystemRoot 'System32\tar.exe'
    if (!(Test-Path $tar)) { throw '7z packaging requires Windows tar.exe.' }
}
if ($SharedMedia -and $SkipBuild) { throw '-SharedMedia requires a build; omit -SkipBuild.' }
if (!$SkipBuild) {
    $buildArgs = @{ QtRoot = $QtRoot }
    if ($SharedMedia) {
        $media = & (Join-Path $root 'tools\get-shared-ffmpeg.ps1')
        $buildArgs.FfmpegExe = Join-Path $media 'bin\ffmpeg.exe'
        $buildArgs.FfprobeExe = Join-Path $media 'bin\ffprobe.exe'
    }
    & (Join-Path $root 'build.ps1') @buildArgs
}
foreach ($rel in @('easy2gif.exe', 'qt.conf', 'bin\media-runtime.txt')) {
    if (!(Test-Path (Join-Path $bundle $rel))) { throw "Missing bundle file: $rel" }
}
& (Join-Path $root 'tools\test-app-icons.ps1') `
    -Executable (Join-Path $bundle 'easy2gif.exe') `
    -Icon (Join-Path $root 'app\assets\easy2gif.ico')
$commit = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot determine source revision.' }
$dirty = [bool](& git -C $root status --porcelain --untracked-files=normal)
$suffix = if ($dirty) { '-dirty' } else { '' }
$name = "easy2gif-1.0.1-windows-x64-$($commit.Substring(0,7))$suffix"
$stage = Join-Path $root ('.probe\package-' + [guid]::NewGuid().ToString('N'))
# A new release gets a new executable path, so Explorer does not reuse the
# cached icon of a previous release extracted into the same destination.
$app = Join-Path $stage $name
$output = Join-Path $root 'b\releases'
New-Item -ItemType Directory -Force -Path $app, (Join-Path $app 'bin'), (Join-Path $app 'licenses'), $output | Out-Null
Copy-Item -LiteralPath $projectLicense -Destination (Join-Path $app 'LICENSE')

# Start from a fresh directory. Include only runtime files, never scratch data
# or executables left behind by an earlier target name.
Get-ChildItem -LiteralPath $bundle -Filter '*.dll' -File | Copy-Item -Destination $app
foreach ($file in @('easy2gif.exe', 'qt.conf')) {
    Copy-Item -LiteralPath (Join-Path $bundle $file) -Destination $app
}
foreach ($dir in @('qml', 'platforms', 'iconengines', 'imageformats', 'networkinformation', 'tls')) {
    $path = Join-Path $bundle $dir
    if (Test-Path $path) { Copy-Item -LiteralPath $path -Destination $app -Recurse }
}
foreach ($file in (Get-Content (Join-Path $bundle 'bin\media-runtime.txt'))) {
    if (!$file) { continue }
    if ([IO.Path]::GetFileName($file) -ne $file) { throw "Invalid media manifest entry: $file" }
    Copy-Item -LiteralPath (Join-Path $bundle "bin\$file") -Destination (Join-Path $app 'bin')
}
Copy-Item (Join-Path $bundle 'bin\media-runtime.txt') (Join-Path $app 'bin')
New-Item -ItemType Directory -Path (Join-Path $app 'branding') | Out-Null
Get-ChildItem (Join-Path $root 'app\assets') -File | Copy-Item -Destination (Join-Path $app 'branding')
Get-ChildItem (Join-Path $root 'docs\brand') -Filter 'logo-*.svg' -File |
    Copy-Item -Destination (Join-Path $app 'branding')

# Read the actual configured media source, including when -SkipBuild is used.
$cache = Get-Content (Join-Path $root 'b\CMakeCache.txt')
$mediaEntry = $cache | Where-Object { $_ -match '^GIFCORE_FFMPEG_EXE:[^=]+=' } | Select-Object -First 1
if (!$mediaEntry) { throw 'Media source missing from CMake cache.' }
$mediaRoot = Split-Path (Split-Path ($mediaEntry -replace '^[^=]+=', '') -Parent) -Parent
$license = @((Join-Path $mediaRoot 'LICENSE.txt'), (Join-Path $mediaRoot 'LICENSE')) |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (!$license) { throw "Media license not found in $mediaRoot" }
Copy-Item -LiteralPath $license -Destination (Join-Path $app 'licenses\FFmpeg-LICENSE.txt')
$sourceInfo = Join-Path $mediaRoot 'SOURCE.json'
if (Test-Path $sourceInfo) { Copy-Item $sourceInfo (Join-Path $app 'licenses\FFmpeg-SOURCE.json') }
$qtLicense = Join-Path (Split-Path (Split-Path $QtRoot -Parent) -Parent) 'Licenses\LICENSE'
if (!(Test-Path $qtLicense)) { throw "Qt license not found: $qtLicense" }
Copy-Item -LiteralPath $qtLicense -Destination (Join-Path $app 'licenses\Qt-LICENSE.txt')
@'
Easy2GIF

Extract the whole folder, then run easy2gif.exe. Keep bin/, qml/ and DLLs beside it.
No separate Qt or FFmpeg installation is required.

Easy2GIF is licensed under the MIT License; see LICENSE in this folder.
Third-party components retain their own licenses in licenses/.

Source: https://github.com/Mrapple051108/Easy2GIF
Qt: https://www.qt.io/licensing/ ; https://download.qt.io/archive/qt/
FFmpeg: https://ffmpeg.org/legal.html
Shared FFmpeg source/build recipes: https://github.com/BtbN/FFmpeg-Builds
The pinned upstream build URL and archive hash are in licenses/FFmpeg-SOURCE.json
when using -SharedMedia. This app dynamically uses the shipped Qt runtime and
runs the shipped FFmpeg tools as separate processes. Their licenses apply.
'@ | Set-Content (Join-Path $app 'README.txt') -Encoding utf8
@{ commit = $commit; dirty = $dirty; builtAtUtc = [DateTime]::UtcNow.ToString('o'); platform = 'windows-x64' } |
    ConvertTo-Json | Set-Content (Join-Path $app 'BUILD.json') -Encoding utf8

$archives = @()
if (!$SevenZipOnly) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = Join-Path $output "$name.zip"
    if (Test-Path $archive) { throw "Archive already exists: $archive" }
    $level = [IO.Compression.CompressionLevel]::Optimal
    if ([Enum]::GetNames([IO.Compression.CompressionLevel]) -contains 'SmallestSize') {
        $level = [Enum]::Parse([IO.Compression.CompressionLevel], 'SmallestSize')
    }
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $archive, $level, $false)
    $archives += $archive
}
if ($SevenZip -or $SevenZipOnly) {
    $seven = Join-Path $output "$name.7z"
    if (Test-Path $seven) { throw "Archive already exists: $seven" }
    & $tar -a -cf $seven -C $stage $name
    if ($LASTEXITCODE -ne 0) { throw '7z packaging failed.' }
    $archives += $seven
}
foreach ($path in $archives) {
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $([IO.Path]::GetFileName($path))" | Set-Content "$path.sha256" -Encoding ascii
    Write-Host ("{0} ({1:N2} MiB)" -f $path, ((Get-Item $path).Length / 1MB))
}
Write-Host "Staged files: $app"
