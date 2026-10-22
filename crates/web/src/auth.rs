// SPDX-License-Identifier: GPL-3.0-or-later
//! People and sessions: password rules and hashing (Argon2id), the session cookie, the
//! signed-in-user extractors, client addresses and the sign-in rate limiter.
//!
//! - **Passwords:** at least 10 characters; Argon2id (m = 19 MiB, t = 2, p = 1, the
//!   OWASP server recommendation), at most two hashes computed at once; stored
//!   parameters are bounded on verification. Unknown names still cost one verification
//!   (no timing oracle for "does this name exist").
//! - **Sessions:** a 256-bit random token in the cookie `opensmarters_session`
//!   (`HttpOnly; Secure; SameSite=Strict; Path=/`), stored only as a keyed hash, 30-day
//!   sliding expiry, a new token at every sign-in.
//! - **Rate limit:** per client address and per name, five free failures, then a lockout
//!   that doubles with each further failure (2 s … 15 min); answered with 429 and
//!   `Retry-After`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::Semaphore;

use crate::db::{self, User};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// The session cookie's name.
pub const SESSION_COOKIE: &str = "opensmarters_session";
/// Session lifetime (sliding).
pub const SESSION_SECS: i64 = 30 * 24 * 3600;
/// Sliding updates are written at most this often.
const TOUCH_EVERY_SECS: i64 = 300;
/// Shortest password.
pub const MIN_PASSWORD_CHARS: usize = 10;
/// Longest password accepted (bounds hashing work).
pub const MAX_PASSWORD_BYTES: usize = 1024;

const ARGON_M_KIB: u32 = 19 * 1024;
const ARGON_T: u32 = 2;
const ARGON_P: u32 = 1;

/// Validates and returns a sign-in name: 2–32 characters, letters, digits, `.`, `_`, `-`.
///
/// # Errors
///
/// 400 `invalid_name`.
pub fn validate_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    let ok = (2..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(name.to_owned())
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "Names are 2 to 32 characters: letters, digits, dots, dashes and underscores.",
        ))
    }
}

/// Checks the password rules.
///
/// # Errors
///
/// 400 `weak_password`.
pub fn validate_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "Passwords need at least 10 characters.",
        ));
    }
    Ok(())
}

/// Argon2id hashing with bounded concurrency.
#[derive(Debug)]
pub struct Passwords {
    permits: Semaphore,
    dummy: String,
}

fn argon() -> Argon2<'static> {
    // The constants are valid parameters; `Params::default()` is only a fallback.
    let params = Params::new(ARGON_M_KIB, ARGON_T, ARGON_P, None).unwrap_or_default();
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

fn hash_blocking(password: &[u8]) -> Option<String> {
    argon().hash_password(password).ok().map(|h| h.to_string())
}

fn verify_blocking(password: &[u8], phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    if parsed.algorithm.as_str() != "argon2id" {
        return false;
    }
    match Params::try_from(&parsed) {
        // A tampered hash must not be able to ask for huge amounts of memory or time.
        Ok(p) if p.m_cost() <= 256 * 1024 && p.t_cost() <= 16 && p.p_cost() <= 8 => {}
        _ => return false,
    }
    Argon2::default().verify_password(password, &parsed).is_ok()
}

impl Passwords {
    /// Ready to hash.
    pub fn new() -> Self {
        Passwords {
            permits: Semaphore::new(2),
            dummy: hash_blocking(b"opensmarters-timing-equaliser").unwrap_or_default(),
        }
    }

    /// Hashes a password (on the blocking pool).
    ///
    /// # Errors
    ///
    /// 500 if hashing fails.
    pub async fn hash(&self, password: &SecretString) -> ApiResult<String> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| ApiError::internal("hash permits"))?;
        let pw = zeroize::Zeroizing::new(password.expose_secret().as_bytes().to_vec());
        tokio::task::spawn_blocking(move || hash_blocking(&pw))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| ApiError::internal("password hashing"))
    }

    /// Verifies a password against a PHC string; `None` verifies against a dummy hash
    /// (same cost) and returns `false`.
    pub async fn verify(&self, password: &SecretString, phc: Option<String>) -> bool {
        let Ok(_permit) = self.permits.acquire().await else {
            return false;
        };
        let real = phc.is_some();
        let phc = phc.unwrap_or_else(|| self.dummy.clone());
        let pw = zeroize::Zeroizing::new(password.expose_secret().as_bytes().to_vec());
        let ok = tokio::task::spawn_blocking(move || verify_blocking(&pw, &phc))
            .await
            .unwrap_or(false);
        ok && real
    }
}

