// SPDX-License-Identifier: GPL-3.0-or-later
//! The recording/download engine of one session: a supervisor task that starts jobs, and
//! the jobs themselves (tokio tasks on the app runtime, never on the UI thread).
//!
//! ## Supervisor
//!
//! Wakes every [`TICK`], when the UI asks ([`Engine::wake`]), and when the connection
//! budget changes (a lease freed). Each pass reads the job list from the store and:
//!
//! 1. moves due **scheduled** recordings (`start − padding ≤ now`) to `queued`, or marks
//!    them failed when their whole window passed while the app was closed;
//! 2. starts **queued live recordings** with a `Recording` lease (never preempted); one
//!    that can't get a connection keeps trying until its end with a clear message, and is
//!    offered to the player ([`Engine::waiting`]): if the user watches that very channel,
//!    `dvr::player` records through the player instead (same connection);
//! 3. starts **queued downloads**, oldest first, up to the "downloads at once" setting,
//!    each with a preemptible `Download` lease.
//!
//! ## Jobs
//!
//! A job owns its lease and a control channel ([`Control`]): pause, cancel, finish
//! ("stop recording"), interrupt (app exit or profile switch). Downloads also watch
//! [`Lease::revoked`](crate::services::connections::Lease::revoked): when viewing takes
//! their connection they stop, go back to `queued` with [`PREEMPTED`] and continue when a
//! slot frees. Progress is published in memory for the UI at once and written to the
//! store at most every [`PROGRESS_EVERY`].
//!
//! ## Start-up
//!
//! [`Engine::start`] first runs recovery: `active` rows (a crash) and rows paused by an
//! app exit ([`INTERRUPTED`]) are queued again (downloads resume from their `.part`; live
//! recordings continue in the same file if their programme hasn't ended, otherwise what
//! was recorded is kept).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use opensmarters_core::chrono::{DateTime, Utc};
use opensmarters_core::download::{DownloadConfig, Downloader};
use opensmarters_store::{
    ProfileId, Recording, RecordingId, RecordingKind, RecordingState, Store, StoreError,
};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use super::{DvrSettings, files};
use crate::error::AppResult;
use crate::services::connections::{Lease, Purpose};
use crate::services::{Services, Session};

/// How often the supervisor looks at the schedule and the queue.
pub const TICK: Duration = Duration::from_secs(4);
/// Progress is written to the store at most this often.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(2);
/// The free-space guard is checked after this many bytes.
pub const DISK_CHECK_BYTES: u64 = 8 * 1024 * 1024;

/// Message of a job stopped by an app exit or a profile switch (it continues next time).
pub const INTERRUPTED: &str =
    "Stopped when opensmarters closed. It continues when the app is open again.";
/// Message of a download that gave its connection to viewing.
pub const PREEMPTED: &str =
    "Paused to free a connection for viewing. It continues when a connection is free.";
/// Message of a scheduled recording whose whole window passed while the app was closed.
pub const MISSED: &str = "Missed: opensmarters wasn't open at the time.";

/// What the UI (or the app) asks a running job to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Keep going.
    Run,
    /// Pause (downloads): stop and keep the partial file.
    Pause,
    /// Cancel: stop, delete the partial file and the row.
    Cancel,
    /// Stop a recording now and keep what was recorded.
    Finish,
    /// App exit / profile switch: stop, keep everything, continue next time.
    Interrupt,
}

/// Live progress of a running job (memory only; the store gets a copy every few seconds).
#[derive(Debug, Clone, Default)]
pub struct Progress {
    /// Bytes in the file.
    pub bytes: u64,
    /// Expected size, when the server said.
    pub total: Option<u64>,
    /// Smoothed speed, bytes per second.
    pub rate: f64,
    /// When this run of the job started.
    pub started: Option<Instant>,
    /// A transient status ("Reconnecting… (2)"), shown instead of the speed.
    pub note: Option<String>,
}

