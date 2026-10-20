// SPDX-License-Identifier: GPL-3.0-or-later
//! Xtream account identity and server URL normalisation.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::error::Error;

/// Endpoint file names that users sometimes paste as part of the server URL.
const KNOWN_ENDPOINTS: [&str; 4] = ["player_api.php", "get.php", "xmltv.php", "panel_api.php"];

/// A normalised Xtream server base URL (`scheme://host[:port][/prefix]`).
///
/// Normalisation ([`ServerUrl::parse`]):
/// - trims surrounding whitespace and strips trailing `/`;
/// - prepends `http://` when no scheme is present (`panel.example.com:8080`);
/// - accepts only `http` and `https`;
/// - rejects empty input, inner whitespace/control characters, a missing host and
///   embedded credentials (`user:pass@host`), which must go in their own fields;
/// - drops any query string and fragment, and a trailing `player_api.php` / `get.php` /
///   `xmltv.php` / `panel_api.php` that was pasted along;
/// - keeps a path prefix (`http://host/panel` → API at `http://host/panel/player_api.php`).
///
/// A `ServerUrl` never contains credentials, so it is safe to display and log.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ServerUrl(Url);

impl ServerUrl {
    /// Parses and normalises a user-typed server URL.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidUrl`] with a human-readable reason (the input is not echoed).
    pub fn parse(input: &str) -> Result<Self, Error> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(Error::invalid_url("the server URL is empty"));
        }
        if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(Error::invalid_url(
                "the server URL contains spaces or control characters",
            ));
        }
        let lower = trimmed.to_ascii_lowercase();
        let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
            trimmed.to_owned()
        } else if trimmed.contains("://") {
            return Err(Error::invalid_url(
                "only http:// and https:// server URLs are supported",
            ));
        } else {
            format!("http://{trimmed}")
        };
        let mut url =
            Url::parse(&with_scheme).map_err(|_| Error::invalid_url("not a valid URL"))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(Error::invalid_url(
                "only http:// and https:// server URLs are supported",
            ));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(Error::invalid_url("the server URL has no host name"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::invalid_url(
                "the server URL must not contain a username or password; enter them in their own fields",
            ));
        }
        url.set_query(None);
        url.set_fragment(None);

        // Strip pasted endpoint files (repeatedly, so normalisation is idempotent).
        let mut path = url.path().trim_end_matches('/').to_owned();
        while let Some(endpoint) = KNOWN_ENDPOINTS
            .iter()
            .find(|e| path.to_ascii_lowercase().ends_with(&format!("/{e}")))
        {
            path.truncate(path.len() - endpoint.len() - 1);
            path.truncate(path.trim_end_matches('/').len());
        }
        url.set_path(&path);
        Ok(ServerUrl(url))
    }

    /// The underlying URL (path is `/` or a prefix without trailing slash).
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    /// `http` or `https`.
    pub fn scheme(&self) -> &str {
        self.0.scheme()
    }

    /// `true` for `https://` servers. For `http://`, the UI should warn that the password
    /// travels unencrypted.
    pub fn is_https(&self) -> bool {
        self.0.scheme() == "https"
    }

    /// Host name (IDNA-encoded) or IP literal.
    pub fn host_str(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    /// Explicit port, or the scheme's default (80/443).
    pub fn port_or_default(&self) -> u16 {
        self.0
            .port_or_known_default()
            .unwrap_or(if self.is_https() { 443 } else { 80 })
    }

    /// URL of an endpoint file under this server (`player_api.php`, `xmltv.php`, …),
    /// without a query string.
    pub fn endpoint(&self, file: &str) -> Url {
        let mut url = self.0.clone();
        let path = format!("{}/{}", url.path().trim_end_matches('/'), file);
        url.set_path(&path);
        url
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str().trim_end_matches('/'))
    }
}

impl fmt::Debug for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerUrl({self})")
    }
}

impl std::str::FromStr for ServerUrl {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        ServerUrl::parse(s)
    }
}

impl Serialize for ServerUrl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ServerUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ServerUrl::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// An Xtream Codes account: server URL and username.
///
/// The password is deliberately **not** part of this type: keep it in the OS keychain
/// and pass it as a [`secrecy::SecretString`] to [`crate::xtream::Client::new`] and
/// [`crate::urls::StreamUrls::new`]. This type is therefore safe to `Debug`-print and to
/// persist.
///
/// The username is used verbatim (not trimmed): it is a credential.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct XtreamAccount {
    server: ServerUrl,
    username: String,
}

