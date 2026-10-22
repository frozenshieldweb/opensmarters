// SPDX-License-Identifier: GPL-3.0-or-later
//! Keys and the few cryptographic operations the server needs.
//!
//! One instance secret (`OPENSMARTERS_SECRET_KEY`, 32 random bytes) is the root of
//! everything; purpose keys are derived from it with keyed BLAKE2b (a distinct
//! personalisation string per purpose), so no key is ever used for two things:
//!
//! | Purpose | Construction |
//! |---|---|
//! | Provider secrets at rest (passwords, M3U / EPG URLs) | XChaCha20-Poly1305, random 192-bit nonce, associated data = user id + account id |
//! | Item snapshots (favourites, progress: names and artwork addresses) | XChaCha20-Poly1305, random nonce, associated data = account id + kind + item |
//! | Session and invite tokens | stored as keyed BLAKE2b-256 hashes; the token itself is never stored |
//! | Image tokens (`/api/img/<token>`) | XChaCha20-Poly1305 with a synthetic nonce (keyed BLAKE2b of the address), so the same address gives the same token (browser caching) and the token both hides and authenticates the provider's address |
//!
//! Losing or changing the secret key makes every stored provider secret unreadable (the
//! accounts must be entered again) and signs everyone out; nothing else is lost.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD_INDIFFERENT, URL_SAFE_NO_PAD};
use blake2::Blake2bMac;
use blake2::digest::consts::U32;
use blake2::digest::{FixedOutput, Update};
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

const NONCE_LEN: usize = 24;
/// The longest provider image address that gets a token.
pub const MAX_IMAGE_URL_LEN: usize = 2048;

/// Errors from the crypto layer. They never contain key material or plaintext.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The operating system's random source failed.
    #[error("the system random number generator failed")]
    Random,
    /// The secret key is not 32 bytes of base64.
    #[error(
        "OPENSMARTERS_SECRET_KEY must be 32 random bytes in base64 (44 characters); generate one with `opensmarters-web --generate-secret-key` or `openssl rand -base64 32`"
    )]
    BadKey,
    /// Encryption failed (should not happen).
    #[error("encryption failed")]
    Seal,
    /// Decryption failed: tampered data, or the secret key changed.
    #[error("stored data could not be decrypted (was the secret key changed?)")]
    Open,
}

/// The instance secret. `Debug` never prints it.
#[derive(Clone)]
pub struct SecretKey(Zeroizing<[u8; 32]>);

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(***)")
    }
}

impl SecretKey {
    /// Parses 32 bytes of base64 (standard or URL-safe alphabet, padding optional,
    /// surrounding whitespace ignored).
    ///
    /// # Errors
    ///
    /// [`CryptoError::BadKey`] for anything else.
    pub fn from_base64(text: &str) -> Result<Self, CryptoError> {
        let normalised: Zeroizing<String> = Zeroizing::new(
            text.trim()
                .trim_end_matches('=')
                .chars()
                .map(|c| match c {
                    '-' => '+',
                    '_' => '/',
                    c => c,
                })
                .collect(),
        );
        let bytes = Zeroizing::new(
            STANDARD_NO_PAD_INDIFFERENT
                .decode(normalised.as_bytes())
                .map_err(|_| CryptoError::BadKey)?,
        );
        let array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::BadKey)?;
        Ok(SecretKey(Zeroizing::new(array)))
    }

    /// A new random key.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Random`].
    pub fn generate() -> Result<Self, CryptoError> {
        Ok(SecretKey(Zeroizing::new(random_bytes::<32>()?)))
    }

    /// Standard base64 with padding (what `openssl rand -base64 32` prints).
    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(self.0.as_slice()))
    }
}

/// `N` bytes from the operating system's random source.
///
/// # Errors
///
/// [`CryptoError::Random`].
pub fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|_| CryptoError::Random)?;
    Ok(out)
}

/// A 256-bit random token, URL-safe base64 without padding (43 characters).
///
/// # Errors
///
/// [`CryptoError::Random`].
pub fn random_token() -> Result<String, CryptoError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<32>()?))
}

/// A 128-bit random id, lowercase hex (32 characters).
///
/// # Errors
///
/// [`CryptoError::Random`].
pub fn random_id() -> Result<String, CryptoError> {
    Ok(hex(&random_bytes::<16>()?))
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    s
}

