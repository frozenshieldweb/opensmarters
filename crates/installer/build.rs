// SPDX-License-Identifier: GPL-3.0-or-later
//! Build script for `opensmarters-setup` (Windows only; elsewhere the crate is a stub):
//!
//! 1. **Payload** (ADR 0002, decision 3). With `OPENSMARTERS_SETUP_PAYLOAD=<dir>` (set by
//!    `tools/package.ps1`), embeds `<dir>/payload.xz` + `<dir>/payload.manifest` made by
//!    `opensmarters-pack` from the staged release folder, after checking the manifest's
//!    version and the archive's size and SHA-256. Without it, packs a small **synthetic**
//!    payload (deterministic fake app files plus licence texts) into `OUT_DIR`, so
//!    `cargo build` / `cargo test` never need a release build. The paths reach the code as
//!    `OPENSMARTERS_PAYLOAD_XZ` / `OPENSMARTERS_PAYLOAD_MANIFEST`.
//! 2. **Resources** for the `opensmarters-setup` binary: the icon, a VERSIONINFO block
//!    (product "opensmarters") and the application manifest (`asInvoker`, PerMonitorV2,
//!    Windows 10/11, long paths, common controls v6).
//! 3. **DLL planting**: `/DEPENDENTLOADFLAG:0x800` so the loader resolves the exe's static
//!    imports from System32 only, never from the folder the setup runs in (Downloads).
//! 4. **No VC++ Redistributable needed**: the VC runtime is linked statically into the
//!    setup binary (the Universal CRT is part of Windows 10/11).