impl Progress {
    /// 0..=1 when the size is known.
    pub fn fraction(&self) -> Option<f64> {
        let t = self.total.filter(|t| *t > 0)?;
        Some((self.bytes as f64 / t as f64).clamp(0.0, 1.0))
    }
}

struct Job {
    kind: RecordingKind,
    item_id: String,
    control: watch::Sender<Control>,
    progress: Progress,
}

/// State shared by the supervisor, the jobs and the UI handle.
pub(crate) struct Shared {
    pub(crate) session: Arc<Session>,
    pub(crate) services: Services,
    pub(crate) downloader: Downloader,
    epoch: AtomicU64,
    wake: Notify,
    stopping: AtomicBool,
    jobs: Mutex<HashMap<RecordingId, Job>>,
    settings: Mutex<DvrSettings>,
    /// Live recordings waiting for a connection: id → channel key.
    waiting: Mutex<HashMap<RecordingId, String>>,
    /// Recordings the player records (same connection), with their channel key: the
    /// supervisor leaves them alone.
    player_owned: Mutex<HashMap<RecordingId, String>>,
    /// Last message written per row (avoids rewriting the same text every tick).
    messages: Mutex<HashMap<RecordingId, String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub(crate) fn profile(&self) -> ProfileId {
        self.session.profile_id()
    }

    pub(crate) fn settings(&self) -> DvrSettings {
        lock(&self.settings).clone()
    }

    /// The recordings folder (created if missing). Blocking.
    pub(crate) fn dir(&self) -> Option<PathBuf> {
        let dir = self.settings().dir?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// A row changed state: the UI reloads its list.
    pub(crate) fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.services.egui().request_repaint();
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Publishes a job's progress (and repaints the UI).
    pub(crate) fn set_progress(&self, id: RecordingId, f: impl FnOnce(&mut Progress)) {
        if let Some(j) = lock(&self.jobs).get_mut(&id) {
            f(&mut j.progress);
        }
        self.services
            .egui()
            .request_repaint_after(Duration::from_millis(250));
    }

    /// Runs a store call on the blocking pool. Errors are logged and give `None`.
    pub(crate) async fn store<T, F>(&self, what: &'static str, f: F) -> Option<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    {
        let store = Arc::clone(&self.services.store);
        match tokio::task::spawn_blocking(move || f(&store)).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "{what} failed");
                None
            }
            Err(_) => None,
        }
    }

    /// Sets a row's state and message, bumps the epoch.
    pub(crate) async fn set_state(
        &self,
        id: RecordingId,
        state: RecordingState,
        message: Option<String>,
    ) {
        {
            let mut m = lock(&self.messages);
            match &message {
                Some(text) => m.insert(id, text.clone()),
                None => m.remove(&id),
            };
        }
        self.store("update a recording", move |s| {
            s.recordings().set_state(id, state, message.as_deref())
        })
        .await;
        self.bump();
    }

    /// Sets only the message of a row that stays in its state, if it changed.
    async fn set_message(&self, r: &Recording, message: String) {
        let unchanged = lock(&self.messages).get(&r.id) == Some(&message)
            || r.error.as_deref() == Some(message.as_str());
        if unchanged {
            return;
        }
        self.set_state(r.id, r.state, Some(message)).await;
    }

    /// Every file name the store references (unique names).
    pub(crate) async fn taken_names(&self) -> HashSet<String> {
        self.store("list file names", |s| s.recordings().file_names())
            .await
            .unwrap_or_default()
            .into_iter()
            .collect()
    }
}

/// The time window a live recording runs in: the programme with the padding settings,
/// or start..end as stored for "until I stop" recordings.
pub fn live_window(r: &Recording, s: &DvrSettings) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = DateTime::from_timestamp_millis(r.start_at?.as_unix_millis())?;
    let end = DateTime::from_timestamp_millis(r.end_at?.as_unix_millis())?;
    if super::is_until_stopped(r) {
        return Some((start, end));
    }
    let before = opensmarters_core::chrono::Duration::seconds(i64::from(s.pad_before));
    let after = opensmarters_core::chrono::Duration::seconds(i64::from(s.pad_after));
    Some((start - before, end + after))
}

