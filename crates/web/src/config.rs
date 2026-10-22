// SPDX-License-Identifier: GPL-3.0-or-later
//! Instance configuration, read from environment variables.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `OPENSMARTERS_SECRET_KEY` | — (required) | 32 random bytes, base64. Encrypts provider secrets at rest and signs sessions and image addresses. `opensmarters-web --generate-secret-key` prints one. |
//! | `OPENSMARTERS_SECRET_KEY_FILE` | — | Read the key from this file instead (Docker secrets). |
//! | `OPENSMARTERS_DATA_DIR` | `./data` (`/data` in the image) | The database, the image cache and the stream folders. |
//! | `OPENSMARTERS_LISTEN` | `0.0.0.0:8080` | Address and port to listen on (plain HTTP; TLS is the reverse proxy's job). |
//! | `OPENSMARTERS_PUBLIC_URL` | — | The instance's public address (`https://tv.example.com`), used in invite links. Without it, links use the request's host. |
//! | `OPENSMARTERS_TRUSTED_PROXY` | — (off) | Trust `X-Forwarded-For` / `-Proto` / `-Host` from these peers: a comma-separated list of IP addresses or CIDR ranges, `private` (loopback and private ranges, e.g. Traefik on a Docker network) or `any`. |
//! | `OPENSMARTERS_MAX_STREAMS` | `8` | Streams the whole instance plays at once. |
//! | `OPENSMARTERS_STREAMS_PER_USER` | `1` | Streams one person plays at once (zapping replaces the previous one). |
//! | `OPENSMARTERS_TRANSCODE_VIDEO` | `never` | `never`: video is only copied; `hevc`: HEVC is converted to H.264 for browsers that can't play it; `always`: any video the browser can't play is converted. CPU-heavy. |
//! | `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS` | `0` | `1` lets providers on private networks (LAN, ULA, CGNAT) be used. Loopback, link-local (cloud metadata) and the container's gateway stay refused. |
//! | `OPENSMARTERS_SETUP_CODE` | random | The one-time code for creating the administrator; when unset, a random code is printed to the log at start. |
//! | `OPENSMARTERS_LOG` | `info` | `error`, `warn`, `info`, `debug` or `trace`. Logs are redacted. |

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use opensmarters_core::url::Url;

use crate::crypto::SecretKey;
use crate::net::IpNet;

/// The server's configuration. Build it with [`Config::from_env`] (the binary) or
/// [`Config::new`] (tests and embedders) and adjust the public fields.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where to listen.
    pub listen: SocketAddr,
    /// Data folder (database, caches, stream folders).
    pub data_dir: PathBuf,
    /// The instance secret.
    pub secret_key: SecretKey,
    /// Public base URL for invite links.
    pub public_url: Option<Url>,
    /// Peers whose `X-Forwarded-*` headers are trusted.
    pub trusted_proxy: TrustedProxy,
    /// Global stream cap.
    pub max_streams: usize,
    /// Per-user stream cap.
    pub streams_per_user: usize,
    /// Video transcoding policy.
    pub transcode_video: TranscodeVideo,
    /// Allow providers on private networks.
    pub allow_private_providers: bool,
    /// A fixed setup code (else a random one is generated and logged).
    pub setup_code: Option<String>,
    /// **Development and tests only, never read from the environment:** exact
    /// `ip:port` pairs on the loopback interface that outbound requests may reach when
    /// the URL names that IP literally (the in-process fake panel of `--demo`, test
    /// servers). Everything else on loopback stays refused.
    pub dev_loopback_allow: Vec<SocketAddr>,
    /// A stream nobody requested for this long is stopped (30 s).
    pub stream_idle_timeout: Duration,
    /// ffmpeg executable (looked up on `PATH`).
    pub ffmpeg: PathBuf,
    /// ffprobe executable (looked up on `PATH`).
    pub ffprobe: PathBuf,
}

/// How video is transcoded (`OPENSMARTERS_TRANSCODE_VIDEO`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscodeVideo {
    /// Never: H.264 is copied, HEVC is copied when the browser can play it, anything
    /// else is refused as unsupported.
    #[default]
    Never,
    /// HEVC is converted to H.264 when the browser can't play HEVC.
    Hevc,
    /// Any video the browser can't play is converted to H.264.
    Always,
}

