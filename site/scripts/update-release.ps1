# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Rewrites the generated block of site/release.js from a dist/ folder, then rebuilds
    site/index.html (node site/scripts/build.mjs).

.DESCRIPTION
    Reads the release files that tools/package.ps1 writes:
      opensmarters-<version>-windows-x64.zip           portable zip (today)
      opensmarters-<version>-windows-x64-setup.exe     installer (once Part B ships it)
      SHA256SUMS.txt                                   checksums covering both, or
      opensmarters-<version>-windows-x64.zip.sha256    the single checksum file of 0.1.0
    and writes version, date, file names, sizes (MB = 1,000,000 bytes), SHA-256 and the
    Authenticode "signed" flag into site/release.js, between its BEGIN/END markers.

    Every SHA-256 is computed from the file itself and checked against SHA256SUMS.txt and
    any .sha256 file next to it; a mismatch stops the script and nothing is written.

    Download URLs: with -BaseUrl, each url is "<BaseUrl>/<file>" ("{version}" in BaseUrl is
    replaced). Without it they stay "TODO", and the page says the links aren't set yet.

.PARAMETER Dist
    The dist/ folder. Default: ..\..\dist from this script (the repository's dist/).

.PARAMETER Version
    The version to publish when dist/ holds several. Default: the highest one found.

.PARAMETER BaseUrl
    Where the files will be downloadable, e.g.
    "https://github.com/frozenshieldweb/opensmarters/releases/download/v{version}". Default: TODO.

.PARAMETER Date
    Release date (yyyy-MM-dd). Default: the release file's last-write date (UTC).

.PARAMETER NoBuild
    Don't run node site/scripts/build.mjs afterwards.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File site\scripts\update-release.ps1
.EXAMPLE
    powershell -ExecutionPolicy Bypass -File site\scripts\update-release.ps1 -Dist D:\builds\dist `
        -BaseUrl "https://github.com/frozenshieldweb/opensmarters/releases/download/v{version}"
#>
[CmdletBinding()]
param(
    [string]$Dist,
    [string]$Version,
    [string]$BaseUrl,
    [string]$Date,
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Not a param default: Windows PowerShell 5.1 leaves $PSScriptRoot empty there.
if (-not $Dist) { $Dist = Join-Path $PSScriptRoot '..\..\dist' }

$Site = Split-Path -Parent $PSScriptRoot
$ReleaseJs = Join-Path $Site 'release.js'
$Product = 'opensmarters'

if (-not (Test-Path -LiteralPath $Dist -PathType Container)) { throw "dist folder not found: $Dist" }
$Dist = (Resolve-Path -LiteralPath $Dist).Path

# ---- find the version ---------------------------------------------------------------
$pattern = "^$Product-(?<v>\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)-windows-x64(?:-setup\.exe|\.zip)$"
$found = @(Get-ChildItem -LiteralPath $Dist -File | ForEach-Object {
        if ($_.Name -match $pattern) { $Matches['v'] }
    } | Sort-Object -Unique)
if ($found.Count -eq 0) { throw "no $Product-<version>-windows-x64.zip or -setup.exe in $Dist" }

function Get-VersionKey([string]$v) {
    # 1.2.3 sorts after 1.2.3-rc.1 (a release after its pre-releases).
    $core, $pre = $v -split '-', 2
    $n = [version]$core
    '{0:D6}.{1:D6}.{2:D6}.{3}' -f $n.Major, $n.Minor, $n.Build, $(if ($pre) { "0$pre" } else { '1' })
}
if ($Version) {
    if ($found -notcontains $Version) { throw "version $Version not in $Dist (found: $($found -join ', '))" }
} else {
    $Version = $found | Sort-Object { Get-VersionKey $_ } | Select-Object -Last 1
}
Write-Host "Release $Version from $Dist"

# ---- files ----------------------------------------------------------------------------
function Get-Entry([string]$name) {
    $path = Join-Path $Dist $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    $item = Get-Item -LiteralPath $path
    [pscustomobject]@{
        File   = $name
        Path   = $path
        Item   = $item
        SizeMB = [math]::Round($item.Length / 1000000, 1)
        Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    }
}
$portable = Get-Entry "$Product-$Version-windows-x64.zip"
$installer = Get-Entry "$Product-$Version-windows-x64-setup.exe"
if (-not $portable -and -not $installer) { throw "nothing to publish for $Version" }
$entries = @($installer, $portable | Where-Object { $_ })

# ---- check against the checksum files ---------------------------------------------------
function Read-Sums([string]$path) {
    $map = @{}
    foreach ($line in Get-Content -LiteralPath $path) {
        if ($line -match '^\s*(?<h>[0-9a-fA-F]{64})\s+\*?(?<f>\S.*?)\s*$') {
            $map[$Matches['f']] = $Matches['h'].ToLowerInvariant()
        }
    }
    $map
}
$sumsTxt = Join-Path $Dist 'SHA256SUMS.txt'
$sumsFiles = @()
if (Test-Path -LiteralPath $sumsTxt -PathType Leaf) { $sumsFiles += $sumsTxt }
foreach ($e in $entries) {
    $side = "$($e.Path).sha256"
    if (Test-Path -LiteralPath $side -PathType Leaf) { $sumsFiles += $side }
}
$coveredBySums = $true
foreach ($e in $entries) {
    $listed = $false
    foreach ($s in $sumsFiles) {
        $map = Read-Sums $s
        if ($map.ContainsKey($e.File)) {
            $listed = $true
            if ($map[$e.File] -ne $e.Sha256) {
                throw "SHA-256 mismatch for $($e.File): $(Split-Path -Leaf $s) says $($map[$e.File]), the file is $($e.Sha256)"
            }
        }
    }
    if (-not $listed) { Write-Warning "$($e.File) is not listed in any checksum file" }
    if ($sumsFiles -notcontains $sumsTxt -or -not (Read-Sums $sumsTxt).ContainsKey($e.File)) { $coveredBySums = $false }
}
# The file the page links as "the checksums": SHA256SUMS.txt when it covers everything,
# else the single .sha256 of the only file.
$sumsFile = $null
if ($coveredBySums) { $sumsFile = 'SHA256SUMS.txt' }
elseif ($entries.Count -eq 1 -and (Test-Path -LiteralPath "$($entries[0].Path).sha256")) { $sumsFile = "$($entries[0].File).sha256" }

# ---- signed? --------------------------------------------------------------------------
# The installer is what people run first; without it, the exe staged next to the zip.
$signTarget = $null
if ($installer) { $signTarget = $installer.Path }
else {
    $staged = Join-Path $Dist "$Product-$Version-windows-x64\$Product.exe"
    if (Test-Path -LiteralPath $staged -PathType Leaf) { $signTarget = $staged }
}
$signed = $false
if ($signTarget) {
    $sig = Get-AuthenticodeSignature -LiteralPath $signTarget
    $signed = ($sig.Status -eq 'Valid')
    Write-Host "Authenticode ($(Split-Path -Leaf $signTarget)): $($sig.Status)"
} else {
    Write-Warning 'no exe to check the signature of; signed = false'
}

# ---- date and URLs ----------------------------------------------------------------------
if (-not $Date) { $Date = $entries[0].Item.LastWriteTimeUtc.ToString('yyyy-MM-dd') }
if ($Date -notmatch '^\d{4}-\d{2}-\d{2}$') { throw "date must be yyyy-MM-dd: $Date" }
function Get-Url([string]$file) {
    if (-not $BaseUrl -or -not $file) { return 'TODO' }
    ($BaseUrl.Replace('{version}', $Version)).TrimEnd('/') + '/' + [uri]::EscapeDataString($file)
}

# ---- write release.js -------------------------------------------------------------------
function Js([string]$s) { '"' + $s.Replace('\', '\\').Replace('"', '\"') + '"' }
function Js-Entry($e) {
    $mb = $e.SizeMB.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    "{ file: $(Js $e.File), url: $(Js (Get-Url $e.File)), sizeMB: $mb, sha256: $(Js $e.Sha256) }"
}
$lines = @(
    '  // BEGIN generated by site/scripts/update-release.ps1 (don''t edit by hand)'
    "  version: $(Js $Version),"
    "  date: $(Js $Date),"
    "  signed: $(if ($signed) { 'true' } else { 'false' }),"
    '  downloads: {'
)
if ($installer) { $lines += "    installer: $(Js-Entry $installer)," }
if ($portable) { $lines += "    portable: $(Js-Entry $portable)," }
$lines += @(
    "    sumsFile: $(if ($sumsFile) { Js $sumsFile } else { Js 'TODO' }),"
    "    sumsUrl: $(Js (Get-Url $sumsFile)),"
    '  },'
    '  // END generated'
)
$block = ($lines -join "`n")

$utf8 = New-Object System.Text.UTF8Encoding $false
$text = [System.IO.File]::ReadAllText($ReleaseJs, $utf8).Replace("`r`n", "`n")
$re = '(?s)  // BEGIN generated by site/scripts/update-release\.ps1[^\n]*\n.*?  // END generated'
if ($text -notmatch $re) { throw "markers not found in $ReleaseJs" }
$new = [regex]::Replace($text, $re, { param($m) $block })
[System.IO.File]::WriteAllText($ReleaseJs, $new, $utf8)
Write-Host "Wrote $ReleaseJs"
foreach ($e in $entries) { Write-Host ("  {0}  {1} MB  {2}" -f $e.File, $e.SizeMB, $e.Sha256) }
if (-not $BaseUrl) { Write-Host '  URLs: TODO (pass -BaseUrl once the release is hosted)' }

# ---- rebuild the page -------------------------------------------------------------------
if (-not $NoBuild) {
    $node = Get-Command node -ErrorAction SilentlyContinue
    if ($node) {
        & $node.Source (Join-Path $PSScriptRoot 'build.mjs')
        if ($LASTEXITCODE -ne 0) { throw "build.mjs failed with exit code $LASTEXITCODE" }
    } else {
        Write-Warning 'node not found: run "node site/scripts/build.mjs" to update index.html'
    }
}