impl Default for Passwords {
    fn default() -> Self {
        Self::new()
    }
}

/// `Set-Cookie` for a new session.
pub fn session_cookie(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_SECS}"
    ))
    .unwrap_or_else(|_| clear_cookie())
}

/// `Set-Cookie` that removes the session cookie.
pub fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "opensmarters_session=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0",
    )
}

/// The session token from the `Cookie` header(s).
pub fn read_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        for pair in text.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=')
                && k.trim() == SESSION_COOKIE
            {
                let v = v.trim();
                if (20..=128).contains(&v.len())
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

/// Creates a session for `user_id` and returns its token.
///
/// # Errors
///
/// 500 on database or random-source failure.
pub async fn create_session(state: &AppState, user_id: &str) -> ApiResult<String> {
    let token = crate::crypto::random_token().map_err(|_| ApiError::internal("random"))?;
    let hash = state.keys.token_hash("session", &token);
    let user_id = user_id.to_owned();
    let now = db::now();
    state
        .db
        .call(move |c| {
            db::purge_expired(c, now)?;
            db::insert_session(c, &hash, &user_id, now, now + SESSION_SECS)
        })
        .await?;
    Ok(token)
}

/// The signed-in user of a request.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    /// The user.
    pub user: User,
    /// Hash of the session token (identifies this browser's session).
    pub session: [u8; 32],
}

/// Looks up the session of a request, sliding its expiry.
///
/// # Errors
///
/// 500 on database failure.
pub async fn session_of(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<CurrentUser>> {
    let Some(token) = read_cookie(headers) else {
        return Ok(None);
    };
    let hash = state.keys.token_hash("session", &token);
    let now = db::now();
    let found = state
        .db
        .call(move |c| {
            let found = db::session_user(c, &hash, now)?;
            if let Some(s) = &found
                && now - s.last_used_at >= TOUCH_EVERY_SECS
            {
                db::touch_session(c, &hash, &s.user.id, now, now + SESSION_SECS)?;
            }
            Ok(found)
        })
        .await?;
    Ok(found.map(|s| CurrentUser {
        user: s.user,
        session: hash,
    }))
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if let Some(u) = parts.extensions.get::<CurrentUser>() {
            return Ok(u.clone());
        }
        let user = session_of(state, &parts.headers)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        parts.extensions.insert(user.clone());
        Ok(user)
    }
}

/// The signed-in administrator of a request.
#[derive(Debug, Clone)]
pub struct AdminUser(pub CurrentUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        if user.user.is_admin {
            Ok(AdminUser(user))
        } else {
            Err(ApiError::forbidden("Only the administrator can do that."))
        }
    }
}

/// The client's address: the TCP peer, or the `X-Forwarded-For` client when the peer is
/// a trusted proxy.
pub fn client_ip(state: &AppState, parts_headers: &HeaderMap, peer: Option<SocketAddr>) -> IpAddr {
    let peer_ip = peer.map_or(IpAddr::from([0, 0, 0, 0]), |p| p.ip());
    if !state.config.trusted_proxy.trusts(peer_ip) {
        return peer_ip;
    }
    // The right-most address that isn't itself a trusted proxy.
    let mut chain: Vec<IpAddr> = Vec::new();
    for value in parts_headers.get_all("x-forwarded-for") {
        if let Ok(text) = value.to_str() {
            chain.extend(
                text.split(',')
                    .filter_map(|s| s.trim().parse::<IpAddr>().ok()),
            );
        }
    }
    chain
        .iter()
        .rev()
        .find(|ip| !state.config.trusted_proxy.trusts(**ip))
        .or(chain.first())
        .copied()
        .unwrap_or(peer_ip)
}

