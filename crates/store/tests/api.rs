//! Behavioural tests of the public API. Synthetic data only.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Env, FAST_PIN, m3u_file, m3u_url, make_xtream, pin, xtream};
use opensmarters_store::settings::{self, Setting, TimeFormat};
use opensmarters_store::{
    CacheScope, CategoryLock, Clock, ContentKind, ItemSnapshot, LiveContainer, MemorySecretStore,
    PinHashParams, PinOutcome, PlaybackKind, PlaybackSnapshot, ProfileKind, ProfileSettings,
    ProfileSource, ProfileUpdate, SecretKey, SecretStore, Store, StoreError, StoreOptions,
    TimeshiftFormat, UserAgent, next_episode,
};
use secrecy::{ExposeSecret, SecretString};

// ---------------------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------------------

#[test]
fn profile_lifecycle_and_secrets() {
    let env = Env::new();
    let store = env.open();
    let p = store.profiles();

    let a = p.create(xtream("  Living room ", "pw-a")).unwrap();
    env.clock.advance(Duration::from_secs(1));
    let b = p
        .create(m3u_url(
            "Playlist",
            "http://m3u.example.invalid/get.php?username=u&password=p",
        ))
        .unwrap();
    env.clock.advance(Duration::from_secs(1));
    let c = p
        .create(m3u_file("Local file", &env.dir.path().join("list.m3u")))
        .unwrap();

    assert_eq!(a.name, "Living room", "names are trimmed");
    assert_eq!(a.kind(), ProfileKind::Xtream);
    assert_eq!(b.kind(), ProfileKind::M3uUrl);
    assert_eq!(c.kind(), ProfileKind::M3uFile);
    assert_eq!(
        a.source,
        ProfileSource::Xtream {
            server_url: "http://panel.example.invalid:8080".into(),
            username: "synthetic_user".into()
        }
    );
    let ids: Vec<_> = p.list().unwrap().into_iter().map(|x| x.id).collect();
    assert_eq!(ids, vec![a.id, b.id, c.id]);

    // Secrets live in the secret store only.
    assert_eq!(p.password(a.id).unwrap().unwrap().expose_secret(), "pw-a");
    assert!(
        p.m3u_url(b.id)
            .unwrap()
            .unwrap()
            .expose_secret()
            .contains("get.php")
    );
    assert!(env.secrets.contains(&SecretKey::profile_password(a.id)));
    assert!(env.secrets.contains(&SecretKey::profile_m3u_url(b.id)));
    p.set_password(a.id, &SecretString::from("pw-a2")).unwrap();
    assert_eq!(p.password(a.id).unwrap().unwrap().expose_secret(), "pw-a2");

    // Kind checks.
    assert!(matches!(
        p.password(b.id),
        Err(StoreError::WrongProfileKind)
    ));
    assert!(matches!(p.m3u_url(a.id), Err(StoreError::WrongProfileKind)));
    assert!(matches!(
        p.set_password(c.id, &SecretString::from("x")),
        Err(StoreError::WrongProfileKind)
    ));

    // EPG override: keychain + flag.
    p.set_epg_url_override(
        a.id,
        Some(&SecretString::from("http://epg.example.invalid/x.xml")),
    )
    .unwrap();
    assert!(p.get(a.id).unwrap().unwrap().has_epg_url_override);
    assert!(p.epg_url_override(a.id).unwrap().is_some());
    p.set_epg_url_override(a.id, None).unwrap();
    assert!(!p.get(a.id).unwrap().unwrap().has_epg_url_override);
    assert!(p.epg_url_override(a.id).unwrap().is_none());

    // Update.
    let updated = p
        .update(
            a.id,
            ProfileUpdate {
                name: Some("Bedroom".into()),
                server_url: Some("https://panel2.example.invalid/".into()),
                settings: Some(ProfileSettings {
                    live_container: LiveContainer::M3u8,
                    timeshift_format: TimeshiftFormat::SmartersCompat,
                    user_agent: UserAgent::Custom("SyntheticAgent/1.0".into()),
                }),
                ..ProfileUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(updated.name, "Bedroom");
    assert_eq!(
        updated.source,
        ProfileSource::Xtream {
            server_url: "https://panel2.example.invalid".into(),
            username: "synthetic_user".into()
        }
    );
    assert_eq!(updated.settings.live_container, LiveContainer::M3u8);
    assert_eq!(
        updated.settings.user_agent,
        UserAgent::Custom("SyntheticAgent/1.0".into())
    );
    assert!(matches!(
        p.update(
            b.id,
            ProfileUpdate {
                username: Some("x".into()),
                ..ProfileUpdate::default()
            }
        ),
        Err(StoreError::WrongProfileKind)
    ));
    assert!(matches!(
        p.update(
            a.id,
            ProfileUpdate {
                settings: Some(ProfileSettings {
                    user_agent: UserAgent::Custom("bad\r\nInjected: header".into()),
                    ..ProfileSettings::default()
                }),
                ..ProfileUpdate::default()
            }
        ),
        Err(StoreError::Invalid { .. })
    ));

    // Reorder + mark_used.
    p.reorder(&[c.id, a.id]).unwrap();
    let ids: Vec<_> = p.list().unwrap().into_iter().map(|x| x.id).collect();
    assert_eq!(ids, vec![c.id, a.id, b.id]);
    p.mark_used(b.id).unwrap();
    assert_eq!(
        store.settings().get(&settings::LAST_PROFILE).unwrap(),
        Some(b.id)
    );
    assert!(p.get(b.id).unwrap().unwrap().last_used_at.is_some());

    // Delete: secrets, cascade, last-profile setting.
    store
        .favourites()
        .add(b.id, ContentKind::Live, "1", &ItemSnapshot::named("One"))
        .unwrap();
    assert!(p.delete(b.id).unwrap());
    assert!(!p.delete(b.id).unwrap(), "second delete reports absence");
    assert!(!env.secrets.contains(&SecretKey::profile_m3u_url(b.id)));
    assert_eq!(store.settings().get(&settings::LAST_PROFILE).unwrap(), None);
    assert!(
        store
            .favourites()
            .list(b.id, ContentKind::Live)
            .unwrap()
            .is_empty()
    );
    assert!(p.delete(a.id).unwrap());
    assert!(!env.secrets.contains(&SecretKey::profile_password(a.id)));
    assert!(!env.secrets.contains(&SecretKey::profile_epg_url(a.id)));
    assert_eq!(p.list().unwrap().len(), 1);
}

#[test]
fn credential_bearing_server_urls_are_rejected() {
    let env = Env::new();
    let store = env.open();
    for url in [
        "http://user:secret@panel.example.invalid",
        "http://panel.example.invalid/player_api.php?username=u&password=secret",
        "panel.example.invalid",
    ] {
        let mut np = xtream("X", "pw");
        if let opensmarters_store::NewProfileSource::Xtream { server_url, .. } = &mut np.source {
            *server_url = url.into();
        }
        assert!(
            matches!(store.profiles().create(np), Err(StoreError::Invalid { .. })),
            "{url}"
        );
    }
    assert!(env.secrets.is_empty(), "nothing written for rejected input");
    assert!(store.profiles().list().unwrap().is_empty());
}

#[test]
fn create_fails_cleanly_when_keychain_fails() {
    let env = Env::new();
    let store = env.open();
    env.secrets.set_failing(true);
    assert!(matches!(
        store.profiles().create(xtream("X", "pw")),
        Err(StoreError::Secret(_))
    ));
    env.secrets.set_failing(false);
    assert!(store.profiles().list().unwrap().is_empty());
    // M3U file profiles need no secret.
    env.secrets.set_failing(true);
    assert!(
        store
            .profiles()
            .create(m3u_file("F", &env.dir.path().join("a.m3u")))
            .is_ok()
    );
}

#[test]
fn delete_keeps_profile_if_keychain_refuses() {
    let env = Env::new();
    let store = env.open();
    let a = make_xtream(&store);
    env.secrets.set_failing(true);
    assert!(matches!(
        store.profiles().delete(a.id),
        Err(StoreError::Secret(_))
    ));
    env.secrets.set_failing(false);
    assert!(
        store.profiles().get(a.id).unwrap().is_some(),
        "kept for retry"
    );
    assert!(store.profiles().delete(a.id).unwrap());

    // A profile that cannot hold secrets is deletable even while the keychain refuses.
    let f = store
        .profiles()
        .create(m3u_file("File", &env.dir.path().join("x.m3u")))
        .unwrap();
    env.secrets.set_failing(true);
    assert!(store.profiles().delete(f.id).unwrap());
    env.secrets.set_failing(false);
}

#[test]
fn debug_output_never_contains_secrets() {
    let np = xtream("Dbg", "debug-secret-pw");
    assert!(!format!("{np:?}").contains("debug-secret-pw"));
    let env = Env::new();
    let store = env.open();
    let p = store.profiles().create(np).unwrap();
    let pw = store.profiles().password(p.id).unwrap();
    assert!(!format!("{pw:?}").contains("debug-secret-pw"));
    assert!(!format!("{store:?}").contains("debug-secret-pw"));
    assert!(!format!("{:?}", env.secrets).contains("debug-secret-pw"));
    assert!(!format!("{p:?}").contains("debug-secret-pw"));
}

// ---------------------------------------------------------------------------------------
// Favourites
// ---------------------------------------------------------------------------------------

#[test]
fn favourites_behaviour() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store).id;
    let f = store.favourites();

    for (i, id) in ["10", "9", "200"].iter().enumerate() {
        assert!(
            f.add(
                p,
                ContentKind::Live,
                id,
                &ItemSnapshot {
                    name: format!("Channel {i}"),
                    icon_url: Some("http://img.example.invalid/logo.png".into()),
                    category_id: Some("5".into()),
                }
            )
            .unwrap()
        );
    }
    // Re-adding refreshes the snapshot, keeps position.
    assert!(
        !f.add(p, ContentKind::Live, "9", &ItemSnapshot::named("Renamed"))
            .unwrap()
    );
    let list = f.list(p, ContentKind::Live).unwrap();
    let ids: Vec<_> = list.iter().map(|x| x.item_id.as_str()).collect();
    assert_eq!(ids, ["10", "9", "200"]);
    assert_eq!(list[1].snapshot.name, "Renamed");
    assert_eq!(list[1].snapshot.icon_url, None);

    // Removing a non-favourite changes nothing (the original removed the last one).
    assert!(!f.remove(p, ContentKind::Live, "999").unwrap());
    assert!(
        !f.remove(p, ContentKind::Movie, "10").unwrap(),
        "kinds are separate"
    );
    assert_eq!(f.list(p, ContentKind::Live).unwrap().len(), 3);

    assert!(f.contains(p, ContentKind::Live, "200").unwrap());
    assert!(
        !f.contains(p, ContentKind::Live, " 200").unwrap(),
        "exact string match"
    );
    assert_eq!(f.ids(p, ContentKind::Live).unwrap().len(), 3);

    f.reorder(p, ContentKind::Live, &["200", "unknown", "10"])
        .unwrap();
    let ids: Vec<_> = f
        .list(p, ContentKind::Live)
        .unwrap()
        .into_iter()
        .map(|x| x.item_id)
        .collect();
    assert_eq!(ids, ["200", "10", "9"]);
    assert!(f.move_to(p, ContentKind::Live, "9", 0).unwrap());
    let ids: Vec<_> = f
        .list(p, ContentKind::Live)
        .unwrap()
        .into_iter()
        .map(|x| x.item_id)
        .collect();
    assert_eq!(ids, ["9", "200", "10"]);
    assert!(!f.move_to(p, ContentKind::Live, "nope", 0).unwrap());

    assert!(
        !f.toggle(p, ContentKind::Live, "9", &ItemSnapshot::named("x"))
            .unwrap()
    );
    assert!(
        f.toggle(p, ContentKind::Live, "9", &ItemSnapshot::named("x"))
            .unwrap()
    );
    let last = f.list(p, ContentKind::Live).unwrap().pop().unwrap();
    assert_eq!(last.item_id, "9", "re-added at the end");

    // Hostile snapshot strings are sanitised; URL ids are refused.
    f.add(
        p,
        ContentKind::Series,
        "s1",
        &ItemSnapshot {
            name: format!("Bad\u{0}\u{7}name{}", "x".repeat(2000)),
            icon_url: Some(format!("http://img.example.invalid/{}", "a".repeat(5000))),
            category_id: Some("c\n1".into()),
        },
    )
    .unwrap();
    let s = &f.list(p, ContentKind::Series).unwrap()[0].snapshot;
    assert!(s.name.starts_with("Bad  name"));
    assert!(s.name.chars().count() <= 512);
    assert_eq!(s.icon_url, None);
    assert_eq!(s.category_id, None);
    assert!(matches!(
        f.add(
            p,
            ContentKind::Live,
            "http://host.example.invalid/live/u/p/1.ts",
            &ItemSnapshot::named("x")
        ),
        Err(StoreError::Invalid { .. })
    ));

    // Unknown profile.
    let ghost = opensmarters_store::ProfileId::new_random();
    assert!(matches!(
        f.add(ghost, ContentKind::Live, "1", &ItemSnapshot::named("x")),
        Err(StoreError::ProfileNotFound(id)) if id == ghost
    ));

    assert_eq!(f.clear(p, Some(ContentKind::Series)).unwrap(), 1);
    assert_eq!(f.clear(p, None).unwrap(), 3);
}

