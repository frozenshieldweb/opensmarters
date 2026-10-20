// SPDX-License-Identifier: GPL-3.0-or-later
//! Login response (`player_api.php` without `action`).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use super::ser;
use crate::de;
use crate::epg::ServerTimeZone;
use crate::error::{AuthFailure, Error};

/// Maximum length kept for an unknown account status string.
const MAX_STATUS_LEN: usize = 64;

/// `user_info.status`. Matching is trimmed and case-insensitive (the original compares
/// `=== "Active"` exactly; accepting `active` is a deliberate, harmless relaxation).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AccountStatus {
    /// `Active`: the only status that allows login.
    Active,
    /// `Expired`.
    Expired,
    /// `Banned`.
    Banned,
    /// `Disabled`.
    Disabled,
    /// Any other value, verbatim (truncated to 64 bytes).
    Other(String),
}

impl AccountStatus {
    /// Parses a status string.
    pub fn parse(s: &str) -> Self {
        let t = s.trim();
        if t.eq_ignore_ascii_case("active") {
            AccountStatus::Active
        } else if t.eq_ignore_ascii_case("expired") {
            AccountStatus::Expired
        } else if t.eq_ignore_ascii_case("banned") {
            AccountStatus::Banned
        } else if t.eq_ignore_ascii_case("disabled") {
            AccountStatus::Disabled
        } else {
            let mut other = t.to_owned();
            de::truncate_at_char_boundary(&mut other, MAX_STATUS_LEN);
            AccountStatus::Other(other)
        }
    }

    /// The canonical spelling (`Active`, `Expired`, …) or the verbatim other value.
    pub fn as_str(&self) -> &str {
        match self {
            AccountStatus::Active => "Active",
            AccountStatus::Expired => "Expired",
            AccountStatus::Banned => "Banned",
            AccountStatus::Disabled => "Disabled",
            AccountStatus::Other(s) => s,
        }
    }

    /// `true` for [`AccountStatus::Active`].
    pub fn is_active(&self) -> bool {
        matches!(self, AccountStatus::Active)
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AccountStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AccountStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(AccountStatus::parse(&de::string(d)?))
    }
}

fn opt_status<'de, D: Deserializer<'de>>(d: D) -> Result<Option<AccountStatus>, D::Error> {
    Ok(de::opt_string(d)?.map(|s| AccountStatus::parse(&s)))
}

/// `user_info` from the login response.
///
/// `user_info.password` is echoed by panels but is **not** deserialised.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UserInfo {
    /// Username as echoed by the panel.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub username: Option<String>,
    /// Free-text message from the panel (provider data: display as plain text).
    #[serde(default, deserialize_with = "de::opt_string")]
    pub message: Option<String>,
    /// Whether the credentials were accepted.
    #[serde(default, deserialize_with = "de::bool")]
    pub auth: bool,
    /// Account status; `None` when the panel omits it.
    #[serde(default, deserialize_with = "opt_status")]
    pub status: Option<AccountStatus>,
    /// Expiry; `None` means unlimited (`null`, `""`, `0`).
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub exp_date: Option<DateTime<Utc>>,
    /// Trial account.
    #[serde(default, deserialize_with = "de::bool")]
    pub is_trial: bool,
    /// Connections currently in use.
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub active_cons: Option<u32>,
    /// Account creation time.
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub created_at: Option<DateTime<Utc>>,
    /// Maximum simultaneous connections (multiscreen limit).
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub max_connections: Option<u32>,
    /// Output formats the account may use (`"m3u8"`, `"ts"`, `"rtmp"`), as sent.
    /// See [`crate::urls::LiveContainer::allowed`].
    #[serde(default, deserialize_with = "de::string_list")]
    pub allowed_output_formats: Vec<String>,
}

/// `server_info` from the login response: where streams are served from, and the
/// server clock.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Stream host (may differ from the API host). Usually a bare host name.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub url: Option<String>,
    /// HTTP port for streams.
    #[serde(default, deserialize_with = "de::opt_u16")]
    pub port: Option<u16>,
    /// HTTPS port for streams.
    #[serde(default, deserialize_with = "de::opt_u16")]
    pub https_port: Option<u16>,
    /// `"http"` or `"https"`.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub server_protocol: Option<String>,
    /// RTMP port (unused).
    #[serde(default, deserialize_with = "de::opt_u16")]
    pub rtmp_port: Option<u16>,
    /// IANA time zone of the server (`Europe/…`).
    #[serde(default, deserialize_with = "de::opt_string")]
    pub timezone: Option<String>,
    /// Server clock at the time of the response.
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub timestamp_now: Option<DateTime<Utc>>,
    /// Server wall-clock time, `YYYY-MM-DD HH:MM:SS`.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub time_now: Option<String>,
}