fn derive(master: &[u8; 32], persona: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    // A 32-byte key and a personalisation of at most 16 bytes are always valid for
    // BLAKE2b; the fallback (an all-zero key) is unreachable.
    if let Ok(mac) = Blake2bMac::<U32>::new_with_salt_and_personal(Some(master), &[], persona) {
        out.copy_from_slice(&mac.finalize_fixed());
    }
    out
}

fn mac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut out = [0u8; 32];
    if let Ok(mut m) = <Blake2bMac<U32> as blake2::digest::KeyInit>::new_from_slice(key) {
        for p in parts {
            // Length-prefix every part so ("ab","c") and ("a","bc") differ.
            m.update(&(p.len() as u64).to_le_bytes());
            m.update(p);
        }
        out.copy_from_slice(&m.finalize_fixed());
    }
    out
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

/// The derived purpose keys.
pub struct Keys {
    secrets: XChaCha20Poly1305,
    snapshots: XChaCha20Poly1305,
    images: XChaCha20Poly1305,
    image_nonce: Zeroizing<[u8; 32]>,
    tokens: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Keys { .. }")
    }
}

fn seal_with(
    cipher: &XChaCha20Poly1305,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let nonce = XNonce::try_generate().map_err(|_| CryptoError::Random)?;
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Seal)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open_with(
    cipher: &XChaCha20Poly1305,
    aad: &[u8],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if sealed.len() < NONCE_LEN {
        return Err(CryptoError::Open);
    }
    let (n, ct) = sealed.split_at(NONCE_LEN);
    let nonce = XNonce::try_from(n).map_err(|_| CryptoError::Open)?;
    cipher
        .decrypt(&nonce, Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Open)
}

fn aad(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p.as_bytes());
        out.push(0);
    }
    out
}

impl Keys {
    /// Derives the purpose keys from the instance secret.
    pub fn new(master: &SecretKey) -> Self {
        let m = &master.0;
        Keys {
            secrets: cipher(&derive(m, b"os.web.secret.v1")),
            snapshots: cipher(&derive(m, b"os.web.snap.v1")),
            images: cipher(&derive(m, b"os.web.img.v1")),
            image_nonce: derive(m, b"os.web.imgn.v1"),
            tokens: derive(m, b"os.web.token.v1"),
        }
    }

    /// Encrypts a provider secret blob for one account of one user.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Random`] / [`CryptoError::Seal`].
    pub fn seal_account(
        &self,
        user_id: &str,
        account_id: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        seal_with(
            &self.secrets,
            &aad(&["account", user_id, account_id]),
            plaintext,
        )
    }

    /// Decrypts what [`Keys::seal_account`] produced for the same user and account.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Open`] for tampered data, another row's data, or a changed key.
    pub fn open_account(
        &self,
        user_id: &str,
        account_id: &str,
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        open_with(
            &self.secrets,
            &aad(&["account", user_id, account_id]),
            sealed,
        )
    }

    /// Encrypts an item snapshot (favourite or progress row).
    ///
    /// # Errors
    ///
    /// [`CryptoError::Random`] / [`CryptoError::Seal`].
    pub fn seal_snapshot(
        &self,
        account_id: &str,
        kind: &str,
        item: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        seal_with(
            &self.snapshots,
            &aad(&["snapshot", account_id, kind, item]),
            plaintext,
        )
    }

    /// Decrypts an item snapshot.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Open`].
    pub fn open_snapshot(
        &self,
        account_id: &str,
        kind: &str,
        item: &str,
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        open_with(
            &self.snapshots,
            &aad(&["snapshot", account_id, kind, item]),
            sealed,
        )
    }

    /// The stored form of a session or invite token (keyed BLAKE2b-256).
    pub fn token_hash(&self, purpose: &str, token: &str) -> [u8; 32] {
        mac(&self.tokens, &[purpose.as_bytes(), token.as_bytes()])
    }