impl XtreamAccount {
    /// Creates an account, normalising the server URL (see [`ServerUrl::parse`]).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidUrl`] when the server URL is unusable.
    pub fn new(server: &str, username: impl Into<String>) -> Result<Self, Error> {
        Ok(XtreamAccount {
            server: ServerUrl::parse(server)?,
            username: username.into(),
        })
    }

    /// Creates an account from an already-normalised server URL.
    pub fn from_parts(server: ServerUrl, username: impl Into<String>) -> Self {
        XtreamAccount {
            server,
            username: username.into(),
        }
    }

    /// The normalised server URL (the "login URL").
    pub fn server(&self) -> &ServerUrl {
        &self.server
    }

    /// The username, verbatim.
    pub fn username(&self) -> &str {
        &self.username
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        ServerUrl::parse(s).unwrap().to_string()
    }

    #[test]
    fn normalises_typed_urls() {
        assert_eq!(norm("panel.example.com"), "http://panel.example.com");
        assert_eq!(
            norm("  panel.example.com:8080/  "),
            "http://panel.example.com:8080"
        );
        assert_eq!(
            norm("http://panel.example.com:8080///"),
            "http://panel.example.com:8080"
        );
        assert_eq!(
            norm("HTTPS://Panel.Example.COM"),
            "https://panel.example.com"
        );
        assert_eq!(
            norm("http://panel.example.com:80"),
            "http://panel.example.com"
        );
        assert_eq!(
            norm("https://panel.example.com:8443/"),
            "https://panel.example.com:8443"
        );
        assert_eq!(
            norm("http://panel.example.com/sub/dir/"),
            "http://panel.example.com/sub/dir"
        );
        assert_eq!(norm("192.0.2.10:25461"), "http://192.0.2.10:25461");
        assert_eq!(norm("[2001:db8::1]:8080"), "http://[2001:db8::1]:8080");
    }

    #[test]
    fn strips_pasted_endpoints_and_queries() {
        assert_eq!(
            norm("http://panel.example.com:8080/player_api.php?username=a&password=b"),
            "http://panel.example.com:8080"
        );
        assert_eq!(
            norm("http://panel.example.com:8080/get.php?username=a&password=b&type=m3u_plus"),
            "http://panel.example.com:8080"
        );
        assert_eq!(
            norm("panel.example.com/x/PLAYER_API.PHP"),
            "http://panel.example.com/x"
        );
        assert_eq!(norm("panel.example.com/#frag"), "http://panel.example.com");
        assert_eq!(
            norm("panel.example.com/a/get.php//get.php"),
            "http://panel.example.com/a"
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "   ",
            "http://",
            "ftp://panel.example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "panel example.com",
            "panel.exa\nmple.com",
            "panel.example.com\u{0}",
            "http://user:pass@panel.example.com",
            "http://user@panel.example.com",
            "http://panel.example.com:99999",
            "http:// panel",
        ] {
            let err = ServerUrl::parse(bad);
            assert!(err.is_err(), "{bad:?} should be rejected");
        }
        // The error never echoes the input (which could contain credentials).
        let err = ServerUrl::parse("http://user:hunter2@panel.example.com").unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
    }

    #[test]
    fn endpoint_urls() {
        let s = ServerUrl::parse("panel.example.com:8080").unwrap();
        assert_eq!(
            s.endpoint("player_api.php").as_str(),
            "http://panel.example.com:8080/player_api.php"
        );
        let s = ServerUrl::parse("panel.example.com/prefix/").unwrap();
        assert_eq!(
            s.endpoint("xmltv.php").as_str(),
            "http://panel.example.com/prefix/xmltv.php"
        );
        assert_eq!(s.port_or_default(), 80);
        assert!(!s.is_https());
    }

    #[test]
    fn account_round_trips_through_serde() {
        let acc = XtreamAccount::new("panel.example.com:8080", "demo user").unwrap();
        let json = serde_json::to_string(&acc).unwrap();
        assert_eq!(
            json,
            r#"{"server":"http://panel.example.com:8080","username":"demo user"}"#
        );
        let back: XtreamAccount = serde_json::from_str(&json).unwrap();
        assert_eq!(back, acc);
        assert!(
            serde_json::from_str::<XtreamAccount>(r#"{"server":"ftp://x","username":"u"}"#)
                .is_err()
        );
    }
}
