// SPDX-License-Identifier: GPL-3.0-or-later
//! `opensmarters-pack <staged release folder> <output folder> [--preset N]`: packs the
//! folder `tools/package.ps1` staged (the same one it zips) into `payload.xz` and
//! `payload.manifest` for `OPENSMARTERS_SETUP_PAYLOAD` (ADR 0002, decision 3). Release
//! tool only; never shipped.

#![forbid(unsafe_code)]

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::Context as _;
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: opensmarters-pack <staged folder> <output folder> [--preset 0-9]";
    let src = std::path::PathBuf::from(args.next().context(usage)?);
    let out = std::path::PathBuf::from(args.next().context(usage)?);
    let mut preset = 9;
    while let Some(a) = args.next() {
        if a == "--preset" {
            preset = args
                .next()
                .and_then(|v| v.to_str().and_then(|s| s.parse().ok()))
                .context(usage)?;
        } else {
            anyhow::bail!("{usage}");
        }
    }
    let version = env!("CARGO_PKG_VERSION");
    let started = std::time::Instant::now();
    let packed = opensmarters_installer::pack::pack(
        &src,
        opensmarters_installer::brand::PRODUCT,
        version,
        preset,
    )
    .with_context(|| format!("packing {}", src.display()))?;
    // Round trip before writing anything: the manifest parses and every file extracts with
    // the right hash (catches a bad encoder before it ships).
    let manifest = opensmarters_installer::manifest::PayloadManifest::parse(&packed.manifest)
        .context("the new manifest does not parse")?;
    let payload = opensmarters_installer::payload::Payload::new(&packed.archive, manifest);
    payload.verify_archive().context("archive hash")?;
    let check = tempfile_dir(&out)?;
    let r = payload.extract(
        &check,
        &|_| {},
        &std::sync::atomic::AtomicBool::new(false),
        None,
    );
    let _ = std::fs::remove_dir_all(&check);
    r.context("the new archive does not extract")?;
    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("payload.xz"), &packed.archive)?;
    std::fs::write(out.join("payload.manifest"), &packed.manifest)?;
    println!(
        "packed {} files, {} bytes -> {} bytes (xz preset {preset}) in {:.0} s",
        payload.manifest.files.len(),
        payload.manifest.installed_size,
        packed.archive.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(windows)]
fn tempfile_dir(out: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let dir = out.join(format!("check-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(not(windows))]
fn main() {
    eprintln!("opensmarters-pack is a Windows tool.");
    std::process::exit(1);
}
