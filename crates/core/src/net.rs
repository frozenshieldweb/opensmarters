// SPDX-License-Identifier: GPL-3.0-or-later
//! HTTP configuration and a size-capped downloader.
//!
//! Everything goes through `reqwest` with **rustls** (certificate verification always
//! on, platform verifier; no native-tls / OpenSSL) and gzip decoding. Each request has
//! a connect timeout, a read (inactivity) timeout and an overall timeout. Bodies are
//! streamed and **capped after decompression** ([`HttpConfig::max_response_bytes`]), so
//! neither a huge `get_live_streams` nor a gzip bomb can exhaust memory.
//!
//! Redirects: at most [`HttpConfig::max_redirects`]; no `Referer` is sent; a redirect
//! from an `https` request to a plain `http` address is refused
//! ([`Error::InsecureRedirect`]) unless [`HttpConfig::allow_https_downgrade`] is set,
//! because API and playlist URLs carry the password.
//!
//! [`HttpClient`] is used by the Xtream client and is also the right tool for M3U and
//! XMLTV downloads (`get_bytes`). Its errors never contain the request URL.

use std::time::Duration;

use url::Url;

use crate::error::Error;
use crate::redact::Redactor;

/// Default User-Agent: `opensmarters/<version>`.
pub const DEFAULT_USER_AGENT: &str = concat!("opensmarters/", env!("CARGO_PKG_VERSION"));

/// The "IPTV Smarters compatible" User-Agent preset, for panels that whitelist known
/// player UAs. Apply it to mpv too (`user-agent` option).
pub const IPTV_SMARTERS_UA: &str = "IPTVSmartersPro";

/// Default response cap: 256 MiB (large panels return huge `get_live_streams`).
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;

/// HTTP settings shared by API calls and playlist / EPG downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// `User-Agent` header. Default [`DEFAULT_USER_AGENT`]; see [`IPTV_SMARTERS_UA`].
    pub user_agent: String,
    /// TCP + TLS connect timeout. Default 10 s.
    pub connect_timeout: Duration,
    /// Maximum time without receiving data (headers or body). Default 30 s.
    pub read_timeout: Duration,
    /// Overall deadline per request, including the body. Default 300 s.
    pub request_timeout: Duration,
    /// Maximum decompressed body size. Default [`DEFAULT_MAX_RESPONSE_BYTES`].
    pub max_response_bytes: u64,
    /// Maximum redirects followed. Default 5.
    pub max_redirects: usize,
    /// Follow a redirect that takes an `https` request to a plain `http` address.
    /// Default `false`: such a redirect fails with [`Error::InsecureRedirect`], because
    /// Xtream API and playlist URLs carry the password and the user chose `https`.
    /// Only turn it on for credential-free downloads (e.g. artwork).
    pub allow_https_downgrade: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(300),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_redirects: 5,
            allow_https_downgrade: false,
        }
    }
}

/// Marker error raised by the redirect policy for an `https` → `http` redirect.
#[derive(Debug)]
struct DowngradeRefused;

impl std::fmt::Display for DowngradeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("refused a redirect from https to http")
    }
}

impl std::error::Error for DowngradeRefused {}

/// Too many redirects (same meaning as reqwest's built-in limit).
#[derive(Debug)]
struct TooManyRedirects;

impl std::fmt::Display for TooManyRedirects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("too many redirects")
    }
}

impl std::error::Error for TooManyRedirects {}

/// True if following `next` would downgrade a request that started as `https`.
fn is_downgrade(original: &Url, next: &Url) -> bool {
    original.scheme() == "https" && next.scheme() != "https"
}

fn redirect_policy(max: usize, allow_downgrade: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > max {
            return attempt.error(TooManyRedirects);
        }
        let downgrade = attempt
            .previous()
            .first()
            .is_some_and(|first| is_downgrade(first, attempt.url()));
        if downgrade && !allow_downgrade {
            attempt.error(DowngradeRefused)
        } else {
            attempt.follow()
        }
    })
}

impl HttpConfig {
    /// The default configuration with the [`IPTV_SMARTERS_UA`] User-Agent.
    pub fn smarters_compatible() -> Self {
        HttpConfig {
            user_agent: IPTV_SMARTERS_UA.to_owned(),
            ..HttpConfig::default()
        }
    }
}

/// A `reqwest` client with the limits of an [`HttpConfig`].
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    max_response_bytes: u64,
    redactor: Redactor,
}

