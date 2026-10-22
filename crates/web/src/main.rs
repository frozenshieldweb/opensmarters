// SPDX-License-Identifier: GPL-3.0-or-later
//! `opensmarters-web`: the server of the self-hosted web edition.
//!
//! ```text
//! opensmarters-web                       serve (configuration from OPENSMARTERS_* variables)
//! opensmarters-web --demo                serve with the in-process fake provider and a demo person
//! opensmarters-web --demo --demo-fresh   the fake provider, but no person yet (try the first-run screens)
//! opensmarters-web --healthcheck         exit 0 if the server on OPENSMARTERS_LISTEN answers (Docker HEALTHCHECK)
//! opensmarters-web --generate-secret-key print a new OPENSMARTERS_SECRET_KEY
//! ```

#![forbid(unsafe_code)]

use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use opensmarters_core::Redactor;
use opensmarters_core::redact::RedactingWriter;
use opensmarters_web::config::{Config, ConfigError};
use opensmarters_web::crypto::SecretKey;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Parser, Debug)]
#[command(
    name = "opensmarters-web",
    version,
    about = "opensmarters web: the self-hosted web edition's server",
    long_about = "Serves the opensmarters web interface and API. Configuration comes from \
                  OPENSMARTERS_* environment variables (see docs/WEB_DEPLOY.md); \
                  OPENSMARTERS_SECRET_KEY is required."
)]
struct Cli {
    /// Run the built-in fake provider in-process and create a demo person
    /// (sign in as demo / demo-password). Demo data only.
    #[arg(long)]
    demo: bool,
    /// With --demo: don't create the demo person (the first-run setup shows instead).
    #[arg(long, requires = "demo")]
    demo_fresh: bool,
    /// Check that the server on OPENSMARTERS_LISTEN answers; exit 0 if it does.
    #[arg(long)]
    healthcheck: bool,
    /// Print a new random secret key for OPENSMARTERS_SECRET_KEY and exit.
    #[arg(long)]
    generate_secret_key: bool,
}

#[derive(Clone)]
struct RedactedStderr {
    redactor: Redactor,
}

impl<'a> MakeWriter<'a> for RedactedStderr {
    type Writer = RedactingWriter<io::Stderr>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(io::stderr(), self.redactor.clone())
    }
}

fn init_logging(redactor: &Redactor) {
    tracing_subscriber::fmt()
        .with_max_level(Config::log_level())
        .with_ansi(false)
        .with_target(false)
        .with_writer(RedactedStderr {
            redactor: redactor.clone(),
        })
        .init();
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
}

fn healthcheck() -> ExitCode {
    let listen: SocketAddr = std::env::var("OPENSMARTERS_LISTEN")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 8080)));
    let ip = match listen.ip() {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let target = SocketAddr::new(ip, listen.port());
    let check = || -> io::Result<bool> {
        let mut stream = TcpStream::connect_timeout(&target, Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(
            b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nUser-Agent: opensmarters-healthcheck\r\n\r\n",
        )?;
        let mut head = [0u8; 64];
        let mut got = 0;
        while got < 12 {
            let n = stream.read(&mut head[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        Ok(head[..got].starts_with(b"HTTP/1.1 200"))
    };
    match check() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("opensmarters-web healthcheck: unexpected answer from {target}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("opensmarters-web healthcheck: {target}: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn run(cli: Cli, redactor: Redactor) -> anyhow::Result<()> {
    let key = match Config::read_secret_key()? {
        Some(k) => k,
        None if cli.demo => {
            tracing::warn!(
                "demo: no OPENSMARTERS_SECRET_KEY, using a temporary one (saved providers won't survive a restart)"
            );
            SecretKey::generate()?
        }
        None => {
            return Err(ConfigError(
                "OPENSMARTERS_SECRET_KEY (or OPENSMARTERS_SECRET_KEY_FILE) is required: 32 random bytes in base64. \
                 Generate one with `opensmarters-web --generate-secret-key` and keep it safe."
                    .into(),
            )
            .into());
        }
    };
    #[allow(unused_mut)]
    let mut config = Config::from_env(key)?;
    if cli.demo && std::env::var_os("OPENSMARTERS_DATA_DIR").is_none() {
        config.data_dir = std::env::temp_dir().join("opensmarters-web-demo");
    }

    #[cfg(feature = "demo")]
    let panel = if cli.demo {
        std::fs::create_dir_all(&config.data_dir)?;
        let panel = opensmarters_web::demo::start_panel(&config.data_dir).await?;
        config.dev_loopback_allow.push(panel.addr());
        tracing::warn!(
            "demo: fake provider at {} (Xtream user demo / demo); only this loopback address is allowed",
            panel.base_url()
        );
        Some(panel)
    } else {
        None
    };
    #[cfg(not(feature = "demo"))]
    if cli.demo {
        anyhow::bail!("this build has no demo mode (built without the `demo` feature)");
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %config.data_dir.display(),
        max_streams = config.max_streams,
        streams_per_user = config.streams_per_user,
        transcode_video = config.transcode_video.as_str(),
        allow_private_providers = config.allow_private_providers,
        "starting opensmarters web"
    );
    let state = opensmarters_web::AppState::new(config, redactor).await?;
    #[cfg(feature = "demo")]
    if let Some(panel) = &panel
        && !cli.demo_fresh
    {
        opensmarters_web::demo::ensure_demo_user(&state, panel).await?;
    }
    let server = opensmarters_web::start_with_state(state).await?;
    let shown = if server.addr.ip().is_unspecified() {
        format!("http://localhost:{}", server.addr.port())
    } else {
        server.url()
    };
    tracing::info!("open {shown} in a browser");
    shutdown_signal().await;
    tracing::info!("shutting down");
    server.shutdown().await;
    #[cfg(feature = "demo")]
    if let Some(panel) = panel {
        panel.shutdown().await;
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.generate_secret_key {
        return match SecretKey::generate() {
            Ok(k) => {
                println!("{}", k.to_base64().as_str());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if cli.healthcheck {
        return healthcheck();
    }
    let redactor = Redactor::new();
    init_logging(&redactor);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("could not start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli, redactor)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("opensmarters-web: {e:#}");
            ExitCode::FAILURE
        }
    }
}
