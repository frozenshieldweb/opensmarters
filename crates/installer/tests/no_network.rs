// SPDX-License-Identifier: GPL-3.0-or-later
//! "No network access at all" (LAUNCH_PROMPT §B6): the installer binary links no HTTP
//! client, TLS stack or async network runtime. Checked on the real dependency graph with
//! `cargo tree` (normal dependencies, Windows target).

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

const NETWORK_CRATES: &[&str] = &[
    "reqwest",
    "hyper",
    "hyper-util",
    "h2",
    "h3",
    "http",
    "ureq",
    "curl",
    "curl-sys",
    "isahc",
    "attohttpc",
    "surf",
    "minreq",
    "rustls",
    "native-tls",
    "openssl",
    "openssl-sys",
    "tokio",
    "async-std",
    "mio",
    "socket2",
    "quinn",
];

#[test]
fn the_installer_links_no_http_client() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let out = std::process::Command::new(cargo)
        .args([
            "tree",
            "--manifest-path",
            manifest,
            "-p",
            "opensmarters-installer",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--target",
            "x86_64-pc-windows-msvc",
            "--offline",
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let crates: std::collections::BTreeSet<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(
        crates.contains("eframe"),
        "unexpected cargo tree output:\n{text}"
    );
    for bad in NETWORK_CRATES {
        assert!(
            !crates.contains(bad),
            "the installer depends on `{bad}`, a network crate"
        );
    }
}
