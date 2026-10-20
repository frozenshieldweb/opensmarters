// SPDX-License-Identifier: GPL-3.0-or-later
//! Third-party components shown in Settings › About. The list covers everything bundled
//! with the app plus every **direct** dependency of the app, core, store and player
//! crates (a unit test keeps it in sync with their `Cargo.toml`s); the full transitive
//! list with licence texts is generated for release packages.

/// One component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    /// Name (crate names, comma-separated, for Rust libraries).
    pub name: &'static str,
    /// SPDX licence expression or plain description.
    pub licence: &'static str,
    /// What it does here.
    pub role: &'static str,
}

const fn c(name: &'static str, licence: &'static str, role: &'static str) -> Component {
    Component {
        name,
        licence,
        role,
    }
}

/// Shipped next to or inside the app.
pub const BUNDLED: &[Component] = &[
    c(
        "libmpv (mpv)",
        "GPL-2.0-or-later (GPL build)",
        "video playback",
    ),
    c(
        "FFmpeg (inside libmpv)",
        "GPL-3.0-or-later build",
        "decoding and streaming",
    ),
    c(
        "SQLite",
        "Public domain",
        "local database (bundled by rusqlite)",
    ),
    c("Source Sans 3", "OFL-1.1", "interface font"),
    c("Fraunces", "OFL-1.1", "display font"),
    c("Phosphor Icons", "MIT", "icons"),
    c(
        "egui default fonts",
        "OFL-1.1, Ubuntu Font Licence 1.0",
        "fallback glyphs",
    ),
    c(
        "Mozilla CA certificates",
        "CDLA-Permissive-2.0",
        "HTTPS certificate roots",
    ),
];

/// Rust libraries the app uses directly (grouped).
pub const RUST: &[Component] = &[
    c(
        "eframe, egui, egui_extras, egui_glow, glow",
        "MIT OR Apache-2.0",
        "user interface",
    ),
    c("tokio", "MIT", "background work"),
    c(
        "reqwest, url, percent-encoding",
        "MIT OR Apache-2.0",
        "HTTP and addresses",
    ),
    c(
        "rustls (via reqwest)",
        "Apache-2.0 OR ISC OR MIT",
        "TLS (HTTPS)",
    ),
    c("rusqlite", "MIT", "database access"),
    c("keyring", "MIT OR Apache-2.0", "Windows Credential Manager"),
    c(
        "argon2, blake2, chacha20poly1305",
        "MIT OR Apache-2.0",
        "PIN hashing, cache encryption",
    ),
    c(
        "secrecy, zeroize",
        "Apache-2.0 OR MIT",
        "handling secrets in memory",
    ),
    c("serde, serde_json", "MIT OR Apache-2.0", "data formats"),
    c("quick-xml", "MIT", "TV guide (XMLTV)"),
    c(
        "chrono, chrono-tz",
        "MIT OR Apache-2.0",
        "dates and time zones",
    ),
    c("base64, flate2", "MIT OR Apache-2.0", "decoding"),
    c(
        "unicode-normalization",
        "(MIT OR Apache-2.0) AND Unicode-3.0",
        "text matching",
    ),
    c("image", "MIT OR Apache-2.0", "pictures"),
    c("lru", "MIT", "picture cache"),
    c("libloading", "ISC", "loading libmpv"),
    c("crossbeam-channel", "MIT OR Apache-2.0", "player events"),
    c("rfd", "MIT", "file dialogs"),
    c("directories", "MIT OR Apache-2.0", "data folder location"),
    c("uuid", "Apache-2.0 OR MIT", "profile ids"),
    c("windows", "MIT OR Apache-2.0", "Windows animation setting"),
    c("clap", "MIT OR Apache-2.0", "command line"),
    c("tracing, tracing-subscriber", "MIT", "logging"),
    c("thiserror, anyhow", "MIT OR Apache-2.0", "error handling"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Dependency names from the `[dependencies]` / `[target.*.dependencies]` sections.
    fn direct_deps(manifest: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut in_deps = false;
        for line in manifest.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_deps = t == "[dependencies]"
                    || (t.starts_with("[target.") && t.ends_with(".dependencies]"));
                continue;
            }
            if !in_deps || t.is_empty() || t.starts_with('#') {
                continue;
            }
            if let Some((name, _)) = t.split_once('=') {
                let name = name.trim().trim_matches('"');
                if !name.starts_with("opensmarters") && name != "fake-panel" {
                    out.push(name.to_owned());
                }
            }
        }
        out
    }

    #[test]
    fn every_direct_dependency_is_listed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let listed: Vec<&str> = RUST
            .iter()
            .flat_map(|c| {
                c.name
                    .split(',')
                    .map(|s| s.trim().split(' ').next().unwrap_or(""))
            })
            .collect();
        for krate in ["app", "core", "store", "player"] {
            let path = root.join(krate).join("Cargo.toml");
            let manifest = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
            for dep in direct_deps(&manifest) {
                assert!(
                    listed.contains(&dep.as_str()),
                    "{dep} (crates/{krate}) is missing from the About licence list"
                );
            }
        }
    }
}