/// Keeps the job registered while its task runs; removes it, wakes the supervisor and
/// refreshes the UI when the task ends (however it ends).
struct JobGuard {
    shared: Arc<Shared>,
    id: RecordingId,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        lock(&self.shared.jobs).remove(&self.id);
        self.shared.bump();
        self.shared.wake();
    }
}

/// The engine of one session (`cx.dvr` holds it). Dropping it does not stop the jobs:
/// call [`Engine::shutdown`].
pub struct Engine {
    shared: Arc<Shared>,
    supervisor: JoinHandle<()>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("profile", &self.shared.profile())
            .field("jobs", &lock(&self.shared.jobs).len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Starts the engine for `session`: its task reads the settings, recovers interrupted
    /// jobs, then supervises.
    ///
    /// # Errors
    ///
    /// The downloader can't be built (a User-Agent the HTTP stack refuses).
    pub fn start(session: Arc<Session>) -> AppResult<Self> {
        let services = session.services().clone();
        let downloader = Downloader::new(
            &DownloadConfig {
                user_agent: session.user_agent().to_owned(),
                ..DownloadConfig::default()
            },
            services.redactor.clone(),
        )?;
        let shared = Arc::new(Shared {
            session,
            services: services.clone(),
            downloader,
            epoch: AtomicU64::new(1),
            wake: Notify::new(),
            stopping: AtomicBool::new(false),
            jobs: Mutex::new(HashMap::new()),
            settings: Mutex::new(DvrSettings::default()),
            waiting: Mutex::new(HashMap::new()),
            player_owned: Mutex::new(HashMap::new()),
            messages: Mutex::new(HashMap::new()),
        });
        let sh = Arc::clone(&shared);
        let supervisor = services.spawn(async move {
            load_settings(&sh).await;
            recover(&sh).await;
            supervise(sh).await;
        });
        Ok(Self { shared, supervisor })
    }

    /// The session this engine works for.
    pub fn session(&self) -> &Arc<Session> {
        &self.shared.session
    }

    /// The profile this engine works for.
    pub fn profile(&self) -> ProfileId {
        self.shared.profile()
    }

    /// Changes whenever a row changes state (reload the list then).
    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::SeqCst)
    }

    /// The settings in effect.
    pub fn settings(&self) -> DvrSettings {
        self.shared.settings()
    }

    /// Reads the settings again (after the Settings page saved them) and wakes the
    /// supervisor.
    pub fn reload_settings(&self) {
        let sh = Arc::clone(&self.shared);
        self.shared.services.spawn(async move {
            load_settings(&sh).await;
            sh.bump();
            sh.wake();
        });
    }

    /// A function that re-reads the settings (call it after writing one).
    pub fn settings_reloader(&self) -> impl Fn() + Send + 'static {
        let sh = Arc::clone(&self.shared);
        move || {
            let sh2 = Arc::clone(&sh);
            sh.services.spawn(async move {
                load_settings(&sh2).await;
                sh2.bump();
                sh2.wake();
            });
        }
    }

    /// A function that refreshes the UI and wakes the supervisor (after a store write).
    pub fn waker(&self) -> impl Fn() + Send + 'static {
        let sh = Arc::clone(&self.shared);
        move || {
            sh.bump();
            sh.wake();
        }
    }

    /// Something changed in the store (a new download or schedule): look now.
    pub fn wake(&self) {
        self.shared.bump();
        self.shared.wake();
    }

    /// Live progress of a running job.
    pub fn progress(&self, id: RecordingId) -> Option<Progress> {
        lock(&self.shared.jobs).get(&id).map(|j| j.progress.clone())
    }

    /// True while a job runs for `id`.
    pub fn is_running(&self, id: RecordingId) -> bool {
        lock(&self.shared.jobs).contains_key(&id)
    }

    /// Channel keys of live recordings running now (● REC badges).
    pub fn recording_keys(&self) -> HashSet<String> {
        let mut keys: HashSet<String> = lock(&self.shared.jobs)
            .values()
            .filter(|j| j.kind == RecordingKind::Live)
            .map(|j| j.item_id.clone())
            .collect();
        keys.extend(lock(&self.shared.player_owned).values().cloned());
        keys
    }

    /// Live recordings waiting for a connection: (row, channel key).
    pub fn waiting(&self) -> Vec<(RecordingId, String)> {
        lock(&self.shared.waiting)
            .iter()
            .map(|(id, k)| (*id, k.clone()))
            .collect()
    }

    /// Sends `control` to a running job. Returns false if it isn't running.
    pub fn control(&self, id: RecordingId, control: Control) -> bool {
        match lock(&self.shared.jobs).get(&id) {
            Some(j) => {
                let _ = j.control.send(control);
                true
            }
            None => false,
        }
    }

    /// The player records `id` (channel `key`, same connection): the supervisor leaves
    /// it alone.
    pub fn claim_for_player(&self, id: RecordingId, key: &str) {
        lock(&self.shared.player_owned).insert(id, key.to_owned());
        lock(&self.shared.waiting).remove(&id);
        self.shared.bump();
    }

    /// The player stopped recording `id`.
    pub fn release_from_player(&self, id: RecordingId) {
        lock(&self.shared.player_owned).remove(&id);
        lock(&self.shared.waiting).remove(&id);
        self.shared.bump();
        self.shared.wake();
    }

    /// True if the player records `id`.
    pub fn is_player_owned(&self, id: RecordingId) -> bool {
        lock(&self.shared.player_owned).contains_key(&id)
    }

    /// Stops every job (app exit, profile switch): their rows are marked paused with
    /// [`INTERRUPTED`] right away (one small store write each), so they continue next
    /// time even if the runtime stops before the tasks finish.
    pub fn shutdown(self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let running: Vec<RecordingId> = {
            let jobs = lock(&self.shared.jobs);
            for j in jobs.values() {
                let _ = j.control.send(Control::Interrupt);
            }
            jobs.keys().copied().collect()
        };
        let owned: Vec<RecordingId> = lock(&self.shared.player_owned).keys().copied().collect();
        let recordings = self.shared.services.store.recordings();
        for id in running {
            if let Err(e) = recordings.set_state(id, RecordingState::Paused, Some(INTERRUPTED)) {
                tracing::warn!(error = %e, "couldn't mark a job interrupted");
            }
        }
        // The player's own recordings end with the player: keep what they wrote.
        for id in owned {
            if let Err(e) = recordings.set_state(
                id,
                RecordingState::Done,
                Some("Stopped early: opensmarters closed."),
            ) {
                tracing::warn!(error = %e, "couldn't finish a recording");
            }
        }
        self.supervisor.abort();
        self.shared.wake();
    }
}

