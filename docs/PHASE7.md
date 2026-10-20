# Phase 7: recording, multiscreen, sleep timer, reminders, stable streams

These are features the user chose on 2026-10-20 after the feature review. This file is
the plan and the contract between the four parallel work streams. Read
[ENGINEERING.md](ENGINEERING.md), [APP_ARCHITECTURE.md](APP_ARCHITECTURE.md) and
[BUILD_PROMPT.md](BUILD_PROMPT.md) §3 (security rules) first. They still apply.

## Scope (what the user asked for)

| Feature | What "done" means |
|---|---|
| **Sleep timer** | Stop after 15/30/45/60/90 min, at the end of this film or episode, or at the end of this programme (live, from the EPG). Fade the volume out over the last ~30 s, then show "Still watching?" with a countdown. Without an answer, stop playback (which frees the connection) and leave fullscreen. |
| **Programme reminders** | On a future programme in the Guide (and Search programme results): "Remind me" or "Switch to it when it starts". A prompt appears 1 min before (setting: 0/1/5 min) with Watch / Dismiss; "switch" changes channel at the start after a short cancellable countdown. If the window isn't focused, it requests attention (taskbar flash). A list sits in Recordings › Scheduled. |
| **Multiscreen 2×2** | 2 or 4 live channels at once (1×2, 2×2), never more than the connection budget allows. Audio follows the focused tile; Enter enlarges a tile; per-tile channel picker and zapping; reconnect per tile. Opened from Live TV (and the player). |
| **Recording & downloads** | 1. **Download films and episodes** (queue, pause/resume with HTTP Range, play offline, including when the provider is unreachable). 2. **Record what I'm watching** (live, until the programme ends or the user stops it). 3. **Scheduled recordings** from the Guide (padding before/after, runs in the background while the app is open). 4. **Save the last N minutes** of a live channel from the player's buffer (no extra connection). |
| **Stable streams** | 1. **Fewer stalls**: transport-level reconnects, initial buffering, a larger VOD buffer (disk-backed), hardware decoding and buffer applied without a restart, faster stall detection. 2. **Pause and rewind live TV**: a disk-backed buffer behind live, "−02:13" behind-live indicator, Go live. 3. **Stream health overlay**: resolution, codecs, hwdec, bitrate, buffer ahead/behind, download speed, dropped frames, reconnects, connections in use. 4. **Auto-tune per stream**: short buffer for live (fast zapping), large for VOD; a channel that stalled gets more buffering next time. |

## Foundation (commit "Phase 7 foundation", branch `phase-7`)

Already built and tested. Build on it; don't rewrite it.

| Piece | Where | Notes |
|---|---|---|
| `recordings` + `reminders` tables (schema v2) | `crates/store/src/{db,recordings,reminders}.rs` | `store.recordings()`: add / get / list / with_states / due / find_live / find_download / set_state / set_file_name / set_progress / set_duration / set_position / reschedule / remove / recover_interrupted / file_names. `store.reminders()`: set (upsert) / find / list / pending / mark_fired / set_action / remove / prune. **Rows never hold URLs**; file names are validated (`recordings::validate_file_name`). |
| Connection budget | `crates/app/src/services/connections.rs`, `session.connections()` | Leases with `Purpose::{Watch, Tile(n), Download(id), Recording(id)}`. Viewing uses `acquire_for_viewing` (it can revoke the newest **download**, never a recording). Background jobs use `try_acquire` (streams on other devices count). `Busy::message()` gives the user-facing text. The limit is the per-profile override (`MAX_STREAMS_OVERRIDE`), else the account's `max_connections`, else 1. |
| Player uses the budget | `crates/app/src/playback/mod.rs` | The shared `PlaybackController` takes a `Watch` lease when it opens a provider stream. It keeps the lease while zapping and reconnecting and releases it on stop, finish or give-up. `set_connection_purpose(Purpose::Tile(n))` is for multiscreen controllers. Local files need no lease. |
| Owned media playback | `crates/player/src/playable.rs` | `PlayableUrl::from_owned_media(root, file)` accepts only a regular file inside `root` whose **content** is a known container (`sniff_container`: TS, MKV/WebM, MP4/MOV, AVI, FLV, MPEG-PS, ASF). Playlists, text and HTML are refused. It plays on every player; `allow_local_files` stays dev-only. |
| Writing media | `MediaOutput::new(root, name)`, `Player::dump_cache(start, end, &out)`, `Player::set_stream_record(Some(&out))` | New file only, bare safe name, `.ts`/`.mkv`/`.mp4`. `dump_cache` **needs an explicit range** (`no no` writes nothing on this libmpv) and a player with `PlayerConfig::seekable_cache(true)` for live TS. `PlayerEvent::CommandDone { op: Op::DumpCache }` fires when the file is complete. `Op::{Tuning, Record, DumpCache}` exist. |
| `MediaKind::Recording` / `PlaybackItem::Recording { id }` | `crates/app/src/playback/request.rs` | Live recordings and clips played from disk. Downloaded films and episodes play as `Movie`/`Episode`, so their resume point is shared with streaming. |
| Routes and stubs | `nav/route.rs`, `screens/{recordings,multiscreen}.rs`, `screens/settings/recordings.rs` | `Route::Recordings { tab: Downloads/Recordings/Scheduled }` (`--route recordings:tab=scheduled`), `Route::Multiscreen` (`--route multiscreen`), `SettingsPage::Recordings` (`settings:recordings`), `Section::Recordings` with a side-rail item. |
| App-wide hooks | `app.rs`, `screens/mod.rs` (`AppCtx`) | New `cx` fields: `cx.dvr` (`dvr::Dvr`), `cx.sleep` (`playback::sleep::SleepTimer`), `cx.alerts` (`services::alerts::Alerts`) and `cx.multi` (`playback::multi::MultiView`). Each frame the app calls `dvr::on_frame`, `playback::sleep::on_frame` and `services::alerts::on_frame` before the screen, and `alerts::show` after the rail. It calls `dvr::on_session_opened` when a profile opens, `cx.dvr.shutdown()` and `cx.multi.destroy(gl)` at exit. **Fill these in. Don't add more edits to `app.rs` / `AppCtx` unless you must.** |
| DVR helpers | `crates/app/src/dvr/mod.rs` | `RECORDINGS_DIR` / `default_recordings_dir` (`Videos\opensmarters`), `PAD_BEFORE`/`PAD_AFTER` (60/180 s), `file_name(title, when, ext)` (always safe), `schedule_programme(store, profile, &ProgrammeToRecord)` (idempotent store insert, the Guide's "Record"), `MAX_RECORDING` 8 h. |

### Facts learned while building the foundation

- **This libmpv build fetches `http(s)` with libcurl** (log prefix `curl`), not ffmpeg's
  `http` protocol. ffmpeg `stream-lavf-o` options (`reconnect=1`, …) therefore do **not**
  apply to top-level streams. They do still apply inside the lavf demuxer (HLS segments,
  `demuxer-lavf-o`). Check which mpv options control the curl stream before relying on any
  of them, and record what you set in `docs/LIBMPV.md`.
- A live TS over HTTP is "not seekable" for mpv. Seeking back in the buffer (pause and
  rewind live) and `dump-cache` both need `demuxer-seekable-cache=yes` +
  `force-seekable=yes` (`PlayerConfig::seekable_cache(true)`).
- `dump-cache` reports success only when the file is written (`CommandDone`), but
  `start=no end=no` silently writes an empty file. Always pass numbers.
- mpv's recorder only writes **selected** tracks. With video disabled (`vid=no`, the
  headless tests) a clip is audio-only.
