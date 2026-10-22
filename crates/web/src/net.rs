// SPDX-License-Identifier: GPL-3.0-or-later
//! The one outbound HTTP layer, safe against server-side request forgery (SSRF).
//!
//! People type provider addresses, and playlists, HLS manifests, API answers and
//! redirects contain more addresses: without care, the server could be made to fetch
//! `http://127.0.0.1:…`, the cloud metadata service, the Docker host or the owner's LAN.
//! **Every** outbound request (Xtream API, M3U, XMLTV, images, streams, HLS playlists and
//! segments, and every redirect of each) goes through [`Net`], which:
//!
//! - accepts only `http` and `https` URLs without credentials in them;
//! - resolves host names itself ([`SafeResolver`]) and connects only to addresses that
//!   pass [`Policy::check_ip`]; IP literals in URLs are checked before the request and at
//!   every redirect ([`Policy::check_url`]), so DNS rebinding can't slip an address past
//!   the check (the address that is checked is the address that is used);
//! - refuses **loopback, private (RFC 1918), CGNAT, link-local (incl. cloud metadata),
//!   multicast, unspecified, reserved/documentation ranges, the container's default
//!   gateway and `host.docker.internal`**, in IPv4 and IPv6, including IPv4-mapped,
//!   IPv4-compatible, NAT64 and 6to4 forms of those addresses;
//! - ignores `HTTP(S)_PROXY` (a proxy would bypass the address check);
//! - has connect, read and total timeouts and response size caps per purpose.
//!
//! `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS=1` allows the private ranges (a provider on the
//! owner's LAN); loopback, link-local, the gateway and the rest stay refused. Tests and
//! `--demo` can allow **exact `ip:port` pairs on loopback** through
//! [`Config::dev_loopback_allow`](crate::config::Config::dev_loopback_allow), which is
//! never read from the environment.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use opensmarters_core::Redactor;
use opensmarters_core::url::{Host, Url};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Why an address was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not `http`/`https`, no host, or credentials in the URL.
    NotWeb,
    /// 127.0.0.0/8, ::1.
    Loopback,
    /// RFC 1918, CGNAT, ULA, site-local, benchmarking.
    Private,
    /// 169.254.0.0/16, fe80::/10 (cloud metadata lives here).
    LinkLocal,
    /// 224.0.0.0/4, ff00::/8.
    Multicast,
    /// 0.0.0.0/8, ::.
    Unspecified,
    /// Reserved, documentation, broadcast, Teredo, discard ranges.
    Reserved,
    /// The container's default gateway or `host.docker.internal`.
    DockerHost,
    /// A host name that resolved to no acceptable address.
    NoAddress,
}

impl Refusal {
    /// A plain-English explanation for people.
    pub fn message(self) -> &'static str {
        match self {
            Refusal::NotWeb => {
                "Only http:// and https:// addresses without a user name or password in them can be used."
            }
            Refusal::Loopback | Refusal::DockerHost => {
                "That address points at this server itself, which isn't allowed."
            }
            Refusal::Private => {
                "That address is on a private network, which this server doesn't connect to. (The server's owner can allow it with OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS=1.)"
            }
            Refusal::LinkLocal
            | Refusal::Multicast
            | Refusal::Unspecified
            | Refusal::Reserved
            | Refusal::NoAddress => "That address can't be used by this server.",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Refusal::NotWeb => "not an http(s) address",
            Refusal::Loopback => "loopback address",
            Refusal::Private => "private network address",
            Refusal::LinkLocal => "link-local address",
            Refusal::Multicast => "multicast address",
            Refusal::Unspecified => "unspecified address",
            Refusal::Reserved => "reserved address",
            Refusal::DockerHost => "the Docker host's address",
            Refusal::NoAddress => "no acceptable address for the host name",
        };
        write!(f, "refused: {what}")
    }
}

impl std::error::Error for Refusal {}

/// An IP network (`10.0.0.0/8`, `fd00::/8`, or a single address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    addr: IpAddr,
    prefix: u8,
}

