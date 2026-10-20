// SPDX-License-Identifier: GPL-3.0-or-later
//! Recordings/downloads and reminders. Synthetic data only.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{Env, make_xtream};
use opensmarters_store::{
    Clock, NewRecording, NewReminder, RecordingKind, RecordingState, ReminderAction, StoreError,
    Timestamp,
};

fn live_at(start_secs: i64, end_secs: i64) -> NewRecording {
    NewRecording {
        kind: RecordingKind::Live,
        state: RecordingState::Scheduled,
        item_id: "1001".into(),
        series_id: None,
        season: None,
        episode: None,
        container_extension: None,
        title: "Evening News\u{0}".into(),
        subtitle: Some("Synthetic One".into()),
        icon_url: Some("http://img.example.invalid/1.png".into()),
        start_at: Some(Timestamp::from_unix_secs(start_secs)),
        end_at: Some(Timestamp::from_unix_secs(end_secs)),
    }
}

#[test]
fn recording_life_cycle() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store);
    let rec = store.recordings();

    let now = env.clock.now();
    let start = now.saturating_add(Duration::from_secs(600));
    let mut new = live_at(0, 1);
    new.start_at = Some(start);
    new.end_at = Some(start.saturating_add(Duration::from_secs(3600)));
    let r = rec.add(p.id, &new).unwrap();
    assert_eq!(r.state, RecordingState::Scheduled);
    assert_eq!(r.title, "Evening News", "control characters are cleaned");
    assert!(r.file_name.is_none());

    // Not due yet; due with a 10-minute lead.
    assert!(rec.due(p.id, now).unwrap().is_empty());
    let due = rec
        .due(p.id, now.saturating_add(Duration::from_secs(600)))
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        rec.find_live(p.id, "1001", start).unwrap().map(|r| r.id),
        Some(r.id)
    );

    assert!(rec.set_state(r.id, RecordingState::Active, None).unwrap());
    assert!(
        rec.set_file_name(r.id, "Evening News 2026-10-20 20-00.ts")
            .unwrap()
    );
    assert!(rec.set_progress(r.id, 1_000, None).unwrap());
    let got = rec.get(r.id).unwrap().unwrap();
    assert_eq!(got.state, RecordingState::Active);
    assert_eq!(got.bytes_done, 1_000);
    assert_eq!(got.fraction(), None);

    // A crash leaves it active: recovery pauses it.
    assert_eq!(rec.recover_interrupted().unwrap(), 1);
    let got = rec.get(r.id).unwrap().unwrap();
    assert_eq!(got.state, RecordingState::Paused);

    assert!(
        rec.set_state(
            r.id,
            RecordingState::Failed,
            Some("The provider closed the stream.")
        )
        .unwrap()
    );
    let got = rec.get(r.id).unwrap().unwrap();
    assert_eq!(
        got.error.as_deref(),
        Some("The provider closed the stream.")
    );
    // Failed ones are no longer "the" recording of that programme.
    assert!(rec.find_live(p.id, "1001", start).unwrap().is_none());

    assert_eq!(rec.file_names().unwrap().len(), 1);
    assert!(rec.remove(r.id).unwrap());
    assert!(!rec.remove(r.id).unwrap());
}

#[test]
fn downloads_queue_in_order() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store);
    let rec = store.recordings();

    let mut a = NewRecording::download(RecordingKind::Movie, "200001", "Film A");
    a.container_extension = Some("mkv".into());
    let a = rec.add(p.id, &a).unwrap();
    env.clock.advance(Duration::from_secs(1));
    let mut b = NewRecording::download(RecordingKind::Episode, "300001", "Episode 1");
    b.series_id = Some("7".into());
    b.season = Some(1);
    b.episode = Some(1);
    b.container_extension = Some("../mp4".into());
    let b = rec.add(p.id, &b).unwrap();
    assert_eq!(a.container_extension.as_deref(), Some("mkv"));
    assert_eq!(b.container_extension, None, "unsafe extensions are dropped");

    let queued = rec.with_states(p.id, &[RecordingState::Queued]).unwrap();
    assert_eq!(
        queued.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a.id, b.id],
        "oldest first"
    );
    assert_eq!(rec.list(p.id).unwrap()[0].id, b.id, "list is newest first");

    assert!(rec.set_progress(a.id, 50, Some(200)).unwrap());
    assert_eq!(rec.get(a.id).unwrap().unwrap().fraction(), Some(0.25));
    assert!(rec.set_duration(a.id, 5400.0).unwrap());
    assert!(!rec.set_duration(a.id, f64::NAN).unwrap());
    assert!(rec.set_position(a.id, Some(61.5)).unwrap());
    assert_eq!(rec.get(a.id).unwrap().unwrap().position_secs, Some(61.5));
    assert_eq!(
        rec.find_download(p.id, RecordingKind::Movie, "200001")
            .unwrap()
            .map(|r| r.id),
        Some(a.id)
    );

    // Deleting the profile deletes its recordings.
    store.profiles().delete(p.id).unwrap();
    assert!(rec.get(a.id).unwrap().is_none());
}

