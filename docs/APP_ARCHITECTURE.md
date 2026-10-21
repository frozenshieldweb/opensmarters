# App architecture (`crates/app`)

This is the map of the desktop app for everyone who builds screens on top of it. Read
[DESIGN.md](DESIGN.md) for the look, [ENGINEERING.md](ENGINEERING.md) for the rules and
[BUILD_PROMPT.md](BUILD_PROMPT.md) §3 for the non-negotiable security requirements.

```powershell
# Run against the built-in synthetic provider (nothing touches your real data or keychain)
cargo run -p opensmarters --features demo -- --demo --ephemeral
# With the dev/test harness flags (--route, --keys, screenshots…; §9)
cargo run -p opensmarters --features demo,harness -- --demo --ephemeral --route live
# All gates (use your own target dir when several agents build at once)
$env:CARGO_TARGET_DIR="target/app"
cargo fmt; cargo clippy -p opensmarters --all-targets --all-features -- -D warnings; cargo test -p opensmarters --all-features
```

---

## 1. Module map

| Path | What lives there |
|---|---|
| `src/main.rs` | CLI parsing, logging, tokio runtime, store, `--demo`, `eframe::run_native` |
| `src/app.rs` | `OpensmartersApp` (the `eframe::App`): frame loop, router, lifecycle hooks, zoom/TV mode, harness hook |
| `src/screens/` | **One module per screen** + the [`Screen`](#3-the-screen-contract) trait and `AppCtx` (`screens/mod.rs`) |
| `src/screens/shell.rs` | Side rail (§6.1; profile menu: Switch / Manage profiles, closed by `shell::close_menus`), `status_cluster` (clock, a focusable accent "Update available" pill that opens Settings › About — never without the side rail, so not over fullscreen playback —, offline, a quiet unencrypted lock with a tooltip, "Not verified" account notice), `page_title` |
| `src/screens/common/` | Logic shared by screens: `locks` (an item is locked when any of its categories is), `channel_list` (unlocked + sorted channel lists), `profile_data::delete_profile` (also forgets the profile's recent searches and remembered categories; use it wherever a profile is deleted), `unlock::UnlockFlow` (after a request builder's `AppError::Locked`: `verify_item` when the categories are unknown, then the PIN, `unlock_category`/`unlock_item`, retry; Home and Detail use it) |
| `src/nav/` | `Route` (typed routes, `--route` syntax, `back_label`), `Router` (back stack), `Navigator` (`cx.nav`), `nav::back_label(ctx)` (what the page below is called: "‹ Movies" back buttons) |
| `src/focus/` | The spatial focus engine (`engine.rs`, pure + unit-tested) and its egui glue (`mod.rs`) |
| `src/ui/` | Design system: `theme` (tokens, fonts, visuals), `icons` (Phosphor), `paint` (focus ring/glow, scrims, cover-fit), `text` (capping, truncation, timecodes), `motion`, `layout` (breakpoints), `assets` (bundled art), `widgets/`. `theme`, `icons`, `paint` and `motion` live in the shared **`crates/afterglow`** crate (also used by the installer, ADR 0002) and are re-exported here unchanged |
| `src/services/` | `Services` (runtime, store, redactor, images, paths, background-error queue, FIFO store writer `writer`), `fetch_queue` (bounded "what is on screen" request queue), `Session` (provider: catalogue, EPG, favourites, parental, playback requests; Phase-4 additions in child modules `session/*.rs`; parental locks at the point of use in `session/locks.rs`), `catalog` (`LiveChannel`, `LiveCatalog`, `Programme`), `images`, `epg`, `prefs` (app-wide preferences), `parental` (PIN length, lockout/wrong-PIN texts, "forgot PIN" check with a limiter persisted in the store: `forgot_pin_attempt`), `backup` (export/import without secrets or machine-local settings), `search_history` (recent searches per profile), `connections` (connection budget, Phase 7), `alerts` (app-wide prompts, §7.2), `reminders` (programme reminders and actions, §7.3), `updates` (`cx.services.updates`: "Check for updates", **notify only**: one GET to GitHub's Releases API (https, 256 KiB, 20 s), strict tag parsing, the release link built locally; the weekly automatic check is off by default (pref `check_updates_automatically`), its state `app.updates` is machine-local; `updates::on_frame` runs it and opens About from the header pill; see SECURITY.md §4.5), `demo` |
| `src/platform.rs` | Safe OS queries: Windows *Animation effects* (`UISettings.AnimationsEnabled` via the `windows` crate's WinRT projection) for reduced motion; `open_folder` / `open_url` through `launcher()` — the system launcher by **absolute path** (`%SystemRoot%\explorer.exe`, `/usr/bin/open`, `xdg-open`), never a bare name resolved via the current directory or `PATH` (SR-16) |
| `src/window_state.rs` | Window size/position/maximised remembered between runs (store setting `app.window`, physical px, debounced; never for `--ephemeral`/`--window-size`) |
| `src/playback/tracks.rs` | Preferred audio/subtitle languages (ISO 639 normalisation, track choice) |
| `src/playback/tuning.rs` | Buffering policy: `PlayerTuning` (hwdec, Buffering Auto/Small/Standard/Large, rewind window), `stream_tuning` (the `StreamTuning` of each load), `ChannelBoosts` (extra buffering per channel), the disk cache folder (`init_disk_cache`) |
| `src/playback/stream.rs` | The controller's per-stream state and hooks: `set_tuning`, `health`, extra buffering for channels that run dry, held last picture, one-shot messages (`take_messages`) |
| `src/playback/timeshift.rs` | Pause and rewind live TV: behind-live accounting, seek targets inside the buffer, `live_view` / `live_seek` / `go_live` |
| `src/playback/` | `PlaybackController` (the one mpv player + video view, reconnect, progress), `PlaybackRequest` and header plumbing (`request.rs`), the sleep timer (`sleep.rs`, §7.1) |
| `src/screens/player/` | Parts of the fullscreen player: `timeshift.rs` (LIVE / −mm:ss badge, rewind bar, ←/→ in time, Go live), `health.rs` (stream details panel), `actions.rs` (`osd_icon` for feature buttons) |
| `src/playback/multi.rs`, `src/screens/multiscreen/` | Multiscreen: `MultiView` (`cx.multi`, the tile players, budget/stagger/sound rules) and the screen (layouts, channel picker); see [§7.4](#74-multiscreen) |
| `src/sessions.rs` | `SessionManager` (`cx.sessions`): open/switch profiles |
| `src/dvr/` | Recordings & downloads: engine, jobs, clips, hooks (`cx.dvr`, see §14) |
| `src/task.rs` | `Loader<T>`: background work with staleness protection |
| `src/logging.rs` | Redacted `tracing` (stderr + rotating log file) |
| `src/cli.rs`, `src/harness.rs` | Command line and the dev/test harness (scripted keys, screenshots; harness flags only with the `harness` feature) |
| `tests/session.rs`, `tests/startup.rs`, `tests/updates.rs` | Integration tests against the in-process `fake-panel` (startup: cached fast start + background check; updates: the update check against `/__updates/releases/latest` and a raw socket, never GitHub) |

The crate is a binary **and** a library (`opensmarters_app`) so integration tests can drive
the session layer, router and focus engine.

## 2. The frame loop (`app.rs`)

Every frame, in order:

1. `raw_input_hook` — the harness injects scripted key events.
2. Zoom: TV mode sets `zoom = window_px / 1100` (§3), else `--ppp` or 1.
3. Startup profile: `Session::open_cached` (store/keychain only) opens a profile that has a
   cached login **and** live catalogue at once, then `Session::verify` checks the account
   in the background (network error → offline; refused → toast + quiet "Not verified"
   header notice). Without a cache the normal sign-in runs while Home shows its skeleton
   (no blocking "Signing in…" screen at startup; a failure lands on the profile picker).
   `sessions.poll()` — a finished profile open navigates to Home (+ pending `--route`).
4. `playback.update()` — mpv events, reconnect, stall detection, progress saves, debounced zaps.
5. Top screen's `on_input(cx, ctx)` — **screens claim their keys first** (digits, PageUp…).
6. `focus::begin_frame` — the engine consumes arrows/Enter/Space/Tab; Esc/Backspace = *back*.
7. Back: the top screen's `on_back` may consume it (close a dialog, hide the OSD); else the
   router goes back; on the root, the focus moves to the side rail.
8. Paint: background, the screen inside the content rect (focus group *content*), then the
   side rail on its own layer (focus group *shell*), then toasts. A route change fades the
   new page in (§7 fade-through: 240 ms cubic-out with an 8 pt rise; reduced motion a
   100 ms fade; not into the player, not between two instances of the same page). The page
   always runs inside the same `with_visual_transform` scope, so ids are stable.
9. `focus::end_frame` — resolves the pending arrow with this frame's geometry.
10. Apply queued navigation (lifecycle hooks, focus save/restore), save preferences.

Nothing in the frame blocks: network, store, keychain, Argon2 and image decoding run on the
tokio runtime and come back through `Loader`s or shared state that triggers
`ctx.request_repaint()`.

## 3. The Screen contract

```rust
pub trait Screen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui);    // draw (never block)
    fn on_enter(&mut self, _cx: &mut AppCtx<'_>) {}               // became top (first time and on return)
    fn on_leave(&mut self, _cx: &mut AppCtx<'_>) {}               // covered or removed; cx.next_route = what's next
    fn on_input(&mut self, _cx: &mut AppCtx<'_>, _ctx: &egui::Context) {} // claim keys before the focus engine
    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool { false } // true = Esc/Backspace consumed
    fn chrome(&self) -> Chrome { Chrome::Shell }                  // Shell (side rail) or Fullscreen
    fn section(&self) -> Option<Section> { None }                 // side-rail highlight
}
```

Screens are created by `screens::create(&Route)` and **live in the back stack**: a covered
screen keeps its state (loaded data, scroll, focus) and gets `on_enter` again when you come
back. `ui` receives a `Ui` whose max rect is `cx.layout.content` (right of the rail for
`Chrome::Shell`, the whole window for `Chrome::Fullscreen`).

### `AppCtx` (`cx`)

| Field / method | Use |
|---|---|
| `cx.session()` | `Option<Arc<Session>>`: the open profile (clone into tasks) |
| `cx.services` | `rt()`, `store` (**never on the UI thread**), `redactor`, `images`, `paths`, `ephemeral`, `blocking(..)`, `spawn(..)` |
| `cx.nav` | `push`, `replace`, `back`, `reset`, `section` (applied after the frame) |
| `cx.playback` | the shared player (see §7) |
| `cx.play(req, fullscreen)` | play a `PlaybackRequest` (fullscreen pushes the player route) |
| `cx.toasts` | `info/success/warning/error` |
| `cx.prefs` + `cx.save_prefs()` | TV mode, UI size, reduced motion (+ follow Windows), 12/24 h, aspect preset, volume, preferred audio/subtitle language, hardware decoding, buffering, live rewind window, check for updates automatically |
| `cx.sessions` | open/switch profiles, last failure |
| `cx.layout` | breakpoint, `screen`, `content`, `margin`, `poster_width`, `tv`, card sizes, `safe_area()`, `grid_columns()` |
| `cx.now` | `DateTime<Utc>` for EPG progress |
| `cx.egui` | the `egui::Context` |
| `cx.sleep` | the sleep timer (§7.1): `choice()`, `is_set()`, `left()`, `player_on_top(ctx)` |
| `cx.alerts` | app-wide prompts (§7.2): `push`, `update`, `dismiss`, `take_answer`; `cx.alerts.reminders` (§7.3): `mark(stream_id, start)` |

### Adding a screen (checklist)

1. Create `src/screens/<name>.rs` (or a folder) with a struct implementing `Screen`.
2. Add a `Route` variant in `nav/route.rs` (with `parse`, `Display`, `same_page`, `section`)
   and the `create` arm in `screens/mod.rs`. *(The routes for Movies, Series, MovieDetail,
   SeriesDetail, Catchup, Guide, Search, Favourites and Settings pages already exist — you
   only replace the stub file.)*
3. Load data in `on_enter` with `Loader`s; poll them at the top of `ui`.
4. Put focusable widgets in `focus::scope`s (one per list/rail/form) with stable ids
   (`Id::unique(("movies.poster", stream_id))`), give the preferred initial widget
   `.autofocus()`.
5. Draw `shell::status_cluster(cx, ui)` if the screen has a header (clock, warnings).
6. Handle Esc in `on_back` only for local state (dialogs, overlays).
7. Check it at 1920×1080, 1280×720 and `--tv` with the harness (§9), keyboard only.

### Settings pages and parental control (Phase 4C)

| Page | File | Notes |
|---|---|---|
| Account | `settings/account.rs` | "Edit account" asks for the PIN |
| Profiles | `settings/profiles.rs` | switch / add / edit / delete (PIN) |
| Playback | `settings/playback.rs` | this profile (save + reconnect); simultaneous streams for this profile (Auto = the account's limit, or 1–4; applied at once to `session.connections()`); every profile: languages, hardware decoding, Buffering (Auto/Small/Standard/Large), Rewind live TV (Off/15/30/60 min), reset the extra buffering of channels that stalled (all applied at once) |
| Parental control | `settings/parental.rs`, `pin_flow.rs` | the page opens only with the PIN; set/change (4–8 digits, confirm) / turn off; locked words; per-category Auto/Lock/Allow; forgot PIN |
| Appearance | `settings/appearance.rs` | TV mode, UI size (desktop), motion (Follow Windows / Reduced / Full) |
| Language & region | `settings/language.rs` | 12/24 h clock, display language (English only) |
| Storage & data | `settings/storage.rs` | cache/pictures usage and clearing, cache limit, history, backup export/import (PIN), data folder, copy log |
| Privacy & security | `settings/privacy.rs` | plain-language facts; optional TMDB key (keychain, checked on request) |
| About | `settings/about.rs`, `licences.rs` | Updates first (`services/updates.rs`): the installed version, **Check for updates** (spinner while checking) with the result — up to date / available (**Get the update** opens the locally built release page, **Not now** dismisses that version) / a plain-language error with Try again — and the **Check automatically** toggle (off by default); then versions, GPL notice, source link, third-party licences (a test keeps the list in sync with direct dependencies) |

While a PIN is set it is required for: locked categories, Settings › Parental, adding /
editing / deleting profiles (picker, Settings › Profiles, Settings › Account) and importing a
backup. **Forgot PIN:** typing the password of one of this computer's Xtream accounts (or an
M3U playlist address) removes it (constant-time comparison against the keychain, in-memory
rate limit); otherwise deleting the data folder is the documented reset. The first run with
no profile shows `Route::Welcome` (explanation → onboarding, or restore a backup).

## 4. Navigation and routes

`--route` and `Route::parse` share one syntax: `home`, `live`, `live:category=<id|all|favourites>`,
`guide`, `movies[:category=<id>]`, `series[:category=<id>]`, `detail:movie=<id>`,
`detail:series=<id>`, `catchup[:stream=<id>]`, `search[:q=<text>]`, `favourites`,
`settings[:account|profiles|playback|parental|appearance|language|storage|privacy|about]`, `welcome`, `onboarding`,
`profiles`, `player:live=<channel id or number>`, `multiscreen`.

Rules (unit-tested in `nav/router.rs`):
- `push` (no-op if the same page is on top), `replace`, `back` (no-op on the root),
  `reset` (sign-in, profile switch), `section` (side rail: stack becomes `[Home, section]`,
  an existing Home keeps its state).
- Routes that need a session go to the profile picker when none is open.
- Each stack entry saves the focused id when covered and restores it on return.
- Esc **and** Backspace (outside text fields) go back on every screen.

## 5. Services and the session

`Session` (`services/session.rs`) is the provider connection of the open profile. It is
`Arc`-shared; async methods take `self: &Arc<Self>`.

| Area | API |
|---|---|
| Info | `profile()`, `profile_id()`, `kind()`, `is_xtream()`, `account()` (status, expiry, connections…), `login()`, `is_insecure()` / `insecure_reason()` → `InsecureReason::{PlainAccount, PlainStreams, PlainPlaylistEntries{count}}` (+ `.message()`; SR-12: an https panel that hands out http stream URLs, or http playlist entries, also count), `is_offline()`, `user_agent()`, `allowed_live_containers()`, `live_container()`, `xtream()` (raw client), `playlist()` (M3U), `http()` |
| Lifetime | `close()` / `is_closed()`: called by `SessionManager` when another open replaces the session (or it is closed); queued EPG/guide/look-up/image work is dropped and background tasks stop before their next provider call |
| Catalogue | `live_catalog()` → `LiveCatalog` (channels + categories, M3U groups too; rebuilt only when its lists change), `last_live_catalog()` (no I/O), `vod_categories()`, `vod_streams(Option<CategoryId>)`, `series_categories()`, `series_list(Option<CategoryId>)`, `vod_info(id)`, `series_info(id)`, `epg_table(stream_id)` (catch-up), `peek_cached::<T>(key)` (cache only, no network) |
| EPG | `epg(&channel)` → `EpgState::{Loading, Missing, Ready(Arc<EpgRow>)}` and `now_next(&channel, now)`: non-blocking, **call every frame for the rows on screen**. Served from the guide's bounded row cache (one cache for every screen), else one short `get_short_epg` through the shared bounded queue (≤ 3 requests, newest rows first, rows that left the screen dropped). Expired rows stay visible while they refresh. `epg_queue_stats()` for logs/tests |
| Requests | `live_request(&channel, Option<ZapContext>)`, `movie_request(..)`, `episode_request(..)`, `catchup_request(&channel, &ArchiveProgramme, resume)`, `resume_request(&PlaybackProgress)` — all return `AppError::Locked(LockedItem)` for items of locked categories (see Parental) |
| Favourites | `is_favourite(kind, id)`, `favourites(kind)` → `Arc<HashSet<String>>` (shared, no copy), `favourites_len(kind)`, `favourites_generation()` (bumped on every change: use it to know when to rebuild a filtered list), `toggle_favourite(kind, id, snapshot)` (optimistic, persisted in order), `reload_favourites()`. `favourite_ids(kind)` copies the set: kept for compatibility, don't call it every frame |
| Parental | `parental_enabled()`, `pin_length()` (4–8: create PIN pads with `PinPad::new(id, session.pin_length())`, or use `widgets::PinPrompt`), `is_category_locked(kind, &id, name)`, `is_selection_locked(..)`, `unlock_category(..)`, `reload_parental()` (call after Settings → Parental changes; wipes the image disk cache when a PIN was just set). **Items** (`session/locks.rs`): `is_item_locked(kind, &[CategoryId])` (every category counts), `categories_lock(kind, &cats)` / `item_lock(kind, id, Option<&cats>)` → `ItemLock::{Unlocked, Locked{category, name}, Unknown}`, `locked_category(..)`, `is_locked(&item)`, `filter_unlocked(items)`, `retain_unlocked(&mut vec)` for any `Lockable` (channels, films, series, favourites, resume points, recent channels), `check_unlocked(..)`, `verify_item(kind, id).await`, `unlock_item(kind, id)`, `relock_all()`, `note_activity()`, `category_name(kind, &id)` |
| Cache | `cached(key, ttl, fetch)` (generic SWR), `refresh()` (drop everything; `SessionManager::reopen_fresh(&services, &session)` awaits it and then reopens the profile — use that for "Refresh") |
| Movies / series (`session/vod.rs`) | `movie_progress(id)`, `movie_progress_map()`, `series_watch(id)` → `SeriesWatch`, `set_episode_watched(..)`, `series_episode_request(..)` (title "Series · Episode", season/number from the info), `last_vod_category(kind)` / `remember_vod_category(kind, id)`, `resume_thresholds()` |

**Caching** is stale-while-revalidate: memory memo → encrypted store cache → provider. TTLs:
lists 6 h, VOD/series info 6 h, EPG 10 min (memory). An expired copy is returned at once and
refreshed in the background; a failed fetch falls back to the expired copy. When the
provider is unreachable at sign-in, the session opens **offline** from the cached login.
JSON (tens of MB for "All" lists), M3U parsing and catalogue builds run on the blocking pool,
never on the async workers; cache writes go through the store writer (not awaited).

**Item ids** for favourites/history: Xtream ids as strings; M3U channels use
`m3u:<hash>` (`catalog::m3u_key`) — **never URLs** (they carry credentials). Repeats of
the same entry (same `tvg-id`, title and group, e.g. backup streams) get `m3u:<hash>#2`,
`#3`, … in playlist order; unique entries keep their ids.

**Parental locks at the point of use** (SR-02). A request for an item of a locked category
returns `AppError::Locked(LockedItem { kind, item, category, category_name })`
(`err.locked()`). The UI flow: `category: Some(c)` → `PinPrompt` → `unlock_category(kind,
&c)` → retry. `category: None` means the item's categories aren't known yet (e.g. a
Continue-watching film whose list was never loaded; a cached look-up already started):
`session.verify_item(kind, &item).await`, then retry; if it is still unverified (offline),
`PinPrompt` → `unlock_item(kind, &item)` → retry. Unknown counts as locked while a PIN is
set (fail closed). Lists use `filter_unlocked`/`retain_unlocked` (Home rails, Favourites,
"More like this", zap lists); `screens::common::locks::is_item_locked` delegates to the
same rule. **Re-lock policy:** unlocks are never stored; they end with the session
(profile switch, restart) and expire after 30 min without user input
(`SessionManager::poll` reports input every frame); `relock_all()` locks at once.

### Real-provider scale (measured on a real panel — design for it)

- 85 live categories / 9 833 channels (All ≈ 3 s); **35 482 movies (VOD All ≈ 23 s, tens of
  MB)**; 5 871 series; one series with 10 seasons / 2 325 episodes.
- ⇒ **Movies/Series load per category by default**; "All" only lazily (with progress),
  cached. Home never fetches VOD/series "All": it uses `peek_cached(&vod_streams_key(None))`
  or the first few categories.
- ⇒ Every list is virtualised (`widgets::VirtualList` for rows, `widgets::VirtualGrid` for poster grids); search over large
  lists runs off the UI thread (in a `Loader`), incrementally, with `catalog::SearchIndex`.
- ⇒ **Per-row requests are bounded**: EPG (≤ 3 at a time) and artwork (≤ 6 downloads,
  ≤ 2 decodes) go through `services::fetch_queue::FetchQueue`: most recently drawn first,
  work for rows no longer drawn is dropped before it reaches the network. Paging 40 pages of
  Live TV against the slow fake panel sends ~10 EPG requests instead of one per row
  (`tests/bounded.rs`).
- The store cache cap (512 MiB default) fits a 30–60 MB list; keep "All" caching optional.
- Provider names often carry prefixes (`"FR | Title (2003)"`): use
  `catalog::split_tag(name)` to show the prefix as a small tag (`widgets::chip::tag`).
- Some EPG/titles contain decorative unicode; all provider text goes through `ui::text::cap`
  (it maps the few symbols our fonts lack); CJK, Arabic, emoji and superscripts render via
  the OS font fallback (`eframe` feature `system_fonts`).

### Images

`cx.services.images.get(url, ImageSize::{Logo, Poster, Card, Backdrop})` → `ImageSlot`
(`None | Loading | Ready(TextureHandle) | Failed`); call it every frame while the image is
drawn (that keeps its download wanted). http/https only, 10 MB cap, decode limits 4096 px /
64 MiB / 2 decodes at a time (SR-10), downscaled to the size class, memory LRU (192 MB) +
disk cache (`<data>/images`, PNG, trimmed to 80 % of 256 MB at startup and after every
16 MB written). Failures are retried after 5 min (bounded LRU). Artwork of locked
categories is registered private by the session (`mark_private`): memory only, cached
files deleted (SR-13); `clear_image_disk_cache()` wipes the disk cache;
`cancel_pending()` drops queued downloads; `queue_stats()`. An https image may follow a
redirect to http only when its URL carries no credential. Cards draw placeholders until
ready. Bundled art: `ui::assets::Asset` (`paint_cover`, `paint_contain`, `paint_exact`,
`forget`).

### Store rules

The store is synchronous and **must not be called on the UI thread** (fsync, keychain IPC,
Argon2). Reads: `Loader::start_blocking(..)` or `cx.services.read(|s| ..).await`. **Writes
go through one FIFO writer thread**, so they land in the order they were made (progress
saves, read-modify-write settings maps, preferences): `cx.services.blocking("what", |s| ..)`
to fire and forget (errors and panics become a toast), `cx.services.write("what", |s| ..)`
→ `WriteHandle<T>` to await a result. A read that must see earlier writes can go through
`write` too.

### Guide, catch-up and search (Phase 4B, `services/session/{guide,catchup}.rs`)

| API | Use |
|---|---|
| `guide_row(&channel)` → `RowState::{Loading, Missing, Ready(Arc<EpgRow>)}` | Non-blocking; call every frame for the rows **on screen**. Xtream: `get_simple_data_table` (else `get_short_epg`), bounded LRU (320 rows, 10 min), 3 requests at a time, requests for rows that scrolled away are dropped; the same cache and queue serve `epg`/`now_next` (a short now/next row is shown at once and upgraded to the full schedule). Expired rows stay visible while refreshing (and after a failed refresh). XMLTV profiles read the session guide. |
| `full_row(&channel).await` | The `get_simple_data_table` schedule (archive flags, server-local starts) for Catch-up. |
| `guide_uses_xmltv()`, `start_guide()`, `xmltv_status()`, `loaded_xmltv()` | XMLTV path: the profile's EPG URL override (keychain), else the M3U `url-tvg`. Parsed off-thread with `core::xmltv::parse_filtered` (window −36 h…+7 d, known channels only, short texts, ≤ 400 k programmes kept, ≤ 2 M looked at, 256 MiB decompressed, 1 MiB text nodes — SR-11); also feeds Live TV now/next. |
| `cached_guide_rows()` | Schedules already fetched (Search's programme results). |
| `server_time_zone()` | Panel zone for server-local strings / catch-up day tabs. |
| `timeshift_format()`, `saved_timeshift_format()`, `set_timeshift_format(f, persist)` | The format in use (a session choice after the fallback worked, else the profile's); `persist` saves it to the profile in the background. |
| `catchup_request_with(&channel, &programme, format, resume)` | Catch-up request with an explicit format (the one-time fallback). |
| `catchup::{Attempt, Fallback}` | Pure fallback policy: failure → offer the other format once → give up; success of the fallback → offer to save it. |

Other screens open Catch-up on a programme with `screens::catchup::open(cx, stream_id,
Some(start_ts), autoplay)` (Guide, Search, Home's Continue watching). Catch-up resume
points use the player's id `"<stream_id>@<start>"`.

## 6. The loader pattern (`task.rs`)

```rust
struct MoviesScreen { movies: Loader<Arc<Vec<VodStream>>>, category: Option<CategoryId> }

fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
    let Some(session) = cx.session() else { return };
    let cat = self.category.clone();
    self.movies.start(cx.rt(), &cx.egui, move |_emit| async move { session.vod_streams(cat).await });
}

fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
    self.movies.poll();
    match (self.movies.value(), self.movies.error()) {
        (Some(list), _) => { /* draw; the old value stays visible while reloading */ }
        (None, Some(err)) => { /* EmptyState with err.title() / err.user_message() + Retry */ }
        (None, None) if self.movies.show_skeleton() => { /* skeletons after 150 ms */ }
        _ => {}
    }
}
```

- Restarting a loader bumps its generation and aborts the old task: a slow old result can
  never overwrite a newer one (unit-tested).
- `emit.partial(v)` delivers an early value (e.g. cached) before the final one.
- A task that panics (or is dropped) without a result ends with `AppError::Internal`
  ("Something went wrong…"), never an endless spinner. `cx.services.spawn(..)` reports a
  panicking task as a background-error toast.
- `start_blocking(rt, ctx, || ..)` for store/keychain work.
- `AppError::user_message()` / `title()` give plain-language text; never show raw errors.

## 7. Playback

One mpv player and one video view, shared by the Live preview and the fullscreen player
(`cx.playback`). Build a `PlaybackRequest` with the session (it validates the URL into a
`PlayableUrl`, applies the profile User-Agent and the M3U `#EXTVLCOPT` UA/Referrer — the
Referrer only for the stream's own host), then:

```rust
match session.movie_request(id, ext.as_deref(), &title, Some(subtitle), poster, resume) {
    Ok(req) => cx.play(req, true),                 // pushes Route::Player(PlayerRoute::Request(..))
    Err(e) => cx.toasts.error(e.user_message()),
}
```

- `PlaybackRequest { kind: Live|Movie|Episode|Catchup, url, title, subtitle, resume_from,
  item (ids for progress), artwork, headers, zap: Option<ZapContext>, duration_hint }`.
- **One stream at a time** (providers count every open stream, `max_connections` is often 1–3,
  and exceeding it gets HTTP 403 on *everything* for a while): the controller stops the
  previous stream before loading, zapping is debounced (`play_after`, 450 ms), never open a
  second player. Multiscreen stays within the connection budget ([§7.4](#74-multiscreen)).
- Stream health is pure and unit-tested (`playback/tracker.rs`): a live stream whose
  position stops advancing for 10 s (20 s before its first progress) is stopped and
  reconnected ("Stream stalled — reconnecting…" via `cx.playback.reconnect_message()`);
  reconnects back off 2, 5, 10, 30, 30 s, then give up with a message. One reconnect is
  pending at a time; the counter resets after 30 s of continuous progress; a live stream
  the server ends cleanly after ≥ 10 s of playback reconnects at the first delay. mpv
  events are tied to their load (`LoadGuard`). After a stream failure on
  Xtream the controller checks `account_info()`; `cx.playback.notice()` then says
  "Connection limit reached (3/3)…". Catch-up failures say "Catch-up isn't available for
  this programme right now" (some panels answer 503 to every timeshift format).
- `open_stream` is the only place a provider stream is opened and `end_stream` the only
  place an item's stream ends for good (finished, failed/gave up, stopped, replaced).
- VOD end of file = **finished** only near the end (≥ 95 % or < 30 s left); earlier it is a
  dropped connection and resumes from the last position. Progress is saved every 10 s, on
  stop and synchronously at exit; `cx.playback.progress_epoch()` changes when a save lands
  (reload "continue watching" when it does).
- **Next episode** (`playback/next.rs`): when an episode starts, the controller looks up its
  series (cached); during its last 20 s `up_next_soon()` shows `UpNextCard` over the video
  (the next episode starts when it ends unless the card is hidden); once it finishes,
  `cx.playback.up_next()` offers the following episode (store `next_episode` order) and
  `play_up_next()` plays it. The player shows `widgets::UpNextPrompt` (10 s countdown; Esc
  stops it).
- **Zap banner** (`widgets::zap_banner`): every live channel change raises
  `cx.playback.banner()` for 3 s (also "First channel"/"Last channel": zapping stops at the
  ends of the list); the last frame stays up under a scrim while the next channel or a
  reconnect opens (`VideoView::set_hold_last_frame`).
- Every load starts with automatic audio and subtitles off; the audio/subtitle language the
  user picks (track sheet, A/S) is reused for later streams (`choose_audio`,
  `choose_subtitle`).
- Live OSD: LIVE badge + EPG programme progress, **never a scrubber** (mpv reports a
  growing duration on live TS). Always show the catalogue title, never mpv's `media-title`.
- Aspect/zoom presets persist (prefs + mpv global options).
- **Buffering per stream** (`playback/tuning.rs`): every load carries a typed
  `StreamTuning` (`Player::load_with`, sent as `loadfile` per-file options). Live: a short
  start buffer (1 s with Auto; channels that ran dry get 3 s, then 6 s, remembered per
  profile and channel in `app.channel_buffering`, capped at 300) and a seekable cache.
  Films/episodes/catch-up: 3 s start buffer and a large cache ahead (1 GiB on disk with
  Auto). Caches go to `<data>/cache/player` (emptied at startup, one file per stream,
  deleted when it closes); `--ephemeral` without `--data-dir` keeps them in memory, capped.
  Settings › Playback changes (hardware decoding, buffering) apply to what is playing
  (`set_tuning`); the start buffer and a new rewind size apply from the next stream.
- **libcurl reads `http(s)`** (streams and HLS segments) in this mpv build and has no read
  timeout: the controller's stall detection is the only remedy for a silent stream (see
  `docs/LIBMPV.md`). Films and HLS segments resume by themselves after network errors
  (Range retries); live TS never does.
- **Pause and rewind live TV** (`playback/timeshift.rs`): the cache behind and ahead of the
  position holds the "Rewind live TV" window (sized for 8 Mbit/s). "Behind live" counts
  what the viewer chose (pause, rewind), so the OSD says LIVE until then. A pause longer
  than the window moves the paused position forward (the stream keeps flowing) and says so
  on resume. Zapping starts at live; a reconnect while behind says the rewind buffer was
  lost (`take_messages` → toasts).
- **Held picture:** during a live zap or a reconnect the last picture stays, dimmed
  (`VideoView::set_hold_last_frame`), under "Buffering…" / "Reconnecting…".
- **Stream details** (D or the ⓘ button): resolution, fps, codecs, decoder, bitrates,
  buffer ahead / to rewind, download speed, dropped frames, reconnects, "2 of 3"
  connections, start buffer, extra buffering for this channel.

Channel keys (`playback/keys.rs`). In the fullscreen player they follow the TV remote:
**PageUp / Ctrl+↑ / media "next" = channel +1**, **PageDown / Ctrl+↓ / media "previous" =
channel −1**. On Live TV the list is on screen, so Ctrl+↓ means the **next row** (+1) and
Ctrl+↑ the previous row (−1).
Player keys (documented in `screens/player.rs`): Space play/pause (on live TV pausing
starts rewinding); with the OSD hidden ←/→ seek ∓10 s (Shift ∓60 s, the scrubber gets the
focus) on VOD, and on live TV move in the rewind buffer (the live bar gets the focus) or zap
−/+ when rewind is off; ↑/↓ zap +/− on live; L go live; D stream details; Z sleep timer
(§7.1); the channel keys above; digits = channel number entry (zaps after 1.5 s or
Enter; the current list, else All); M mute, F window fullscreen, I show/hide OSD, A/S next
audio/subtitle, +/− volume; Esc closes the track sheet or the stream details, then hides the
OSD, then leaves the player. Live TV: PageUp/PageDown **page the list**; Ctrl+↓/↑ (next/previous row) and
the media keys zap the preview; the same digits; M mute, F fullscreen, I channel banner.
Other features add OSD buttons with `screens::player::actions::osd_icon` and keys in the
marked blocks of `player.rs`.

### 7.1 Sleep timer (`playback/sleep.rs`, Phase 7)

- **Choices:** Off, 15/30/45/60/90 min, "End of this film/episode/recording" (VOD: the
  player's end of file) and "End of this programme (21:30)" (live, from the session's EPG
  now/next; hidden when unknown).
- **Setting it:** the moon button in the player's OSD (`sleep::osd_button`, a small menu
  with a focus trap; Esc closes it) or `Z` (`sleep::player_input`: cycles the choices and
  flashes "Sleep in 30 min"). While it is set, the moon button is highlighted and shows
  the time left ("28 min", "1 h 59").
- **Counting:** wall-clock time, while something plays in the main player or in
  multiscreen (`cx.multi.is_active()`). Nothing playing for 15 s (`IDLE_GRACE`, covers
  zapping and the next-episode countdown) turns it off quietly.
- **The last 30 s:** the mpv volume fades out (smoothstep; Settings: `SLEEP_FADE_OUT`,
  default on; the volume saved in the preferences is never touched) and the "Still
  watching?" prompt (§7.2) counts down with Stop now / **Keep watching**. Any other key,
  Esc or Keep watching restores the volume and turns the timer off ("Sleep timer off");
  that key is swallowed so it doesn't also zap or mute.
- **At zero:** restores the volume, `cx.playback.stop()`, `cx.multi.stop_all()`, leaves
  window fullscreen and goes back if the player screen is on top.
- The timing is the pure `sleep::Machine` (time injected, unit-tested).

### 7.2 App-wide prompts (`services/alerts.rs`, Phase 7)

`cx.alerts` is a small queue of `Prompt`s (title, body, 1–3 actions, an optional visible
`Countdown`, an optional expiry with its answer, a priority), keyed for de-duplication:
pushing a key that is already queued updates it. The most important prompt is drawn by
`alerts::show` (after the screen and the rail) as a card at the bottom right
(`ui::widgets::alert::AlertCard`: `surface-1`, r-lg, `e3`; above the OSD controls in the
player). Owners read what happened with `cx.alerts.take_answer(key)` on their next frame:
the action id, the cancel action for Esc, or the expiry answer.

Keyboard: a prompt that needs an answer (`.modal()`) takes the focus with `focus::trap`,
never from a text field (it waits until the field loses the focus). While it holds the
focus, `alerts::on_frame` (before the screen) routes ←/→/Tab, Enter/Space and
Esc/Backspace to it, so screen shortcuts (the player's arrows with the OSD hidden) can't
eat them. The focus goes back to where it was when the prompt closes, one frame later (a
trap holds the focus until the end of the frame it was drawn in).

### 7.3 Programme reminders and actions (`services/reminders.rs`, Phase 7)

- **Guide and Search:** Enter on a programme that is **on now** opens Watch / Record until
  it ends; on one **coming up**: Remind me / Switch to it when it starts / Record / Watch
  the channel now, with "Cancel reminder" / "Cancel recording" when set
  (`reminders::programme_menu`, a dialog reached through `guide::details_dialog`). *This
  replaces the old Enter behaviours "play the channel" (on now) and "details" (coming
  up)*; past programmes keep Catch-up or the details, and a row without a guide still
  plays the channel. "Record" is `dvr::schedule_programme` off the UI thread; the
  recording engine starts it. An active recording offers "Open Recordings" instead of
  cancelling (stopping a running job is the engine's).
- **Markers:** a bell (reminder) and a red dot (scheduled or recording) in Guide cells
  and Search programme rows (`reminders::paint_cell_marks` / `paint_row_marks`), from one
  list query per profile (reminders + live recordings not final), reloaded every 30 s and
  after every write made here, and updated optimistically at once.
- **Firing** (`alerts::on_frame` → `reminders::on_frame`, open profile only):
  `REMINDER_LEAD_SECS` (0/60/300, default 60) before the start, "Starting in 1 min" with
  "<title> on <channel>" and Dismiss / **Watch** (opens `PlayerRoute::LiveChannel`; replaces
  the player if it is on top). A "switch" reminder appears 10 s before the start
  ("Switching to <channel> in 10 s", Cancel / **Switch now**) so the switch lands on the
  start. Fired reminders are marked in the store. When the window isn't focused the
  taskbar button flashes (`ViewportCommand::RequestUserAttention`); the focus is never taken
  from another app. Nothing is shown when that channel already plays in the player.
- **Missed:** a reminder found more than 90 s late (the profile or the app was closed)
  whose programme is still on shows once, "Started 5 min ago" (it never switches on its
  own); reminders whose programme ended are pruned at every load. The decision is the pure
  `reminders::due` (unit-tested).
- **Settings:** `alerts::settings_rows(cx, ui)` (reminder lead time, sleep fade-out) for
  Settings › Recordings & downloads.

### 7.4 Multiscreen

Two to four live channels at once (`Route::Multiscreen`, `--route multiscreen`; Phase 7,
`playback/multi.rs` + `screens/multiscreen.rs`). It opens with
`screens::multiscreen::open(cx, replace)`: Live TV's preview column has a **Multiscreen**
button (an icon button on medium/TV), the player's live OSD a `SQUARES_FOUR` button (it
*replaces* the player, so Esc returns below it). Opening stops the main player (freeing its
`Watch` lease) and carries its live channel into tile 1.

| Rule | How |
|---|---|
| Budget | `multi::capacity(limit, holders)`: the limit minus what recordings hold (never preempted; downloads give way to viewing), at most 4. Below 2 it doesn't open: a toast says why (a direct `--route` shows a page). With a limit of 1: "raise Simultaneous streams in Settings › Playback". The screen shows the tiles, at least two slots (1×2, the rest empty "Add a channel" slots); past two tiles it adds an empty slot only while the user adds a channel ("Add channel" in the header, or A, which opens the picker; Esc cancels and the grid goes back), so two tiles stay side by side (`slot_count(tiles, capacity, adding)`). |
| Tiles | One `PlaybackController` each, built like the main one (the main player's hwdec, the user's volume, aspect and audio language) but with the **Small** buffer and `set_connection_purpose(Purpose::Tile(n))`. Each takes its own lease when its stream opens (`acquire_for_viewing`); a tile that gets none shows `Busy::message()`. Reconnects are per tile (the controller's own). |
| Staggered starts | No two tile streams open less than 0.8 s apart (`multi::next_start`), counting the main player's close and zaps (debounced 450 ms like the player). Real panels count a burst of opens as stale connections and then answer 403. |
| Sound | Only the last focused tile is audible (`multi::follow_focus`; focus on the empty slot or a header button keeps it); the others are muted. M mutes that one too. The audible tile has a speaker icon and the footer names it. |
| Layouts | `multiscreen/layout.rs` (pure): 1×2 for two slots, 2×2 for three or four (with a 3-stream budget the unused fourth cell says so), and *enlarged* (Enter on a playing tile: it goes big on the left, the others in a column). Tiles are 16:9 and every `VideoView` renders at its tile's size. |
| Channel picker | `multiscreen/picker.rs`: a modal (`Dialog`, focus trap) with the categories (All, Favourites, provider categories), a `VirtualList` of channels ("In tile 2" marks) and number entry. A locked category is unlocked in Live TV (with the PIN). |
| Parental control | A tile never reaches a channel of a locked category (this session's PIN unlocks count). `multi::LockedCategories` (from the session and the catalogue) feeds `multi::playable`, which builds every tile list: picker lists (in every category, not only All/Favourites), number entry, the carried channel (placed once the catalogue has loaded). `MultiView::place` refuses a locked channel and drops locked ones from a list it is given (`sanitize`); `zap`/`zap_to_number` skip or refuse locked targets (`step_playable`, `number_playable`). Tested in `tests/session.rs` against the fake panel's "Adult Night". |
| Lifecycle | Leaving the screen (or being covered) stops every tile and frees its connection (`MultiView::stop_all`, logged with the budget's `in_use`); coming back to a covered multiscreen restarts them, staggered. Tiles of another profile stop at the next `MultiView::update`. The idle mpv cores are kept for reuse, so no render context is ever freed outside a GL context; `MultiView::destroy(gl)` frees them in `on_exit`. The sleep timer and profile switches use `is_active()` / `stop_all()`. |

Keys: arrows move between the tiles (the sound follows) and the header buttons (**Change
channel**, **Remove**: they act on the last focused tile, outlined in champagne while the
focus is on a button); Enter: empty slot → picker, playing tile → enlarge / back to all
tiles, stopped or failed tile → play again; PageUp/PageDown channel ± in the focused tile
(its zap list); digits = channel number for the focused tile (its list, else all channels;
on the empty slot it adds that channel); M mute, F window fullscreen, I show/hide the channel
banners (they fade 3 s after the last key or channel change); Esc closes the picker, then the
enlarged view, then leaves multiscreen.

## 8. The focus system

One focus for keyboard, remote and mouse. Widgets register **once per frame**; the engine
moves the focus geometrically (nearest neighbour in the arrow's direction: same scope
first, then across scopes, restoring each scope's and region's last focus).

```rust
use crate::focus::{self, FocusOpts};

focus::scope(ui, Id::unique("movies.grid"), |ui| {
    for m in movies {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let (resp, f) = focus::focusable(ui, Id::unique(("movies.poster", m.stream_id)), rect,
                                         FocusOpts::default().autofocus_if(first));
        // paint: f.focused → ring now; f.anim (0..1, 160 ms) → 1.06 scale and glow
        if f.activated { cx.nav.push(Route::MovieDetail { stream_id: m.stream_id }); }
    }
});
```

Or with an existing response: `focus::register(ui, &response, FocusOpts::default())`.
All widgets in `ui::widgets` already do this.

| Need | API |
|---|---|
| Group widgets (list, rail, form) with memory | `focus::scope(ui, id, |ui| ..)` |
| Initial focus | `FocusOpts::autofocus()` (first registered wins); a fallback picks the top-left content item; a fallback focus is *provisional* and yields to an autofocus widget that appears later (data loaded) until the user presses a key |
| Move programmatically | `focus::request(ctx, id)` (scrolls it into view; may be not-yet-rendered for up to 3 frames) |
| Virtualised list/grid | `focus::take_nav(ctx, scope, Axis::Vertical)` → handle by index, `request` the next item id, or `give_back_nav(ctx, dir)` at a boundary. `widgets::VirtualList` and `widgets::VirtualGrid` do it (Up/Down by index, PageUp/PageDown, scroll-to-item) |
| Nested scroll areas | egui's inner `ScrollArea` swallows scroll targets: call `focus::forward_scroll(ui)` after an inner scroll area (Rail, VirtualList and VirtualGrid do) |
| Modals | `focus::trap(ctx, scope)` every frame (nested scopes included); `widgets::Dialog` does it. When a dialog closes, `focus::request` the button that opened it (otherwise the fallback picks the screen's autofocus) |
| Text input | `FocusOpts::text()` (Left/Right/Enter/Space go to the field; Right at the end / Left at the start move on); `widgets::TextField`/`SearchField` |
| Keys of your own | `on_input` + `ctx.input_mut(|i| i.consume_key(..))` (runs before the engine); check `focus::text_input_focused(ctx)` first |
| Queries | `focus::current`, `is_focused`, `is_in_scope`, `is_in_group`, `scope_memory`, `group_memory`, `pending_dir`, `clear` |

Rules and gotchas:
- Use `Sense::CLICK` (or `focusable`), **not** `Sense::click()`: egui-focusable widgets would
  run egui's own arrow navigation alongside ours.
- Ids must be stable across frames (derive from content ids, not positions).
- Hover moves the focus only when the pointer actually moves (and never onto text fields);
  harness runs (`--keys`/`--screenshot-*`) drop all real mouse/keyboard input, so a stray
  cursor or click on the machine cannot change the result.
- Draw the treatment exactly as `ui::paint::{focus_glow, focus_ring, focus_ring_circle}`;
  cards scale by `0.03 * f.anim` per side unless `motion::reduced(ctx)`.
- The side rail is the *shell* group; entering it lands on the current section. Esc on a
  root screen moves the focus to the rail. Only Left/Right cross between the rail and the
  page (Up/Down at the bottom of a page stay put).
- While a screen's content has nothing focusable yet (loading), the fallback does **not**
  park the focus on the rail; it waits for the content (so Home's hero button gets the
  initial focus), unless the user presses a key.

## 9. Dev/test harness

The harness flags (`--route`, `--keys`, `--screenshot-after/-out`, `--window-size`, `--ppp`,
`--exit-after`) are only parsed in builds with the **`harness` cargo feature** (SR-03):
build with `--features demo,harness` for reviews and agents. Release builds reject them.
`quit` (and `--exit-after`) wait until every requested screenshot is written, so
`shot:x.png,quit` is safe; put a short `wait:` before a `shot:` that should catch an
animation in its final state.

| Flag | Effect |
|---|---|
| `--demo` (feature `demo`) | runs `fake-panel` in-process and signs in to a demo profile (`demo`/`demo`); with `--ephemeral` also sets the parental PIN **1234** (locked: "Adult Night") |
| `--ephemeral` | in-memory database + in-memory keychain: never touches real data |
| `--data-dir <dir>` | isolated database, image cache and `logs/opensmarters.log` |
| `--route <route>` | open a route (after sign-in, or at once for `profiles`/`onboarding`; with profiles on the machine `onboarding`/`welcome` open the picker's PIN-gated "Add profile", and `detail:` of an item in a locked category asks for the PIN) |
| `--keys "<script>"` | comma-separated: key names (`Up Down Left Right Enter Space Esc Back Tab Shift+Tab PageUp PageDown 0–9 M F …`), `wait:<s>`, `text:<chars>`, `shot:<png>`, `gap:<s>` (delay after the following keys, default 0.18 s: `gap:0.05,Enter,shot:fade.png` catches a transition mid-way), `quit`; one key per frame |
| `--screenshot-after <s> --screenshot-out <png>` | screenshot then exit (feature `harness`, like the flags below except `--tv`) |
| `--window-size WxH` | inner size in **physical pixels** |
| `--ppp <f>` | force pixels-per-point (use `1` for 1:1 screenshots on a scaled display) |
| `--tv` | TV mode for this run |
| `--exit-after <s>` | quit after N seconds |
| `--log-stderr`, `--mpv-log <level>` | logging (`OPENSMARTERS_LOG` sets the filter) |

```powershell
$exe = "target/app/debug/opensmarters.exe"   # after: cargo build -p opensmarters --features demo,harness
& $exe --demo --ephemeral --route live --window-size 1920x1080 --ppp 1 `
  --keys "wait:4,Enter,wait:3,Down,Down,shot:live.png,PageDown,wait:2,shot:zapped.png" --exit-after 12
& $exe --demo --ephemeral --window-size 1920x1080 --tv --screenshot-after 6 --screenshot-out home-tv.png
```

The demo provider is plain HTTP on `127.0.0.1:<random>`; to test onboarding by keyboard run
`fake-panel --port 18765` and type `127.0.0.1:18765`, `demo`, `demo` (`expired`/`expired`,
`banned`/`banned` show the refusal messages).

Dev-only environment switches for `--demo`:

| Variable | Effect |
|---|---|
| `OPENSMARTERS_DEMO_QUIRKS=huge,timeshift-smarters-only` | fake-panel quirks (`slow` = 0–3 s per request, to see loading skeletons; `huge` = 50 000 channels; `timeshift-503` = every catch-up stream fails like the real panel; `timeshift-smarters-only` = only the Smarters start format plays, to exercise the fallback; `stall` = live `.ts` streams send 40 % of the clip then hang open, to exercise stall detection) |
| `OPENSMARTERS_DEMO_FAVOURITES=1` | seeds a few synthetic favourites (live, films, series) |
| `OPENSMARTERS_DEMO_XMLTV=1` | sets the demo profile's EPG URL override to the panel's `xmltv.xml.gz` (guide on the XMLTV path) |
| `OPENSMARTERS_DEMO_REMINDERS=notify:70,switch:25,missed:300` | sets synthetic reminders on the first demo channels, starting that many seconds from now (`missed:` = started that long ago), to see the prompts fire |
| `OPENSMARTERS_DEMO_SLEEP_SECS_PER_MIN=2` | a sleep-timer "minute" lasts 2 s (15 min = 30 s) |
| `OPENSMARTERS_DEMO_MAX_STREAMS=4` | sets the demo profile's "simultaneous streams" override (the demo account allows 2), e.g. to try multiscreen with 1, 3 or 4 tiles |
| `OPENSMARTERS_UPDATE_URL=fake?tag=v0.3.0` | **`harness` builds only**: the update check asks the demo panel's GitHub-shaped `/__updates/releases/latest` instead of GitHub (`fake` alone: this version with the minor bumped; `?status=429`, `?draft=true`, `?big=1`, `?delay_ms=8000` for the other states), or any full URL (https, or http to 127.0.0.1/localhost). Release builds always ask GitHub |

## 10. UI kit catalogue (`src/ui`)

Tokens, icons, paint and motion come from `crates/afterglow` (re-exported as `crate::ui::{theme, icons, paint, motion}`); change them there, and both the app and the installer follow.

- **Tokens** (`theme`): `color::*` (§4.1–4.3), `space::*`, `radius::*`, `stroke::*`,
  `elevation::{E1,E2,E3}`, `motion::*`, `font::{caption, caption_strong, overline, body,
  body_strong, button, title_sm, title, heading, display, display_sized, hero, numeric_xl,
  icon, icon_fill}`. Fonts: Source Sans 3 400/600/700 and Fraunces (opsz 72, wght 560, SOFT
  100, WONK 0) with variation coords; Phosphor Regular/Fill; egui defaults + OS fallback.
- **Icons** (`icons`): every Phosphor glyph as a constant (`icons::HOUSE`, …, generated from
  the official stylesheet), `icons::paint(painter, center, glyph, size, Style, color)`.
- **Paint**: `focus_glow`, `focus_ring`, `focus_ring_circle`, `shadow`, `h_gradient`/`v_gradient`
  (multi-stop), `scrim_left`, `scrim_bottom`, `scrim_osd_top/bottom`, `cover_uv`,
  `contain_rect`, `image_rounded`, `round_corners_mask`, `progress_bar`, `spinner`.
- **Text**: `cap`/`cap_paragraph` (sanitise provider strings), `job`/`galley`/`line`/`block`
  (truncate with ellipsis to N rows), `overline`, `timecode`, `duration_short`, `remaining`.
- **Widgets** (`ui::widgets`): `Button` (primary/secondary/ghost/danger, large/small, icon,
  trailing, `scroll_margin` to bring surroundings into view on focus), `IconButton` (round; `.primary()` for the OSD play), `Chip`, `Segmented`,
  `Toggle`, `Tabs`, `chip::tag`, `chip::ChipRow` (sideways chip row with edge fades), `TextField` (label, password + reveal, error; it lays out
  label + field vertically, so wrap it in `ui.vertical` inside a horizontal row),
  `SearchField`, `Callout` (+ `Callout::unencrypted()`), `Badge::{live, live_dot, pill}`,
  `PosterCard`, `BackdropCard`, `ChannelTile`, `ChannelLogo` (plate / monogram / fallback),
  `Avatar`, `Rail` (heading + carousel, paint allowance, memory, See all), `VirtualList`,
  `VirtualGrid` (row-virtualised card grid; `columns_for(width)`),
  `MenuRow`, `SettingRow`, `Dialog` (modal, focus trap; actions right-to-left, primary
  first), `Toasts`, `EmptyState` (+ an optional ghost `second_action`, read with `show_both`), `coming_soon`, `feedback::skeleton(_line)`, `feedback::skeleton_rail` + `rail_height` + `SkeletonCard` (a loading rail with the real `Rail` geometry), `PinPad`
  (fixed length to check a PIN; `PinPad::variable` to choose one), `PinPrompt` (loads the
  PIN state first so a lockout shows at once, verifies off-thread; dialog or inline;
  passes immediately when no PIN is set), `clock::clock`.
- **More widgets** (Phase 4A): `Dropdown` (menu with focus trap; close it from `on_back`),
  `EpisodeRow` (still, progress, "✓ Watched" pill; play glyph and watched toggle on the
  focused row only), `UpNextPrompt`.
- **VOD helpers** (`services::vod`): `display_title` (tag + trailing year split off),
  `release_year` (release date, else "(YYYY)"; never `added`), `movie_card_meta` /
  `series_card_meta`, `rating_label` (★ drawn with the icon font by `ui::text`),
  `VodSort`, season/episode labels, `next_episode_id`, `continue_target`, `youtube_id`.
- **Layout**: `Layout::{breakpoint, content, margin, poster_width, poster_size,
  backdrop_card_size, channel_tile_size, rail_width, safe_area, grid_columns}`.

## 11. Logging and security conventions

- `tracing` only; every line goes through `RedactingWriter` with the app-wide `Redactor`
  (profile passwords, usernames and M3U URLs are registered on sign-in). mpv lines are
  redacted again before logging. Never log a URL yourself unless it went through
  `cx.services.redactor.redact(..)`.
- Provider strings are plain text (egui never interprets markup) and are capped with
  `ui::text::cap` before layout. Images only via the image service. Media only via
  `PlayableUrl` (built by the session). No telemetry: besides the provider, the app talks
  only to TMDB (opt-in) and to GitHub when the user checks for updates or turns on the
  weekly check (`services/updates.rs`).
- Show `Callout::unencrypted()` wherever an `http://` account is edited or shown.

## 12. Who owns what (Phase 4)

| Agent | Files to create/replace | Notes |
|---|---|---|
| Movies / Series / Detail | `screens/movies.rs`, `screens/series.rs`, `screens/detail.rs` (+ helpers in new files) | per-category loading, `VirtualGrid` of `PosterCard`s, detail with resume/play-from-start/♥, seasons (`Tabs`), episodes, "next episode" (store `next_episode`) |
| Catch-up / Guide / Search / Favourites | `screens/catchup.rs`, `guide.rs`, `search.rs`, `favourites.rs` | `session.epg_table`, `archive_programmes`, `catchup_request`; EPG grid §6.9; search off-thread; favourites from the store snapshots |
| Parental / Settings | `screens/settings/parental.rs`, `storage.rs` (+ new pages) | PIN set/change/remove (`store.parental()`, off-thread), keywords, category locks, then `session.reload_parental()`; cache size/clear (`store.cache()`, `services::images::clear_disk_cache`) |

Shared files (`ui/widgets/*`, `services/*`, `focus/*`, `nav/*`, `app.rs`) are stable: add new
widgets in **new files** and new session methods at the end of `session.rs`; avoid
reformatting existing code so parallel work merges cleanly.

## 13. Known gaps

- Home: "Recently added" samples the first
  categories when the VOD "All" list isn't cached.
- The compact breakpoint (< 960 pt) uses the collapsed rail (no top bar); the window minimum
  keeps desktop use out of it.
- Window-state restore can't be exercised by the harness (it is off for `--ephemeral`, and
  non-ephemeral runs would touch the real keychain); it is unit-tested only.
- EPG for M3U profiles uses the first `url-tvg` only (or the profile's EPG URL override).
- Catch-up is Xtream-only: M3U `catchup*` attributes are parsed but no catch-up URL is
  built from `catchup-source` templates yet.
- Search finds programmes only in schedules already fetched (Guide rows, XMLTV); films and
  series in what is cached unless the user loads the full lists.
- The connection-limit check cannot see the HTTP status mpv got; it asks the account API.

## 14. Recordings & downloads (Phase 7, `src/dvr/`)

Films and episodes downloaded for offline viewing, live channels recorded now or on a
schedule, and clips saved from the player's buffer. The job list lives in the store
(`store.recordings()`, never a URL); files live in the recordings folder.

| Module | Role |
|---|---|
| `dvr/mod.rs` | Settings (`RECORDINGS_DIR`, `PAD_BEFORE`/`PAD_AFTER`, `DOWNLOADS_AT_ONCE`, `KEEP_FREE_GB`, `CLIP_MINUTES` → `DvrSettings`), `file_name`, `schedule_programme` (the Guide's "Record"), `record_until_stopped`, `queue_download`, and `cx.dvr` (`Dvr`): job snapshot, live progress, user actions (`queue`, `pause`, `resume`, `stop_recording`, `cancel`, `delete`) |
| `dvr/engine.rs` | One engine per open profile: supervisor (tick 4 s, woken by the UI and by freed connections), job registry with controls, start-up recovery |
| `dvr/download.rs`, `dvr/record.rs` | The jobs: film/episode download (Range resume), live recording (TS or HLS) |
| `dvr/source.rs` | Stream sources rebuilt from the profile; `PlaybackRequest`s for saved files |
| `dvr/files.rs`, `dvr/jobs.rs` | Safe names, `.part` writer, free-space guard, retry delays |
| `dvr/player.rs` | UI-thread work with the shared player: clips (`dump-cache`), recording through the player (`stream-record`), resume points of played recordings |
| `dvr/ui.rs` | Hooks for other screens (detail pages, player OSD, Live TV badge) and the DVR dialogs |
| `screens/recordings.rs` | Recordings screen: tabs Downloads / Recordings / Scheduled (scheduled recordings + reminders) |
| `screens/settings/recordings.rs` | Settings › Recordings & downloads |
| `core::download`, `core::hls` | Streaming downloader (timeouts, safe redirects, checked Range) and the defensive HLS playlist parser |

**Life cycle.** `dvr::on_session_opened` stops the previous profile's engine and starts
this one's; `Dvr::shutdown` (app exit) marks running jobs paused with
`engine::INTERRUPTED`. At start the engine re-queues those and `active` rows left by a
crash: downloads resume from their `.part`, live recordings continue in the same file if
their window is still open, otherwise what was recorded is kept. `dvr::on_frame` reloads
the snapshot when `Dvr::epoch()` changes (a row changed state); running jobs publish
progress in memory (`Dvr::progress`) and write it to the store at most every 2 s.

**Connections.** Downloads take a `Download` lease with `try_acquire` (oldest first, 1–2 at
once). Viewing may revoke it: the job stops at once, goes back to `queued` with
`engine::PREEMPTED`, and continues when a slot frees. Live recordings take a `Recording`
lease (never preempted); one that can't get a connection keeps trying until its end and
is listed by `Engine::waiting()`: if the user watches that very channel, `dvr::player`
records it through the player instead (same connection; changing channel stops it and
the row says so). An offline profile keeps everything waiting (`engine::OFFLINE`).

**Scheduling.** Rows keep the programme's start/end; the window is `start − PAD_BEFORE`
… `end + PAD_AFTER`. A programme already on air starts at once ("Record" in the player
schedules the current programme from the guide; without a guide it records "until you
stop", capped at `MAX_RECORDING`, no padding). A window that passed while the app was
closed is marked `engine::MISSED`.

**Files.** Names come from `dvr::file_name` (recordings, with the date) or
`files::download_name` (films and episodes), made unique against the database, final
files and `.part` files. A job writes `<name>.part` (created new, never overwriting),
checks the first bytes with `sniff_container` before keeping anything (an HTML page or
a playlist is deleted: "The provider sent something that isn't a video"), and renames
it at the end (downloads get the extension of what the file really is). Jobs stop when
free space drops below `KEEP_FREE_GB` (downloads pause, recordings keep what they have).
`--ephemeral` without `--data-dir` saves into a temporary folder, never the user's
Videos folder.

**Sources.** URLs are rebuilt when a job starts: `movie_request` / `episode_request`;
live recordings use the Xtream client's own URL builder with the `.ts` container when
the account allows it, else `live_request` (M3U entries). HLS is detected by the content
(`#EXTM3U`): best variant with muxed audio, live edge (last three segments), ordered
segments, fMP4 init sections; encrypted streams fail with a clear message.

**Playing saved files.** `dvr::ui::play_saved` builds the request off the UI thread
(`source::playback_request`, `PlayableUrl::from_owned_media`): films and episodes play
as `Movie`/`Episode` (resume point shared with streaming), live recordings and clips as
`Recording` (resume point `recordings.position_secs`, saved by `dvr::player`). Saved
files play when the provider is unreachable.

**Clips.** "Save clip" in the live OSD offers 1/5/15/30 min (the setting is focused
first) and calls `Player::dump_cache(time_pos − N min, time_pos)` into a new
`MediaOutput`. The buffer holds at most what played since the stream (re)started, so the
clip is cut to that and the toast says how long it is. The `Clip` row (done) is added when
`PlayerEvent::CommandDone` arrives (forwarded by `PlaybackController::take_dvr_events`).
Needs `PlayerConfig::seekable_cache(true)` (enabled in `PlaybackController::new`).

**Hooks in other screens** (one call each, logic in `dvr::ui`): film detail
(`movie_download`, `movie_status`), series detail (`episode_downloads`,
`split_episode_row`, `episode_button`, `season_button`, `episode_action`,
`detail_dialogs`), player (`player_buttons` inside the OSD for live, `player_overlay`
every frame: ● REC and the clip dialog), Live TV (`live_row_badge`). DVR dialogs close
on Esc/Backspace in `dvr::on_frame`, before the screen sees the key.

**Parental control.** Download (films, episodes, seasons) and Record go through
`dvr::ui::gate`: `Session::check_unlocked` for the item, else `UnlockFlow` (look-up of
unknown categories, then the PIN) and the action afterwards. Playing a saved file
(`play_saved`) checks the original item (`dvr::lock_target`: film; episode → its series;
recording/clip → its channel). The Recordings screen shows locked rows as "Locked" (no
title, no artwork; `dvr::is_row_locked`). Jobs themselves never check: their URLs come
from `Session::dvr_movie_url` / `dvr_episode_url` / `dvr_live_url`
(`services/session/dvr.rs`, the request builders' URL logic minus `check_unlocked`,
for `crate::dvr` only).

**Dev/test.** fake-panel quirks `slow-media` (≈ 256 KiB/s, Range still works),
`live-endless` (live `.ts` loops forever at its real bitrate), `media-html` /
`media-playlist` (a `200 OK` that isn't video), e.g.
`OPENSMARTERS_DEMO_QUIRKS=slow-media,live-endless`; `OPENSMARTERS_DEMO_RECORDINGS=1`
seeds two scheduled recordings (one on a locked "Adult Night" channel), a reminder and a
locked clip (`dvr::seed_demo`). Engine tests: `tests/dvr.rs`.
Routes: `recordings[:tab=downloads|recordings|scheduled]`, `settings:recordings`.
