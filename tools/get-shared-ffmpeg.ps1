#Requires -Version 5.1
[CmdletBinding()]
param([string]$ArchivePath = '')

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$destination = Join-Path $root '.deps\ffmpeg-8.1-shared'
$url = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-14-13-03/ffmpeg-n8.1.3-18-g46d899d7a0-win64-lgpl-shared-8.1.zip'
$sha256 = '38dd3fb973c2e10723489e2173135d60f569953e70a04b885446175f9e9380d6'
$stamp = Join-Path $destination 'SOURCE.json'
if ((Test-Path $stamp) -and (Test-Path (Join-Path $destination 'bin\ffmpeg.exe')) -and
    (Test-Path (Join-Path $destination 'bin\ffprobe.exe'))) {
    $saved = Get-Content $stamp -Raw | ConvertFrom-Json
    $required = @('ffmpeg.exe', 'ffprobe.exe', 'avcodec-62.dll', 'avfilter-11.dll',
        'avformat-62.dll', 'avdevice-62.dll', 'avutil-60.dll', 'swresample-6.dll', 'swscale-9.dll')
    $missing = @($required | Where-Object { !(Test-Path (Join-Path $destination "bin\$_")) })
    if ($saved.sha256 -eq $sha256 -and !$missing.Count) {
        foreach ($name in @('ffmpeg.exe', 'ffprobe.exe')) {
            & icacls (Join-Path $destination "bin\$name") /setintegritylevel M /C | Out-Host
        }
        return $destination
    }
}

if (!$ArchivePath) {
    $ArchivePath = Join-Path $root '.deps\downloads\ffmpeg-8.1-shared.zip'
    New-Item -ItemType Directory -Force -Path (Split-Path $ArchivePath -Parent) | Out-Null
    if (!(Test-Path $ArchivePath)) {
        Write-Host 'Downloading the pinned shared FFmpeg build...'
        Invoke-WebRequest -UseBasicParsing $url -OutFile $ArchivePath
    }
}
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sha256) {
    throw 'Shared FFmpeg SHA256 mismatch; refusing to use this archive.'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$bin = Join-Path $destination 'bin'
New-Item -ItemType Directory -Force -Path $bin | Out-Null
$zip = [IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $ArchivePath).Path)
try {
    foreach ($entry in $zip.Entries) {
        $name = [IO.Path]::GetFileName($entry.FullName)
        if ($entry.FullName -match '/bin/[^/]+$' -and
            ($name -in @('ffmpeg.exe', 'ffprobe.exe') -or $name -like '*.dll')) {
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $bin $name), $true)
        } elseif ($entry.FullName -match '/LICENSE.txt$') {
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $destination 'LICENSE.txt'), $true)
        }
    }
} finally { $zip.Dispose() }

# Check DLL resolution as well as the archive's contents before marking it ready.
foreach ($name in @('ffmpeg.exe', 'ffprobe.exe')) {
    & icacls (Join-Path $bin $name) /setintegritylevel M /C | Out-Host
    & (Join-Path $bin $name) -version | Select-Object -First 1 | Write-Host
    if ($LASTEXITCODE -ne 0) { throw "$name cannot load its shared runtime." }
}
@{ url = $url; sha256 = $sha256; version = '8.1.3-18-g46d899d7a0-20261014'; provider = 'BtbN/FFmpeg-Builds' } |
    ConvertTo-Json | Set-Content -LiteralPath $stamp -Encoding utf8
return $destination