impl IpNet {
    /// Parses `addr` or `addr/prefix`.
    pub fn parse(s: &str) -> Option<IpNet> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.trim().parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.trim().parse::<u8>().ok().filter(|p| *p <= max)?,
            None => max,
        };
        Some(IpNet { addr, prefix })
    }

    /// Whether `ip` is inside.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, canonical_ip(ip)) {
            (IpAddr::V4(n), IpAddr::V4(i)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(n) & mask == u32::from(i) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(i)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(n) & mask == u128::from(i) & mask
            }
            _ => false,
        }
    }
}

fn v4_in(ip: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(net)) & mask
}

fn v6_in(ip: Ipv6Addr, net: u128, prefix: u32) -> bool {
    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
    u128::from(ip) & mask == net & mask
}

/// The IPv4 address an IPv6 address stands for, if it embeds one: IPv4-mapped
/// (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::/96`) and 6to4
/// (`2002:AABB:CCDD::/16`).
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let bits = u128::from(ip);
    let low32 = Ipv4Addr::from((bits & 0xffff_ffff) as u32);
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    // IPv4-compatible (deprecated), but not :: and ::1.
    if bits >> 32 == 0 && bits > 1 {
        return Some(low32);
    }
    if v6_in(ip, 0x0064_ff9b_0000_0000_0000_0000_0000_0000, 96) {
        return Some(low32);
    }
    if v6_in(ip, 0x2002_0000_0000_0000_0000_0000_0000_0000, 16) {
        return Some(Ipv4Addr::from(((bits >> 80) & 0xffff_ffff) as u32));
    }
    None
}

/// IPv6 forms of IPv4 addresses become the IPv4 address; everything else is unchanged.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => embedded_v4(v6).map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Private ranges: RFC 1918, CGNAT, benchmarking, ULA, site-local, local-use NAT64.
pub fn is_private_range(ip: IpAddr) -> bool {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => {
            v4_in(v4, [10, 0, 0, 0], 8)
                || v4_in(v4, [172, 16, 0, 0], 12)
                || v4_in(v4, [192, 168, 0, 0], 16)
                || v4_in(v4, [100, 64, 0, 0], 10)
                || v4_in(v4, [198, 18, 0, 0], 15)
        }
        IpAddr::V6(v6) => {
            v6_in(v6, 0xfc00 << 112, 7)
                || v6_in(v6, 0xfec0 << 112, 10)
                || v6_in(v6, 0x0064_ff9b_0001 << 80, 48)
        }
    }
}

/// What kind of address `ip` is, or `None` for an ordinary public address.
pub fn classify(ip: IpAddr) -> Option<Refusal> {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => {
            if v4_in(v4, [0, 0, 0, 0], 8) {
                Some(Refusal::Unspecified)
            } else if v4.is_loopback() {
                Some(Refusal::Loopback)
            } else if v4_in(v4, [169, 254, 0, 0], 16) {
                Some(Refusal::LinkLocal)
            } else if v4_in(v4, [224, 0, 0, 0], 4) {
                Some(Refusal::Multicast)
            } else if v4_in(v4, [240, 0, 0, 0], 4)
                || v4_in(v4, [192, 0, 0, 0], 24)
                || v4_in(v4, [192, 0, 2, 0], 24)
                || v4_in(v4, [198, 51, 100, 0], 24)
                || v4_in(v4, [203, 0, 113, 0], 24)
                || v4_in(v4, [192, 88, 99, 0], 24)
            {
                Some(Refusal::Reserved)
            } else if is_private_range(IpAddr::V4(v4)) {
                Some(Refusal::Private)
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() {
                Some(Refusal::Unspecified)
            } else if v6.is_loopback() {
                Some(Refusal::Loopback)
            } else if v6_in(v6, 0xfe80 << 112, 10) {
                Some(Refusal::LinkLocal)
            } else if v6_in(v6, 0xff00 << 112, 8) {
                Some(Refusal::Multicast)
            } else if v6_in(v6, 0x2001_0db8 << 96, 32)
                || v6_in(v6, 0x2001_0000 << 96, 32)
                || v6_in(v6, 0x0100 << 112, 64)
                || v6_in(v6, 0x3fff << 112, 20)
            {
                Some(Refusal::Reserved)
            } else if is_private_range(IpAddr::V6(v6)) {
                Some(Refusal::Private)
            } else {
                None
            }
        }
    }
}