impl TranscodeVideo {
    /// The wire name (`never`, `hevc`, `always`).
    pub fn as_str(self) -> &'static str {
        match self {
            TranscodeVideo::Never => "never",
            TranscodeVideo::Hevc => "hevc",
            TranscodeVideo::Always => "always",
        }
    }
}

impl FromStr for TranscodeVideo {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "never" | "off" | "0" => Ok(TranscodeVideo::Never),
            "hevc" => Ok(TranscodeVideo::Hevc),
            "always" => Ok(TranscodeVideo::Always),
            _ => Err(()),
        }
    }
}

/// Which peers may set `X-Forwarded-*` (`OPENSMARTERS_TRUSTED_PROXY`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TrustedProxy {
    /// Never trust forwarding headers (default).
    #[default]
    None,
    /// Trust every peer.
    Any,
    /// Trust loopback and private-range peers.
    Private,
    /// Trust these networks.
    Networks(Vec<IpNet>),
}

impl TrustedProxy {
    /// Parses the variable's value.
    ///
    /// # Errors
    ///
    /// A message naming the bad entry.
    pub fn parse(value: &str) -> Result<Self, String> {
        let v = value.trim();
        match v.to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" => return Ok(TrustedProxy::None),
            "any" | "*" | "1" | "true" | "yes" => return Ok(TrustedProxy::Any),
            "private" => return Ok(TrustedProxy::Private),
            _ => {}
        }
        let mut nets = Vec::new();
        for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            nets.push(
                IpNet::parse(part)
                    .ok_or_else(|| format!("{part:?} is not an IP address or CIDR range"))?,
            );
        }
        Ok(TrustedProxy::Networks(nets))
    }

    /// Whether a peer address is trusted.
    pub fn trusts(&self, peer: IpAddr) -> bool {
        let peer = crate::net::canonical_ip(peer);
        match self {
            TrustedProxy::None => false,
            TrustedProxy::Any => true,
            TrustedProxy::Private => peer.is_loopback() || crate::net::is_private_range(peer),
            TrustedProxy::Networks(nets) => nets.iter().any(|n| n.contains(peer)),
        }
    }
}

/// A configuration problem (the message is shown as is, then the server exits).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn parse_count(name: &str, default: usize, max: usize) -> Result<usize, ConfigError> {
    match env(name) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| ConfigError(format!("{name} must be a whole number from 1 to {max}"))),
    }
}

fn parse_bool(name: &str) -> Result<bool, ConfigError> {
    match env(name).map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(false),
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => Ok(true),
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => Ok(false),
        Some(_) => Err(ConfigError(format!("{name} must be 1 or 0"))),
    }
}

/// Validates a setup code: 8–64 printable ASCII characters without spaces.
pub fn valid_setup_code(code: &str) -> bool {
    (8..=64).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_graphic())
}

impl Config {
    /// Defaults for everything except the data folder and the key.
    pub fn new(data_dir: PathBuf, secret_key: SecretKey) -> Self {
        Config {
            listen: SocketAddr::from(([0, 0, 0, 0], 8080)),
            data_dir,
            secret_key,
            public_url: None,
            trusted_proxy: TrustedProxy::None,
            max_streams: 8,
            streams_per_user: 1,
            transcode_video: TranscodeVideo::Never,
            allow_private_providers: false,
            setup_code: None,
            dev_loopback_allow: Vec::new(),
            stream_idle_timeout: Duration::from_secs(30),
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
        }
    }