/// Reads the settings from the store into the engine.
async fn load_settings(sh: &Arc<Shared>) {
    let store = Arc::clone(&sh.services.store);
    let paths = sh.services.paths.clone();
    let ephemeral = sh.services.ephemeral;
    if let Ok(s) =
        tokio::task::spawn_blocking(move || DvrSettings::load(&store, &paths, ephemeral)).await
    {
        *lock(&sh.settings) = s;
    }
}

/// Start-up recovery (see the [module docs](self)).
async fn recover(sh: &Arc<Shared>) {
    let profile = sh.profile();
    let settings = sh.settings();
    let Some(rows) = sh
        .store("recover recordings", move |s| {
            let crashed: HashSet<RecordingId> = s
                .recordings()
                .with_states(profile, &[RecordingState::Active])?
                .into_iter()
                .map(|r| r.id)
                .collect();
            s.recordings().recover_interrupted()?;
            let paused = s
                .recordings()
                .with_states(profile, &[RecordingState::Paused])?;
            Ok(paused
                .into_iter()
                .filter(|r| crashed.contains(&r.id) || r.error.as_deref() == Some(INTERRUPTED))
                .collect::<Vec<_>>())
        })
        .await
    else {
        return;
    };
    let now = Utc::now();
    for r in rows {
        let ended = r.kind == RecordingKind::Live
            && live_window(&r, &settings).is_none_or(|(_, end)| end <= now);
        if r.kind == RecordingKind::Clip {
            continue;
        }
        if ended {
            finalize_live(
                sh,
                &r,
                Some("Part of the programme is missing: opensmarters was closed during the recording."),
            )
            .await;
        } else {
            sh.set_state(r.id, RecordingState::Queued, None).await;
        }
    }
}