// ---------------------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------------------

fn movie(title: &str) -> PlaybackSnapshot {
    PlaybackSnapshot {
        title: title.into(),
        container_extension: Some("mkv".into()),
        ..PlaybackSnapshot::default()
    }
}

fn episode(series: &str, season: u32, ep: u32) -> PlaybackSnapshot {
    PlaybackSnapshot {
        title: format!("{series} S{season:02}E{ep:02}"),
        series_id: Some(series.into()),
        season: Some(season),
        episode: Some(ep),
        container_extension: Some("mp4".into()),
        ..PlaybackSnapshot::default()
    }
}

#[test]
fn resume_points_and_thresholds() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store).id;
    let pb = store.playback();

    // Short positions are not resumable.
    pb.record_progress(
        p,
        PlaybackKind::Movie,
        "m1",
        12.0,
        Some(6000.0),
        &movie("M1"),
    )
    .unwrap();
    assert_eq!(
        pb.resume_position(p, PlaybackKind::Movie, "m1").unwrap(),
        None
    );
    let r = pb
        .record_progress(
            p,
            PlaybackKind::Movie,
            "m1",
            1200.0,
            Some(6000.0),
            &movie("M1"),
        )
        .unwrap();
    assert!(!r.finished);
    assert_eq!(
        pb.resume_position(p, PlaybackKind::Movie, "m1").unwrap(),
        Some(1200.0)
    );

    // Duration is remembered when a later report omits it; position clamps to it.
    let r = pb
        .record_progress(p, PlaybackKind::Movie, "m1", 9999.0, None, &movie("M1"))
        .unwrap();
    assert_eq!(r.duration_secs, Some(6000.0));
    assert_eq!(r.position_secs, 6000.0);
    assert!(r.finished && r.ever_finished, ">= 95 % counts as finished");
    assert_eq!(
        pb.resume_position(p, PlaybackKind::Movie, "m1").unwrap(),
        None
    );

    // Re-watching clears `finished`, keeps `ever_finished`.
    let r = pb
        .record_progress(
            p,
            PlaybackKind::Movie,
            "m1",
            300.0,
            Some(6000.0),
            &movie("M1"),
        )
        .unwrap();
    assert!(!r.finished && r.ever_finished);

    // mark_finished.
    pb.record_progress(
        p,
        PlaybackKind::Movie,
        "m2",
        100.0,
        Some(200.0),
        &movie("M2"),
    )
    .unwrap();
    assert!(pb.mark_finished(p, PlaybackKind::Movie, "m2").unwrap());
    let m2 = pb.get(p, PlaybackKind::Movie, "m2").unwrap().unwrap();
    assert!(m2.finished);
    assert_eq!(m2.position_secs, 200.0);
    assert!(!pb.mark_finished(p, PlaybackKind::Movie, "never").unwrap());

    // Validation.
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        assert!(
            pb.record_progress(p, PlaybackKind::Movie, "m3", bad, None, &movie("M3"))
                .is_err()
        );
    }
    assert!(
        pb.record_progress(
            p,
            PlaybackKind::Movie,
            "m3",
            1.0,
            Some(f64::NAN),
            &movie("M3")
        )
        .is_err()
    );
}