/// The address policy shared by the resolver and the URL checks.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Allow the private ranges.
    pub allow_private: bool,
    /// Exact loopback `ip:port` pairs allowed (tests, `--demo`).
    pub dev_loopback_allow: Vec<SocketAddr>,
    /// Always refused (default gateway, `host.docker.internal`).
    pub docker_host: Vec<IpAddr>,
}

impl Policy {
    /// Checks one address. `port` is known for IP-literal URLs (needed for the
    /// `dev_loopback_allow` exceptions), `None` for resolved names.
    ///
    /// # Errors
    ///
    /// The [`Refusal`].
    pub fn check_ip(&self, ip: IpAddr, port: Option<u16>) -> Result<(), Refusal> {
        let ip = canonical_ip(ip);
        if self.docker_host.iter().any(|h| canonical_ip(*h) == ip) {
            return Err(Refusal::DockerHost);
        }
        match classify(ip) {
            None => Ok(()),
            Some(Refusal::Private) if self.allow_private => Ok(()),
            Some(Refusal::Loopback)
                if port.is_some_and(|p| {
                    self.dev_loopback_allow
                        .iter()
                        .any(|a| canonical_ip(a.ip()) == ip && a.port() == p)
                }) =>
            {
                Ok(())
            }
            Some(r) => Err(r),
        }
    }

    /// Checks a URL before it is requested (or followed): scheme, no credentials, and
    /// the address when the host is an IP literal (names are checked by the resolver).
    ///
    /// # Errors
    ///
    /// The [`Refusal`].
    pub fn check_url(&self, url: &Url) -> Result<(), Refusal> {
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Refusal::NotWeb);
        }
        let port = url.port_or_known_default();
        match url.host() {
            Some(Host::Ipv4(v4)) => self.check_ip(IpAddr::V4(v4), port),
            Some(Host::Ipv6(v6)) => self.check_ip(IpAddr::V6(v6), port),
            Some(Host::Domain(d)) if !d.is_empty() => Ok(()),
            _ => Err(Refusal::NotWeb),
        }
    }
}

/// Default gateways from `/proc/net/route` (Linux; nothing elsewhere).
pub fn default_gateways() -> Vec<IpAddr> {
    let Ok(text) = std::fs::read_to_string("/proc/net/route") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() > 2
            && cols[1] == "00000000"
            && let Ok(gw) = u32::from_str_radix(cols[2], 16)
            && gw != 0
        {
            // The kernel prints the address in host byte order (little endian).
            out.push(IpAddr::V4(Ipv4Addr::from(gw.swap_bytes())));
        }
    }
    out
}

/// The DNS resolver used for every outbound connection: system resolution, then only
/// the addresses the [`Policy`] accepts.
#[derive(Debug, Clone)]
pub struct SafeResolver {
    policy: Arc<Policy>,
}

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let lookup = tokio::time::timeout(
                Duration::from_secs(10),
                tokio::net::lookup_host((host.as_str(), 0u16)),
            )
            .await
            .map_err(|_| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "DNS lookup timed out",
                ))
            })?
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let mut first_refusal = None;
            let allowed: Vec<SocketAddr> = lookup
                .filter(|a| match policy.check_ip(a.ip(), None) {
                    Ok(()) => true,
                    Err(r) => {
                        first_refusal.get_or_insert(r);
                        false
                    }
                })
                .collect();
            if allowed.is_empty() {
                return Err(Box::new(first_refusal.unwrap_or(Refusal::NoAddress)) as _);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// Outbound request errors. They never contain the URL (which may hold credentials).
#[derive(Debug, Clone, thiserror::Error)]
pub enum NetError {
    /// The address was refused by the SSRF policy.
    #[error("{0}")]
    Blocked(Refusal),
    /// Malformed URL.
    #[error("invalid address")]
    InvalidUrl,
    /// Connect, read or total timeout.
    #[error("the request timed out")]
    Timeout,
    /// A non-success HTTP status.
    #[error("HTTP status {0}")]
    Http(u16),
    /// The body exceeded the cap.
    #[error("the response is larger than {0} bytes")]
    TooLarge(u64),
    /// A redirect from https to http.
    #[error("refused a redirect from https to http")]
    InsecureRedirect,
    /// Anything else (redacted, without the URL).
    #[error("network error: {0}")]
    Network(String),
}

/// What a download is for (decides limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Xtream API JSON (big "All" lists).
    Api,
    /// M3U playlists.
    Playlist,
    /// XMLTV guides (often gzip).
    Epg,
    /// HLS playlists.
    Hls,
    /// Artwork.
    Image,
}