impl ServerInfo {
    /// `true` when `server_protocol` is `https` (case-insensitive).
    pub fn is_https(&self) -> bool {
        self.server_protocol
            .as_deref()
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
    }

    /// The server's time zone (IANA name, else offset derived from `time_now` vs
    /// `timestamp_now`, else UTC). See [`ServerTimeZone::from_server_info`].
    pub fn time_zone(&self) -> ServerTimeZone {
        ServerTimeZone::from_server_info(self)
    }
}

/// The full login response.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LoginInfo {
    /// Account information.
    #[serde(default, deserialize_with = "de::object_or_default")]
    pub user_info: UserInfo,
    /// Stream server information.
    #[serde(default, deserialize_with = "de::object_or_default")]
    pub server_info: ServerInfo,
}

impl LoginInfo {
    /// Parses a login response body. An empty body, `[]`, `null` or an object without
    /// `user_info` yields a value with `auth == false` (panels answer that way to wrong
    /// credentials); [`LoginInfo::check`] then reports
    /// [`AuthFailure::InvalidCredentials`].
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] when the body is not JSON (e.g. an HTML error page).
    pub fn from_json(body: &[u8]) -> Result<Self, Error> {
        let text = de::body_text(body);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(LoginInfo::default());
        }
        let top: &RawValue = serde_json::from_str(trimmed).map_err(|e| de::not_json(&e))?;
        let s = top.get().trim();
        match s.as_bytes().first() {
            Some(b'{') => serde_json::from_str(s)
                .map_err(|e| Error::parse(format!("unexpected login response shape ({e})"))),
            Some(b'[') | Some(b'n') | Some(b'f') => Ok(LoginInfo::default()),
            _ if s == "\"\"" => Ok(LoginInfo::default()),
            _ => Err(Error::parse("the login response is not a JSON object")),
        }
    }

    /// The server's current time: `server_info.timestamp_now` when present, else
    /// `local_now`.
    pub fn server_now(&self, local_now: DateTime<Utc>) -> DateTime<Utc> {
        self.server_info.timestamp_now.unwrap_or(local_now)
    }

    /// Applies the login rules from `docs/PROTOCOL.md`:
    /// 1. `user_info.auth` must be truthy, else [`AuthFailure::InvalidCredentials`];
    /// 2. `status` must be `Active` (a missing status is tolerated), else
    ///    [`AuthFailure::Status`];
    /// 3. `exp_date`, when set, must be after the server clock (see
    ///    [`LoginInfo::server_now`]), else [`AuthFailure::Expired`].
    ///
    /// # Errors
    ///
    /// The [`AuthFailure`] describing the first rule that failed.
    pub fn check(&self, local_now: DateTime<Utc>) -> Result<(), AuthFailure> {
        let user = &self.user_info;
        if !user.auth {
            return Err(AuthFailure::InvalidCredentials);
        }
        if let Some(status) = user.status.as_ref().filter(|s| !s.is_active()) {
            return Err(AuthFailure::Status(status.clone()));
        }
        let server_now = self.server_now(local_now);
        if let Some(exp_date) = user.exp_date.filter(|e| *e <= server_now) {
            return Err(AuthFailure::Expired { exp_date });
        }
        Ok(())
    }

    /// The server's time zone; see [`ServerInfo::time_zone`].
    pub fn time_zone(&self) -> ServerTimeZone {
        self.server_info.time_zone()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const ACTIVE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/xtream/login_active.json"
    ));
    const LOOSE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/xtream/login_loose_types.json"
    ));

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_704_067_200, 0).unwrap() // 2024-01-01T00:00:00Z
    }

    #[test]
    fn parses_canonical_login() {
        let info = LoginInfo::from_json(ACTIVE.as_bytes()).unwrap();
        let u = &info.user_info;
        assert!(u.auth);
        assert_eq!(u.status, Some(AccountStatus::Active));
        assert_eq!(u.username.as_deref(), Some("demo_user"));
        assert_eq!(u.exp_date, DateTime::from_timestamp(1_893_456_000, 0));
        assert!(!u.is_trial);
        assert_eq!(u.active_cons, Some(0));
        assert_eq!(u.max_connections, Some(2));
        assert_eq!(u.allowed_output_formats, vec!["m3u8", "ts", "rtmp"]);
        let s = &info.server_info;
        assert_eq!(s.url.as_deref(), Some("stream.example.net"));
        assert_eq!(s.port, Some(8080));
        assert_eq!(s.https_port, Some(8443));
        assert!(!s.is_https());
        assert_eq!(s.timezone.as_deref(), Some("Europe/Paris"));
        assert_eq!(s.timestamp_now, DateTime::from_timestamp(1_704_067_200, 0));
        assert!(info.check(now()).is_ok());
        // The echoed password is never kept.
        let debug = format!("{info:?}");
        assert!(!debug.contains("demo_pass"));
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("demo_pass"));
    }

    #[test]
    fn parses_loosely_typed_login() {
        let info = LoginInfo::from_json(LOOSE.as_bytes()).unwrap();
        let u = &info.user_info;
        assert!(u.auth); // "1"
        assert_eq!(u.exp_date, None); // null
        assert!(u.is_trial); // 1
        assert_eq!(u.active_cons, Some(1)); // 1
        assert_eq!(u.max_connections, Some(1)); // "1"
        assert_eq!(u.allowed_output_formats, vec!["ts"]); // "ts"
        let s = &info.server_info;
        assert_eq!(s.port, Some(80)); // 80
        assert_eq!(s.https_port, None); // ""
        assert!(s.is_https()); // "HTTPS"
        assert_eq!(s.timestamp_now, DateTime::from_timestamp(1_704_067_200, 0)); // "1704067200"
        assert!(info.check(now()).is_ok());
    }

    #[test]
    fn login_rules() {
        let mut info = LoginInfo::from_json(ACTIVE.as_bytes()).unwrap();
        info.user_info.auth = false;
        assert_eq!(info.check(now()), Err(AuthFailure::InvalidCredentials));

        let mut info = LoginInfo::from_json(ACTIVE.as_bytes()).unwrap();
        info.user_info.status = Some(AccountStatus::parse("Expired"));
        assert_eq!(
            info.check(now()),
            Err(AuthFailure::Status(AccountStatus::Expired))
        );
        info.user_info.status = Some(AccountStatus::parse("Banned"));
        assert_eq!(
            info.check(now()),
            Err(AuthFailure::Status(AccountStatus::Banned))
        );
        info.user_info.status = Some(AccountStatus::parse("Suspended"));
        assert_eq!(
            info.check(now()),
            Err(AuthFailure::Status(AccountStatus::Other(
                "Suspended".into()
            )))
        );
        info.user_info.status = Some(AccountStatus::parse(" active "));
        assert!(info.check(now()).is_ok());
        info.user_info.status = None;
        assert!(info.check(now()).is_ok());
    }

    #[test]
    fn expiry_uses_server_clock() {
        let mut info = LoginInfo::from_json(ACTIVE.as_bytes()).unwrap();
        let exp = DateTime::from_timestamp(1_704_000_000, 0).unwrap(); // before server now
        info.user_info.exp_date = Some(exp);
        // The local clock is far in the past, but the server clock says expired.
        let local = DateTime::from_timestamp(1_600_000_000, 0).unwrap();
        assert_eq!(
            info.check(local),
            Err(AuthFailure::Expired { exp_date: exp })
        );
        // Without a server clock, the local clock is used.
        info.server_info.timestamp_now = None;
        assert!(info.check(local).is_ok());
        assert!(info.check(now()).is_err());
    }

    #[test]
    fn empty_answers_mean_invalid_credentials() {
        for body in [
            &b""[..],
            b"[]",
            b"null",
            b"{}",
            b"{\"user_info\":[]}",
            b"{\"user_info\":{\"auth\":0}}",
        ] {
            let info = LoginInfo::from_json(body).unwrap();
            assert_eq!(
                info.check(now()),
                Err(AuthFailure::InvalidCredentials),
                "{body:?}"
            );
        }
        assert!(matches!(
            LoginInfo::from_json(b"<html>Not Found</html>"),
            Err(Error::Parse { .. })
        ));
        assert!(matches!(
            LoginInfo::from_json(b"12"),
            Err(Error::Parse { .. })
        ));
    }

    #[test]
    fn login_round_trips() {
        let info = LoginInfo::from_json(LOOSE.as_bytes()).unwrap();
        let json = serde_json::to_vec(&info).unwrap();
        assert_eq!(LoginInfo::from_json(&json).unwrap(), info);
    }

    #[test]
    fn status_strings() {
        assert_eq!(AccountStatus::parse("Disabled"), AccountStatus::Disabled);
        assert_eq!(AccountStatus::parse("Disabled").to_string(), "Disabled");
        let long = "x".repeat(500);
        assert_eq!(AccountStatus::parse(&long).as_str().len(), 64);
    }
}
