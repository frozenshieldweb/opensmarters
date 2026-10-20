// SPDX-License-Identifier: GPL-3.0-or-later
//! Streaming HTTP downloads for media saved to disk (films, episodes, live recordings and
//! HLS segments).
//!
//! [`net::HttpClient`](crate::net::HttpClient) buffers whole bodies under a size cap, which
//! suits API answers and playlists. Media is different: a film is gigabytes and a live
//! channel never ends. [`Downloader`] streams the body chunk by chunk instead, so the
//! caller writes it to disk as it arrives (and decides when to stop: a disk-space guard,
//! a schedule, the user).
//!
//! What it guarantees:
//!
//! - **Timeouts:** a connect timeout and a read-idle timeout (no data for that long fails
//!   with [`Error::Timeout`]). There is no overall deadline: a live recording runs for
//!   hours.
//! - **Redirects:** at most [`DownloadConfig::max_redirects`], only to `http`/`https`, and
//!   never from `https` down to `http` (the URL carries the password), like
//!   [`net::HttpClient`](crate::net::HttpClient). No `Referer` is sent unless the caller
//!   asks for one; no cookies.
//! - **No transparent decompression** (`Accept-Encoding: identity`): byte offsets are the
//!   file's own, so a resume (`Range`) lines up.
//! - **Resumes are checked:** when asked to start at byte `n`, a `206` answer must carry a
//!   `Content-Range` that starts at exactly `n` ([`Error::RangeMismatch`] otherwise); a
//!   `200` means the server ignored the range and sends the whole file again
//!   ([`Download::start`] is then `0`, so the caller starts over); `416` is reported as
//!   [`Error::RangeNotSatisfiable`] with the size the server gave, if any (a finished file).
//! - **Nothing secret in errors:** messages never contain the URL and pass through the
//!   [`Redactor`]. Log URLs only through the redactor too.
//!
//! The downloader does **not** judge the content. Callers check the first bytes (the app
//! uses `opensmarters_player::sniff_container`) before keeping anything, because a
//! provider can answer `200 OK` with an HTML page or a playlist.

use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{self, HeaderValue};
use url::Url;

use crate::error::Error;
use crate::net::{DEFAULT_USER_AGENT, network_error, redirect_policy, request_error};
use crate::redact::Redactor;

/// Settings of a [`Downloader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadConfig {
    /// Default `User-Agent` (a request may override it). Default [`DEFAULT_USER_AGENT`].
    pub user_agent: String,
    /// TCP + TLS connect timeout. Default 10 s.
    pub connect_timeout: Duration,
    /// Longest wait for data (response headers or the next body bytes). Default 30 s.
    pub read_timeout: Duration,
    /// Maximum redirects followed. Default 5.
    pub max_redirects: usize,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        DownloadConfig {
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            max_redirects: 5,
        }
    }
}

/// Per-request options of [`Downloader::open`].
#[derive(Debug, Clone, Copy, Default)]
pub struct DownloadRequest<'a> {
    /// Start at this byte (`0` = the whole file). Anything else sends `Range: bytes=n-`.
    pub offset: u64,
    /// `User-Agent` for this request (an M3U entry's own), instead of the default.
    pub user_agent: Option<&'a str>,
    /// `Referer` for this request (an M3U entry's own, already checked by the caller).
    pub referrer: Option<&'a str>,
}

/// A streaming downloader. Cheap to clone (the connection pool is shared).
#[derive(Debug, Clone)]
pub struct Downloader {
    inner: reqwest::Client,
    redactor: Redactor,
}

/// An open download: the response headers were checked; read the body with
/// [`Download::chunk`].
#[derive(Debug)]
pub struct Download {
    response: reqwest::Response,
    start: u64,
    total: Option<u64>,
    redactor: Redactor,
}

/// The parts of a `Content-Range` header (`bytes first-last/total`, or `bytes */total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    /// First byte sent (`None` for the unsatisfied form `*/total`).
    pub first: Option<u64>,
    /// Last byte sent, inclusive.
    pub last: Option<u64>,
    /// Size of the whole resource, when the server knows it.
    pub total: Option<u64>,
}

/// Parses a `Content-Range` value. Returns `None` for anything malformed (unknown unit,
/// `last < first`, `last >= total`, garbage).
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse::<u64>().ok()?),
    };
    let range = range.trim();
    if range == "*" {
        return Some(ContentRange {
            first: None,
            last: None,
            total,
        });
    }
    let (first, last) = range.split_once('-')?;
    let first = first.trim().parse::<u64>().ok()?;
    let last = last.trim().parse::<u64>().ok()?;
    if last < first || total.is_some_and(|t| last >= t) {
        return None;
    }
    Some(ContentRange {
        first: Some(first),
        last: Some(last),
        total,
    })
}

impl Downloader {
    /// Builds a downloader. `redactor` scrubs error messages (pass the app-wide one).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] for an unusable User-Agent, [`Error::Network`] if the TLS
    /// backend cannot be initialised.
    pub fn new(config: &DownloadConfig, redactor: Redactor) -> Result<Self, Error> {
        if HeaderValue::from_str(&config.user_agent).is_err() {
            return Err(Error::InvalidConfig {
                reason: "the User-Agent contains characters that are not allowed in an HTTP header"
                    .into(),
            });
        }
        let inner = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .redirect(redirect_policy(config.max_redirects, false))
            .referer(false)
            .no_gzip()
            .build()
            .map_err(|e| network_error(&e, &redactor))?;
        Ok(Downloader { inner, redactor })
    }

