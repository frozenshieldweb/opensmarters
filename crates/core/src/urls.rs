// SPDX-License-Identifier: GPL-3.0-or-later
//! Stream URL builders (live, movie, episode, timeshift).
//!
//! ```text
//! base      = https://{server_info.url}:{https_port}   if server_protocol == "https"
//!             http://{server_info.url}:{port}          otherwise
//! LIVE      = {base}/live/{u}/{p}/{stream_id}.{ts|m3u8}
//! MOVIE     = {base}/movie/{u}/{p}/{stream_id}.{container_extension}
//! EPISODE   = {base}/series/{u}/{p}/{episode_id}.{container_extension}
//! TIMESHIFT = {base}/timeshift/{u}/{p}/{minutes}/{start}/{stream_id}.{ts|m3u8}
//! ```
//!
//! Differences from the original client (all compatible):
//! - **Fallbacks:** an empty `server_info.url` falls back to the login URL's host; an
//!   empty `server_protocol` to the login URL's scheme; an empty port to the login URL's
//!   port when the host and scheme match, else the scheme's default port. A
//!   `server_info.url` that contains a scheme, port or path is cleaned up.
//! - **Encoding:** the username, password and extension are percent-encoded (everything
//!   but `A–Z a–z 0–9 - . _ ~`), so `/`, `#`, `?`, `%`, spaces and Unicode are safe.
//!   (A username or password that is exactly `.` or `..` cannot be represented in a
//!   URL path; such URLs will not work.)
//! - **Default ports are implicit:** `http://host:80/…` is written `http://host/…`.
//! - A missing `container_extension` defaults to `mp4`.
//!
//! The returned [`Url`]s contain the password: never log them without
//! [`crate::Redactor`].

use std::fmt;

use chrono::NaiveDateTime;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::model::{ArchiveProgramme, LoginInfo, ServerInfo, ServerUrl, UserInfo, XtreamAccount};

/// Everything except RFC 3986 unreserved characters.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Container for live and timeshift URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LiveContainer {
    /// MPEG-TS (`.ts`), the original's default.
    #[default]
    Ts,
    /// HLS (`.m3u8`).
    M3u8,
}

impl LiveContainer {
    /// The extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            LiveContainer::Ts => "ts",
            LiveContainer::M3u8 => "m3u8",
        }
    }

    /// The containers allowed by `user_info.allowed_output_formats` (`ts`/`mpegts` →
    /// [`LiveContainer::Ts`], `m3u8`/`hls` → [`LiveContainer::M3u8`], others ignored),
    /// in the panel's order. When the list is empty or names neither, both are allowed.
    pub fn allowed(user_info: &UserInfo) -> Vec<LiveContainer> {
        let mut out = Vec::new();
        for f in &user_info.allowed_output_formats {
            let c = match f.trim().to_ascii_lowercase().as_str() {
                "ts" | "mpegts" => LiveContainer::Ts,
                "m3u8" | "hls" => LiveContainer::M3u8,
                _ => continue,
            };
            if !out.contains(&c) {
                out.push(c);
            }
        }
        if out.is_empty() {
            out = vec![LiveContainer::Ts, LiveContainer::M3u8];
        }
        out
    }

    /// `preferred` if the account allows it, else the first allowed container.
    pub fn preferred(preferred: LiveContainer, user_info: &UserInfo) -> LiveContainer {
        let allowed = Self::allowed(user_info);
        if allowed.contains(&preferred) {
            preferred
        } else {
            allowed.first().copied().unwrap_or(preferred)
        }
    }
}

impl fmt::Display for LiveContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

/// How the programme start is written in timeshift URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TimeshiftFormat {
    /// `YYYY-MM-DD:HH-MM` — what most clients send. The default.
    #[default]
    Standard,
    /// `YYYY-MM-DD:HH:MM:SS` — what IPTV Smarters sends (the EPG `start` with its first
    /// space replaced by `:`). Offer it as a per-account fallback.
    SmartersCompat,
}

impl TimeshiftFormat {
    /// Formats a server-local start time.
    pub fn format_start(self, start_local: NaiveDateTime) -> String {
        match self {
            TimeshiftFormat::Standard => start_local.format("%Y-%m-%d:%H-%M").to_string(),
            TimeshiftFormat::SmartersCompat => start_local.format("%Y-%m-%d:%H:%M:%S").to_string(),
        }
    }
}