#[test]
fn continue_watching_and_up_next() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store).id;
    let pb = store.playback();
    let tick = || env.clock.advance(Duration::from_secs(5));

    pb.record_progress(
        p,
        PlaybackKind::Movie,
        "m-old",
        600.0,
        Some(5000.0),
        &movie("Old"),
    )
    .unwrap();
    tick();
    // Series A: E1 finished, then E2 half-way → only E2 shows.
    pb.record_progress(
        p,
        PlaybackKind::Episode,
        "a-1-1",
        1400.0,
        Some(1400.0),
        &episode("A", 1, 1),
    )
    .unwrap();
    tick();
    pb.record_progress(
        p,
        PlaybackKind::Episode,
        "a-1-2",
        700.0,
        Some(1400.0),
        &episode("A", 1, 2),
    )
    .unwrap();
    tick();
    // Series B: E3 in progress then finished → not in continue watching, in up next.
    pb.record_progress(
        p,
        PlaybackKind::Episode,
        "b-2-3",
        100.0,
        Some(1500.0),
        &episode("B", 2, 3),
    )
    .unwrap();
    tick();
    pb.record_progress(
        p,
        PlaybackKind::Episode,
        "b-2-3",
        1490.0,
        None,
        &episode("B", 2, 3),
    )
    .unwrap();
    tick();
    // Too short.
    pb.record_progress(
        p,
        PlaybackKind::Movie,
        "m-short",
        5.0,
        Some(5000.0),
        &movie("Short"),
    )
    .unwrap();
    tick();
    pb.record_progress(
        p,
        PlaybackKind::Catchup,
        "77@1700000000",
        900.0,
        Some(3600.0),
        &movie("News"),
    )
    .unwrap();

    let cw: Vec<_> = pb
        .continue_watching(p, 10)
        .unwrap()
        .into_iter()
        .map(|x| x.item_id)
        .collect();
    assert_eq!(cw, ["77@1700000000", "a-1-2", "m-old"]);
    assert_eq!(pb.continue_watching(p, 2).unwrap().len(), 2, "limit");

    let un = pb.up_next_candidates(p, 10).unwrap();
    assert_eq!(un.len(), 1);
    assert_eq!(un[0].snapshot.series_id.as_deref(), Some("B"));
    let (s, e) = (
        un[0].snapshot.season.unwrap(),
        un[0].snapshot.episode.unwrap(),
    );
    let panel_episodes = [(2, 3, "b-2-3"), (2, 4, "b-2-4"), (3, 1, "b-3-1")];
    assert_eq!(next_episode((s, e), panel_episodes), Some("b-2-4"));

    let series_a = pb.series_progress(p, "A").unwrap();
    assert_eq!(
        series_a
            .iter()
            .map(|x| x.item_id.as_str())
            .collect::<Vec<_>>(),
        ["a-1-1", "a-1-2"]
    );
    assert_eq!(pb.last_episode(p, "A").unwrap().unwrap().item_id, "a-1-2");
    assert_eq!(pb.history(p, 100).unwrap().len(), 6);
    assert!(pb.remove(p, PlaybackKind::Movie, "m-short").unwrap());
    assert_eq!(pb.clear_history(p).unwrap(), 5);
    assert!(pb.continue_watching(p, 10).unwrap().is_empty());
}

