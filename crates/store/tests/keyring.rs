//! Real OS keychain round trip. Ignored by default because it writes to the user's
//! keychain (Windows Credential Manager on Windows). Run explicitly:
//!
//! ```text
//! cargo test -p opensmarters-store --test keyring -- --ignored
//! ```
//!
//! Uses synthetic values under unique keys and removes everything it creates, even when
//! an assertion fails.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use opensmarters_store::{
    KeyringSecretStore, SecretKey, SecretStore, Store, StoreOptions, Timestamp,
};
use secrecy::{ExposeSecret, SecretString};

/// Deletes the listed keys on drop (also on panic).
struct Cleanup<'a> {
    store: &'a dyn SecretStore,
    keys: Vec<SecretKey>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for k in &self.keys {
            let _ = self.store.delete(k);
        }
    }
}

#[test]
#[ignore = "touches the real OS keychain; run with --ignored"]
fn real_keyring_round_trip() {
    let ks = KeyringSecretStore::new().expect("OS keychain available");
    let unique = format!("test/{}", Timestamp::now().as_unix_millis());
    let key = SecretKey::custom(&unique).unwrap();
    let _cleanup = Cleanup {
        store: &ks,
        keys: vec![key.clone()],
    };

    assert!(ks.get(&key).unwrap().is_none(), "fresh key is empty");
    ks.set(&key, &SecretString::from("synthetic-value-1 ✓ ünïcode"))
        .unwrap();
    assert_eq!(
        ks.get(&key).unwrap().unwrap().expose_secret(),
        "synthetic-value-1 ✓ ünïcode"
    );
    ks.set(&key, &SecretString::from("synthetic-value-2"))
        .unwrap();
    assert_eq!(
        ks.get(&key).unwrap().unwrap().expose_secret(),
        "synthetic-value-2"
    );

    // A long value (typical long M3U URL) fits.
    let long = format!(
        "http://m3u.example.invalid/get.php?token={}",
        "a".repeat(1500)
    );
    ks.set(&key, &SecretString::from(long.as_str())).unwrap();
    assert_eq!(ks.get(&key).unwrap().unwrap().expose_secret(), long);

    ks.delete(&key).unwrap();
    assert!(ks.get(&key).unwrap().is_none());
    ks.delete(&key).unwrap(); // idempotent
    assert!(!format!("{ks:?}").contains("synthetic"));
}

#[test]
#[ignore = "touches the real OS keychain; run with --ignored"]
fn store_with_real_keyring_deletes_profile_secrets() {
    let env = common::Env::new();
    let ks: Arc<KeyringSecretStore> = Arc::new(KeyringSecretStore::new().expect("OS keychain"));
    let store = Store::open(
        StoreOptions::at_path(env.db_path()).with_pin_hash(common::FAST_PIN),
        ks.clone(),
    )
    .unwrap();

    let p = store
        .profiles()
        .create(common::xtream(
            "Keyring test (synthetic)",
            "synthetic-keyring-pw",
        ))
        .unwrap();
    let mut keys = SecretKey::all_for_profile(p.id).to_vec();
    keys.push(SecretKey::cache_master_key()); // created below if absent
    let had_cache_key = ks.get(&SecretKey::cache_master_key()).unwrap().is_some();
    if had_cache_key {
        keys.pop(); // do not delete a real installation's cache key
    }
    let _cleanup = Cleanup {
        store: ks.as_ref(),
        keys,
    };

    assert_eq!(
        store
            .profiles()
            .password(p.id)
            .unwrap()
            .unwrap()
            .expose_secret(),
        "synthetic-keyring-pw"
    );
    store
        .cache()
        .put(
            opensmarters_store::CacheScope::Profile(p.id),
            "k",
            b"v",
            std::time::Duration::from_secs(60),
        )
        .unwrap();
    assert!(store.profiles().delete(p.id).unwrap());
    assert!(
        ks.get(&SecretKey::profile_password(p.id))
            .unwrap()
            .is_none(),
        "profile deletion removed the keychain entry"
    );
}

/// Regression test: concurrent keychain calls from several threads (on different keys)
/// used to lose writes on Windows until all calls were serialised process-wide.
#[test]
#[ignore = "touches the real OS keychain; run with --ignored"]
fn concurrent_access_from_threads_is_reliable() {
    let ks = Arc::new(KeyringSecretStore::new().expect("OS keychain"));
    let base = Timestamp::now().as_unix_millis();
    let keys: Vec<SecretKey> = (0..4)
        .flat_map(|t| (0..8).map(move |i| (t, i)))
        .map(|(t, i)| SecretKey::custom(&format!("test/concurrency/{base}/{t}/{i}")).unwrap())
        .collect();
    let _cleanup = Cleanup {
        store: ks.as_ref(),
        keys: keys.clone(),
    };
    let handles: Vec<_> = keys
        .chunks(8)
        .map(|chunk| {
            let ks = Arc::clone(&ks);
            let chunk = chunk.to_vec();
            std::thread::spawn(move || {
                for key in &chunk {
                    let value = format!("synthetic-{}", key.as_str());
                    ks.set(key, &SecretString::from(value.as_str())).unwrap();
                    assert_eq!(
                        ks.get(key).unwrap().expect("just written").expose_secret(),
                        value
                    );
                    ks.delete(key).unwrap();
                    assert!(ks.get(key).unwrap().is_none(), "just deleted");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
