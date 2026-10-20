# App architecture (`crates/app`)

This is the map of the desktop app for everyone who builds screens on top of it. Read
[DESIGN.md](DESIGN.md) for the look, [ENGINEERING.md](ENGINEERING.md) for the rules and
[BUILD_PROMPT.md](BUILD_PROMPT.md) §3 for the non-negotiable security requirements.

```powershell
# Run against the built-in synthetic provider (nothing touches your real data or keychain)
cargo run -p opensmarters --features demo -- --demo --ephemeral
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
| `src/screens/shell.rs` | Side rail (§6.1), `status_cluster` (clock, offline, unencrypted), `page_title` |
| `src/nav/` | `Route` (typed routes, `--route` syntax), `Router` (back stack), `Navigator` (`cx.nav`) |
| `src/focus/` | The spatial focus engine (`engine.rs`, pure + unit-tested) and its egui glue (`mod.rs`) |
| `src/ui/` | Design system: `theme` (tokens, fonts, visuals), `icons` (Phosphor), `paint` (focus ring/glow, scrims, cover-fit), `text` (capping, truncation, timecodes), `motion`, `layout` (breakpoints), `assets` (bundled art), `widgets/` |
| `src/services/` | `Services` (runtime, store, redactor, images, paths), `Session` (provider: catalogue, EPG, favourites, parental, playback requests), `catalog` (`LiveChannel`, `LiveCatalog`, `Programme`), `images`, `epg`, `prefs`, `demo` |
| `src/playback/` | `PlaybackController` (the one mpv player + video view, reconnect, progress), `PlaybackRequest` and header plumbing (`request.rs`) |
| `src/sessions.rs` | `SessionManager` (`cx.sessions`): open/switch profiles |
| `src/task.rs` | `Loader<T>`: background work with staleness protection |
| `src/logging.rs` | Redacted `tracing` (stderr + rotating log file) |
| `src/cli.rs`, `src/harness.rs` | Command line and the dev/test harness (scripted keys, screenshots) |
| `tests/session.rs` | Integration tests against the in-process `fake-panel` |

The crate is a binary **and** a library (`opensmarters_app`) so integration tests can drive
the session layer, router and focus engine.

## 2. The frame loop (`app.rs`)

Every frame, in order:

1. `raw_input_hook` — the harness injects scripted key events.
2. Zoom: TV mode sets `zoom = window_px / 1100` (§3), else `--ppp` or 1.
3. `sessions.poll()` — a finished profile open navigates to Home (+ pending `--route`).
4. `playback.update()` — mpv events, reconnect, stall detection, progress saves, debounced zaps.
5. Top screen's `on_input(cx, ctx)` — **screens claim their keys first** (digits, PageUp…).
6. `focus::begin_frame` — the engine consumes arrows/Enter/Space/Tab; Esc/Backspace = *back*.
7. Back: the top screen's `on_back` may consume it (close a dialog, hide the OSD); else the
   router goes back; on the root, the focus moves to the side rail.
8. Paint: background, the screen inside the content rect (focus group *content*), then the
   side rail on its own layer (focus group *shell*), then toasts.
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
| `cx.prefs` + `cx.save_prefs()` | TV mode, reduced motion, 12/24 h, aspect preset, volume |
| `cx.sessions` | open/switch profiles, last failure |
| `cx.layout` | breakpoint, `screen`, `content`, `margin`, `poster_width`, `tv`, card sizes, `safe_area()`, `grid_columns()` |
| `cx.now` | `DateTime<Utc>` for EPG progress |
| `cx.egui` | the `egui::Context` |

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

## 4. Navigation and routes

`--route` and `Route::parse` share one syntax: `home`, `live`, `live:category=<id|all|favourites>`,
`guide`, `movies[:category=<id>]`, `series[:category=<id>]`, `detail:movie=<id>`,
`detail:series=<id>`, `catchup[:stream=<id>]`, `search[:q=<text>]`, `favourites`,
`settings[:account|playback|profiles|parental|appearance|storage|about]`, `onboarding`,
`profiles`, `player:live=<channel id or number>`.

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
| Info | `profile()`, `profile_id()`, `kind()`, `is_xtream()`, `account()` (status, expiry, connections…), `login()`, `is_insecure()`, `is_offline()`, `user_agent()`, `allowed_live_containers()`, `live_container()`, `xtream()` (raw client), `playlist()` (M3U), `http()` |
| Catalogue | `live_catalog()` → `LiveCatalog` (channels + categories, M3U groups too), `vod_categories()`, `vod_streams(Option<CategoryId>)`, `series_categories()`, `series_list(Option<CategoryId>)`, `vod_info(id)`, `series_info(id)`, `epg_table(stream_id)` (catch-up), `peek_cached::<T>(key)` (cache only, no network) |
| EPG | `epg(&channel)` → `EpgState` (non-blocking, fetches in the background), `now_next(&channel, now)` |
| Requests | `live_request(&channel, Option<ZapContext>)`, `movie_request(..)`, `episode_request(..)`, `catchup_request(&channel, &ArchiveProgramme, resume)`, `resume_request(&PlaybackProgress)` |
| Favourites | `is_favourite(kind, id)`, `favourite_ids(kind)`, `toggle_favourite(kind, id, snapshot)` (optimistic, persisted in the background), `reload_favourites()` |
| Parental | `parental_enabled()`, `is_category_locked(kind, &id, name)`, `is_selection_locked(..)`, `unlock_category(..)` (session-only), `reload_parental()` (call after Settings → Parental changes) |
| Cache | `cached(key, ttl, fetch)` (generic SWR), `refresh()` (drop everything) |

**Caching** is stale-while-revalidate: memory memo → encrypted store cache → provider. TTLs:
lists 6 h, VOD/series info 6 h, EPG 10 min (memory). An expired copy is returned at once and
refreshed in the background; a failed fetch falls back to the expired copy. When the
provider is unreachable at sign-in, the session opens **offline** from the cached login.

**Item ids** for favourites/history: Xtream ids as strings; M3U channels use
`m3u:<hash>` (`catalog::m3u_key`) — **never URLs** (they carry credentials).

### Real-provider scale (measured on a real panel — design for it)

- 85 live categories / 9 833 channels (All ≈ 3 s); **35 482 movies (VOD All ≈ 23 s, tens of
  MB)**; 5 871 series; one series with 10 seasons / 2 325 episodes.
- ⇒ **Movies/Series load per category by default**; "All" only lazily (with progress),
  cached. Home never fetches VOD/series "All": it uses `peek_cached(&vod_streams_key(None))`
  or the first few categories.
- ⇒ Every list is virtualised (`widgets::VirtualList` for rows, `widgets::VirtualGrid` for poster grids); search over large
  lists runs off the UI thread (in a `Loader`), incrementally, with `catalog::SearchIndex`.
- The store cache cap (512 MiB default) fits a 30–60 MB list; keep "All" caching optional.
- Provider names often carry prefixes (`"FR | Title (2003)"`): use
  `catalog::split_tag(name)` to show the prefix as a small tag (`widgets::chip::tag`).
- Some EPG/titles contain decorative unicode; all provider text goes through `ui::text::cap`
  (it maps the few symbols our fonts lack); CJK, Arabic, emoji and superscripts render via
  the OS font fallback (`eframe` feature `system_fonts`).

### Images

`cx.services.images.get(url, ImageSize::{Logo, Poster, Card, Backdrop})` → `ImageSlot`
(`None | Loading | Ready(TextureHandle) | Failed`). http/https only, 10 MB cap, 8192 px
dimension limit, downscaled to the size class, memory LRU (192 MB) + disk cache
(`<data>/images`, PNG, trimmed at 256 MB). Cards draw placeholders until ready. Bundled art:
`ui::assets::Asset` (`paint_cover`, `paint_contain`, `paint_exact`, `forget`).

### Store rules

The store is synchronous and **must not be called on the UI thread** (fsync, keychain IPC,
Argon2). Use `Loader::start_blocking(..)` to read and `cx.services.blocking("what", |s| ..)`
for fire-and-forget writes.

### Guide, catch-up and search (Phase 4B, `services/session/{guide,catchup}.rs`)

| API | Use |
|---|---|
| `guide_row(&channel)` → `RowState::{Loading, Missing, Ready(Arc<EpgRow>)}` | Non-blocking; call every frame for the rows **on screen**. Xtream: `get_simple_data_table` (else `get_short_epg`), bounded LRU (320 rows, 10 min), 3 requests at a time, requests for rows that scrolled away are dropped. XMLTV profiles read the session guide. |
| `full_row(&channel).await` | The `get_simple_data_table` schedule (archive flags, server-local starts) for Catch-up. |
| `guide_uses_xmltv()`, `start_guide()`, `xmltv_status()`, `loaded_xmltv()` | XMLTV path: the profile's EPG URL override (keychain), else the M3U `url-tvg`. Parsed off-thread with `core::xmltv::parse_filtered` (window −36 h…+7 d, known channels only, short texts, ≤ 400 k programmes); also feeds Live TV now/next. |
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
  second player. Multiscreen (later) must stay within `max_connections`.
- Live reconnect: 2, 5, 10, 30, 30 s, then give up with a message. After a stream failure on
  Xtream the controller checks `account_info()`; `cx.playback.notice()` then says
  "Connection limit reached (3/3)…". Catch-up failures say "Catch-up isn't available for
  this programme right now" (some panels answer 503 to every timeshift format).
- VOD end of file = **finished** (store `mark_finished`). Progress is saved every 10 s and on
  stop; `cx.playback.progress_epoch()` changes when a save lands (reload "continue
  watching" when it does).
- Subtitles are **off** by default (mpv auto-selects embedded CC otherwise); a language the
  user picks in the track sheet is reused for later VOD (`choose_subtitle`).
- Live OSD: LIVE badge + EPG programme progress, **never a scrubber** (mpv reports a
  growing duration on live TS). Always show the catalogue title, never mpv's `media-title`.
- Aspect/zoom presets persist (prefs + mpv global options).

Player keys (documented in `screens/player.rs`): Space play/pause; with the OSD hidden
←/→ seek ∓10 s (Shift ∓60 s, the scrubber gets the focus) on VOD or zap on live, ↑/↓ zap on
live; PageUp/PageDown zap; digits = channel number entry (zaps after 1.5 s or Enter);
M mute, F window fullscreen, I show/hide OSD, A/S next audio/subtitle, +/− volume;
Esc closes the track sheet, then hides the OSD, then leaves the player. Live TV has the
same digits and PageUp/PageDown for the preview.

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
| Modals | `focus::trap(ctx, scope)` every frame (nested scopes included); `widgets::Dialog` does it |
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
  root screen moves the focus to the rail.

## 9. Dev/test harness

| Flag | Effect |
|---|---|
| `--demo` (feature `demo`) | runs `fake-panel` in-process and signs in to a demo profile (`demo`/`demo`); with `--ephemeral` also sets the parental PIN **1234** (locked: "Adult Night") |
| `--ephemeral` | in-memory database + in-memory keychain: never touches real data |
| `--data-dir <dir>` | isolated database, image cache and `logs/opensmarters.log` |
| `--route <route>` | open a route (after sign-in, or at once for `profiles`/`onboarding`) |
| `--keys "<script>"` | comma-separated: key names (`Up Down Left Right Enter Space Esc Back Tab Shift+Tab PageUp PageDown 0–9 M F …`), `wait:<s>`, `text:<chars>`, `shot:<png>`, `quit`; one key per frame with a 0.18 s gap |
| `--screenshot-after <s> --screenshot-out <png>` | screenshot then exit |
| `--window-size WxH` | inner size in **physical pixels** |
| `--ppp <f>` | force pixels-per-point (use `1` for 1:1 screenshots on a scaled display) |
| `--tv` | TV mode for this run |
| `--exit-after <s>` | quit after N seconds |
| `--log-stderr`, `--mpv-log <level>` | logging (`OPENSMARTERS_LOG` sets the filter) |

```powershell
$exe = "target/app/debug/opensmarters.exe"   # after: cargo build -p opensmarters --features demo
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
| `OPENSMARTERS_DEMO_QUIRKS=huge,timeshift-smarters-only` | fake-panel quirks (`huge` = 50 000 channels; `timeshift-503` = every catch-up stream fails like the real panel; `timeshift-smarters-only` = only the Smarters start format plays, to exercise the fallback) |
| `OPENSMARTERS_DEMO_FAVOURITES=1` | seeds a few synthetic favourites (live, films, series) |
| `OPENSMARTERS_DEMO_XMLTV=1` | sets the demo profile's EPG URL override to the panel's `xmltv.xml.gz` (guide on the XMLTV path) |