#[test]
fn custom_resume_thresholds() {
    let env = Env::new();
    let mut opts = env.options();
    opts.resume = opensmarters_store::ResumeThresholds {
        min_position_secs: 5.0,
        max_fraction: 0.5,
    };
    let store = Store::open(opts, env.secrets.clone()).unwrap();
    let p = make_xtream(&store).id;
    let pb = store.playback();
    pb.record_progress(p, PlaybackKind::Movie, "m", 10.0, Some(100.0), &movie("M"))
        .unwrap();
    assert_eq!(
        pb.resume_position(p, PlaybackKind::Movie, "m").unwrap(),
        Some(10.0)
    );
    let r = pb
        .record_progress(p, PlaybackKind::Movie, "m", 50.0, Some(100.0), &movie("M"))
        .unwrap();
    assert!(r.finished);
}

// ---------------------------------------------------------------------------------------
// Recent live channels
// ---------------------------------------------------------------------------------------

#[test]
fn recent_live_channels() {
    let env = Env::new();
    let mut opts = env.options();
    opts.recent_live_limit = 3;
    let store = Store::open(opts, env.secrets.clone()).unwrap();
    let p = make_xtream(&store).id;
    let r = store.recent_live();

    for id in ["1", "2", "3", "4"] {
        r.touch(p, id, &ItemSnapshot::named(format!("Ch {id}")))
            .unwrap();
    }
    let ids = |r: &opensmarters_store::RecentLive<'_>| -> Vec<String> {
        r.list(p, 10)
            .unwrap()
            .into_iter()
            .map(|c| c.stream_id)
            .collect()
    };
    assert_eq!(
        ids(&r),
        ["4", "3", "2"],
        "trimmed to the limit, newest first"
    );
    assert_eq!(r.previous(p).unwrap().unwrap().stream_id, "3");
    r.touch(p, "2", &ItemSnapshot::named("Ch 2")).unwrap();
    assert_eq!(ids(&r), ["2", "4", "3"]);
    assert_eq!(r.previous(p).unwrap().unwrap().stream_id, "4");
    assert!(r.remove(p, "4").unwrap());
    assert_eq!(r.clear(p).unwrap(), 2);
    assert!(r.previous(p).unwrap().is_none());
}

// ---------------------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------------------

