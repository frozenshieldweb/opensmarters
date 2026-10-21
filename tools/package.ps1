# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Builds a release of opensmarters and packages it, in dist/:
      opensmarters-<version>-windows-x64.zip         the portable zip
      opensmarters-<version>-windows-x64-setup.exe   the per-user installer (crates/installer)
      SHA256SUMS.txt                                 SHA-256 of both (sha256sum format)

.DESCRIPTION
    1. Ensures the pinned libmpv build is present (tools/fetch-libmpv.ps1, hash-verified).
    2. Builds `opensmarters.exe` in release mode WITHOUT dev features (no demo provider, no harness).
    3. Generates THIRD_PARTY_LICENSES.html with cargo-about (all Rust crates + licence texts).
    4. Assembles exe + libmpv-2.dll + LICENSE + THIRD_PARTY.md + licence texts + README.txt.
       Signs opensmarters.exe when a certificate is configured (see SIGNING).
    5. Writes the folder's SHA256SUMS.txt and the zip (+ .zip.sha256).
    6. Packs the SAME staged folder into the installer payload (opensmarters-pack: xz + SHA-256
       manifest), builds `opensmarters-setup.exe` in release mode WITHOUT dev features with
       OPENSMARTERS_SETUP_PAYLOAD pointing at it, signs it when configured, and copies it to dist/.
    7. Writes dist/SHA256SUMS.txt covering the zip and the setup exe, and checks that the
       setup exe is no bigger than the zip (LAUNCH_PROMPT §B6).

    Requires: Rust (MSVC), cargo-about (`cargo install --locked cargo-about`).

    SIGNING (optional; without it the build is unsigned and the script says so):
      OPENSMARTERS_SIGN_CERT        path to a .pfx code-signing certificate
      OPENSMARTERS_SIGN_PASSWORD    its password (if any)
      OPENSMARTERS_SIGN_THUMBPRINT  or: SHA-1 thumbprint of a certificate in the user's store
      OPENSMARTERS_SIGN_TIMESTAMP   RFC 3161 timestamp URL (default http://timestamp.digicert.com)
      OPENSMARTERS_SIGNTOOL         signtool.exe path (default: newest Windows 10/11 SDK)

.PARAMETER SkipBuild
    Reuse the existing release opensmarters.exe instead of building it (the installer is
    always rebuilt, because it embeds this run's staged folder).

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

function Get-Sha256([string]$path) {
    (Get-FileHash -Algorithm SHA256 $path).Hash.ToLower()
}

# ---- signing (optional) -----------------------------------------------------
$signCert = $env:OPENSMARTERS_SIGN_CERT
$signThumb = $env:OPENSMARTERS_SIGN_THUMBPRINT
$signing = [bool]($signCert -or $signThumb)
$signtool = $null
if ($signing) {
    $signtool = $env:OPENSMARTERS_SIGNTOOL
    if (-not $signtool) {
        $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
        $signtool = Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\' } |
            Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not $signtool -or -not (Test-Path $signtool)) {
        throw "A signing certificate is configured but signtool.exe was not found (set OPENSMARTERS_SIGNTOOL)."
    }
}

function Invoke-Sign([string]$file) {
    if (-not $signing) { return }
    $ts = if ($env:OPENSMARTERS_SIGN_TIMESTAMP) { $env:OPENSMARTERS_SIGN_TIMESTAMP } else { 'http://timestamp.digicert.com' }
    $signArgs = @('sign', '/fd', 'SHA256', '/tr', $ts, '/td', 'SHA256', '/d', 'opensmarters')
    if ($signCert) {
        $signArgs += @('/f', $signCert)
        if ($env:OPENSMARTERS_SIGN_PASSWORD) { $signArgs += @('/p', $env:OPENSMARTERS_SIGN_PASSWORD) }
    } else {
        $signArgs += @('/sha1', $signThumb)
    }
    $signArgs += $file
    Invoke-Checked "signtool sign $(Split-Path -Leaf $file)" { & $signtool @signArgs }
    Invoke-Checked "signtool verify $(Split-Path -Leaf $file)" { & $signtool verify /pa /q $file }
    Write-Host "Signed: $file"
}

# ---- version ----------------------------------------------------------------
$version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$name = "opensmarters-$version-windows-x64"
$dist = Join-Path $Root 'dist'
$stage = Join-Path $dist $name
Write-Host "Packaging $name"
if ($signing) { Write-Host "Code signing: on ($signtool)" }
else { Write-Host "Code signing: OFF (no OPENSMARTERS_SIGN_CERT / OPENSMARTERS_SIGN_THUMBPRINT): this build is UNSIGNED" }

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
opensmarters ${version}: a secure, open-source IPTV player
=======================================================

Run opensmarters.exe. Keep libmpv-2.dll in the same folder.
- Portable zip: no installation is needed.
- Installed with opensmarters-$version-windows-x64-setup.exe: use the Start menu shortcut;
  remove it from Settings > Apps. Your data and saved passwords stay unless you choose otherwise.

- Your provider passwords are stored in Windows Credential Manager, never in files.
- App data (settings, favourites, cache): %LOCALAPPDATA%\opensmarters\opensmarters\data
- No telemetry. The app only talks to your provider, plus TMDB if you turn it on.

Requirements: Windows 10/11 x64 and a GPU driver (libmpv uses vulkan-1.dll / Direct3D;
these come with any modern graphics driver).

Licence: GPL-3.0-or-later (LICENSE.txt). Third-party components: THIRD_PARTY.md,
THIRD_PARTY_LICENSES.html, licenses\.
"@ | Set-Content -Encoding utf8 (Join-Path $stage 'README.txt')

# The app exe is signed before anything is hashed, zipped or packed.
Invoke-Sign (Join-Path $stage 'opensmarters.exe')

# ---- 5. checksums + zip -----------------------------------------------------
$sums = Get-ChildItem -Recurse -File $stage | Sort-Object FullName | ForEach-Object {
    $h = Get-Sha256 $_.FullName
    $rel = $_.FullName.Substring($stage.Length + 1).Replace('\', '/')
    "$h  $rel"
}
$sums | Set-Content -Encoding ascii (Join-Path $stage 'SHA256SUMS.txt')

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
$zipHash = Get-Sha256 $zip
"$zipHash  $name.zip" | Set-Content -Encoding ascii (Join-Path $dist "$name.zip.sha256")

$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "Done: $zip ($size MB)"
Write-Host "SHA-256: $zipHash"

# ---- 6. installer: pack the same staged folder, build, sign -----------------
# The payload lives outside dist/ (it is an intermediate, not a release file).
$payloadDir = Join-Path $env:CARGO_TARGET_DIR 'setup-payload'
if (Test-Path $payloadDir) { Remove-Item -Recurse -Force $payloadDir }
Invoke-Checked 'cargo build (opensmarters-pack)' {
    cargo build --release --locked -p opensmarters-installer --bin opensmarters-pack
}
$pack = Join-Path $env:CARGO_TARGET_DIR 'release\opensmarters-pack.exe'
Invoke-Checked 'opensmarters-pack' { & $pack $stage $payloadDir }

$env:OPENSMARTERS_SETUP_PAYLOAD = $payloadDir
try {
    Invoke-Checked 'cargo build (opensmarters-setup)' {
        cargo build --release --locked -p opensmarters-installer --bin opensmarters-setup
    }
} finally {
    Remove-Item Env:OPENSMARTERS_SETUP_PAYLOAD -ErrorAction SilentlyContinue
}
$setupBuilt = Join-Path $env:CARGO_TARGET_DIR 'release\opensmarters-setup.exe'
if (-not (Test-Path $setupBuilt)) { throw "setup exe missing at $setupBuilt" }
$setupName = "$name-setup.exe"
$setup = Join-Path $dist $setupName
Copy-Item -Force $setupBuilt $setup
Invoke-Sign $setup

# ---- 7. release checksums + size budget -------------------------------------
$setupHash = Get-Sha256 $setup
@(
    "$zipHash  $name.zip"
    "$setupHash  $setupName"
) | Set-Content -Encoding ascii (Join-Path $dist 'SHA256SUMS.txt')

$setupBytes = (Get-Item $setup).Length
$zipBytes = (Get-Item $zip).Length
$setupSize = [math]::Round($setupBytes / 1MB, 1)
Write-Host "Done: $setup ($setupSize MB)"
Write-Host "SHA-256: $setupHash"
if ($setupBytes -gt $zipBytes) {
    Write-Warning "The setup exe ($setupSize MB) is bigger than the portable zip ($size MB): over the size budget."
}
if (-not $signing) {
    Write-Warning "These files are UNSIGNED. Windows SmartScreen will warn that the publisher is unknown."
}
Write-Host "Release files and checksums: $(Join-Path $dist 'SHA256SUMS.txt')"