/// Keeps what a live recording wrote (rename `.part` → final) and marks it done, or
/// failed when nothing was recorded. Used when its time is up without a running job.
pub(crate) async fn finalize_live(sh: &Arc<Shared>, r: &Recording, note: Option<&str>) {
    let dir = sh.settings().dir;
    let name = r.file_name.clone();
    let kept = match (dir, name) {
        (Some(dir), Some(name)) => tokio::task::spawn_blocking(move || {
            let part = files::part_path(&dir, &name)?;
            let fin = files::path_in(&dir, &name)?;
            let len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            if len > 0 && files::sniff_file(&part).is_some() && !fin.exists() {
                std::fs::rename(&part, &fin).ok()?;
                Some(len)
            } else if fin.exists() {
                std::fs::metadata(&fin).ok().map(|m| m.len())
            } else {
                None
            }
        })
        .await
        .ok()
        .flatten(),
        _ => None,
    };
    let id = r.id;
    match kept {
        Some(len) => {
            sh.store("save recording size", move |s| {
                s.recordings().set_progress(id, len, Some(len))
            })
            .await;
            sh.set_state(id, RecordingState::Done, note.map(str::to_owned))
                .await;
        }
        None => {
            sh.set_state(
                id,
                RecordingState::Failed,
                Some(
                    if r.state == RecordingState::Scheduled {
                        MISSED
                    } else {
                        "Nothing was recorded: no connection was free, or the channel sent no video."
                    }
                    .to_owned(),
                ),
            )
            .await;
        }
    }
}

async fn supervise(sh: Arc<Shared>) {
    loop {
        if sh.stopping() {
            return;
        }
        // Register for the wake-ups before looking, so none is missed.
        let budget = Arc::clone(sh.session.connections());
        let woken = sh.wake.notified();
        let freed = budget.changed();
        tokio::pin!(woken, freed);
        woken.as_mut().enable();
        tick(&sh).await;
        if sh.stopping() {
            return;
        }
        tokio::select! {
            () = &mut woken => {}
            () = &mut freed => {}
            () = tokio::time::sleep(TICK) => {}
        }
    }
}