#[cfg(windows)]
#[path = "src/pack.rs"]
#[allow(dead_code)]
mod pack;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/pack.rs");
    println!("cargo:rerun-if-env-changed=OPENSMARTERS_SETUP_PAYLOAD");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    #[cfg(windows)]
    windows::main();
}

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};

    use super::pack;

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("cargo did not set {name}"))
    }

    pub fn main() {
        let out = PathBuf::from(env("OUT_DIR"));
        let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
        let version = env("CARGO_PKG_VERSION");
        let (xz, manifest) = match std::env::var_os("OPENSMARTERS_SETUP_PAYLOAD") {
            Some(dir) if !dir.is_empty() => release_payload(Path::new(&dir), &version),
            _ => synthetic_payload(&out, &manifest_dir, &version),
        };
        println!("cargo:rustc-env=OPENSMARTERS_PAYLOAD_XZ={}", xz.display());
        println!(
            "cargo:rustc-env=OPENSMARTERS_PAYLOAD_MANIFEST={}",
            manifest.display()
        );
        resources(&out, &manifest_dir, &version);
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            let arg = |a: &str| println!("cargo:rustc-link-arg-bin=opensmarters-setup={a}");
            // LOAD_LIBRARY_SEARCH_SYSTEM32 for the static imports (Windows 10 1607+).
            arg("/DEPENDENTLOADFLAG:0x800");
            // The VC++ runtime (vcruntime140.dll) is not part of Windows: link it statically
            // so setup starts on a clean PC without the Visual C++ Redistributable. The
            // Universal CRT stays dynamic: it is a Windows 10/11 system component.
            for lib in [
                "libvcruntimed.lib",
                "vcruntime.lib",
                "vcruntimed.lib",
                "libcmtd.lib",
                "msvcrt.lib",
                "msvcrtd.lib",
                "libucrt.lib",
                "libucrtd.lib",
            ] {
                arg(&format!("/NODEFAULTLIB:{lib}"));
            }
            for lib in ["libcmt.lib", "libvcruntime.lib", "ucrt.lib"] {
                arg(&format!("/DEFAULTLIB:{lib}"));
            }
        }
    }

    /// The real payload made by `opensmarters-pack`: checked, then embedded as-is.
    fn release_payload(dir: &Path, version: &str) -> (PathBuf, PathBuf) {
        let dir = dir
            .canonicalize()
            .unwrap_or_else(|e| panic!("OPENSMARTERS_SETUP_PAYLOAD {}: {e}", dir.display()));
        let xz = dir.join("payload.xz");
        let manifest = dir.join("payload.manifest");
        println!("cargo:rerun-if-changed={}", xz.display());
        println!("cargo:rerun-if-changed={}", manifest.display());
        let text =
            fs::read_to_string(&manifest).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
        let archive = fs::read(&xz).unwrap_or_else(|e| panic!("{}: {e}", xz.display()));
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(' ')))
                .unwrap_or_else(|| panic!("payload.manifest has no {key}"))
                .trim()
                .to_owned()
        };
        assert_eq!(
            text.lines().next(),
            Some(pack::PAYLOAD_HEADER),
            "payload.manifest: unknown format"
        );
        assert_eq!(
            field("version"),
            version,
            "the payload's version is not the installer's version"
        );
        assert_eq!(
            field("archive-size"),
            archive.len().to_string(),
            "payload.xz size does not match payload.manifest"
        );
        assert_eq!(
            field("archive-sha256"),
            pack::hex(&Sha256::digest(&archive)),
            "payload.xz SHA-256 does not match payload.manifest"
        );
        (xz, manifest)
    }

    /// A small deterministic payload for development and tests. Its "app" is not a
    /// program: sandbox runs never launch it.
    fn synthetic_payload(out: &Path, manifest_dir: &Path, version: &str) -> (PathBuf, PathBuf) {
        let root = manifest_dir.join("../..");
        let licence = root.join("LICENSE");
        let ofl = root.join("assets/fonts/OFL-Fraunces.txt");
        println!("cargo:rerun-if-changed={}", licence.display());
        let stage = out.join("synthetic-stage");
        if stage.exists() {
            fs::remove_dir_all(&stage).unwrap_or_else(|e| panic!("{}: {e}", stage.display()));
        }
        fs::create_dir_all(stage.join("licenses")).unwrap_or_else(|e| panic!("{e}"));
        let write = |rel: &str, bytes: &[u8]| {
            fs::write(stage.join(rel), bytes).unwrap_or_else(|e| panic!("{rel}: {e}"));
        };
        // xorshift64: incompressible "code" for the exe, a compressible pattern for the DLL.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let exe: Vec<u8> = (0..(256 * 1024 / 8))
            .flat_map(|_| next().to_le_bytes())
            .collect();
        let dll: Vec<u8> = (0..(768 * 1024))
            .map(|i: u32| {
                let noise = (next() & 0x3) as u8;
                (i % 251) as u8 ^ noise
            })
            .collect();
        write("opensmarters.exe", &exe);
        write("libmpv-2.dll", &dll);
        write(
            "README.txt",
            b"opensmarters: synthetic test payload\r\n\r\nThis is the payload of a development build of opensmarters-setup.\r\nIt contains no program; release builds embed the real app.\r\n",
        );
        write(
            "THIRD_PARTY_LICENSES.html",
            b"<!doctype html><title>Third-party licences (synthetic)</title><p>Development build.</p>\n",
        );
        write(
            "LICENSE.txt",
            &fs::read(&licence).unwrap_or_else(|e| panic!("{}: {e}", licence.display())),
        );
        write(
            "licenses/OFL-Fraunces.txt",
            &fs::read(&ofl).unwrap_or_else(|e| panic!("{}: {e}", ofl.display())),
        );
        let packed = pack::pack(&stage, "opensmarters", version, 1)
            .unwrap_or_else(|e| panic!("packing the synthetic payload: {e}"));
        let xz = out.join("payload.xz");
        let manifest = out.join("payload.manifest");
        fs::write(&xz, &packed.archive).unwrap_or_else(|e| panic!("{e}"));
        fs::write(&manifest, packed.manifest).unwrap_or_else(|e| panic!("{e}"));
        (xz, manifest)
    }

    fn resources(out: &Path, manifest_dir: &Path, version: &str) {
        let icon = manifest_dir.join("../../assets/icon/opensmarters.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        let icon = icon.to_string_lossy().replace('\\', "\\\\");
        let mut parts = version
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u16>().unwrap_or(0));
        let (major, minor, patch) = (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        );
        let app_manifest = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="opensmarters.setup" version="{major}.{minor}.{patch}.0" processorArchitecture="amd64"/>
  <description>opensmarters setup</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#
        );
        let manifest_path = out.join("opensmarters-setup.manifest");
        fs::write(&manifest_path, app_manifest).unwrap_or_else(|e| panic!("{e}"));
        let manifest_rc = manifest_path.to_string_lossy().replace('\\', "\\\\");
        let rc = format!(
            r#"// Generated by build.rs. Do not edit.
1 ICON "{icon}"
1 24 "{manifest_rc}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "opensmarters contributors"
            VALUE "FileDescription", "opensmarters setup"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "opensmarters-setup"
            VALUE "LegalCopyright", "GPL-3.0-or-later"
            VALUE "OriginalFilename", "opensmarters-setup.exe"
            VALUE "ProductName", "opensmarters"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
        );
        let rc_path = out.join("opensmarters-setup.rc");
        fs::write(&rc_path, rc).unwrap_or_else(|e| panic!("{e}"));
        // Unlike the app's icon, the manifest is not optional: without it the installer
        // would not be DPI-aware or long-path aware. Fail the build loudly.
        if let Err(e) =
            embed_resource::compile_for(&rc_path, ["opensmarters-setup"], embed_resource::NONE)
                .manifest_required()
        {
            panic!("could not embed the setup resources: {e}");
        }
    }
}