- Real provider (see PROGRESS.md): `max_connections=3`; stale connections count for
  1–3 min after a burst of opens; everything returns 403 when over the limit. Don't open
  and close streams in quick succession.

## Ownership (four parallel streams)

Each stream works in its own git worktree and branch, created from `phase-7`. Edit the
files you own. A shared file named here may get **small, clearly separated hooks** only.

| Stream | Owns | May add small hooks to |
|---|---|---|
| **A: Stability & live rewind** (`phase-7a`) | `crates/player/src/{config,player}.rs` (properties, state, tuning setters), new player tests (new files; **not** `tests/mpv.rs`), `playback/mod.rs`, `playback/tracks.rs`, `screens/player.rs` (OSD, timeshift bar, health overlay), `screens/settings/playback.rs`, `services/prefs.rs`, `docs/LIBMPV.md` | `screens/live.rs` (preview indicators) |
| **B: Recording & downloads** (`phase-7b`) | `crates/core` (new streaming download + HLS playlist modules, fuzz target), `crates/app/src/dvr/**`, `screens/recordings.rs`, `screens/settings/recordings.rs`, `crates/fake-panel` (Range support, quirks) | `screens/detail.rs` + `screens/detail/episodes.rs` (Download buttons), `screens/player.rs` (Record / Save-clip buttons: call into `dvr::ui`), `screens/live.rs` (● REC badge) |
| **C: Sleep timer & reminders** (`phase-7c`) | `playback/sleep.rs`, `services/alerts.rs` (+ new `services/reminders.rs` if useful), new widget files | `screens/guide.rs` (programme action menu: Remind me / Switch / **Record**, which calls `dvr::schedule_programme`), `screens/search.rs` (same for programme results), `screens/player.rs` (Sleep button, `Z` key) |
| **D: Multiscreen** (`phase-7d`) | `playback/multi.rs`, `screens/multiscreen.rs` | `screens/live.rs` (Multiscreen button), `screens/player.rs` (open multiscreen with this channel) |

Shared rules:
- **`screens/player.rs` has one owner (A).** B, C and D add at most a few lines there
  (a button call and a key), clearly separated, and put the logic in their own modules.
- Store changes need a **new migration** (`MIGRATION_3`). Coordinate through the
  coordinator; avoid them if you can (settings go in `AppSettings` with `Setting<T>`).
- No new `AppCtx` fields beyond the four above without asking.
- The coordinator merges A → B → C → D and resolves the hook conflicts.

## Gates (each stream, before reporting done)

```powershell
$env:CARGO_TARGET_DIR="target/<stream>"   # e.g. target/p7a
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

Then check it in the app with the harness (`--demo --ephemeral`, keyboard only, at
1920×1080, 1280×720 and `--tv`), as APP_ARCHITECTURE §9 describes. Look at the
screenshots. Commit on your own branch (`phase-7a`…`d`), never on `main` or `phase-7`.

## Security reminders specific to this phase

- Files: only names from `dvr::file_name`, only inside the recordings folder, `.part`
  while writing, never overwrite, check the first bytes with `sniff_container` before
  keeping a download, and stop when free disk space runs low.
- URLs come from the session's request builders (`PlayableUrl`), never from the database.
  Never log them unredacted (`cx.services.redactor`).
- Downloads: connect/read timeouts, redirects only to `http(s)` (at most 5), no response
  size cap but a disk-space guard, Range resume validated against `Content-Range`.
- HLS parsing (recording HLS-only channels) treats the playlist as hostile input. It is
  capped, fuzzed and never follows non-`http(s)` URIs. Encrypted segments are unsupported
  (say so).
- mpv writes files only through `MediaOutput`.
