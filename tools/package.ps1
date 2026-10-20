# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Builds a release of opensmarters and packages it as a portable zip:
    dist/opensmarters-<version>-windows-x64.zip

.DESCRIPTION
    1. Ensures the pinned libmpv build is present (tools/fetch-libmpv.ps1, hash-verified).
    2. Builds `opensmarters.exe` in release mode WITHOUT dev features (no demo provider, no harness).
    3. Generates THIRD_PARTY_LICENSES.html with cargo-about (all Rust crates + licence texts).
    4. Assembles exe + libmpv-2.dll + LICENSE + THIRD_PARTY.md + licence texts + README.txt.
    5. Writes SHA256SUMS.txt and the zip.

    Requires: Rust (MSVC), cargo-about (`cargo install --locked cargo-about`).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\package.ps1
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Split-Path -Parent $ScriptDir
Set-Location $Root

function Invoke-Checked([string]$what, [scriptblock]$block) {
    & $block
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

# ---- version ----------------------------------------------------------------
$version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$name = "opensmarters-$version-windows-x64"
$dist = Join-Path $Root 'dist'
$stage = Join-Path $dist $name
Write-Host "Packaging $name"

# ---- 1. libmpv --------------------------------------------------------------
Invoke-Checked 'fetch-libmpv' { powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $ScriptDir 'fetch-libmpv.ps1') }
$dll = Join-Path $Root 'vendor\libmpv\libmpv-2.dll'
if (-not (Test-Path $dll)) { throw "libmpv-2.dll missing at $dll" }

# ---- 2. release build (no dev features) -------------------------------------
$env:CARGO_TARGET_DIR = Join-Path $Root 'target\release-pkg'
if (-not $SkipBuild) {
    Invoke-Checked 'cargo build' { cargo build --release --locked -p opensmarters }
}
$exe = Join-Path $env:CARGO_TARGET_DIR 'release\opensmarters.exe'
if (-not (Test-Path $exe)) { throw "release exe missing at $exe" }

# ---- 3. third-party licences -----------------------------------------------
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage 'licenses') | Out-Null
$aboutOut = Join-Path $stage 'THIRD_PARTY_LICENSES.html'
Invoke-Checked 'cargo about' {
    cargo about generate --manifest-path crates/app/Cargo.toml -o $aboutOut packaging/about.hbs
}

# ---- 4. assemble ------------------------------------------------------------
Copy-Item $exe $stage
Copy-Item $dll $stage
Copy-Item 'LICENSE' (Join-Path $stage 'LICENSE.txt')
Copy-Item 'packaging/THIRD_PARTY.md' $stage
Copy-Item 'assets/fonts/OFL-SourceSans3.txt', 'assets/fonts/OFL-Fraunces.txt', 'assets/fonts/LICENSE-Phosphor.txt' (Join-Path $stage 'licenses')

@"
opensmarters $version: a secure, open-source IPTV player
=======================================================

Run opensmarters.exe. No installation is needed. Keep libmpv-2.dll in the same folder.

- Your provider passwords are stored in Windows Credential Manager, never in files.
- App data (settings, favourites, cache): %LOCALAPPDATA%\opensmarters\opensmarters\data
- No telemetry. The app only talks to your provider, plus TMDB if you turn it on.

Requirements: Windows 10/11 x64 and a GPU driver (libmpv uses vulkan-1.dll / Direct3D;
these come with any modern graphics driver).

Licence: GPL-3.0-or-later (LICENSE.txt). Third-party components: THIRD_PARTY.md,
THIRD_PARTY_LICENSES.html, licenses\.
"@ | Set-Content -Encoding utf8 (Join-Path $stage 'README.txt')

# ---- 5. checksums + zip -----------------------------------------------------
$sums = Get-ChildItem -Recurse -File $stage | Sort-Object FullName | ForEach-Object {
    $h = (Get-FileHash -Algorithm SHA256 $_.FullName).Hash.ToLower()
    $rel = $_.FullName.Substring($stage.Length + 1).Replace('\', '/')
    "$h  $rel"
}
$sums | Set-Content -Encoding ascii (Join-Path $stage 'SHA256SUMS.txt')

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
$zipHash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
"$zipHash  $name.zip" | Set-Content -Encoding ascii (Join-Path $dist "$name.zip.sha256")

$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "Done: $zip ($size MB)"
Write-Host "SHA-256: $zipHash"
