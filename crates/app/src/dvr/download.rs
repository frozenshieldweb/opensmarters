// SPDX-License-Identifier: GPL-3.0-or-later
//! A film or episode download job.
//!
//! 1. Rebuild the stream URL from the profile ([`super::source::download_source`]).
//! 2. Write `<name>.part`: a new file, or the existing one to resume with
//!    `Range: bytes=<its length>-` (checked against `Content-Range` by the core
//!    downloader; a server that ignores the range sends the whole file again, so the
//!    part is emptied first).
//! 3. Keep nothing until the first bytes are recognised as video
//!    (`opensmarters_player::sniff_container`); an HTML page or a playlist deletes the
//!    part and fails with [`NOT_VIDEO`].
//! 4. Stop at once when viewing takes the connection (back to `queued`, [`PREEMPTED`]),
//!    when paused or cancelled, when the app closes, or when free space drops below the
//!    setting.
//! 5. At the end, rename `.part` to the final name (with the extension of what it really
//!    is) and mark it done.

use std::sync::Arc;

use opensmarters_core::Error as CoreError;
use opensmarters_core::download::DownloadRequest;
use opensmarters_player::{Container, SNIFF_BYTES, sniff_container};
use opensmarters_store::{Recording, RecordingId, RecordingKind, RecordingState};
use tokio::sync::watch;

use super::engine::{Control, INTERRUPTED, PREEMPTED, Shared};
use super::jobs::{
    Meter, NOT_VIDEO, PartWriter, backoff, disk_ok, failure_message, is_transient,
    low_disk_message, next_control, sleep_or_control,
};
use super::{files, source};
use crate::services::connections::Lease;

/// How a run ended.
#[derive(Debug)]
enum Outcome {
    Done,
    Paused(Option<String>),
    Requeued(String),
    Failed(String),
    Cancelled,
    Interrupted,
}

fn on_control(c: Control) -> Outcome {
    match c {
        Control::Cancel => Outcome::Cancelled,
        Control::Interrupt => Outcome::Interrupted,
        Control::Pause | Control::Finish | Control::Run => Outcome::Paused(None),
    }
}

/// Runs the download of row `id` while holding `lease`.
pub(crate) async fn run(
    sh: Arc<Shared>,
    id: RecordingId,
    lease: Lease,
    mut control: watch::Receiver<Control>,
) {
    let Some(Some(rec)) = sh
        .store("load a download", move |s| s.recordings().get(id))
        .await
    else {
        return;
    };
    if !matches!(rec.state, RecordingState::Queued | RecordingState::Active) {
        return;
    }
    sh.set_state(id, RecordingState::Active, None).await;
    let outcome = download(&sh, &rec, &lease, &mut control).await;
    drop(lease);
    tracing::info!(%id, ?outcome, "download ended");
    match outcome {
        Outcome::Done => {}
        Outcome::Paused(msg) => sh.set_state(id, RecordingState::Paused, msg).await,
        Outcome::Requeued(msg) => sh.set_state(id, RecordingState::Queued, Some(msg)).await,
        Outcome::Failed(msg) => sh.set_state(id, RecordingState::Failed, Some(msg)).await,
        Outcome::Interrupted => {
            sh.set_state(id, RecordingState::Paused, Some(INTERRUPTED.to_owned()))
                .await;
        }
        Outcome::Cancelled => remove(&sh, id).await,
    }
}

/// Deletes the row and its files (cancel).
pub(crate) async fn remove(sh: &Arc<Shared>, id: RecordingId) {
    let dir = sh.settings().dir;
    sh.store("cancel a download", move |s| {
        if let (Some(dir), Some(r)) = (dir, s.recordings().get(id)?)
            && let Some(name) = r.file_name
        {
            files::delete(&dir, &name);
        }
        s.recordings().remove(id)
    })
    .await;
    sh.bump();
}

/// The file title of a download: the film, or "Series – S1E2 – Episode".
fn file_title(r: &Recording) -> String {
    match r.kind {
        RecordingKind::Episode => {
            let se = match (r.season, r.episode) {
                (Some(s), Some(e)) => format!("S{s:02}E{e:02}"),
                (None, Some(e)) => format!("E{e:02}"),
                _ => String::new(),
            };
            [r.subtitle.clone().unwrap_or_default(), se, r.title.clone()]
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" - ")
        }
        _ => r.title.clone(),
    }
}

