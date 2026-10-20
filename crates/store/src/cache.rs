//! Encrypted response cache with TTLs and a total-size cap.
//!
//! Intended for provider responses that are slow to fetch (category lists, stream lists,
//! series info, M3U playlists, EPG, TMDB). Such responses can contain credentials — the
//! Xtream login response echoes the password and every M3U stream URL embeds it — so the
//! cache is **encrypted at rest**:
//!
//! - a random 256-bit master key lives in the [`SecretStore`](crate::SecretStore) under
//!   `cache_key` (created on first use; if it is lost, the cache is simply emptied);
//! - values are sealed with XChaCha20-Poly1305 (random 192-bit nonce per write), with the
//!   row's key id as associated data so rows cannot be swapped;
//! - cache keys are not stored either: rows are addressed by a keyed BLAKE2b-256 of
//!   `(scope, key)`, so a key like a TMDB request URL with `api_key=` never hits the disk.
//!
//! Freshness: [`Cache::get_fresh`] honours the TTL; [`Cache::get_stale_ok`] also returns
//! expired entries (flagged) for offline use or show-stale-then-refresh.
//! [`Cache::purge_expired`] deletes entries past TTL + grace.
//!
//! Size: the total stored size is capped ([`CACHE_MAX_BYTES`](crate::settings::CACHE_MAX_BYTES),
//! default 512 MiB). After each write the least recently *used* entries are evicted until
//! the total fits ("LRU-ish": reads refresh the access time at most once a minute to avoid
//! write amplification). A value larger than the whole cap is not stored.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use blake2::Blake2bMac;
use blake2::digest::consts::U32;
use blake2::digest::{FixedOutput, Update};
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{OptionalExtension, Transaction, params};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::Store;
use crate::error::{Result, StoreError, map_fk};
use crate::ids::ProfileId;
use crate::secrets::SecretKey;
use crate::settings::CACHE_MAX_BYTES;
use crate::time::{Timestamp, duration_to_millis};

const NONCE_LEN: usize = 24;
const MAX_KEY_BYTES: usize = 4096;
/// Reads refresh `last_access` at most this often.
const ACCESS_REFRESH: Duration = Duration::from_secs(60);
/// TTLs are clamped to this (≈ 10 years) to keep timestamp arithmetic in range.
const MAX_TTL: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

/// Whom a cache entry belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CacheScope {
    /// Not tied to a profile (e.g. TMDB responses). Survives profile deletion.
    Global,
    /// Owned by a profile; deleted with it or by [`Cache::invalidate_profile`].
    Profile(ProfileId),
}

impl CacheScope {
    fn profile(self) -> Option<ProfileId> {
        match self {
            Self::Global => None,
            Self::Profile(p) => Some(p),
        }
    }
}

/// A cache hit.
#[derive(Clone, PartialEq, Eq)]
pub struct CachedValue {
    /// The cached bytes.
    pub data: Vec<u8>,
    /// When the value was stored.
    pub fetched_at: Timestamp,
    /// When it stops being fresh.
    pub expires_at: Timestamp,
    /// `now < expires_at`.
    pub is_fresh: bool,
}

impl fmt::Debug for CachedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedValue")
            .field("len", &self.data.len())
            .field("fetched_at", &self.fetched_at)
            .field("expires_at", &self.expires_at)
            .field("is_fresh", &self.is_fresh)
            .finish()
    }
}

/// Key material derived from the cache master key.
pub(crate) struct CacheCrypto {
    id_key: Zeroizing<[u8; 32]>,
    cipher: XChaCha20Poly1305,
}

impl fmt::Debug for CacheCrypto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CacheCrypto { .. }")
    }
}

fn derive(master: &[u8; 32], persona: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mac = Blake2bMac::<U32>::new_with_salt_and_personal(Some(master), &[], persona)
        .map_err(|_| StoreError::CacheCrypto("key derivation"))?;
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&mac.finalize_fixed());
    Ok(out)
}

impl CacheCrypto {
    fn from_master(master: &[u8; 32]) -> Result<Self> {
        let id_key = derive(master, b"os.cache.id.v1")?;
        let enc_key = derive(master, b"os.cache.enc.v1")?;
        let cipher = XChaCha20Poly1305::new_from_slice(enc_key.as_slice())
            .map_err(|_| StoreError::CacheCrypto("cipher init"))?;
        Ok(Self { id_key, cipher })
    }