## 10. UI kit catalogue (`src/ui`)

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
  trailing), `IconButton` (round; `.primary()` for the OSD play), `Chip`, `Segmented`,
  `Toggle`, `Tabs`, `chip::tag`, `TextField` (label, password + reveal, error),
  `SearchField`, `Callout` (+ `Callout::unencrypted()`), `Badge::{live, live_dot, pill}`,
  `PosterCard`, `BackdropCard`, `ChannelTile`, `ChannelLogo` (plate / monogram / fallback),
  `Avatar`, `Rail` (heading + carousel, paint allowance, memory, See all), `VirtualList`,
  `VirtualGrid` (row-virtualised card grid; `columns_for(width)`),
  `MenuRow`, `SettingRow`, `Dialog` (modal, focus trap; actions right-to-left, primary
  first), `Toasts`, `EmptyState`, `coming_soon`, `feedback::skeleton(_line)`, `PinPad`,
  `clock::clock`.
- **Layout**: `Layout::{breakpoint, content, margin, poster_width, poster_size,
  backdrop_card_size, channel_tile_size, rail_width, safe_area, grid_columns}`.

## 11. Logging and security conventions

- `tracing` only; every line goes through `RedactingWriter` with the app-wide `Redactor`
  (profile passwords, usernames and M3U URLs are registered on sign-in). mpv lines are
  redacted again before logging. Never log a URL yourself unless it went through
  `cx.services.redactor.redact(..)`.
- Provider strings are plain text (egui never interprets markup) and are capped with
  `ui::text::cap` before layout. Images only via the image service. Media only via
  `PlayableUrl` (built by the session). No telemetry.
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

- Home: no backdrop crossfade on focus changes yet; "Recently added" samples the first
  categories when the VOD "All" list isn't cached.
- The compact breakpoint (< 960 pt) uses the collapsed rail (no top bar); the window minimum
  keeps desktop use out of it.
- Reduced motion does not follow the Windows "Show animations" setting yet (needs a
  platform call); it is a manual setting.
- EPG for M3U profiles uses the first `url-tvg` only (or the profile's EPG URL override).
- Catch-up is Xtream-only: M3U `catchup*` attributes are parsed but no catch-up URL is
  built from `catchup-source` templates yet.
- Search finds programmes only in schedules already fetched (Guide rows, XMLTV); films and
  series in what is cached unless the user loads the full lists.
- The connection-limit check cannot see the HTTP status mpv got; it asks the account API.