    /// Reads every variable but the key (see [`Config::read_secret_key`]).
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] naming the variable and what is expected.
    pub fn from_env(secret_key: SecretKey) -> Result<Self, ConfigError> {
        let data_dir =
            env("OPENSMARTERS_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from);
        let mut c = Config::new(data_dir, secret_key);
        if let Some(v) = env("OPENSMARTERS_LISTEN") {
            c.listen = v.trim().parse().map_err(|_| {
                ConfigError(
                    "OPENSMARTERS_LISTEN must be an address and port, e.g. 0.0.0.0:8080 or [::]:8080"
                        .into(),
                )
            })?;
        }
        if let Some(v) = env("OPENSMARTERS_PUBLIC_URL") {
            let url = Url::parse(v.trim())
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
                .filter(|u| u.username().is_empty() && u.password().is_none())
                .ok_or_else(|| {
                    ConfigError(
                        "OPENSMARTERS_PUBLIC_URL must be an http(s) address such as https://tv.example.com"
                            .into(),
                    )
                })?;
            c.public_url = Some(url);
        }
        if let Some(v) = env("OPENSMARTERS_TRUSTED_PROXY") {
            c.trusted_proxy = TrustedProxy::parse(&v)
                .map_err(|e| ConfigError(format!("OPENSMARTERS_TRUSTED_PROXY: {e}")))?;
        }
        c.max_streams = parse_count("OPENSMARTERS_MAX_STREAMS", c.max_streams, 1000)?;
        c.streams_per_user = parse_count("OPENSMARTERS_STREAMS_PER_USER", c.streams_per_user, 100)?;
        if let Some(v) = env("OPENSMARTERS_TRANSCODE_VIDEO") {
            c.transcode_video = v.parse().map_err(|()| {
                ConfigError("OPENSMARTERS_TRANSCODE_VIDEO must be never, hevc or always".into())
            })?;
        }
        c.allow_private_providers = parse_bool("OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS")?;
        if let Some(code) = env("OPENSMARTERS_SETUP_CODE") {
            let code = code.trim().to_owned();
            if !valid_setup_code(&code) {
                return Err(ConfigError(
                    "OPENSMARTERS_SETUP_CODE must be 8 to 64 visible characters without spaces"
                        .into(),
                ));
            }
            c.setup_code = Some(code);
        }
        Ok(c)
    }

    /// The key from `OPENSMARTERS_SECRET_KEY`, else from the file named by
    /// `OPENSMARTERS_SECRET_KEY_FILE`; `Ok(None)` when neither is set.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] when a value is set but unusable.
    pub fn read_secret_key() -> Result<Option<SecretKey>, ConfigError> {
        if let Some(v) = env("OPENSMARTERS_SECRET_KEY") {
            return SecretKey::from_base64(&v)
                .map(Some)
                .map_err(|e| ConfigError(e.to_string()));
        }
        if let Some(path) = env("OPENSMARTERS_SECRET_KEY_FILE") {
            let text =
                zeroize::Zeroizing::new(std::fs::read_to_string(path.trim()).map_err(|e| {
                    ConfigError(format!(
                        "OPENSMARTERS_SECRET_KEY_FILE could not be read: {e}"
                    ))
                })?);
            return SecretKey::from_base64(&text)
                .map(Some)
                .map_err(|e| ConfigError(format!("OPENSMARTERS_SECRET_KEY_FILE: {e}")));
        }
        Ok(None)
    }

    /// The log level from `OPENSMARTERS_LOG` (default `info`).
    pub fn log_level() -> tracing::Level {
        match env("OPENSMARTERS_LOG")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("error") => tracing::Level::ERROR,
            Some("warn") => tracing::Level::WARN,
            Some("debug") => tracing::Level::DEBUG,
            Some("trace") => tracing::Level::TRACE,
            _ => tracing::Level::INFO,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn transcode_values() {
        assert_eq!("never".parse(), Ok(TranscodeVideo::Never));
        assert_eq!("HEVC".parse(), Ok(TranscodeVideo::Hevc));
        assert_eq!("always".parse(), Ok(TranscodeVideo::Always));
        assert!("sometimes".parse::<TranscodeVideo>().is_err());
    }

    #[test]
    fn trusted_proxy_values() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(TrustedProxy::parse("").unwrap(), TrustedProxy::None);
        assert!(
            TrustedProxy::parse("any")
                .unwrap()
                .trusts(ip("203.0.113.9"))
        );
        let private = TrustedProxy::parse("private").unwrap();
        assert!(private.trusts(ip("10.0.1.5")));
        assert!(private.trusts(ip("::ffff:172.18.0.2")));
        assert!(private.trusts(ip("127.0.0.1")));
        assert!(!private.trusts(ip("203.0.113.9")));
        let nets = TrustedProxy::parse("10.0.1.0/24, 192.168.1.7").unwrap();
        assert!(nets.trusts(ip("10.0.1.200")));
        assert!(!nets.trusts(ip("10.0.2.1")));
        assert!(nets.trusts(ip("192.168.1.7")));
        assert!(!nets.trusts(ip("192.168.1.8")));
        assert!(TrustedProxy::parse("10.0.0.0/33").is_err());
        assert!(TrustedProxy::parse("traefik").is_err());
    }

    #[test]
    fn setup_codes() {
        assert!(valid_setup_code("ABCD-EFGH"));
        assert!(!valid_setup_code("short"));
        assert!(!valid_setup_code("has a space"));
    }
}
