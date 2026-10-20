# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Downloads the pinned Windows x86_64 libmpv dev build, verifies its SHA-256
    and extracts libmpv-2.dll plus the C headers into vendor/libmpv/.

.DESCRIPTION
    Source: shinchiro/mpv-winbuild-cmake (linked from https://mpv.io/installation/).
    GitHub only keeps the last ~30 daily releases, so the same file is also
    fetched from the project's SourceForge mirror, which keeps the weekly
    builds. Whatever mirror answers, the archive must match the pinned hash or
    nothing is extracted.

    See docs/LIBMPV.md for licence notes and manual steps.

.PARAMETER Dest
    Output directory. Defaults to <repo>/vendor/libmpv.

.PARAMETER Force
    Re-download and re-extract even if the pinned version is already present.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\fetch-libmpv.ps1
#>
[CmdletBinding()]
param(
    [string]$Dest = '',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is very slow with the progress bar

# ---- Pinned build -----------------------------------------------------------
# To upgrade: pick a newer mpv-dev-x86_64-*.7z that exists on BOTH mirrors,
# update the three values below, run the script, run the player tests and the
# spike, and update docs/LIBMPV.md.
$ReleaseTag = '20261018'
$Version    = '20261018-git-01e2533f31'
$Sha256     = 'D7F5621F42B6E161FE032DF914A10BDECE28A89DF3E57C6C3E16A5A250929B5E'
$Archive    = "mpv-dev-x86_64-$Version.7z"
$Urls = @(
    "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$ReleaseTag/$Archive",
    "https://downloads.sourceforge.net/project/mpv-player-windows/libmpv/$Archive"
)
# -----------------------------------------------------------------------------

# $PSScriptRoot is not usable in param() defaults on Windows PowerShell 5.1.
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Dest) { $Dest = Join-Path (Split-Path -Parent $ScriptDir) 'vendor\libmpv' }
$Dest = [System.IO.Path]::GetFullPath($Dest)
$Marker = Join-Path $Dest 'VERSION.txt'

if (-not $Force -and (Test-Path (Join-Path $Dest 'libmpv-2.dll')) -and (Test-Path $Marker)) {
    $existing = Get-Content $Marker -Raw
    if ($existing -match [regex]::Escape($Sha256)) {
        Write-Host "libmpv $Version already present in $Dest (use -Force to refetch)."
        exit 0
    }
}

# TLS 1.2 for Windows PowerShell 5.1 (PowerShell 7 already defaults to it).
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# Work in a fresh, empty temp directory; the archive is untrusted until hashed.
$Work = Join-Path ([System.IO.Path]::GetTempPath()) ("opensmarters-libmpv-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Work | Out-Null
$ArchivePath = Join-Path $Work $Archive
$Extract = Join-Path $Work 'x'
New-Item -ItemType Directory -Path $Extract | Out-Null

try {
    $ok = $false
    foreach ($u in $Urls) {
        Write-Host "Downloading $u"
        try {
            Invoke-WebRequest -Uri $u -OutFile $ArchivePath -UseBasicParsing -UserAgent 'opensmarters-fetch-libmpv' -MaximumRedirection 10
        } catch {
            Write-Warning "  download failed: $($_.Exception.Message)"
            continue
        }
        $got = (Get-FileHash -Algorithm SHA256 -Path $ArchivePath).Hash
        if ($got -ieq $Sha256) {
            Write-Host "  SHA-256 OK ($got)"
            $ok = $true
            $SourceUrl = $u
            break
        }
        Write-Warning "  SHA-256 mismatch: expected $Sha256, got $got - trying next mirror"
        Remove-Item -Force $ArchivePath
    }
    if (-not $ok) {
        throw "Could not obtain $Archive with the pinned SHA-256 from any mirror. See docs/LIBMPV.md for manual steps."
    }

    # Windows ships bsdtar (libarchive) as System32\tar.exe, which reads .7z.
    # Git Bash's GNU tar does not, so call System32's explicitly.
    $tar = Join-Path $env:SystemRoot 'System32\tar.exe'
    if (-not (Test-Path $tar)) { throw "System32\tar.exe not found; extract $ArchivePath manually with 7-Zip (see docs/LIBMPV.md)." }
    & $tar -xf $ArchivePath -C $Extract
    if ($LASTEXITCODE -ne 0) { throw "tar.exe failed to extract $Archive (exit $LASTEXITCODE)." }

    $dll = Get-ChildItem -Path $Extract -Recurse -Filter 'libmpv-2.dll' | Select-Object -First 1
    if (-not $dll) { throw "libmpv-2.dll not found inside $Archive." }
    $inc = Get-ChildItem -Path $Extract -Recurse -Directory -Filter 'mpv' |
        Where-Object { Test-Path (Join-Path $_.FullName 'client.h') } | Select-Object -First 1

    New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    Copy-Item -Force $dll.FullName (Join-Path $Dest 'libmpv-2.dll')
    $importLib = Get-ChildItem -Path $Extract -Recurse -Filter 'libmpv.dll.a' | Select-Object -First 1
    if ($importLib) { Copy-Item -Force $importLib.FullName (Join-Path $Dest 'libmpv.dll.a') }
    if ($inc) {
        $incDest = Join-Path $Dest 'include\mpv'
        New-Item -ItemType Directory -Force -Path $incDest | Out-Null
        Copy-Item -Force (Join-Path $inc.FullName '*.h') $incDest
    }

    $dllHash = (Get-FileHash -Algorithm SHA256 -Path (Join-Path $Dest 'libmpv-2.dll')).Hash
    @(
        "archive=$Archive",
        "archive_sha256=$Sha256",
        "source=$SourceUrl",
        "libmpv-2.dll_sha256=$dllHash",
        "fetched_utc=$((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))"
    ) | Set-Content -Encoding ascii -Path $Marker

    Write-Host "libmpv $Version installed to $Dest"
    Write-Host "  libmpv-2.dll SHA-256: $dllHash"
} finally {
    Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
}