impl Purpose {
    fn cap(self) -> u64 {
        match self {
            Purpose::Api | Purpose::Playlist => 128 * 1024 * 1024,
            Purpose::Epg => 96 * 1024 * 1024,
            Purpose::Hls => 4 * 1024 * 1024,
            Purpose::Image => 5 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
struct TooManyRedirects;
impl fmt::Display for TooManyRedirects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("too many redirects")
    }
}
impl std::error::Error for TooManyRedirects {}

#[derive(Debug)]
struct Downgrade;
impl fmt::Display for Downgrade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("refused a redirect from https to http")
    }
}
impl std::error::Error for Downgrade {}

fn redirect_policy(
    policy: Arc<Policy>,
    max: usize,
    allow_downgrade: bool,
) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > max {
            return attempt.error(TooManyRedirects);
        }
        if let Err(r) = policy.check_url(attempt.url()) {
            return attempt.error(r);
        }
        let downgrade = attempt
            .previous()
            .first()
            .is_some_and(|first| first.scheme() == "https" && attempt.url().scheme() != "https");
        if downgrade && !allow_downgrade {
            return attempt.error(Downgrade);
        }
        attempt.follow()
    })
}

fn find_in_chain<'a, T: std::error::Error + 'static>(
    e: &'a (dyn std::error::Error + 'static),
) -> Option<&'a T> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = current {
        if let Some(t) = err.downcast_ref::<T>() {
            return Some(t);
        }
        current = err.source();
    }
    None
}

fn map_error(e: &reqwest::Error, redactor: &Redactor) -> NetError {
    if let Some(r) = find_in_chain::<Refusal>(e) {
        return NetError::Blocked(*r);
    }
    if find_in_chain::<Downgrade>(e).is_some() {
        return NetError::InsecureRedirect;
    }
    if e.is_timeout()
        || find_in_chain::<std::io::Error>(e)
            .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut)
    {
        return NetError::Timeout;
    }
    let mut parts = Vec::new();
    let top = e.to_string();
    parts.push(match e.url() {
        Some(url) => top.replace(url.as_str(), "<url>"),
        None => top,
    });
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        let text = err.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = err.source();
    }
    let mut message = redactor.redact(&opensmarters_core::redact::redact_url(&parts.join(": ")));
    if message.len() > 300 {
        let mut cut = 300;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
    }
    NetError::Network(message)
}

/// The SSRF-safe HTTP client (cheap to clone).
#[derive(Clone)]
pub struct Net {
    policy: Arc<Policy>,
    api: reqwest::Client,
    media: reqwest::Client,
    image: reqwest::Client,
    redactor: Redactor,
}

impl fmt::Debug for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Net")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

fn builder(policy: &Arc<Policy>) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(opensmarters_core::DEFAULT_USER_AGENT)
        .dns_resolver(Arc::new(SafeResolver {
            policy: Arc::clone(policy),
        }))
        .no_proxy()
        .referer(false)
        .connect_timeout(Duration::from_secs(10))
}

/// A streaming response (stream bodies, segments).
pub type Response = reqwest::Response;