/// The TCP peer of a request, when known (extractor).
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        Ok(Peer(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// The client address of a request (extractor).
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        Ok(ClientIp(client_ip(state, &parts.headers, peer)))
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    last: Instant,
    blocked_until: Option<Instant>,
}

/// Failure-counting rate limiter for sign-in and other secret checks.
#[derive(Debug, Default)]
pub struct Limiter {
    entries: Mutex<HashMap<String, Entry>>,
}

const FREE_FAILURES: u32 = 5;
const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);
const FORGET_AFTER: Duration = Duration::from_secs(3600);
const MAX_ENTRIES: usize = 20_000;

impl Limiter {
    /// `Err(wait)` when any of `keys` is locked out.
    ///
    /// # Errors
    ///
    /// The remaining wait.
    pub fn check(&self, keys: &[String]) -> Result<(), Duration> {
        let now = Instant::now();
        let map = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let wait = keys
            .iter()
            .filter_map(|k| map.get(k))
            .filter_map(|e| e.blocked_until)
            .filter(|until| *until > now)
            .map(|until| until - now)
            .max();
        match wait {
            Some(w) => Err(w),
            None => Ok(()),
        }
    }

    /// Records a failure for every key.
    pub fn fail(&self, keys: &[String]) {
        let now = Instant::now();
        let mut map = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() > MAX_ENTRIES {
            map.retain(|_, e| now.duration_since(e.last) < FORGET_AFTER);
        }
        for k in keys {
            let e = map.entry(k.clone()).or_insert(Entry {
                failures: 0,
                last: now,
                blocked_until: None,
            });
            if now.duration_since(e.last) >= FORGET_AFTER {
                e.failures = 0;
            }
            e.failures = e.failures.saturating_add(1);
            e.last = now;
            if e.failures >= FREE_FAILURES {
                let exp = (e.failures - FREE_FAILURES).min(10);
                let lock = Duration::from_secs(2u64 << exp).min(MAX_LOCKOUT);
                e.blocked_until = Some(now + lock);
            }
        }
    }

    /// Forgets the failures of `keys` after a success.
    pub fn succeed(&self, keys: &[String]) {
        let mut map = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        for k in keys {
            map.remove(k);
        }
    }

    /// A 429 answer for a lockout.
    pub fn rejection(wait: Duration) -> ApiError {
        let secs = wait.as_secs().max(1);
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!("Too many attempts. Wait {secs} seconds and try again."),
        )
        .retry_after(secs)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn names_and_passwords() {
        assert_eq!(validate_name(" alice ").unwrap(), "alice");
        assert!(validate_name("a").is_err());
        assert!(validate_name("al ice").is_err());
        assert!(validate_name("élise").is_err());
        assert!(validate_name(&"a".repeat(33)).is_err());
        assert!(validate_name("first.last-2_x").is_ok());
        assert!(validate_password("123456789").is_err());
        assert!(validate_password("1234567890").is_ok());
    }

    #[test]
    fn hashing() {
        let phc = hash_blocking(b"correct horse").unwrap();
        assert!(phc.starts_with("$argon2id$"));
        assert!(verify_blocking(b"correct horse", &phc));
        assert!(!verify_blocking(b"wrong horse", &phc));
        // A tampered hash asking for 4 GiB is refused without computing it.
        let greedy = phc.replace("m=19456", "m=4194304");
        assert!(!verify_blocking(b"correct horse", &greedy));
    }

    #[test]
    fn cookies() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "a=b; opensmarters_session=abcdefghijklmnopqrstuvwxyz0123456789-_ABCDE; c=d",
            ),
        );
        assert_eq!(
            read_cookie(&h).as_deref(),
            Some("abcdefghijklmnopqrstuvwxyz0123456789-_ABCDE")
        );
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("opensmarters_session=bad value"),
        );
        assert!(read_cookie(&h).is_none());
        let c = session_cookie("tok");
        let s = c.to_str().unwrap();
        for part in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(s.contains(part), "{s}");
        }
    }

    #[test]
    fn limiter() {
        let l = Limiter::default();
        let keys = vec!["ip:1".to_owned(), "name:a".to_owned()];
        for _ in 0..FREE_FAILURES - 1 {
            l.fail(&keys);
            assert!(l.check(&keys).is_ok());
        }
        l.fail(&keys);
        assert!(l.check(&keys).is_err());
        assert!(l.check(&["name:a".to_owned()]).is_err());
        assert!(l.check(&["ip:2".to_owned()]).is_ok());
        l.succeed(&keys);
        assert!(l.check(&keys).is_ok());
    }
}