#[test]
fn cache_ttl_stale_and_purge() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store).id;
    let c = store.cache();
    let scope = CacheScope::Profile(p);

    assert!(c.get_fresh(scope, "live_categories").unwrap().is_none());
    assert!(
        c.put(
            scope,
            "live_categories",
            b"[1,2,3]",
            Duration::from_secs(60)
        )
        .unwrap()
    );
    assert_eq!(
        c.get_fresh(scope, "live_categories").unwrap().unwrap(),
        b"[1,2,3]"
    );
    assert!(
        c.get_fresh(CacheScope::Global, "live_categories")
            .unwrap()
            .is_none(),
        "scopes are separate"
    );

    env.clock.advance(Duration::from_secs(61));
    assert!(c.get_fresh(scope, "live_categories").unwrap().is_none());
    let stale = c.get_stale_ok(scope, "live_categories").unwrap().unwrap();
    assert!(!stale.is_fresh);
    assert_eq!(stale.data, b"[1,2,3]");
    assert!(
        !format!("{stale:?}").contains("[1,2,3]"),
        "Debug hides data"
    );

    assert_eq!(
        c.purge_expired(Duration::from_secs(3600)).unwrap(),
        0,
        "within grace"
    );
    assert_eq!(c.purge_expired(Duration::ZERO).unwrap(), 1);
    assert!(c.get_stale_ok(scope, "live_categories").unwrap().is_none());

    // Profile scoping, invalidation and cascade.
    c.put(scope, "a", b"1", Duration::from_secs(60)).unwrap();
    c.put(scope, "b", b"2", Duration::from_secs(60)).unwrap();
    c.put(
        CacheScope::Global,
        "tmdb/movie/1",
        b"g",
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(c.invalidate_profile(p).unwrap(), 2);
    c.put(scope, "a", b"1", Duration::from_secs(60)).unwrap();
    assert!(c.remove(scope, "a").unwrap());
    c.put(scope, "a", b"1", Duration::from_secs(60)).unwrap();
    store.profiles().delete(p).unwrap();
    assert_eq!(
        c.len().unwrap(),
        1,
        "global entry survives profile deletion"
    );
    assert!(matches!(
        c.put(scope, "x", b"1", Duration::from_secs(1)),
        Err(StoreError::ProfileNotFound(_))
    ));
    assert_eq!(c.clear().unwrap(), 1);
}

#[test]
fn cache_size_cap_and_lru_eviction() {
    let env = Env::new();
    let store = env.open();
    let c = store.cache();
    let g = CacheScope::Global;
    let blob = vec![0xAB; 1000]; // stored as 1000 + 40 bytes
    c.set_max_bytes(3200).unwrap();
    assert_eq!(c.max_bytes(), 3200);

    c.put(g, "a", &blob, Duration::from_secs(600)).unwrap();
    env.clock.advance(Duration::from_secs(120));
    c.put(g, "b", &blob, Duration::from_secs(600)).unwrap();
    env.clock.advance(Duration::from_secs(120));
    c.put(g, "c", &blob, Duration::from_secs(600)).unwrap();
    env.clock.advance(Duration::from_secs(120));
    // Touch "a" so "b" becomes least recently used.
    assert!(c.get_fresh(g, "a").unwrap().is_some());
    env.clock.advance(Duration::from_secs(120));
    c.put(g, "d", &blob, Duration::from_secs(600)).unwrap();

    assert!(c.total_bytes().unwrap() <= 3200);
    assert!(
        c.get_stale_ok(g, "b").unwrap().is_none(),
        "LRU entry evicted"
    );
    for k in ["a", "c", "d"] {
        assert!(c.get_stale_ok(g, k).unwrap().is_some(), "{k} kept");
    }

    // A value bigger than the cap is not stored (and replaces nothing).
    assert!(
        !c.put(g, "huge", &vec![0; 5000], Duration::from_secs(60))
            .unwrap()
    );
    assert!(c.get_stale_ok(g, "huge").unwrap().is_none());

    // Lowering the cap evicts immediately; the setting persists.
    c.set_max_bytes(1100).unwrap();
    assert_eq!(c.len().unwrap(), 1);
    drop(store);
    let store = env.open();
    assert_eq!(store.cache().max_bytes(), 1100);
}

#[test]
fn cache_survives_reopen_and_resets_with_lost_key() {
    let env = Env::new();
    {
        let store = env.open();
        store
            .cache()
            .put(
                CacheScope::Global,
                "k",
                b"persisted",
                Duration::from_secs(600),
            )
            .unwrap();
        store.close().unwrap();
    }
    {
        let store = env.open();
        assert_eq!(
            store
                .cache()
                .get_fresh(CacheScope::Global, "k")
                .unwrap()
                .unwrap(),
            b"persisted"
        );
    }
    // Losing the keychain entry makes old entries unreadable: they are dropped, not
    // returned, and a new key is created.
    env.secrets.delete(&SecretKey::cache_master_key()).unwrap();
    let store = env.open();
    assert!(
        store
            .cache()
            .get_fresh(CacheScope::Global, "k")
            .unwrap()
            .is_none()
    );
    assert!(store.cache().is_empty().unwrap());
    assert!(env.secrets.contains(&SecretKey::cache_master_key()));
}

#[test]
fn cache_detects_tampering() {
    let env = Env::new();
    let store = env.open();
    store
        .cache()
        .put(CacheScope::Global, "k", b"value", Duration::from_secs(600))
        .unwrap();
    {
        let raw = rusqlite::Connection::open(env.db_path()).unwrap();
        let mut value: Vec<u8> = raw
            .query_row("SELECT value FROM cache", [], |r| r.get(0))
            .unwrap();
        let last = value.len() - 1;
        value[last] ^= 0x01;
        raw.execute("UPDATE cache SET value = ?1", [value]).unwrap();
    }
    assert!(
        store
            .cache()
            .get_stale_ok(CacheScope::Global, "k")
            .unwrap()
            .is_none()
    );
    assert!(store.cache().is_empty().unwrap(), "corrupt entry removed");
}

// ---------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------

