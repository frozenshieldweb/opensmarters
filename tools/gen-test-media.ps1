# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Generates small synthetic test media into testdata/generated/player/ with ffmpeg.

.DESCRIPTION
    Everything is synthetic (ffmpeg's testsrc2 pattern and sine tones), so it
    is safe to use in tests and demos. The output folder is gitignored.

      test.ts               ~20 s MPEG-TS, H.264 640x360@25 + AAC 440 Hz
      hls/index.m3u8        HLS VOD playlist (4 s segments) of the same
      hls/seg_NNN.ts
      test-tracks.mkv       ~30 s MKV: H.264 + two AAC tracks (eng 440 Hz,
                            fra 880 Hz) + one SRT subtitle track (eng)
      subs.srt              the subtitle source
      evil-file-segment.m3u8  security test: an HLS playlist whose segment is
                            a file:// URL. Served over HTTP, the player must
                            refuse it (ffmpeg protocol whitelist).

.PARAMETER Out
    Output directory. Defaults to <repo>/testdata/generated/player (a
    subfolder, because other tools such as fake-panel also write to
    testdata/generated/).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\gen-test-media.ps1
#>
[CmdletBinding()]
param(
    [string]$Out = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Out) { $Out = Join-Path (Split-Path -Parent $ScriptDir) 'testdata\generated\player' }
$Out = [System.IO.Path]::GetFullPath($Out)

if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    throw 'ffmpeg not found on PATH.'
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Hls = Join-Path $Out 'hls'
New-Item -ItemType Directory -Force -Path $Hls | Out-Null

function Invoke-Ffmpeg([string[]]$FfArgs) {
    & ffmpeg -hide_banner -loglevel error -y @FfArgs
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed (exit $LASTEXITCODE): $($FfArgs -join ' ')" }
}

$ts = Join-Path $Out 'test.ts'
Write-Host "Generating $ts"
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=25',
    '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
    '-t', '20',
    '-c:v', 'libx264', '-preset', 'veryfast', '-pix_fmt', 'yuv420p', '-g', '50', '-b:v', '600k',
    '-c:a', 'aac', '-b:a', '96k',
    '-metadata', 'title=opensmarters test stream',
    '-f', 'mpegts', $ts
)

Write-Host "Generating HLS VOD in $Hls"
Get-ChildItem -Path $Hls -Filter '*.ts' -ErrorAction SilentlyContinue | Remove-Item -Force
Invoke-Ffmpeg @(
    '-i', $ts, '-c', 'copy',
    '-f', 'hls', '-hls_time', '4', '-hls_playlist_type', 'vod',
    '-hls_segment_filename', (Join-Path $Hls 'seg_%03d.ts'),
    (Join-Path $Hls 'index.m3u8')
)

$srt = Join-Path $Out 'subs.srt'
$cues = @()
for ($i = 0; $i -lt 10; $i++) {
    $s = $i * 3
    $e = $s + 2
    $cues += "$($i + 1)"
    $cues += ('00:00:{0:D2},000 --> 00:00:{1:D2},500' -f $s, $e)
    $cues += "Subtitle cue $($i + 1) at $s s"
    $cues += ''
}
[System.IO.File]::WriteAllLines($srt, $cues)

$mkv = Join-Path $Out 'test-tracks.mkv'
Write-Host "Generating $mkv"
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=25',
    '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
    '-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=48000',
    '-i', $srt,
    '-t', '30',
    '-map', '0:v', '-map', '1:a', '-map', '2:a', '-map', '3:s',
    '-c:v', 'libx264', '-preset', 'veryfast', '-pix_fmt', 'yuv420p', '-g', '50', '-b:v', '500k',
    '-c:a', 'aac', '-b:a', '64k',
    '-c:s', 'srt',
    '-metadata:s:a:0', 'language=eng', '-metadata:s:a:0', 'title=Tone 440 Hz',
    '-metadata:s:a:1', 'language=fra', '-metadata:s:a:1', 'title=Tone 880 Hz',
    '-metadata:s:s:0', 'language=eng', '-metadata:s:s:0', 'title=Test subtitles',
    $mkv
)

# Security fixture: remote playlist pointing at a local file.
$seg0 = (Join-Path $Hls 'seg_000.ts') -replace '\\', '/'
$evil = Join-Path $Out 'evil-file-segment.m3u8'
[System.IO.File]::WriteAllLines($evil, @(
    '#EXTM3U',
    '#EXT-X-VERSION:3',
    '#EXT-X-TARGETDURATION:4',
    '#EXT-X-PLAYLIST-TYPE:VOD',
    '#EXTINF:4.0,',
    "file:///$seg0",
    '#EXT-X-ENDLIST'
))

Get-ChildItem -Recurse $Out | Where-Object { -not $_.PSIsContainer } |
    Select-Object @{n='File';e={$_.FullName.Substring($Out.Length + 1)}}, @{n='KiB';e={[math]::Round($_.Length / 1KB)}} |
    Format-Table -AutoSize
