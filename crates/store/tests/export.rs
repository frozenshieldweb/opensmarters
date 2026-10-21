//! Export / import of non-secret data.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Env, m3u_file, m3u_url, pin, xtream};
use opensmarters_store::settings::{self, TimeFormat};
use opensmarters_store::{
    CategoryLock, ContentKind, ItemSnapshot, PlaybackKind, PlaybackSnapshot, StoreError,
};
use secrecy::SecretString;

#[test]
fn export_contains_no_secrets_and_round_trips() {
    let src_env = Env::new();
    let src = src_env.open();
    let x = src
        .profiles()
        .create(xtream("Export Xtream", "ExportSecretPw-123"))
        .unwrap();
    let m = src
        .profiles()
        .create(m3u_url(
            "Export M3U",
            "http://m3u.example.invalid/get.php?username=u&password=ExportM3uPw-456",
        ))
        .unwrap();
    let f = src
        .profiles()
        .create(m3u_file("Export File", &src_env.dir.path().join("a.m3u")))
        .unwrap();
    src.profiles()
        .set_epg_url_override(
            x.id,
            Some(&SecretString::from(
                "http://epg.example.invalid/?password=ExportEpgPw",
            )),
        )
        .unwrap();
    src.settings()
        .set_tmdb_api_key(Some(&SecretString::from("ExportTmdbKey")))
        .unwrap();
    src.settings()
        .set(&settings::TIME_FORMAT, &TimeFormat::H12)
        .unwrap();
    src.profiles().mark_used(x.id).unwrap();
    src.parental().set_pin(&pin("24681357")).unwrap();
    src.parental().add_keyword("late night").unwrap();
    for (i, id) in ["3", "1", "2"].iter().enumerate() {
        src.favourites()
            .add(
                x.id,
                ContentKind::Live,
                id,
                &ItemSnapshot::named(format!("C{i}")),
            )
            .unwrap();
    }
    src.favourites()
        .add(
            m.id,
            ContentKind::Movie,
            "m9",
            &ItemSnapshot::named("Movie 9"),
        )
        .unwrap();
    src.parental()
        .set_category_lock(x.id, ContentKind::Live, "77", Some(CategoryLock::Locked))
        .unwrap();
    src.categories()
        .set_hidden_many(x.id, ContentKind::Live, &["77", "5"], true)
        .unwrap();
    src.categories()
        .set_order(x.id, ContentKind::Live, &["9", "5"])
        .unwrap();
    src.categories()
        .set_regions(x.id, ContentKind::Live, &["FR", ""])
        .unwrap();
    src.categories()
        .set_hidden(x.id, ContentKind::Movie, "101", true)
        .unwrap();
    src.playback()
        .record_progress(
            x.id,
            PlaybackKind::Movie,
            "m1",
            100.0,
            Some(1000.0),
            &PlaybackSnapshot::default(),
        )
        .unwrap();

    let json = src.export_json().unwrap();
    for secret in [
        "ExportSecretPw-123",
        "ExportM3uPw-456",
        "ExportEpgPw",
        "ExportTmdbKey",
        "24681357",
        "argon2",
    ] {
        assert!(!json.contains(secret), "export leaked {secret:?}");
    }
    assert!(json.contains("Export Xtream"));

    let dst_env = Env::new();
    let dst = dst_env.open();
    let summary = dst.import_json(&json).unwrap();
    assert_eq!(summary.profiles_added, vec![x.id, m.id, f.id]);
    assert_eq!(summary.needs_secrets, vec![x.id, m.id]);
    assert_eq!(summary.favourites_added, 4);
    assert_eq!(summary.category_locks_added, 1);
    assert_eq!(
        summary.category_prefs_added, 4,
        "77, 5 (hidden + ordered), 9, movie 101"
    );
    assert_eq!(summary.category_regions_added, 2);
    assert_eq!(summary.keywords_added, 1);

    let a = src.profiles().list().unwrap();
    let b = dst.profiles().list().unwrap();
    assert_eq!(a.len(), b.len());
    for (pa, pb) in a.iter().zip(&b) {
        assert_eq!(
            (pa.id, &pa.name, &pa.source, &pa.settings),
            (pb.id, &pb.name, &pb.source, &pb.settings)
        );
        assert!(!pb.has_epg_url_override, "secret-backed flag not imported");
    }
    let favs: Vec<_> = dst
        .favourites()
        .list(x.id, ContentKind::Live)
        .unwrap()
        .into_iter()
        .map(|f| f.item_id)
        .collect();
    assert_eq!(favs, ["3", "1", "2"], "order preserved");
    assert_eq!(
        dst.parental()
            .category_lock(x.id, ContentKind::Live, "77")
            .unwrap(),
        Some(CategoryLock::Locked)
    );
    assert_eq!(
        dst.settings().get(&settings::TIME_FORMAT).unwrap(),
        TimeFormat::H12
    );
    assert_eq!(
        dst.settings().get(&settings::LAST_PROFILE).unwrap(),
        Some(x.id)
    );
    assert!(
        dst.parental()
            .keywords()
            .unwrap()
            .contains(&"late night".to_owned())
    );
    for kind in ContentKind::ALL {
        assert_eq!(
            dst.categories().prefs(x.id, kind).unwrap(),
            src.categories().prefs(x.id, kind).unwrap(),
            "category choices round-trip ({kind:?})"
        );
    }
    assert!(!dst.parental().is_pin_set().unwrap(), "PIN is not exported");
    assert!(dst.profiles().password(x.id).unwrap().is_none());
    assert!(dst.playback().history(x.id, 10).unwrap().is_empty());
    assert!(dst_env.secrets.is_empty(), "import writes no secrets");

    // Importing again merges: nothing new.
    let again = dst.import_json(&json).unwrap();
    assert!(again.profiles_added.is_empty());
    assert_eq!(again.profiles_skipped, 3);
    assert_eq!(again.favourites_added, 0);
    assert_eq!(again.category_prefs_added, 0);
    assert_eq!(again.category_regions_added, 0);
}