    /// The redactor used for error messages (and for logging URLs).
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Requests `url` (from [`DownloadRequest::offset`]) and checks the answer's status and
    /// `Content-Range`. Only `http` and `https` URLs are accepted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidUrl`], [`Error::InvalidConfig`] (bad header value),
    /// [`Error::Timeout`], [`Error::Network`], [`Error::InsecureRedirect`],
    /// [`Error::Http`] for other statuses, [`Error::RangeMismatch`],
    /// [`Error::RangeNotSatisfiable`].
    pub async fn open(&self, url: &Url, req: &DownloadRequest<'_>) -> Result<Download, Error> {
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(Error::InvalidUrl {
                reason: "only http and https addresses can be downloaded".into(),
            });
        }
        tracing::debug!(url = %self.redactor.redact(url.as_str()), offset = req.offset, "download");
        let mut builder = self
            .inner
            .get(url.clone())
            .header(header::ACCEPT_ENCODING, "identity");
        if req.offset > 0 {
            builder = builder.header(header::RANGE, format!("bytes={}-", req.offset));
        }
        for (name, value) in [
            (header::USER_AGENT, req.user_agent),
            (header::REFERER, req.referrer),
        ] {
            if let Some(v) = value {
                let v = HeaderValue::from_str(v).map_err(|_| Error::InvalidConfig {
                    reason: "a request header contains characters that are not allowed".into(),
                })?;
                builder = builder.header(name, v);
            }
        }
        let response = builder
            .send()
            .await
            .map_err(|e| request_error(&e, &self.redactor))?;
        let status = response.status().as_u16();
        let content_range = response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(parse_content_range);
        let length = response.content_length();
        let (start, total) = match status {
            200 => (0, length),
            206 => {
                let Some(Some(range)) = content_range else {
                    return Err(Error::RangeMismatch);
                };
                if range.first != Some(req.offset) {
                    return Err(Error::RangeMismatch);
                }
                // The body must be exactly the announced range when its length is known.
                if let (Some(first), Some(last), Some(len)) = (range.first, range.last, length)
                    && last - first + 1 != len
                {
                    return Err(Error::RangeMismatch);
                }
                (req.offset, range.total)
            }
            416 => {
                return Err(Error::RangeNotSatisfiable {
                    total: content_range.flatten().and_then(|r| r.total),
                });
            }
            s => return Err(Error::Http { status: s }),
        };
        Ok(Download {
            response,
            start,
            total,
            redactor: self.redactor.clone(),
        })
    }
}

impl Download {
    /// Byte offset of the first body byte in the file: the requested offset, or `0` when
    /// the server ignored the range (start the file over).
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Size of the whole file, when the server said (`Content-Length` of a full answer,
    /// the total of a `Content-Range`).
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// The final URL after redirects (HLS resolves relative segment addresses against
    /// it). **Secret**: it may carry credentials; never log it unredacted.
    pub fn url(&self) -> &Url {
        self.response.url()
    }

    /// The `Content-Type` header, if any (informative only: never trust it).
    pub fn content_type(&self) -> Option<&str> {
        self.response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
    }

    /// The next piece of the body; `None` at the end.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] when no data arrived for the read timeout, [`Error::Network`].
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        self.response
            .chunk()
            .await
            .map_err(|e| request_error(&e, &self.redactor))
    }

    /// Reads the whole body, failing past `limit` bytes (playlists).
    ///
    /// # Errors
    ///
    /// [`Error::TooLarge`] and the errors of [`chunk`](Self::chunk).
    pub async fn read_to_end(mut self, limit: u64) -> Result<Vec<u8>, Error> {
        if self.total.is_some_and(|t| t > limit) {
            return Err(Error::TooLarge { limit });
        }
        let mut body = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            if body.len() as u64 + chunk.len() as u64 > limit {
                return Err(Error::TooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_ranges() {
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some(ContentRange {
                first: Some(100),
                last: Some(199),
                total: Some(1000)
            })
        );
        assert_eq!(
            parse_content_range(" bytes 0-0/*"),
            Some(ContentRange {
                first: Some(0),
                last: Some(0),
                total: None
            })
        );
        assert_eq!(
            parse_content_range("bytes */1234"),
            Some(ContentRange {
                first: None,
                last: None,
                total: Some(1234)
            })
        );
        for bad in [
            "",
            "bytes",
            "items 0-1/2",
            "bytes 5-4/10",
            "bytes 0-10/10",
            "bytes a-b/c",
            "bytes 0-1",
            "bytes -1-2/3",
            "bytes 0-99999999999999999999999/1",
        ] {
            assert_eq!(parse_content_range(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bad_user_agent_is_refused() {
        let c = DownloadConfig {
            user_agent: "x\r\ny".into(),
            ..DownloadConfig::default()
        };
        assert!(matches!(
            Downloader::new(&c, Redactor::new()),
            Err(Error::InvalidConfig { .. })
        ));
    }
}