    fn key_id(&self, scope: CacheScope, key: &str) -> Result<[u8; 32]> {
        let mut mac = Blake2bMac::<U32>::new_from_slice(self.id_key.as_slice())
            .map_err(|_| StoreError::CacheCrypto("key id"))?;
        match scope {
            CacheScope::Global => mac.update(b"g"),
            CacheScope::Profile(p) => {
                mac.update(b"p");
                mac.update(p.as_uuid().as_bytes());
            }
        }
        mac.update(&[0]);
        mac.update(key.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&mac.finalize_fixed());
        Ok(out)
    }

    fn seal(&self, key_id: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = XNonce::try_generate().map_err(|_| StoreError::CacheCrypto("random nonce"))?;
        let ct = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: key_id,
                },
            )
            .map_err(|_| StoreError::CacheCrypto("encrypt"))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn open(&self, key_id: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < NONCE_LEN {
            return Err(StoreError::CacheCrypto("truncated value"));
        }
        let (n, ct) = sealed.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(n).map_err(|_| StoreError::CacheCrypto("nonce length"))?;
        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ct,
                    aad: key_id,
                },
            )
            .map_err(|_| StoreError::CacheCrypto("decrypt"))
    }
}

fn encode_hex(bytes: &[u8]) -> Zeroizing<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = Zeroizing::new(String::with_capacity(bytes.len() * 2));
    for b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

fn decode_hex32(s: &str) -> Option<Zeroizing<[u8; 32]>> {
    let s = s.trim().as_bytes();
    if s.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = Zeroizing::new([0u8; 32]);
    for (i, pair) in s.chunks_exact(2).enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

impl Store {
    /// Loads (or creates) the cache master key and derives the cache keys. Never call this
    /// while holding the connection lock.
    pub(crate) fn cache_crypto(&self) -> Result<Arc<CacheCrypto>> {
        let mut slot = self
            .cache_crypto
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = slot.as_ref() {
            return Ok(Arc::clone(c));
        }
        let name = SecretKey::cache_master_key();
        let existing = self
            .secrets
            .get(&name)?
            .and_then(|hex| decode_hex32(hex.expose_secret()));
        let master = match existing {
            Some(m) => m,
            None => {
                let key = chacha20poly1305::Key::try_generate()
                    .map_err(|_| StoreError::CacheCrypto("random key"))?;
                let mut m = Zeroizing::new([0u8; 32]);
                m.copy_from_slice(key.as_slice());
                let hex = encode_hex(m.as_slice());
                self.secrets.set(&name, &SecretString::from(hex.as_str()))?;
                // Anything already cached was sealed under a key we no longer have.
                self.conn().execute("DELETE FROM cache", [])?;
                tracing::info!("created a new cache encryption key");
                m
            }
        };
        let crypto = Arc::new(CacheCrypto::from_master(&master)?);
        *slot = Some(Arc::clone(&crypto));
        Ok(crypto)
    }

    /// Re-reads the cap from settings and evicts down to it.
    pub(crate) fn reload_cache_cap(&self) -> Result<()> {
        let cap = self.settings().get(&CACHE_MAX_BYTES)?;
        self.cache_max_bytes.store(cap, Ordering::SeqCst);
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        evict_to_fit(&tx, cap, None)?;
        tx.commit()?;
        Ok(())
    }
}

/// Deletes least-recently-used rows (never `keep`) until the total size is ≤ `cap`.
fn evict_to_fit(tx: &Transaction<'_>, cap: u64, keep: Option<&[u8; 32]>) -> Result<usize> {
    let total: i64 = tx.query_row("SELECT COALESCE(SUM(size), 0) FROM cache", [], |r| r.get(0))?;
    let mut total = u64::try_from(total).unwrap_or(0);
    if total <= cap {
        return Ok(0);
    }
    let mut victims: Vec<Vec<u8>> = Vec::new();
    {
        let mut stmt =
            tx.prepare_cached("SELECT key_id, size FROM cache ORDER BY last_access, size DESC")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if total <= cap {
                break;
            }
            let id: Vec<u8> = row.get(0)?;
            if keep.is_some_and(|k| k.as_slice() == id.as_slice()) {
                continue;
            }
            let size: i64 = row.get(1)?;
            total = total.saturating_sub(u64::try_from(size).unwrap_or(0));
            victims.push(id);
        }
    }
    let mut del = tx.prepare_cached("DELETE FROM cache WHERE key_id = ?1")?;
    for id in &victims {
        del.execute([id])?;
    }
    if !victims.is_empty() {
        tracing::debug!(evicted = victims.len(), "cache eviction");
    }
    Ok(victims.len())
}