#[allow(clippy::too_many_lines)]
async fn download(
    sh: &Arc<Shared>,
    rec: &Recording,
    lease: &Lease,
    control: &mut watch::Receiver<Control>,
) -> Outcome {
    let id = rec.id;
    let settings = sh.settings();
    let dir = {
        let sh2 = Arc::clone(sh);
        tokio::task::spawn_blocking(move || sh2.dir())
            .await
            .ok()
            .flatten()
    };
    let Some(dir) = dir else {
        return Outcome::Failed(
            "The recordings folder can't be used. Choose another one in Settings › \
             Recordings & downloads."
                .to_owned(),
        );
    };
    let source = match source::download_source(&sh.session, rec) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.user_message()),
    };
    let Ok(url) = source.http_url() else {
        return Outcome::Failed("This address can't be downloaded.".to_owned());
    };

    // The file name: kept from an earlier run, else a new unique one.
    let name = match &rec.file_name {
        Some(n) => n.clone(),
        None => {
            let taken = sh.taken_names().await;
            let base = files::download_name(
                &file_title(rec),
                rec.container_extension.as_deref().unwrap_or("mkv"),
            );
            let d = dir.clone();
            let Some(name) =
                tokio::task::spawn_blocking(move || files::unique_name(&d, &taken, &base))
                    .await
                    .ok()
                    .flatten()
            else {
                return Outcome::Failed(
                    "No free file name was found in the recordings folder.".to_owned(),
                );
            };
            let n = name.clone();
            sh.store("save a file name", move |s| {
                s.recordings().set_file_name(id, &n)
            })
            .await;
            name
        }
    };
    let (Some(part), Some(final_path)) =
        (files::part_path(&dir, &name), files::path_in(&dir, &name))
    else {
        return Outcome::Failed("This file name isn't valid.".to_owned());
    };
    let (part_exists, final_exists) = {
        let (p, f) = (part.clone(), final_path.clone());
        tokio::task::spawn_blocking(move || (p.exists(), f.exists()))
            .await
            .unwrap_or((false, false))
    };
    if final_exists && !part_exists {
        // Finished earlier (the row didn't get the news).
        return finish(sh, rec, &dir, &name, None).await;
    }
    let mut writer = match PartWriter::open(&part, !part_exists).await {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "couldn't open a download file");
            return Outcome::Failed(
                "Couldn't write to the recordings folder. Check that it still exists and \
                 that you can write to it."
                    .to_owned(),
            );
        }
    };
    // A resumed part must already start like video.
    let mut container: Option<Container> = None;
    if !writer.is_empty() {
        let p = part.clone();
        container = tokio::task::spawn_blocking(move || files::sniff_file(&p))
            .await
            .ok()
            .flatten();
        if container.is_none() && writer.restart().await.is_err() {
            return Outcome::Failed(NOT_VIDEO.to_owned());
        }
    }

    let ua = source
        .headers
        .user_agent
        .as_ref()
        .map(|u| u.as_str().to_owned());
    let referrer = source
        .headers
        .referrer
        .as_ref()
        .map(|r| r.as_str().to_owned());
    let mut meter = Meter::new(writer.len());
    let mut attempt: u32 = 0;
    let mut mismatches: u32 = 0;
    let mut last_store = std::time::Instant::now();
    let mut next_disk_check = writer.len();
    let mut total: Option<u64> = rec.bytes_total;
    'connect: loop {
        if !disk_ok(&dir, settings.keep_free_bytes()).await {
            writer.close().await;
            return Outcome::Paused(Some(low_disk_message(settings.keep_free_bytes(), true)));
        }
        let offset = writer.len();
        let req = DownloadRequest {
            offset,
            user_agent: ua.as_deref(),
            referrer: referrer.as_deref(),
        };
        let opened = tokio::select! {
            r = sh.downloader.open(&url, &req) => r,
            () = lease.revoked() => {
                writer.close().await;
                return Outcome::Requeued(PREEMPTED.to_owned());
            }
            c = next_control(control) => {
                writer.close().await;
                return on_control(c);
            }
        };
        let mut dl = match opened {
            Ok(dl) => dl,
            Err(CoreError::RangeNotSatisfiable { total: Some(t) })
                if t == offset && offset > 0 && container.is_some() =>
            {
                break 'connect;
            }
            Err(CoreError::RangeMismatch | CoreError::RangeNotSatisfiable { .. }) => {
                mismatches += 1;
                if mismatches > 2 || writer.restart().await.is_err() {
                    writer.close().await;
                    return Outcome::Failed(failure_message(&CoreError::RangeMismatch));
                }
                container = None;
                continue 'connect;
            }
            Err(e) if is_transient(&e) && attempt < 5 => {
                sh.set_progress(id, |p| {
                    p.note = Some(format!("Connection lost. Retrying… ({})", attempt + 1));
                });
                if let Some(c) = sleep_or_control(backoff(attempt), control).await {
                    writer.close().await;
                    return on_control(c);
                }
                attempt += 1;
                continue 'connect;
            }
            Err(e) => {
                writer.close().await;
                return Outcome::Failed(failure_message(&e));
            }
        };
        if dl.start() == 0 && offset > 0 {
            // The server ignored the range: start the file over.
            if writer.restart().await.is_err() {
                writer.close().await;
                return Outcome::Failed("Couldn't write to the recordings folder.".to_owned());
            }
            container = None;
            meter = Meter::new(0);
            next_disk_check = 0;
        }
        if dl.total().is_some() {
            total = dl.total();
        }
        sh.set_progress(id, |p| {
            p.total = total;
            p.note = None;
        });
        let mut head: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::select! {
                r = dl.chunk() => r,
                () = lease.revoked() => {
                    writer.close().await;
                    return Outcome::Requeued(PREEMPTED.to_owned());
                }
                c = next_control(control) => {
                    writer.close().await;
                    return on_control(c);
                }
            };
            match chunk {
                Ok(Some(bytes)) => {
                    let written = if container.is_none() {
                        head.extend_from_slice(&bytes);
                        if head.len() < SNIFF_BYTES {
                            continue;
                        }
                        let Some(c) = sniff_container(&head) else {
                            return not_video(sh, writer, &dir, &name).await;
                        };
                        container = Some(c);
                        writer.write(&std::mem::take(&mut head)).await
                    } else {
                        writer.write(&bytes).await
                    };
                    if let Err(e) = written {
                        tracing::warn!(error = %e, "download write failed");
                        writer.close().await;
                        return Outcome::Paused(Some(
                            "Couldn't write to the disk. It may be full. Free some space, then \
                             resume."
                                .to_owned(),
                        ));
                    }
                    attempt = 0;
                    let bytes_now = writer.len();
                    let rate = meter.update(bytes_now);
                    sh.set_progress(id, |p| {
                        p.bytes = bytes_now;
                        p.rate = rate;
                        p.note = None;
                    });
                    if last_store.elapsed() >= super::engine::PROGRESS_EVERY {
                        last_store = std::time::Instant::now();
                        sh.store("save download progress", move |s| {
                            s.recordings().set_progress(id, bytes_now, total)
                        })
                        .await;
                    }
                    if bytes_now >= next_disk_check {
                        next_disk_check = bytes_now + super::engine::DISK_CHECK_BYTES;
                        if !disk_ok(&dir, settings.keep_free_bytes()).await {
                            writer.close().await;
                            return Outcome::Paused(Some(low_disk_message(
                                settings.keep_free_bytes(),
                                true,
                            )));
                        }
                    }
                }
                Ok(None) => {
                    if container.is_none() {
                        // A short file: judge what arrived.
                        match sniff_container(&head) {
                            Some(c) => {
                                container = Some(c);
                                if writer.write(&std::mem::take(&mut head)).await.is_err() {
                                    writer.close().await;
                                    return Outcome::Failed(
                                        "Couldn't write to the recordings folder.".to_owned(),
                                    );
                                }
                            }
                            None => return not_video(sh, writer, &dir, &name).await,
                        }
                    }
                    if total.is_some_and(|t| writer.len() < t) {
                        // The body ended early: resume from here.
                        if attempt >= 5 {
                            writer.close().await;
                            return Outcome::Failed(
                                "The download kept stopping before the end. Try again later."
                                    .to_owned(),
                            );
                        }
                        if let Some(c) = sleep_or_control(backoff(attempt), control).await {
                            writer.close().await;
                            return on_control(c);
                        }
                        attempt += 1;
                        continue 'connect;
                    }
                    break 'connect;
                }
                Err(e) if is_transient(&e) && attempt < 5 => {
                    sh.set_progress(id, |p| {
                        p.note = Some(format!("Connection lost. Retrying… ({})", attempt + 1));
                    });
                    if let Some(c) = sleep_or_control(backoff(attempt), control).await {
                        writer.close().await;
                        return on_control(c);
                    }
                    attempt += 1;
                    continue 'connect;
                }
                Err(e) => {
                    writer.close().await;
                    return Outcome::Failed(failure_message(&e));
                }
            }
        }
    }
    writer.close().await;
    finish(sh, rec, &dir, &name, container).await
}