async fn tick(sh: &Arc<Shared>) {
    let profile = sh.profile();
    let settings = sh.settings();
    let Some(rows) = sh
        .store("list recordings", move |s| {
            s.recordings().with_states(
                profile,
                &[RecordingState::Scheduled, RecordingState::Queued],
            )
        })
        .await
    else {
        return;
    };
    let now = Utc::now();
    let running: HashSet<RecordingId> = lock(&sh.jobs).keys().copied().collect();
    let owned: HashSet<RecordingId> = lock(&sh.player_owned).keys().copied().collect();
    let mut still_waiting: HashSet<RecordingId> = HashSet::new();

    // 1–2. Live recordings: due → queued → running.
    for r in rows.iter().filter(|r| r.kind == RecordingKind::Live) {
        if running.contains(&r.id) || owned.contains(&r.id) {
            continue;
        }
        let Some((start, end)) = live_window(r, &settings) else {
            continue;
        };
        if end <= now {
            finalize_live(sh, r, None).await;
            continue;
        }
        let mut r = r.clone();
        if r.state == RecordingState::Scheduled {
            if start > now {
                continue;
            }
            sh.set_state(r.id, RecordingState::Queued, None).await;
            r.state = RecordingState::Queued;
            r.error = None;
        }
        match sh
            .session
            .connections()
            .try_acquire(Purpose::Recording(r.id))
        {
            Ok(lease) => spawn_job(sh, &r, lease),
            Err(busy) => {
                still_waiting.insert(r.id);
                lock(&sh.waiting).insert(r.id, r.item_id.clone());
                let msg = format!(
                    "Waiting for a free connection. {} It keeps trying until the programme ends.",
                    busy.message()
                );
                sh.set_message(&r, msg).await;
            }
        }
    }
    lock(&sh.waiting).retain(|id, _| still_waiting.contains(id) || owned.contains(id));

    // 3. Downloads, oldest first.
    let mut slots = usize::from(settings.at_once.max(1)).saturating_sub(
        lock(&sh.jobs)
            .values()
            .filter(|j| j.kind.is_download())
            .count(),
    );
    for r in rows
        .iter()
        .filter(|r| r.kind.is_download() && r.state == RecordingState::Queued)
    {
        if running.contains(&r.id) {
            continue;
        }
        if slots == 0 {
            break;
        }
        match sh
            .session
            .connections()
            .try_acquire(Purpose::Download(r.id))
        {
            Ok(lease) => {
                spawn_job(sh, r, lease);
                slots -= 1;
            }
            Err(busy) => {
                if r.error.as_deref() != Some(PREEMPTED) {
                    let msg = format!("Waiting for a free connection. {}", busy.message());
                    sh.set_message(r, msg).await;
                }
                break;
            }
        }
    }
}

fn spawn_job(sh: &Arc<Shared>, r: &Recording, lease: Lease) {
    let (tx, rx) = watch::channel(Control::Run);
    lock(&sh.jobs).insert(
        r.id,
        Job {
            kind: r.kind,
            item_id: r.item_id.clone(),
            control: tx,
            progress: Progress {
                started: Some(Instant::now()),
                bytes: r.bytes_done,
                total: r.bytes_total,
                ..Progress::default()
            },
        },
    );
    lock(&sh.waiting).remove(&r.id);
    let guard = JobGuard {
        shared: Arc::clone(sh),
        id: r.id,
    };
    let sh = Arc::clone(sh);
    let id = r.id;
    let kind = r.kind;
    sh.services.clone().spawn(async move {
        let _guard = guard;
        if kind.is_download() {
            super::download::run(sh, id, lease, rx).await;
        } else {
            super::record::run(sh, id, lease, rx).await;
        }
    });
    tracing::info!(%id, ?kind, "job started");
}

#[cfg(test)]
mod tests {
    use opensmarters_store::Timestamp;

    use super::*;

    fn row(start: i64, end: i64) -> Recording {
        Recording {
            id: RecordingId(1),
            profile: ProfileId::new_random(),
            kind: RecordingKind::Live,
            state: RecordingState::Scheduled,
            item_id: "1".into(),
            series_id: None,
            season: None,
            episode: None,
            container_extension: None,
            title: "News".into(),
            subtitle: None,
            icon_url: None,
            start_at: Some(Timestamp::from_unix_secs(start)),
            end_at: Some(Timestamp::from_unix_secs(end)),
            file_name: None,
            bytes_done: 0,
            bytes_total: None,
            duration_secs: None,
            position_secs: None,
            error: None,
            created_at: Timestamp::EPOCH,
            updated_at: Timestamp::EPOCH,
        }
    }

    #[test]
    fn windows_add_padding_except_until_stopped() {
        let s = DvrSettings {
            pad_before: 60,
            pad_after: 180,
            ..DvrSettings::default()
        };
        let (a, b) = live_window(&row(10_000, 13_600), &s).unwrap_or_default();
        assert_eq!(a.timestamp(), 10_000 - 60);
        assert_eq!(b.timestamp(), 13_600 + 180);
        let manual = row(
            10_000,
            10_000 + super::super::MAX_RECORDING.as_secs() as i64,
        );
        let (a, b) = live_window(&manual, &s).unwrap_or_default();
        assert_eq!(a.timestamp(), 10_000);
        assert_eq!(b.timestamp(), 10_000 + 8 * 3600);
    }
}
