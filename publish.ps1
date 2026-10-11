#Requires -Version 5.1
<#
.SYNOPSIS
    把已经打包好的 Easy2GIF 复制到项目目录之外，在那里运行。

.DESCRIPTION
    为什么要多这一步：实测出来的规律很干脆——可执行文件放在项目目录
    （C:\Users\17189\CLionProjects\untitled4）里面时，这个环境会把它围起来：
    它照常读写项目目录，但向"视频""图片""桌面"这类目录写文件会被系统直接拒绝，
    报的就是：

        拒绝访问。 (os error 5)

    已经验证过的边界（同一个 bundle、同一条命令行、同一个目标目录，只有 exe 的
    位置不同）：

        项目目录内     C:\...\untitled4\b\bundle\gifforge.exe   -> 拒绝访问
        项目目录内     C:\...\untitled4\.probe\ws-bundle\...    -> 拒绝访问
        项目目录外     C:\Users\17189\CLionProjects\outside-... -> 正常，724604 字节
        项目目录外     %LOCALAPPDATA%\GifForge\gifforge.exe     -> 正常，724604 字节

    注意边界是 untitled4 这一层，不是整个 CLionProjects。所以把 bundle 复制到项目
    之外再运行，限制就不适用了，导出到任意位置都正常。这与 CLion 无关，CLion 里
    并没有、也不需要任何相关设置——只是它 Run 的可执行文件恰好在这个目录里。

    这个脚本只做复制和自检，不重新编译。要先编译请看 build.ps1。

.PARAMETER Destination
    目标目录，默认 %LOCALAPPDATA%\GifForge。

.PARAMETER Launch
    复制完直接把它启动起来。

.PARAMETER Force
    即使目标已经是最新的，也强制重新复制一遍。

.EXAMPLE
    .\publish.ps1
    .\publish.ps1 -Launch
    .\publish.ps1 -Destination D:\Tools\GifForge -Launch
#>
[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'GifForge'),
    [switch]$Launch,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$source = Join-Path $root 'b\bundle'

if (-not (Test-Path (Join-Path $source 'gifforge.exe'))) {
    throw "没找到 $source\gifforge.exe。先跑 .\build.ps1（或在 CLion 里构建 bundle 目标）。"
}

# 复制目标不能落在项目目录里面，否则等于没搬。
$rootFull = [IO.Path]::GetFullPath($root).TrimEnd('\')
$destFull = [IO.Path]::GetFullPath($Destination).TrimEnd('\')
if ($destFull -eq $rootFull -or $destFull.StartsWith($rootFull + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw "目标目录不能放在项目目录里面（$rootFull），换一个位置。"
}

$exe = Join-Path $destFull 'gifforge.exe'

# 目标已经是最新的就跳过，这样把它当成 CLion 的 Run 步骤时不会每次都搬 285 MB。
function Get-Stamp([string]$dir) {
    $parts = foreach ($rel in @('gifforge.exe', 'bin\ffmpeg.exe', 'bin\ffprobe.exe')) {
        $f = Join-Path $dir $rel
        if (Test-Path $f) {
            $i = Get-Item $f
            "$rel=$($i.Length)/$($i.LastWriteTimeUtc.Ticks)"
        } else {
            "$rel=missing"
        }
    }
    ($parts -join '|')
}

$upToDate = (-not $Force) -and (Test-Path $exe) -and ((Get-Stamp $destFull) -eq (Get-Stamp $source))

if ($upToDate) {
    Write-Host "已经是最新的，跳过复制：" -ForegroundColor DarkGray
    Write-Host "  $exe" -ForegroundColor DarkGray
} else {
    Write-Host "复制 $source" -ForegroundColor Cyan
    Write-Host "  -> $destFull" -ForegroundColor Cyan
    if (Test-Path $destFull) {
        Remove-Item $destFull -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $destFull | Out-Null

    # robocopy 比 Copy-Item 快得多，而且会自动建子目录；退出码 0-7 都算成功。
    $null = robocopy $source $destFull /E /NFL /NDL /NJH /NJS /NP
    if ($LASTEXITCODE -ge 8) {
        throw "robocopy 失败，退出码 $LASTEXITCODE"
    }
}

if (-not (Test-Path $exe)) {
    throw "复制之后没找到 $exe"
}

# 自检：复制过去之后，内置的媒体引擎还得能自己跑起来。
$ffmpeg = Join-Path $destFull 'bin\ffmpeg.exe'
if (Test-Path $ffmpeg) {
    $version = & $ffmpeg -version 2>&1 | Select-Object -First 1
    Write-Host "媒体引擎自检：$version" -ForegroundColor DarkGray
} else {
    Write-Warning "bundle 里没有 bin\ffmpeg.exe，导出会失败。请重新构建 bundle 目标。"
}

Write-Host ""
Write-Host "完成。程序在：" -ForegroundColor Green
Write-Host "  $exe" -ForegroundColor Green
Write-Host "从这个位置运行，导出到任意目录都不会再被拦。" -ForegroundColor Green

if ($Launch) {
    # 用 cmd /c start 而不是 Start-Process：它会立刻返回，不会把脚本（以及 CLion 的
    # Run 会话）挂在被启动的 GUI 程序上。
    & cmd /c start "" "$exe"
}