/// Deletes the part of a body that isn't video and fails.
async fn not_video(
    sh: &Arc<Shared>,
    writer: PartWriter,
    dir: &std::path::Path,
    name: &str,
) -> Outcome {
    writer.close().await;
    let (d, n) = (dir.to_path_buf(), name.to_owned());
    let _ = tokio::task::spawn_blocking(move || files::delete(&d, &n)).await;
    let _ = sh;
    Outcome::Failed(NOT_VIDEO.to_owned())
}

/// Renames the finished part (fixing the extension to what the file really is) and
/// marks the row done.
async fn finish(
    sh: &Arc<Shared>,
    rec: &Recording,
    dir: &std::path::Path,
    name: &str,
    container: Option<Container>,
) -> Outcome {
    let id = rec.id;
    let taken = sh.taken_names().await;
    let (d, n) = (dir.to_path_buf(), name.to_owned());
    let renamed = tokio::task::spawn_blocking(move || -> Option<(String, u64)> {
        let part = files::part_path(&d, &n)?;
        let mut final_name = n.clone();
        if let Some(c) = container {
            let ext = files::container_extension(c);
            if !n.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
                let wanted = files::with_extension(&n, ext);
                let mut others = taken.clone();
                others.remove(&n);
                final_name = files::unique_name(&d, &others, &wanted)?;
            }
        }
        let target = files::path_in(&d, &final_name)?;
        if part.exists() {
            if target.exists() {
                return None;
            }
            std::fs::rename(&part, &target).ok()?;
        }
        let len = std::fs::metadata(&target).ok()?.len();
        Some((final_name, len))
    })
    .await
    .ok()
    .flatten();
    let Some((final_name, len)) = renamed else {
        return Outcome::Failed(
            "The download finished but couldn't be saved under its final name.".to_owned(),
        );
    };
    sh.store("finish a download", move |s| {
        s.recordings().set_file_name(id, &final_name)?;
        s.recordings().set_progress(id, len, Some(len))
    })
    .await;
    sh.set_state(id, RecordingState::Done, None).await;
    Outcome::Done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_file_titles() {
        let mut r = Recording {
            id: RecordingId(1),
            profile: opensmarters_store::ProfileId::new_random(),
            kind: RecordingKind::Episode,
            state: RecordingState::Queued,
            item_id: "5".into(),
            series_id: Some("2".into()),
            season: Some(1),
            episode: Some(3),
            container_extension: None,
            title: "The Return".into(),
            subtitle: Some("Saltward".into()),
            icon_url: None,
            start_at: None,
            end_at: None,
            file_name: None,
            bytes_done: 0,
            bytes_total: None,
            duration_secs: None,
            position_secs: None,
            error: None,
            created_at: opensmarters_store::Timestamp::EPOCH,
            updated_at: opensmarters_store::Timestamp::EPOCH,
        };
        assert_eq!(file_title(&r), "Saltward - S01E03 - The Return");
        r.kind = RecordingKind::Movie;
        assert_eq!(file_title(&r), "The Return");
    }
}