#[test]
fn typed_settings() {
    let env = Env::new();
    let store = env.open();
    let s = store.settings();

    assert_eq!(s.get(&settings::TIME_FORMAT).unwrap(), TimeFormat::H24);
    assert_eq!(s.get(&settings::LANGUAGE).unwrap(), None);
    assert_eq!(s.get(&settings::UI_SCALE).unwrap(), 1.0);
    assert!(!s.get(&settings::TMDB_ENABLED).unwrap());

    s.set(&settings::TIME_FORMAT, &TimeFormat::H12).unwrap();
    s.set(&settings::LANGUAGE, &Some("fr-CA".to_owned()))
        .unwrap();
    s.set(&settings::UI_SCALE, &1.25).unwrap();
    s.set(&settings::TMDB_ENABLED, &true).unwrap();

    // App-defined setting.
    const WINDOW_W: Setting<u32> = Setting::new("app.window_width", || 1280);
    assert_eq!(s.get(&WINDOW_W).unwrap(), 1280);
    s.set(&WINDOW_W, &1600).unwrap();

    store.close().unwrap();
    let store = env.open();
    let s = store.settings();
    assert_eq!(s.get(&settings::TIME_FORMAT).unwrap(), TimeFormat::H12);
    assert_eq!(
        s.get(&settings::LANGUAGE).unwrap().as_deref(),
        Some("fr-CA")
    );
    assert_eq!(s.get(&settings::UI_SCALE).unwrap(), 1.25);
    assert!(s.get(&settings::TMDB_ENABLED).unwrap());
    assert_eq!(s.get(&WINDOW_W).unwrap(), 1600);
    s.reset(&WINDOW_W).unwrap();
    assert_eq!(s.get(&WINDOW_W).unwrap(), 1280);

    // A corrupt value falls back to the default instead of failing.
    {
        let raw = rusqlite::Connection::open(env.db_path()).unwrap();
        raw.execute(
            "UPDATE settings SET value = '\"weird\"' WHERE key = 'time_format'",
            [],
        )
        .unwrap();
    }
    assert_eq!(s.get(&settings::TIME_FORMAT).unwrap(), TimeFormat::H24);

    // Bad keys are refused.
    const BAD: Setting<u32> = Setting::new("has space", || 0);
    assert!(s.set(&BAD, &1).is_err());

    // TMDB key goes to the keychain.
    s.set_tmdb_api_key(Some(&SecretString::from("synthetic-tmdb-key")))
        .unwrap();
    assert_eq!(
        s.tmdb_api_key().unwrap().unwrap().expose_secret(),
        "synthetic-tmdb-key"
    );
    assert!(env.secrets.contains(&SecretKey::tmdb_api_key()));
    s.set_tmdb_api_key(None).unwrap();
    assert!(s.tmdb_api_key().unwrap().is_none());
}

// ---------------------------------------------------------------------------------------
// Parental control
// ---------------------------------------------------------------------------------------

#[test]
fn pin_set_verify_change_remove() {
    let env = Env::new();
    let store = env.open();
    let par = store.parental();

    assert!(!par.is_pin_set().unwrap());
    assert!(matches!(
        par.verify_pin(&pin("1234")),
        Err(StoreError::PinNotSet)
    ));
    for bad in ["123", "123456789", "12a4", ""] {
        assert!(matches!(
            par.set_pin(&pin(bad)),
            Err(StoreError::InvalidPinFormat)
        ));
    }
    par.set_pin(&pin("2468")).unwrap();
    assert!(par.is_pin_set().unwrap());
    assert!(matches!(
        par.set_pin(&pin("1357")),
        Err(StoreError::PinAlreadySet)
    ));

    assert_eq!(par.verify_pin(&pin("2468")).unwrap(), PinOutcome::Ok);
    assert_eq!(
        par.verify_pin(&pin("0000")).unwrap(),
        PinOutcome::Wrong {
            remaining_before_lockout: 4
        }
    );
    assert_eq!(par.status().unwrap().failed_attempts, 1);
    assert_eq!(par.verify_pin(&pin("2468")).unwrap(), PinOutcome::Ok);
    assert_eq!(par.status().unwrap().failed_attempts, 0, "success resets");

    // change_pin validates the new PIN before spending an attempt.
    assert!(matches!(
        par.change_pin(&pin("2468"), &pin("12")),
        Err(StoreError::InvalidPinFormat)
    ));
    assert_eq!(par.status().unwrap().failed_attempts, 0);
    assert!(matches!(
        par.change_pin(&pin("9999"), &pin("13579")).unwrap(),
        PinOutcome::Wrong { .. }
    ));
    assert_eq!(
        par.change_pin(&pin("2468"), &pin("13579")).unwrap(),
        PinOutcome::Ok
    );
    assert!(matches!(
        par.verify_pin(&pin("2468")).unwrap(),
        PinOutcome::Wrong { .. }
    ));
    assert_eq!(par.verify_pin(&pin("13579")).unwrap(), PinOutcome::Ok);

    assert!(matches!(
        par.remove_pin(&pin("0000")).unwrap(),
        PinOutcome::Wrong { .. }
    ));
    assert!(par.is_pin_set().unwrap());
    assert_eq!(par.remove_pin(&pin("13579")).unwrap(), PinOutcome::Ok);
    assert!(!par.is_pin_set().unwrap());

    par.set_pin(&pin("11223344")).unwrap();
    par.force_remove_pin().unwrap();
    assert!(!par.is_pin_set().unwrap());
}