fn validate_cache_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(StoreError::invalid("cache key", "must be 1-4096 bytes"));
    }
    Ok(())
}

/// Cache operations. Obtain with [`Store::cache`].
///
/// The first call creates the master key in the keychain if needed, so cache calls can
/// fail with [`StoreError::Secret`] when no keychain is available; the app should treat
/// cache errors as misses.
#[derive(Clone, Copy, Debug)]
pub struct Cache<'a> {
    pub(crate) store: &'a Store,
}

impl Cache<'_> {
    /// Stores `data` under `(scope, key)` for `ttl`, replacing any previous value, then
    /// evicts least-recently-used entries until the total fits the cap. Returns `false`
    /// (storing nothing, and dropping any previous value) if the value alone exceeds the
    /// cap.
    pub fn put(&self, scope: CacheScope, key: &str, data: &[u8], ttl: Duration) -> Result<bool> {
        validate_cache_key(key)?;
        let crypto = self.store.cache_crypto()?;
        let id = crypto.key_id(scope, key)?;
        let cap = self.store.cache_max_bytes.load(Ordering::SeqCst);
        let sealed = crypto.seal(&id, data)?;
        let size = sealed.len();
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        if u64::try_from(size).unwrap_or(u64::MAX) > cap {
            tx.execute("DELETE FROM cache WHERE key_id = ?1", [&id[..]])?;
            tx.commit()?;
            return Ok(false);
        }
        let now = self.store.now();
        tx.execute(
            "INSERT INTO cache (key_id, profile_id, value, size, fetched_at, ttl_ms, last_access)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5)
             ON CONFLICT (key_id) DO UPDATE SET
                 value = excluded.value, size = excluded.size, fetched_at = excluded.fetched_at,
                 ttl_ms = excluded.ttl_ms, last_access = excluded.last_access",
            params![
                &id[..],
                scope.profile(),
                sealed,
                i64::try_from(size).unwrap_or(i64::MAX),
                now,
                duration_to_millis(ttl.min(MAX_TTL))
            ],
        )
        .map_err(|e| match scope.profile() {
            Some(p) => map_fk(p)(e),
            None => StoreError::Db(e),
        })?;
        evict_to_fit(&tx, cap, Some(&id))?;
        tx.commit()?;
        Ok(true)
    }

    /// The value if present **and** within its TTL.
    pub fn get_fresh(&self, scope: CacheScope, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .get_stale_ok(scope, key)?
            .filter(|v| v.is_fresh)
            .map(|v| v.data))
    }

    /// The value if present, fresh or not (check [`CachedValue::is_fresh`]). An entry that
    /// fails to decrypt (e.g. the key was reset) is deleted and reported as a miss.
    pub fn get_stale_ok(&self, scope: CacheScope, key: &str) -> Result<Option<CachedValue>> {
        validate_cache_key(key)?;
        let crypto = self.store.cache_crypto()?;
        let id = crypto.key_id(scope, key)?;
        let now = self.store.now();
        let row: Option<(Vec<u8>, Timestamp, i64)> = {
            let conn = self.store.conn();
            let row = conn
                .prepare_cached(
                    "SELECT value, fetched_at, ttl_ms, last_access FROM cache WHERE key_id = ?1",
                )?
                .query_row([&id[..]], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, Timestamp>(3)?))
                })
                .optional()?;
            match row {
                None => None,
                Some((value, fetched_at, ttl_ms, last_access)) => {
                    if now.duration_since(last_access) >= ACCESS_REFRESH {
                        conn.execute(
                            "UPDATE cache SET last_access = ?2 WHERE key_id = ?1",
                            params![&id[..], now],
                        )?;
                    }
                    Some((value, fetched_at, ttl_ms))
                }
            }
        };
        let Some((sealed, fetched_at, ttl_ms)) = row else {
            return Ok(None);
        };
        // Decrypt without holding the database lock (values can be large).
        match crypto.open(&id, &sealed) {
            Ok(data) => {
                let expires_at = Timestamp::from_unix_millis(
                    fetched_at.as_unix_millis().saturating_add(ttl_ms.max(0)),
                );
                Ok(Some(CachedValue {
                    data,
                    fetched_at,
                    expires_at,
                    is_fresh: now < expires_at,
                }))
            }
            Err(_) => {
                tracing::warn!("dropping a cache entry that failed authentication");
                self.store
                    .conn()
                    .execute("DELETE FROM cache WHERE key_id = ?1", [&id[..]])?;
                Ok(None)
            }
        }
    }

    /// Removes one entry. Returns `false` if it was absent.
    pub fn remove(&self, scope: CacheScope, key: &str) -> Result<bool> {
        validate_cache_key(key)?;
        let id = self.store.cache_crypto()?.key_id(scope, key)?;
        let n = self
            .store
            .conn()
            .execute("DELETE FROM cache WHERE key_id = ?1", [&id[..]])?;
        Ok(n > 0)
    }

    /// Drops every entry of a profile (e.g. after the user hits "refresh" or edits the
    /// account). Returns the number removed.
    pub fn invalidate_profile(&self, profile: ProfileId) -> Result<usize> {
        Ok(self
            .store
            .conn()
            .execute("DELETE FROM cache WHERE profile_id = ?1", [profile])?)
    }

    /// Deletes entries whose TTL ended more than `grace` ago (`Duration::ZERO`: every
    /// expired entry) and reclaims free pages. Returns the number removed.
    pub fn purge_expired(&self, grace: Duration) -> Result<usize> {
        let cutoff = self.store.now().saturating_sub(grace.min(MAX_TTL));
        let conn = self.store.conn();
        let n = conn.execute("DELETE FROM cache WHERE fetched_at + ttl_ms < ?1", [cutoff])?;
        if n > 0 {
            conn.execute_batch("PRAGMA incremental_vacuum;")?;
        }
        Ok(n)
    }

    /// Empties the cache. Returns the number of entries removed.
    pub fn clear(&self) -> Result<usize> {
        let conn = self.store.conn();
        let n = conn.execute("DELETE FROM cache", [])?;
        conn.execute_batch("PRAGMA incremental_vacuum;")?;
        Ok(n)
    }

    /// Total stored size in bytes (ciphertext, i.e. data + 40 bytes per entry).
    pub fn total_bytes(&self) -> Result<u64> {
        let total: i64 =
            self.store
                .conn()
                .query_row("SELECT COALESCE(SUM(size), 0) FROM cache", [], |r| r.get(0))?;
        Ok(u64::try_from(total).unwrap_or(0))
    }

    /// Number of entries.
    pub fn len(&self) -> Result<usize> {
        let n: i64 = self
            .store
            .conn()
            .query_row("SELECT COUNT(*) FROM cache", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// True if the cache holds nothing.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// The current size cap in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.store.cache_max_bytes.load(Ordering::SeqCst)
    }

    /// Changes (and persists) the size cap, evicting immediately if needed.
    pub fn set_max_bytes(&self, bytes: u64) -> Result<()> {
        self.store.settings().set(&CACHE_MAX_BYTES, &bytes)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let bytes: Vec<u8> = (0u8..32).collect();
        let hex = encode_hex(&bytes);
        assert_eq!(decode_hex32(&hex).unwrap().as_slice(), bytes.as_slice());
        assert!(decode_hex32("zz").is_none());
        assert!(decode_hex32(&"g".repeat(64)).is_none());
    }

    #[test]
    fn seal_open_and_binding() {
        let c = CacheCrypto::from_master(&[7u8; 32]).unwrap();
        let p = ProfileId::new_random();
        let id = c.key_id(CacheScope::Profile(p), "k").unwrap();
        let id_global = c.key_id(CacheScope::Global, "k").unwrap();
        assert_ne!(id, id_global, "scope separates key ids");
        let sealed = c.seal(&id, b"secret payload").unwrap();
        assert_eq!(c.open(&id, &sealed).unwrap(), b"secret payload");
        assert!(c.open(&id_global, &sealed).is_err(), "AAD binds the row");
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(c.open(&id, &tampered).is_err());
        assert!(c.open(&id, &sealed[..10]).is_err());
        let other = CacheCrypto::from_master(&[8u8; 32]).unwrap();
        assert!(other.open(&id, &sealed).is_err(), "wrong key");
    }
}