impl HttpClient {
    /// Builds a client. `redactor` scrubs error messages (pass the app-wide one so known
    /// secrets are covered).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] if the User-Agent is not a valid header value,
    /// [`Error::Network`] if the TLS backend cannot be initialised.
    pub fn new(config: &HttpConfig, redactor: Redactor) -> Result<Self, Error> {
        if reqwest::header::HeaderValue::from_str(&config.user_agent).is_err() {
            return Err(Error::InvalidConfig {
                reason: "the User-Agent contains characters that are not allowed in an HTTP header"
                    .into(),
            });
        }
        let inner = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .timeout(config.request_timeout)
            .redirect(redirect_policy(
                config.max_redirects,
                config.allow_https_downgrade,
            ))
            .referer(false)
            .gzip(true)
            .build()
            .map_err(|e| network_error(&e, &redactor))?;
        Ok(HttpClient {
            inner,
            max_response_bytes: config.max_response_bytes,
            redactor,
        })
    }

    /// The redactor used for error messages.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// `GET`s `url` and returns the (decompressed) body, enforcing the size cap.
    ///
    /// Only `http` and `https` URLs are accepted. The URL is logged at `debug` level,
    /// redacted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidUrl`], [`Error::Timeout`], [`Error::Network`],
    /// [`Error::Http`] for non-2xx statuses, [`Error::TooLarge`].
    pub async fn get_bytes(&self, url: &Url) -> Result<Vec<u8>, Error> {
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(Error::invalid_url(
                "only http and https URLs can be downloaded",
            ));
        }
        tracing::debug!(url = %self.redactor.redact(url.as_str()), "GET");
        let mut response = self
            .inner
            .get(url.clone())
            .send()
            .await
            .map_err(|e| map_reqwest_error(&e, &self.redactor))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
            });
        }
        let limit = self.max_response_bytes;
        if response.content_length().is_some_and(|len| len > limit) {
            return Err(Error::TooLarge { limit });
        }
        let initial = response
            .content_length()
            .unwrap_or(0)
            .min(limit)
            .min(16 * 1024 * 1024);
        let mut body = Vec::with_capacity(usize::try_from(initial).unwrap_or(0));
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| map_reqwest_error(&e, &self.redactor))?
        {
            if body.len() as u64 + chunk.len() as u64 > limit {
                return Err(Error::TooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

fn map_reqwest_error(e: &reqwest::Error, redactor: &Redactor) -> Error {
    if e.is_timeout() || source_chain_has_timeout(e) {
        Error::Timeout
    } else if source_chain_has::<DowngradeRefused>(e) {
        Error::InsecureRedirect
    } else {
        network_error(e, redactor)
    }
}

fn source_chain_has<T: std::error::Error + 'static>(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = current {
        if err.downcast_ref::<T>().is_some() {
            return true;
        }
        current = err.source();
    }
    false
}

fn source_chain_has_timeout(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = current {
        if err
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut)
        {
            return true;
        }
        current = err.source();
    }
    false
}

/// Describes a `reqwest` error without its URL, redacted.
fn network_error(e: &reqwest::Error, redactor: &Redactor) -> Error {
    let mut parts: Vec<String> = Vec::new();
    // `without_url` needs ownership; render the top level ourselves instead.
    let top = e.to_string();
    let top = match e.url() {
        Some(url) => top.replace(url.as_str(), "<url>"),
        None => top,
    };
    parts.push(top);
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        let text = err.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = err.source();
    }
    let mut message = redactor.redact(&parts.join(": "));
    crate::de::truncate_at_char_boundary(&mut message, 500);
    Error::Network { message }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = HttpConfig::default();
        assert!(c.user_agent.starts_with("opensmarters/"));
        assert_eq!(c.max_response_bytes, 256 * 1024 * 1024);
        assert_eq!(
            HttpConfig::smarters_compatible().user_agent,
            "IPTVSmartersPro"
        );
    }

    #[test]
    fn https_to_http_is_a_downgrade() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(is_downgrade(
            &u("https://panel.example.com/player_api.php?password=x"),
            &u("http://panel.example.com/player_api.php?password=x")
        ));
        assert!(!is_downgrade(
            &u("https://a.example.com/"),
            &u("https://b.example.com/")
        ));
        assert!(!is_downgrade(
            &u("http://a.example.com/"),
            &u("https://a.example.com/")
        ));
        assert!(!is_downgrade(
            &u("http://a.example.com/"),
            &u("http://b.example.com/")
        ));
        assert!(!HttpConfig::default().allow_https_downgrade);
    }

    #[test]
    fn rejects_bad_user_agent() {
        let c = HttpConfig {
            user_agent: "bad\nagent".into(),
            ..HttpConfig::default()
        };
        assert!(matches!(
            HttpClient::new(&c, Redactor::new()),
            Err(Error::InvalidConfig { .. })
        ));
    }
}