    /// The `/api/img/<token>` token for a provider image address, or `None` for an
    /// address that isn't `http`/`https` or is too long.
    pub fn image_token(&self, url: &str) -> Option<String> {
        let url = url.trim();
        if url.is_empty() || url.len() > MAX_IMAGE_URL_LEN {
            return None;
        }
        let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return None;
        }
        let digest = mac(&self.image_nonce, &[url.as_bytes()]);
        let nonce = XNonce::try_from(&digest[..NONCE_LEN]).ok()?;
        let ct = self
            .images
            .encrypt(
                &nonce,
                Payload {
                    msg: url.as_bytes(),
                    aad: b"img",
                },
            )
            .ok()?;
        let mut raw = Vec::with_capacity(NONCE_LEN + ct.len());
        raw.extend_from_slice(&digest[..NONCE_LEN]);
        raw.extend_from_slice(&ct);
        Some(URL_SAFE_NO_PAD.encode(raw))
    }

    /// A file name for the image cache (keyed hash of the address).
    pub fn image_cache_name(&self, url: &str) -> String {
        hex(&mac(&self.image_nonce, &[b"cache", url.as_bytes()])[..16])
    }

    /// The provider address behind an image token, if the token is genuine.
    pub fn image_address(&self, token: &str) -> Option<String> {
        if token.len() > 4 * MAX_IMAGE_URL_LEN {
            return None;
        }
        let raw = URL_SAFE_NO_PAD.decode(token.as_bytes()).ok()?;
        let plain = open_with(&self.images, b"img", &raw).ok()?;
        let url = String::from_utf8(plain.to_vec()).ok()?;
        // The nonce must be the synthetic one: a token is only valid in the exact form
        // this server issued.
        let digest = mac(&self.image_nonce, &[url.as_bytes()]);
        (raw.get(..NONCE_LEN) == Some(&digest[..NONCE_LEN])).then_some(url)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys::new(&SecretKey::from_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap())
    }

    #[test]
    fn key_parsing() {
        let k = SecretKey::generate().unwrap();
        let text = k.to_base64();
        assert_eq!(text.len(), 44);
        assert!(SecretKey::from_base64(&text).is_ok());
        assert!(SecretKey::from_base64(&format!("  {}\n", text.as_str())).is_ok());
        let url_safe = text.replace('+', "-").replace('/', "_").replace('=', "");
        assert!(SecretKey::from_base64(&url_safe).is_ok());
        assert!(SecretKey::from_base64("too short").is_err());
        assert!(SecretKey::from_base64("").is_err());
        assert!(!format!("{k:?}").contains(text.as_str()));
    }

    #[test]
    fn account_secrets_are_bound_to_their_row() {
        let k = keys();
        let sealed = k.seal_account("u1", "a1", b"hunter2").unwrap();
        assert_eq!(
            k.open_account("u1", "a1", &sealed).unwrap().as_slice(),
            b"hunter2"
        );
        assert!(k.open_account("u2", "a1", &sealed).is_err());
        assert!(k.open_account("u1", "a2", &sealed).is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(k.open_account("u1", "a1", &tampered).is_err());
        // Random nonces: the same secret never encrypts the same way twice.
        assert_ne!(sealed, k.seal_account("u1", "a1", b"hunter2").unwrap());
        // Another instance key can't read it.
        let other = Keys::new(&SecretKey::generate().unwrap());
        assert!(other.open_account("u1", "a1", &sealed).is_err());
    }

    #[test]
    fn image_tokens() {
        let k = keys();
        let url = "http://img.example.com/logo.png?x=1";
        let t = k.image_token(url).unwrap();
        assert_eq!(t, k.image_token(url).unwrap(), "deterministic");
        assert!(!t.contains("example"));
        assert_eq!(k.image_address(&t).as_deref(), Some(url));
        assert!(k.image_token("file:///etc/passwd").is_none());
        assert!(k.image_token("javascript:alert(1)").is_none());
        assert!(
            k.image_token(&format!("http://x/{}", "a".repeat(3000)))
                .is_none()
        );
        // Tampering or forging fails.
        let mut bytes = URL_SAFE_NO_PAD.decode(&t).unwrap();
        bytes[30] ^= 1;
        assert!(k.image_address(&URL_SAFE_NO_PAD.encode(&bytes)).is_none());
        assert!(k.image_address("not-a-token").is_none());
        let other = Keys::new(&SecretKey::generate().unwrap());
        assert!(other.image_address(&t).is_none());
    }

    #[test]
    fn token_hashes_depend_on_purpose() {
        let k = keys();
        assert_eq!(
            k.token_hash("session", "abc"),
            k.token_hash("session", "abc")
        );
        assert_ne!(
            k.token_hash("session", "abc"),
            k.token_hash("invite", "abc")
        );
        assert_eq!(random_token().unwrap().len(), 43);
        assert_eq!(random_id().unwrap().len(), 32);
    }
}