/// Cleans a `server_info.url` value into `(host, port)`. Accepts `host`, `host:port`,
/// `http://host:port/path`, IPv6 literals.
fn clean_host(raw: &str) -> Option<(String, Option<u16>)> {
    let t = raw.trim();
    let lower = t.to_ascii_lowercase();
    let rest = if lower.starts_with("http://") {
        &t[7..]
    } else if lower.starts_with("https://") {
        &t[8..]
    } else {
        t
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty()
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let parsed = Url::parse(&format!("http://{authority}/")).ok()?;
    let host = parsed.host_str().filter(|h| !h.is_empty())?.to_owned();
    Some((host, parsed.port()))
}

/// Computes the stream base URL (`scheme://host[:port]/`) from `server_info`, with the
/// fallbacks described in the [module docs](self).
pub fn stream_base(server_info: &ServerInfo, login_url: &ServerUrl) -> Url {
    let login = login_url.as_url();
    let scheme = match server_info.server_protocol.as_deref().map(str::trim) {
        Some(p) if p.eq_ignore_ascii_case("https") => "https",
        Some(p) if p.eq_ignore_ascii_case("http") => "http",
        _ => login_url.scheme(),
    };
    let declared = server_info.url.as_deref().and_then(clean_host);
    let login_host = login_url.host_str().to_owned();
    let (host, host_port) = declared.unwrap_or_else(|| (login_host.clone(), None));
    let declared_port = if scheme == "https" {
        server_info.https_port
    } else {
        server_info.port
    };
    let port = declared_port.or(host_port).or_else(|| {
        if host.eq_ignore_ascii_case(&login_host) && scheme == login_url.scheme() {
            login.port()
        } else {
            None
        }
    });
    let built = Url::parse(&format!("{scheme}://{host}/"))
        .ok()
        .and_then(|mut u| {
            u.set_port(port).ok()?;
            Some(u)
        });
    built.unwrap_or_else(|| {
        let mut u = login.clone();
        u.set_path("/");
        u
    })
}

fn sanitize_extension(ext: Option<&str>) -> String {
    let e = ext.unwrap_or_default().trim().trim_start_matches('.');
    let e = if e.is_empty() { "mp4" } else { e };
    utf8_percent_encode(e, SEGMENT).to_string()
}

/// Builds stream URLs for one account. `Debug` hides the password.
#[derive(Clone)]
pub struct StreamUrls {
    base: Url,
    username: String,
    password: SecretString,
}

impl fmt::Debug for StreamUrls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamUrls")
            .field("base", &self.base.as_str())
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

impl StreamUrls {
    /// From a login response and the account it belongs to.
    pub fn new(login: &LoginInfo, account: &XtreamAccount, password: SecretString) -> Self {
        Self::from_server_info(
            &login.server_info,
            account.server(),
            account.username(),
            password,
        )
    }

    /// From the individual parts.
    pub fn from_server_info(
        server_info: &ServerInfo,
        login_url: &ServerUrl,
        username: &str,
        password: SecretString,
    ) -> Self {
        StreamUrls {
            base: stream_base(server_info, login_url),
            username: username.to_owned(),
            password,
        }
    }

    /// The stream base URL (`scheme://host[:port]/`), credential-free.
    pub fn base(&self) -> &Url {
        &self.base
    }

    fn build(&self, kind: &str, tail: &[&str]) -> Url {
        let mut path = format!(
            "/{kind}/{}/{}",
            utf8_percent_encode(&self.username, SEGMENT),
            utf8_percent_encode(self.password.expose_secret(), SEGMENT)
        );
        for seg in tail {
            path.push('/');
            path.push_str(seg);
        }
        let mut url = self.base.clone();
        url.set_path(&path);
        url
    }

    /// `{base}/live/{u}/{p}/{stream_id}.{ts|m3u8}`.
    pub fn live(&self, stream_id: u64, container: LiveContainer) -> Url {
        self.build("live", &[&format!("{stream_id}.{}", container.extension())])
    }

    /// `{base}/movie/{u}/{p}/{stream_id}.{ext}` (`ext` from the VOD list item's
    /// `container_extension`; `mp4` when missing).
    pub fn movie(&self, stream_id: u64, container_extension: Option<&str>) -> Url {
        let ext = sanitize_extension(container_extension);
        self.build("movie", &[&format!("{stream_id}.{ext}")])
    }

    /// `{base}/series/{u}/{p}/{episode_id}.{ext}`.
    pub fn episode(&self, episode_id: u64, container_extension: Option<&str>) -> Url {
        let ext = sanitize_extension(container_extension);
        self.build("series", &[&format!("{episode_id}.{ext}")])
    }

    /// Timeshift URL for an archived programme: duration in whole minutes rounded up
    /// ([`ArchiveProgramme::duration_minutes`]) and the server-local start in `format`.
    pub fn timeshift(
        &self,
        stream_id: u64,
        programme: &ArchiveProgramme,
        format: TimeshiftFormat,
        container: LiveContainer,
    ) -> Url {
        self.timeshift_at(
            stream_id,
            programme.start_local,
            programme.duration_minutes(),
            format,
            container,
        )
    }

    /// `{base}/timeshift/{u}/{p}/{minutes}/{start}/{stream_id}.{ts|m3u8}`.
    pub fn timeshift_at(
        &self,
        stream_id: u64,
        start_local: NaiveDateTime,
        duration_minutes: u32,
        format: TimeshiftFormat,
        container: LiveContainer,
    ) -> Url {
        self.build(
            "timeshift",
            &[
                &duration_minutes.max(1).to_string(),
                &format.format_start(start_local),
                &format!("{stream_id}.{}", container.extension()),
            ],
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn login_url(s: &str) -> ServerUrl {
        ServerUrl::parse(s).unwrap()
    }

    fn info(url: &str, port: Option<u16>, https_port: Option<u16>, proto: &str) -> ServerInfo {
        ServerInfo {
            url: (!url.is_empty()).then(|| url.to_owned()),
            port,
            https_port,
            server_protocol: (!proto.is_empty()).then(|| proto.to_owned()),
            ..ServerInfo::default()
        }
    }

    fn urls(si: &ServerInfo, login: &str, user: &str, pass: &str) -> StreamUrls {
        StreamUrls::from_server_info(
            si,
            &login_url(login),
            user,
            SecretString::from(pass.to_owned()),
        )
    }

    #[test]
    fn base_from_server_info() {
        let si = info("stream.example.net", Some(8080), Some(8443), "http");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "http://stream.example.net:8080/"
        );
        let si = info("stream.example.net", Some(8080), Some(8443), "https");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "https://stream.example.net:8443/"
        );
        // Port 80 is implicit.
        let si = info("stream.example.net", Some(80), None, "http");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "http://stream.example.net/"
        );
    }

    #[test]
    fn base_fallbacks() {
        // Everything empty → login URL scheme/host/port.
        let si = ServerInfo::default();
        assert_eq!(
            stream_base(&si, &login_url("https://panel.example.com:2096/sub")).as_str(),
            "https://panel.example.com:2096/"
        );
        // Host empty, port given → login host with that port.
        let si = info("", Some(25461), None, "http");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com:8080")).as_str(),
            "http://panel.example.com:25461/"
        );
        // Different host, no port → scheme default.
        let si = info("stream.example.net", None, None, "");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com:8080")).as_str(),
            "http://stream.example.net/"
        );
        // Same host as login, no port → login port.
        let si = info("panel.example.com", None, None, "");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com:8080")).as_str(),
            "http://panel.example.com:8080/"
        );
        // https protocol, https_port missing → 443.
        let si = info("stream.example.net", Some(8080), None, "https");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "https://stream.example.net/"
        );
        // Messy url field.
        let si = info(" http://stream.example.net:9000/path ", None, None, "http");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "http://stream.example.net:9000/"
        );
        // Garbage url field → login host.
        let si = info("not a host", Some(81), None, "http");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "http://panel.example.com:81/"
        );
        // IPv6.
        let si = info("[2001:db8::2]", Some(8000), None, "http");
        assert_eq!(
            stream_base(&si, &login_url("panel.example.com")).as_str(),
            "http://[2001:db8::2]:8000/"
        );
    }

    #[test]
    fn live_movie_episode_urls() {
        let si = info("stream.example.net", Some(8080), Some(8443), "http");
        let u = urls(&si, "panel.example.com", "demo_user", "demo_pass");
        assert_eq!(
            u.live(101, LiveContainer::Ts).as_str(),
            "http://stream.example.net:8080/live/demo_user/demo_pass/101.ts"
        );
        assert_eq!(
            u.live(101, LiveContainer::M3u8).as_str(),
            "http://stream.example.net:8080/live/demo_user/demo_pass/101.m3u8"
        );
        assert_eq!(
            u.movie(201, Some("mkv")).as_str(),
            "http://stream.example.net:8080/movie/demo_user/demo_pass/201.mkv"
        );
        assert_eq!(
            u.movie(205, Some(".avi")).as_str(),
            "http://stream.example.net:8080/movie/demo_user/demo_pass/205.avi"
        );
        assert_eq!(
            u.movie(206, None).as_str(),
            "http://stream.example.net:8080/movie/demo_user/demo_pass/206.mp4"
        );
        assert_eq!(
            u.episode(1001, Some("mp4")).as_str(),
            "http://stream.example.net:8080/series/demo_user/demo_pass/1001.mp4"
        );
    }

    #[test]
    fn credentials_are_percent_encoded() {
        let si = info("stream.example.net", Some(8080), None, "http");
        let u = urls(&si, "panel.example.com", "us er/é", "p@ss/w#rd?%20+&");
        assert_eq!(
            u.live(7, LiveContainer::Ts).as_str(),
            "http://stream.example.net:8080/live/us%20er%2F%C3%A9/p%40ss%2Fw%23rd%3F%2520%2B%26/7.ts"
        );
        // Hostile extension cannot escape the segment.
        assert_eq!(
            u.movie(1, Some("../../x?y#z")).as_str(),
            "http://stream.example.net:8080/movie/us%20er%2F%C3%A9/p%40ss%2Fw%23rd%3F%2520%2B%26/1.%2F..%2Fx%3Fy%23z"
        );
        let dbg = format!("{u:?}");
        assert!(!dbg.contains("p@ss") && !dbg.contains("p%40ss"));
    }

    #[test]
    fn dot_only_credentials_cannot_be_represented_but_stay_redacted() {
        // WHATWG URLs normalise "." and ".." segments (even percent-encoded), so such
        // credentials cannot work with any client. No panic, and known-secret redaction
        // still hides the password.
        let si = info("stream.example.net", Some(8080), None, "http");
        let url = urls(&si, "panel.example.com", ".", "secretpw").live(1, LiveContainer::Ts);
        assert_eq!(
            url.as_str(),
            "http://stream.example.net:8080/live/secretpw/1.ts"
        );
        let r = crate::Redactor::new();
        r.add_secret("secretpw");
        assert!(!r.redact(url.as_str()).contains("secretpw"));
    }

    #[test]
    fn timeshift_formats() {
        let si = info("stream.example.net", Some(8080), None, "http");
        let u = urls(&si, "panel.example.com", "demo_user", "demo_pass");
        let start = DateTime::from_timestamp(1_704_088_800, 0).unwrap();
        let prog = ArchiveProgramme {
            title: "T".into(),
            description: String::new(),
            start,
            stop: start + chrono::Duration::seconds(45 * 60 + 30),
            start_local: crate::epg::parse_server_datetime("2024-01-01 07:00:00").unwrap(),
        };
        assert_eq!(
            u.timeshift(101, &prog, TimeshiftFormat::Standard, LiveContainer::Ts)
                .as_str(),
            "http://stream.example.net:8080/timeshift/demo_user/demo_pass/46/2024-01-01:07-00/101.ts"
        );
        assert_eq!(
            u.timeshift(
                101,
                &prog,
                TimeshiftFormat::SmartersCompat,
                LiveContainer::M3u8
            )
            .as_str(),
            "http://stream.example.net:8080/timeshift/demo_user/demo_pass/46/2024-01-01:07:00:00/101.m3u8"
        );
        assert_eq!(
            u.timeshift_at(
                5,
                prog.start_local,
                0,
                TimeshiftFormat::Standard,
                LiveContainer::Ts
            )
            .as_str(),
            "http://stream.example.net:8080/timeshift/demo_user/demo_pass/1/2024-01-01:07-00/5.ts"
        );
    }

    #[test]
    fn allowed_containers() {
        let mut ui = UserInfo::default();
        assert_eq!(
            LiveContainer::allowed(&ui),
            vec![LiveContainer::Ts, LiveContainer::M3u8]
        );
        ui.allowed_output_formats = vec!["m3u8".into(), "ts".into(), "rtmp".into()];
        assert_eq!(
            LiveContainer::allowed(&ui),
            vec![LiveContainer::M3u8, LiveContainer::Ts]
        );
        ui.allowed_output_formats = vec!["M3U8".into(), "rtmp".into()];
        assert_eq!(LiveContainer::allowed(&ui), vec![LiveContainer::M3u8]);
        assert_eq!(
            LiveContainer::preferred(LiveContainer::Ts, &ui),
            LiveContainer::M3u8
        );
        ui.allowed_output_formats = vec!["rtmp".into()];
        assert_eq!(
            LiveContainer::preferred(LiveContainer::Ts, &ui),
            LiveContainer::Ts
        );
        ui.allowed_output_formats = vec!["ts".into(), "ts".into()];
        assert_eq!(LiveContainer::allowed(&ui), vec![LiveContainer::Ts]);
        assert_eq!(
            LiveContainer::preferred(LiveContainer::M3u8, &ui),
            LiveContainer::Ts
        );
    }

    #[test]
    fn from_login_info() {
        let login = LoginInfo::from_json(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/xtream/login_loose_types.json"
        )))
        .unwrap();
        // url "", port 80, https_port "", protocol "HTTPS" → login host over https:443.
        let account = XtreamAccount::new("panel.example.com:8080", "demo_user").unwrap();
        let u = StreamUrls::new(&login, &account, SecretString::from("demo_pass".to_owned()));
        assert_eq!(u.base().as_str(), "https://panel.example.com/");
    }
}
