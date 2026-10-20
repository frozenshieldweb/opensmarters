//! `fake-panel` binary: run the dev-only fake IPTV panel.
//!
//! Prints exactly one line to stdout once ready — `LISTENING http://127.0.0.1:<port>` —
//! and logs to stderr. Stops on Ctrl-C.

#![forbid(unsafe_code)]

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, ValueEnum};
use fake_panel::{Config, MediaMode, Quirks};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MediaArg {
    /// Generate missing media with ffmpeg before listening.
    Generate,
    /// Listen immediately, generate in the background (streams answer 503 until ready).
    Background,
    /// Never run ffmpeg.
    Never,
}

/// Dev-only fake Xtream Codes / M3U / XMLTV panel with synthetic data (binds 127.0.0.1).
#[derive(Debug, Parser)]
#[command(name = "fake-panel", version)]
struct Cli {
    /// Port on 127.0.0.1 (0 = pick a free port).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Directory holding (or receiving) the generated test media.
    #[arg(long, default_value = "testdata/generated")]
    media_dir: PathBuf,

    /// Comma-separated quirks; prefix with `no-` to disable (e.g. `no-category-all-literal`).
    /// Known: mixed-types, html-content-type, backdrop-string, episodes-array,
    /// category-all-literal (default on), slow, huge, stream-host, malformed-item,
    /// timeshift-503, timeshift-smarters-only, slow-media, live-endless, media-html,
    /// media-playlist.
    #[arg(long, value_delimiter = ',')]
    quirks: Vec<String>,

    /// Media generation mode.
    #[arg(long, value_enum, default_value_t = MediaArg::Generate)]
    media: MediaArg,

    /// Catalogue seed.
    #[arg(long, default_value_t = fake_panel::DEFAULT_SEED)]
    seed: u64,

    /// More logging (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_target(false)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let quirks = Quirks::parse_list(&cli.quirks)?;
    let config = Config {
        port: cli.port,
        media_dir: cli.media_dir,
        media: match cli.media {
            MediaArg::Generate => MediaMode::Generate,
            MediaArg::Background => MediaMode::Background,
            MediaArg::Never => MediaMode::Never,
        },
        quirks,
        seed: cli.seed,
    };
    let handle = fake_panel::spawn(config)
        .await
        .context("starting fake-panel")?;

    let mut out = std::io::stdout().lock();
    writeln!(out, "LISTENING {}", handle.base_url())?;
    out.flush()?;
    drop(out);

    tokio::signal::ctrl_c()
        .await
        .context("waiting for Ctrl-C")?;
    tracing::info!("shutting down");
    handle.shutdown().await;
    Ok(())
}