#[test]
fn old_backups_without_category_choices_still_import() {
    let src_env = Env::new();
    let src = src_env.open();
    let x = src.profiles().create(xtream("Old", "pw")).unwrap();
    let json = src.export_json().unwrap();
    // A backup from before category management: the fields don't exist.
    let mut doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let profile = doc["profiles"][0].as_object_mut().unwrap();
    assert!(profile.remove("category_prefs").is_some());
    assert!(profile.remove("category_regions").is_some());
    let dst_env = Env::new();
    let dst = dst_env.open();
    let summary = dst.import_json(&doc.to_string()).unwrap();
    assert_eq!(summary.profiles_added, vec![x.id]);
    assert_eq!(summary.category_prefs_added, 0);
    assert!(
        dst.categories()
            .prefs(x.id, ContentKind::Live)
            .unwrap()
            .is_default()
    );
}

#[test]
fn hostile_category_choices_abort_the_import() {
    let env = Env::new();
    let store = env.open();
    let good = {
        let src_env = Env::new();
        let src = src_env.open();
        src.profiles().create(xtream("Ok", "pw")).unwrap();
        src.export_json().unwrap()
    };
    let with = |field: &str, value: serde_json::Value| {
        let mut doc: serde_json::Value = serde_json::from_str(&good).unwrap();
        doc["profiles"][0][field] = value;
        doc.to_string()
    };
    for (field, value) in [
        (
            "category_prefs",
            serde_json::json!([{"kind": "live", "category_id": "http://h.example.invalid/x",
                                "hidden": true}]),
        ),
        (
            "category_prefs",
            serde_json::json!([{"kind": "radio", "category_id": "1", "hidden": true}]),
        ),
        (
            "category_regions",
            serde_json::json!([{"kind": "live", "tag": "not a tag"}]),
        ),
    ] {
        assert!(store.import_json(&with(field, value)).is_err(), "{field}");
        assert!(
            store.profiles().list().unwrap().is_empty(),
            "nothing written"
        );
    }
    // More rows than a section may hold.
    let rows: Vec<serde_json::Value> = (0..=opensmarters_store::categories::MAX_CATEGORY_ROWS)
        .map(
            |i| serde_json::json!({"kind": "live", "category_id": format!("c{i}"), "hidden": true}),
        )
        .collect();
    assert!(matches!(
        store.import_json(&with("category_prefs", serde_json::Value::Array(rows))),
        Err(StoreError::Invalid { .. })
    ));
    assert!(store.profiles().list().unwrap().is_empty());
    assert!(store.import_json(&good).is_ok());
}

#[test]
fn hostile_imports_are_rejected_atomically() {
    let env = Env::new();
    let store = env.open();
    let good = {
        let src_env = Env::new();
        let src = src_env.open();
        src.profiles().create(xtream("Ok", "pw")).unwrap();
        src.export_json().unwrap()
    };

    assert!(store.import_json("not json").is_err());
    assert!(matches!(
        store.import_json(&good.replace("opensmarters-export", "something-else")),
        Err(StoreError::Invalid { .. })
    ));
    assert!(matches!(
        store.import_json(&good.replace("\"version\": 1", "\"version\": 99")),
        Err(StoreError::Invalid { .. })
    ));

    // A credential-bearing server URL in the second profile aborts the whole import.
    let mut doc: serde_json::Value = serde_json::from_str(&good).unwrap();
    let mut evil = doc["profiles"][0].clone();
    evil["id"] = serde_json::json!("00000000-0000-4000-8000-0000000000ee");
    evil["source"]["server_url"] = serde_json::json!("http://u:p@panel.example.invalid");
    doc["profiles"].as_array_mut().unwrap().push(evil);
    assert!(matches!(
        store.import_json(&doc.to_string()),
        Err(StoreError::Invalid { .. })
    ));
    assert!(
        store.profiles().list().unwrap().is_empty(),
        "nothing written"
    );

    // URL item ids are refused too.
    let mut doc: serde_json::Value = serde_json::from_str(&good).unwrap();
    doc["profiles"][0]["favourites"] = serde_json::json!([{
        "kind": "live", "item_id": "http://h.example.invalid/live/u/p/1.ts",
        "position": 0, "added_at": 0, "snapshot": {"name": "x"}
    }]);
    assert!(store.import_json(&doc.to_string()).is_err());
    assert!(store.profiles().list().unwrap().is_empty());

    // Bad setting keys are refused.
    let mut doc: serde_json::Value = serde_json::from_str(&good).unwrap();
    doc["settings"] = serde_json::json!({"bad key": 1});
    assert!(store.import_json(&doc.to_string()).is_err());

    assert!(store.import_json(&good).is_ok());
}