#[test]
fn recordings_reject_bad_input() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store);
    let rec = store.recordings();

    let mut url_id = live_at(10, 20);
    url_id.item_id = "http://panel.example.invalid/live/u/p/1.ts".into();
    assert!(matches!(
        rec.add(p.id, &url_id),
        Err(StoreError::Invalid { .. })
    ));
    let backwards = live_at(20, 10);
    assert!(matches!(
        rec.add(p.id, &backwards),
        Err(StoreError::Invalid { .. })
    ));
    let mut no_times = live_at(10, 20);
    no_times.start_at = None;
    assert!(rec.add(p.id, &no_times).is_err());
    let mut done = live_at(10, 20);
    done.state = RecordingState::Done;
    assert!(rec.add(p.id, &done).is_err());

    let r = rec.add(p.id, &live_at(10, 20)).unwrap();
    for bad in ["../escape.ts", "a/b.ts", "C:\\x.ts", "CON.ts", ""] {
        assert!(rec.set_file_name(r.id, bad).is_err(), "{bad:?}");
    }
    assert!(
        rec.reschedule(
            r.id,
            Timestamp::from_unix_secs(30),
            Timestamp::from_unix_secs(30)
        )
        .is_err()
    );
    assert!(
        rec.reschedule(
            r.id,
            Timestamp::from_unix_secs(30),
            Timestamp::from_unix_secs(90)
        )
        .unwrap()
    );
}

#[test]
fn reminders_set_fire_and_prune() {
    let env = Env::new();
    let store = env.open();
    let p = make_xtream(&store);
    let rem = store.reminders();

    let now = env.clock.now();
    let start = now.saturating_add(Duration::from_secs(300));
    let end = start.saturating_add(Duration::from_secs(1800));
    let new = NewReminder {
        stream_id: "1001".into(),
        start_at: start,
        end_at: end,
        title: "Match".into(),
        channel_name: "Synthetic Sport".into(),
        icon_url: None,
        action: ReminderAction::Notify,
    };
    let r = rem.set(p.id, &new).unwrap();
    assert_eq!(rem.pending(p.id, now).unwrap().len(), 1);

    assert!(rem.mark_fired(r.id).unwrap());
    assert!(!rem.mark_fired(r.id).unwrap(), "fires once");
    assert!(rem.pending(p.id, now).unwrap().is_empty());

    // Setting it again (e.g. a different action) re-arms it, without a duplicate.
    let again = rem
        .set(
            p.id,
            &NewReminder {
                action: ReminderAction::Switch,
                ..new.clone()
            },
        )
        .unwrap();
    assert_eq!(again.id, r.id);
    assert_eq!(again.action, ReminderAction::Switch);
    assert!(again.fired_at.is_none());
    assert_eq!(rem.list(p.id).unwrap().len(), 1);

    assert!(rem.set_action(r.id, ReminderAction::Notify).unwrap());
    assert_eq!(
        rem.find(p.id, "1001", start).unwrap().map(|r| r.action),
        Some(ReminderAction::Notify)
    );

    // Pending stops once the programme ended; prune removes it.
    let later = end.saturating_add(Duration::from_secs(1));
    assert!(rem.pending(p.id, later).unwrap().is_empty());
    assert_eq!(rem.prune(p.id, later).unwrap(), 1);
    assert!(rem.list(p.id).unwrap().is_empty());

    let bad = NewReminder {
        end_at: start,
        ..new
    };
    assert!(rem.set(p.id, &bad).is_err());
}