impl Net {
    /// Builds the clients.
    ///
    /// # Errors
    ///
    /// [`NetError::Network`] if the TLS backend can't start.
    pub fn new(policy: Policy, redactor: Redactor) -> Result<Self, NetError> {
        let policy = Arc::new(policy);
        let build = |b: reqwest::ClientBuilder| b.build().map_err(|e| map_error(&e, &redactor));
        let api = build(
            builder(&policy)
                .read_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(180))
                .redirect(redirect_policy(Arc::clone(&policy), 5, false)),
        )?;
        // Stream connections are never pooled: a stopped stream must close its
        // connection at once (providers count open connections).
        let media = build(
            builder(&policy)
                .read_timeout(Duration::from_secs(30))
                .pool_max_idle_per_host(0)
                .redirect(redirect_policy(Arc::clone(&policy), 5, false)),
        )?;
        let image = build(
            builder(&policy)
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .redirect(redirect_policy(Arc::clone(&policy), 3, true)),
        )?;
        Ok(Net {
            policy,
            api,
            media,
            image,
            redactor,
        })
    }

    /// The address policy.
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The redactor used for error messages and logs.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Parses and checks a URL typed by a person or found in provider data.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidUrl`] / [`NetError::Blocked`].
    pub fn check(&self, url: &Url) -> Result<(), NetError> {
        self.policy.check_url(url).map_err(NetError::Blocked)
    }

    /// `GET` with the purpose's size cap; returns the body and its `Content-Type`.
    ///
    /// # Errors
    ///
    /// See [`NetError`].
    pub async fn get(
        &self,
        url: &Url,
        purpose: Purpose,
    ) -> Result<(Vec<u8>, Option<String>), NetError> {
        self.check(url)?;
        let client = if purpose == Purpose::Image {
            &self.image
        } else {
            &self.api
        };
        tracing::debug!(url = %self.redactor.redact(url.as_str()), ?purpose, "GET");
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| map_error(&e, &self.redactor))?;
        let status = response.status();
        if !status.is_success() {
            return Err(NetError::Http(status.as_u16()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(200).collect());
        let cap = purpose.cap();
        if response.content_length().is_some_and(|l| l > cap) {
            return Err(NetError::TooLarge(cap));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| map_error(&e, &self.redactor))?
        {
            if body.len() as u64 + chunk.len() as u64 > cap {
                return Err(NetError::TooLarge(cap));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((body, content_type))
    }

    /// Opens a media stream (`GET`, optionally from a byte offset). The caller reads the
    /// body in chunks; dropping the response closes the connection.
    ///
    /// # Errors
    ///
    /// See [`NetError`]; non-2xx statuses are [`NetError::Http`].
    pub async fn open(
        &self,
        url: &Url,
        from_byte: Option<u64>,
        user_agent: Option<&str>,
    ) -> Result<Response, NetError> {
        self.check(url)?;
        tracing::debug!(url = %self.redactor.redact(url.as_str()), from_byte, "open stream");
        let mut request = self.media.get(url.clone());
        if let Some(from) = from_byte.filter(|b| *b > 0) {
            request = request.header(reqwest::header::RANGE, format!("bytes={from}-"));
        }
        if let Some(ua) = user_agent.filter(|ua| valid_header_value(ua)) {
            request = request.header(reqwest::header::USER_AGENT, ua);
        }
        let response = request
            .send()
            .await
            .map_err(|e| map_error(&e, &self.redactor))?;
        if !response.status().is_success() {
            return Err(NetError::Http(response.status().as_u16()));
        }
        Ok(response)
    }

    /// Reads the next chunk of a stream body.
    ///
    /// # Errors
    ///
    /// See [`NetError`].
    pub async fn chunk(&self, response: &mut Response) -> Result<Option<bytes::Bytes>, NetError> {
        response
            .chunk()
            .await
            .map_err(|e| map_error(&e, &self.redactor))
    }
}

/// Printable ASCII, at most 1 KiB (no CR/LF header injection).
pub fn valid_header_value(v: &str) -> bool {
    !v.is_empty() && v.len() <= 1024 && v.bytes().all(|b| (0x20..0x7f).contains(&b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classification() {
        for (a, want) in [
            ("8.8.8.8", None),
            ("2606:4700:4700::1111", None),
            ("127.0.0.1", Some(Refusal::Loopback)),
            ("127.255.0.9", Some(Refusal::Loopback)),
            ("::1", Some(Refusal::Loopback)),
            ("::ffff:127.0.0.1", Some(Refusal::Loopback)),
            ("::127.0.0.1", Some(Refusal::Loopback)),
            ("64:ff9b::7f00:1", Some(Refusal::Loopback)),
            ("2002:7f00:0001::", Some(Refusal::Loopback)),
            ("0.0.0.0", Some(Refusal::Unspecified)),
            ("::", Some(Refusal::Unspecified)),
            ("10.1.2.3", Some(Refusal::Private)),
            ("172.17.0.1", Some(Refusal::Private)),
            ("192.168.1.1", Some(Refusal::Private)),
            ("100.64.0.1", Some(Refusal::Private)),
            ("::ffff:10.0.0.1", Some(Refusal::Private)),
            ("fd12:3456::1", Some(Refusal::Private)),
            ("fc00::1", Some(Refusal::Private)),
            ("169.254.169.254", Some(Refusal::LinkLocal)),
            ("fe80::1", Some(Refusal::LinkLocal)),
            ("::ffff:169.254.169.254", Some(Refusal::LinkLocal)),
            ("224.0.0.1", Some(Refusal::Multicast)),
            ("ff02::1", Some(Refusal::Multicast)),
            ("255.255.255.255", Some(Refusal::Reserved)),
            ("192.0.2.1", Some(Refusal::Reserved)),
            ("2001:db8::1", Some(Refusal::Reserved)),
        ] {
            assert_eq!(classify(ip(a)), want, "{a}");
        }
    }

    #[test]
    fn policy() {
        let p = Policy {
            allow_private: false,
            dev_loopback_allow: vec!["127.0.0.1:4000".parse().unwrap()],
            docker_host: vec![ip("172.17.0.1")],
        };
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(p.check_url(&url("http://127.0.0.1:4000/x")).is_ok());
        assert_eq!(
            p.check_url(&url("http://127.0.0.1:4001/x")),
            Err(Refusal::Loopback)
        );
        assert_eq!(
            p.check_url(&url("http://[::ffff:127.0.0.1]:4001/")),
            Err(Refusal::Loopback)
        );
        assert_eq!(
            p.check_url(&url("http://2130706433/")),
            Err(Refusal::Loopback)
        );
        assert_eq!(p.check_url(&url("http://0x7f.1/")), Err(Refusal::Loopback));
        assert_eq!(p.check_url(&url("http://10.0.0.1/")), Err(Refusal::Private));
        assert_eq!(
            p.check_url(&url("http://[fd00::1]/")),
            Err(Refusal::Private)
        );
        assert_eq!(
            p.check_url(&url("ftp://example.com/")),
            Err(Refusal::NotWeb)
        );
        assert_eq!(
            p.check_url(&url("http://u:p@example.com/")),
            Err(Refusal::NotWeb)
        );
        assert!(p.check_url(&url("https://example.com/")).is_ok());
        // Resolved names have no port: loopback exceptions never apply to them.
        assert_eq!(p.check_ip(ip("127.0.0.1"), None), Err(Refusal::Loopback));
        let lan = Policy {
            allow_private: true,
            ..p.clone()
        };
        assert!(lan.check_url(&url("http://192.168.1.20:8080/")).is_ok());
        assert!(lan.check_url(&url("http://[fd00::1]/")).is_ok());
        assert_eq!(
            lan.check_url(&url("http://172.17.0.1/")),
            Err(Refusal::DockerHost)
        );
        assert_eq!(
            lan.check_url(&url("http://169.254.169.254/")),
            Err(Refusal::LinkLocal)
        );
        assert_eq!(
            lan.check_url(&url("http://127.0.0.2/")),
            Err(Refusal::Loopback)
        );
    }

    #[test]
    fn nets() {
        let n = IpNet::parse("10.0.0.0/8").unwrap();
        assert!(n.contains(ip("10.255.0.1")));
        assert!(n.contains(ip("::ffff:10.1.1.1")));
        assert!(!n.contains(ip("11.0.0.1")));
        assert!(IpNet::parse("fd00::/8").unwrap().contains(ip("fd12::1")));
        assert!(IpNet::parse("0.0.0.0/0").unwrap().contains(ip("1.2.3.4")));
        assert!(IpNet::parse("nope").is_none());
    }

    #[test]
    fn header_values() {
        assert!(valid_header_value("VLC/3.0"));
        assert!(!valid_header_value("a\r\nX: y"));
        assert!(!valid_header_value(""));
    }
}