#[test]
fn pin_lockout_escalates_persists_and_resets() {
    let env = Env::new();
    let store = env.open();
    store.parental().set_pin(&pin("4321")).unwrap();
    let wrong = pin("0000");

    for remaining in [4, 3, 2, 1] {
        assert_eq!(
            store.parental().verify_pin(&wrong).unwrap(),
            PinOutcome::Wrong {
                remaining_before_lockout: remaining
            }
        );
    }
    let t0 = env.clock.now();
    let PinOutcome::LockedOut { until } = store.parental().verify_pin(&wrong).unwrap() else {
        panic!("5th failure locks");
    };
    assert_eq!(until, t0.saturating_add(Duration::from_secs(30)));

    // While locked, even the right PIN is not evaluated.
    assert_eq!(
        store.parental().verify_pin(&pin("4321")).unwrap(),
        PinOutcome::LockedOut { until }
    );

    // The lockout survives a restart.
    drop(store);
    let store = env.open();
    assert_eq!(store.parental().status().unwrap().locked_until, Some(until));
    assert_eq!(
        store.parental().verify_pin(&pin("4321")).unwrap(),
        PinOutcome::LockedOut { until }
    );

    // After expiry, each further failure doubles the lockout.
    env.clock.advance(Duration::from_secs(31));
    let t1 = env.clock.now();
    assert_eq!(
        store.parental().verify_pin(&wrong).unwrap(),
        PinOutcome::LockedOut {
            until: t1.saturating_add(Duration::from_secs(60))
        }
    );
    env.clock.advance(Duration::from_secs(61));
    let t2 = env.clock.now();
    assert_eq!(
        store.parental().verify_pin(&wrong).unwrap(),
        PinOutcome::LockedOut {
            until: t2.saturating_add(Duration::from_secs(120))
        }
    );

    // Moving the clock back does not extend the wait beyond the lockout length.
    env.clock.rewind(Duration::from_secs(86_400));
    let t3 = env.clock.now();
    assert_eq!(
        store.parental().verify_pin(&pin("4321")).unwrap(),
        PinOutcome::LockedOut {
            until: t3.saturating_add(Duration::from_secs(120))
        }
    );

    // Correct PIN after the lockout resets everything.
    env.clock.advance(Duration::from_secs(121));
    assert_eq!(
        store.parental().verify_pin(&pin("4321")).unwrap(),
        PinOutcome::Ok
    );
    let st = store.parental().status().unwrap();
    assert_eq!((st.failed_attempts, st.locked_until), (0, None));
    assert_eq!(
        store.parental().verify_pin(&wrong).unwrap(),
        PinOutcome::Wrong {
            remaining_before_lockout: 4
        }
    );
}

#[test]
fn pin_hash_is_argon2id_phc_and_upgrades_params() {
    let env = Env::new();
    {
        let store = env.open();
        store.parental().set_pin(&pin("8642")).unwrap();
    }
    let read_phc = || -> String {
        let raw = rusqlite::Connection::open(env.db_path()).unwrap();
        raw.query_row("SELECT phc FROM parental_pin WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let phc = read_phc();
    assert!(phc.starts_with("$argon2id$v=19$m=256,t=1,p=1$"), "{phc}");
    assert!(!phc.contains("8642"));

    // Reopen with stronger params: a successful verify re-hashes.
    let stronger = PinHashParams {
        m_cost_kib: 512,
        t_cost: 2,
        p_cost: 1,
    };
    let store = Store::open(env.options().with_pin_hash(stronger), env.secrets.clone()).unwrap();
    assert!(matches!(
        store.parental().verify_pin(&pin("0000")).unwrap(),
        PinOutcome::Wrong { .. }
    ));
    assert!(
        read_phc().contains("m=256,t=1,p=1"),
        "no upgrade on failure"
    );
    assert_eq!(
        store.parental().verify_pin(&pin("8642")).unwrap(),
        PinOutcome::Ok
    );
    let upgraded = read_phc();
    assert!(upgraded.contains("m=512,t=2,p=1"), "{upgraded}");
    assert_ne!(upgraded, phc, "fresh salt");
    assert_eq!(
        store.parental().verify_pin(&pin("8642")).unwrap(),
        PinOutcome::Ok
    );
}

#[test]
fn tampered_pin_hash_with_huge_params_is_refused() {
    let env = Env::new();
    let store = env.open();
    store.parental().set_pin(&pin("1234")).unwrap();
    {
        let raw = rusqlite::Connection::open(env.db_path()).unwrap();
        let phc: String = raw
            .query_row("SELECT phc FROM parental_pin", [], |r| r.get(0))
            .unwrap();
        let evil = phc.replace("m=256", "m=4194304");
        raw.execute("UPDATE parental_pin SET phc = ?1", [evil])
            .unwrap();
    }
    assert!(matches!(
        store.parental().verify_pin(&pin("1234")),
        Err(StoreError::PinHash(_))
    ));
}

#[test]
fn default_params_hash_and_verify() {
    // Production parameters (64 MiB, t=3, p=4) on an in-memory store.
    let store = Store::open(
        StoreOptions::in_memory(),
        Arc::new(MemorySecretStore::new()),
    )
    .unwrap();
    let started = std::time::Instant::now();
    store.parental().set_pin(&pin("5555")).unwrap();
    assert_eq!(
        store.parental().verify_pin(&pin("5555")).unwrap(),
        PinOutcome::Ok
    );
    eprintln!(
        "default Argon2id params: hash + verify took {:?}",
        started.elapsed()
    );
}

#[test]
fn keywords_and_category_locks() {
    let env = Env::new();
    let store = env.open();
    let par = store.parental();
    let p = make_xtream(&store).id;

    assert_eq!(
        par.keywords().unwrap(),
        ["18+", "adult", "erotic", "porn", "xxx"]
    );
    assert!(par.add_keyword("  Late Night ").unwrap());
    assert!(
        !par.add_keyword("late night").unwrap(),
        "normalised duplicate"
    );
    assert!(par.remove_keyword("PORN").unwrap());
    assert!(!par.remove_keyword("missing").unwrap());
    assert!(par.add_keyword("").is_err());
    par.set_keywords(&["B", "a", "b"]).unwrap();
    assert_eq!(par.keywords().unwrap(), ["a", "b"]);
    par.reset_keywords().unwrap();
    assert_eq!(par.keywords().unwrap().len(), 5);

    par.set_category_lock(p, ContentKind::Live, "12", Some(CategoryLock::Locked))
        .unwrap();
    par.set_category_lock(p, ContentKind::Movie, "12", Some(CategoryLock::Unlocked))
        .unwrap();
    assert_eq!(
        par.category_lock(p, ContentKind::Live, "12").unwrap(),
        Some(CategoryLock::Locked)
    );
    assert_eq!(par.category_locks(p, ContentKind::Movie).unwrap().len(), 1);
    par.set_category_lock(p, ContentKind::Live, "12", None)
        .unwrap();
    assert_eq!(par.category_lock(p, ContentKind::Live, "12").unwrap(), None);
    let ghost = opensmarters_store::ProfileId::new_random();
    assert!(matches!(
        par.set_category_lock(ghost, ContentKind::Live, "1", Some(CategoryLock::Locked)),
        Err(StoreError::ProfileNotFound(_))
    ));
    store.profiles().delete(p).unwrap();
    assert!(
        par.category_locks(p, ContentKind::Movie)
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------------------
// Persistence and concurrency
// ---------------------------------------------------------------------------------------

#[test]
fn everything_survives_reopen() {
    let env = Env::new();
    let id;
    {
        let store = env.open();
        let p = make_xtream(&store);
        id = p.id;
        store
            .favourites()
            .add(id, ContentKind::Movie, "m1", &ItemSnapshot::named("Movie"))
            .unwrap();
        store
            .playback()
            .record_progress(
                id,
                PlaybackKind::Movie,
                "m1",
                600.0,
                Some(3600.0),
                &movie("Movie"),
            )
            .unwrap();
        store
            .recent_live()
            .touch(id, "5", &ItemSnapshot::named("Five"))
            .unwrap();
        store.parental().set_pin(&pin("97531")).unwrap();
        store.close().unwrap();
    }
    let store = env.open();
    assert_eq!(store.profiles().list().unwrap().len(), 1);
    assert!(
        store
            .favourites()
            .contains(id, ContentKind::Movie, "m1")
            .unwrap()
    );
    assert_eq!(
        store
            .playback()
            .resume_position(id, PlaybackKind::Movie, "m1")
            .unwrap(),
        Some(600.0)
    );
    assert_eq!(store.recent_live().list(id, 5).unwrap().len(), 1);
    assert_eq!(
        store.parental().verify_pin(&pin("97531")).unwrap(),
        PinOutcome::Ok
    );
    assert_eq!(
        store
            .profiles()
            .password(id)
            .unwrap()
            .unwrap()
            .expose_secret(),
        "synthetic-password"
    );
}

#[test]
fn shared_across_threads() {
    let env = Env::new();
    let store = Arc::new(env.open());
    let p = make_xtream(&store).id;
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                for i in 0..25 {
                    let id = format!("{t}-{i}");
                    store
                        .favourites()
                        .add(p, ContentKind::Live, &id, &ItemSnapshot::named(id.clone()))
                        .unwrap();
                    store
                        .playback()
                        .record_progress(
                            p,
                            PlaybackKind::Movie,
                            &id,
                            60.0,
                            Some(600.0),
                            &movie("M"),
                        )
                        .unwrap();
                    store
                        .cache()
                        .put(
                            CacheScope::Profile(p),
                            &id,
                            id.as_bytes(),
                            Duration::from_secs(60),
                        )
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let favs = store.favourites().list(p, ContentKind::Live).unwrap();
    assert_eq!(favs.len(), 200);
    let mut positions: Vec<_> = favs.iter().map(|f| f.position).collect();
    positions.dedup();
    assert_eq!(positions.len(), 200, "unique positions");
    assert_eq!(store.playback().history(p, 1000).unwrap().len(), 200);
    assert_eq!(store.cache().len().unwrap(), 200);
}

#[test]
fn second_process_style_connection_coexists() {
    // Two stores on the same file (as two app instances would be) both work.
    let env = Env::new();
    let a = env.open();
    let b = env.open();
    let p = make_xtream(&a).id;
    b.favourites()
        .add(p, ContentKind::Live, "1", &ItemSnapshot::named("One"))
        .unwrap();
    assert!(a.favourites().contains(p, ContentKind::Live, "1").unwrap());
}

#[test]
fn in_memory_store_works() {
    let store = Store::open(
        StoreOptions::in_memory().with_pin_hash(FAST_PIN),
        Arc::new(MemorySecretStore::new()),
    )
    .unwrap();
    assert!(store.path().is_none());
    let p = make_xtream(&store);
    assert_eq!(store.profiles().list().unwrap(), vec![p]);
    store.close().unwrap();
}

#[test]
fn newer_schema_is_refused() {
    let env = Env::new();
    env.open().close().unwrap();
    {
        let raw = rusqlite::Connection::open(env.db_path()).unwrap();
        raw.pragma_update(None, "user_version", 999).unwrap();
    }
    assert!(matches!(
        Store::open(env.options(), env.secrets.clone()),
        Err(StoreError::SchemaTooNew { found: 999, .. })
    ));
}

#[test]
fn changing_a_profile_source_invalidates_its_cache() {
    let env = Env::new();
    let store = env.open();
    let x = make_xtream(&store);
    let m = store
        .profiles()
        .create(m3u_url("M", "http://m3u.example.invalid/a.m3u"))
        .unwrap();
    let put = |p| {
        store
            .cache()
            .put(CacheScope::Profile(p), "k", b"v", Duration::from_secs(600))
            .unwrap()
    };
    put(x.id);
    put(m.id);

    // A rename keeps the cache.
    store
        .profiles()
        .update(
            x.id,
            ProfileUpdate {
                name: Some("Renamed".into()),
                ..ProfileUpdate::default()
            },
        )
        .unwrap();
    assert!(
        store
            .cache()
            .get_fresh(CacheScope::Profile(x.id), "k")
            .unwrap()
            .is_some()
    );

    // A new server drops it.
    store
        .profiles()
        .update(
            x.id,
            ProfileUpdate {
                server_url: Some("http://other.example.invalid".into()),
                ..ProfileUpdate::default()
            },
        )
        .unwrap();
    assert!(
        store
            .cache()
            .get_fresh(CacheScope::Profile(x.id), "k")
            .unwrap()
            .is_none()
    );

    // A new playlist URL drops it too.
    store
        .profiles()
        .set_m3u_url(
            m.id,
            &SecretString::from("http://m3u.example.invalid/b.m3u"),
        )
        .unwrap();
    assert!(
        store
            .cache()
            .get_fresh(CacheScope::Profile(m.id), "k")
            .unwrap()
            .is_none()
    );
}
