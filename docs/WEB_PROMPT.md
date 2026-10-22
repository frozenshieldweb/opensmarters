# Prompt: opensmarters web, a self-hosted web edition

This is the plan and the contract for a **web edition** of opensmarters: a light version of
the Windows app that runs in the browser, served by one Docker container on the owner's
own server (deployed with [Dokploy](https://dokploy.com)). Read it with
[DESIGN.md](DESIGN.md) (the look), [SECURITY.md](SECURITY.md) (the security rules, which
still apply) and [ENGINEERING.md](ENGINEERING.md) (conventions).

## 1. Feasibility (investigated 2026-10-21)

**Yes, with a server in the middle.**

| Part of the Windows app | In the web edition |
|---|---|
| `crates/core`: Xtream client, M3U / XMLTV / HLS parsers, catalogue model and sorting, URL builders, log redaction | **Reused as is** (pure Rust, rustls, no native dependencies; runs on Linux) |
| `crates/fake-panel`: synthetic provider with generated media | **Reused** for the server's tests and its `--demo` mode |
| Afterglow design system (DESIGN.md tokens, Fraunces + Source Sans 3, Phosphor icons) and the landing page's DOM replica of the app (`site/styles.css`, `site/replica.js`) | **Reused** as the starting point of the web UI's styles |
| `crates/app` (egui UI), `crates/player` (libmpv), `crates/store` (Windows keychain), `crates/installer` | **Not reused.** The browser needs a DOM UI and its own `<video>` element. (egui can compile to WebAssembly, but a canvas UI handles video overlays, text input, screen readers and download size poorly.) |

What the browser can't do, and what the server therefore does:

1. **Reach the provider.** Panels send no CORS headers and are often plain `http://`; an
   `https://` page may not fetch them (mixed content). The server talks to the provider.
2. **Play the streams.** Browsers play HLS through Media Source Extensions (hls.js) or
   natively (Safari), with H.264 video and AAC/MP3 audio. Provider streams are MPEG-TS,
   MKV or HLS, often with AC-3/E-AC-3/DTS audio and sometimes HEVC video. The server
   **remuxes** every stream to HLS with ffmpeg: video copied, audio transcoded to AAC
   when it isn't AAC/MP3 (cheap). HEVC is passed through when the browser says it can play
   it (`MediaSource.isTypeSupported`), else transcoded to H.264 only if the instance allows
   it (CPU-heavy, off by default), else the player says the format isn't supported.
3. **Keep secrets.** The provider credentials (Xtream password, M3U URL) live on the
   server, encrypted at rest, and **never reach the browser**: stream, image and API URLs
   the browser sees are the server's own.

Costs the owner should know: every stream passes through the server (bandwidth: about
3–8 Mbit/s per HD viewer), audio transcoding costs a little CPU per stream, video
transcoding a lot, and each viewer uses one of the provider account's connections.

## 2. Product decisions

- **A private instance.** The owner runs it for themselves and their household. There is
  **no public sign-up**: the first visit creates the administrator (with a one-time setup
  code from the server log), and the administrator invites the other people with one-time
  links. The web edition must never be offered as a public service that relays IPTV for
  strangers; the docs say so. (Open registration could come later, with abuse controls.)
- **Light edition.** In: sign-in, providers (Xtream Codes login or M3U URL with an optional
  EPG URL), Home, Live TV with now/next, a simple Guide, Movies, Series, Search,
  Favourites, resume and Continue watching, the player, Settings. Out: recording and
  downloads, multiscreen, pause and rewind live TV, sleep timer, reminders, parental
  control, category management, backups, the stream details overlay. Catch-up is optional
  (do it last if time allows; the URL builder exists in `crates/core`).
- **Feels like a desktop app.** A full-window single-page app with the Windows app's side
  rail and screens, keyboard and remote navigation everywhere (arrows, Enter, Esc /
  Backspace back, the same player keys), the Afterglow focus ring, no page scrolling of the
  shell (lists scroll inside their panels), no browser-looking elements, smooth Afterglow
  motion that respects `prefers-reduced-motion`, and a **PWA manifest** so Chrome and Edge
  offer "Install app" and it opens in its own window. It works with the mouse and on a
  phone or tablet too, but the 16:9 desktop/TV layout comes first.
- **The landing page.** The instance's front page (signed out) presents the product in
  the Afterglow style with a **Sign in** panel, and links to the Windows download page and
  the source. It is not a sign-up page.
- Brand rules (LAUNCH_PROMPT §0): lowercase "opensmarters", never "Smarters" alone, one
  factual non-affiliation line in the footer.

## 3. Architecture

```
browser ──https──▶ Traefik (Dokploy) ──▶ opensmarters-web container :8080
                                          ├─ / , /app/*  static SPA (embedded in the binary)
                                          ├─ /api/*      JSON API (axum)
                                          ├─ /api/img/*  image proxy (signed, cached)
                                          └─ /api/streams/<sid>/*  HLS from ffmpeg
                                               │
                                               └─ provider panel / M3U host (internet)
```

- **Server:** a new crate `crates/web` (binary `opensmarters-web`), Rust, axum + tokio,
  SQLite (rusqlite, bundled) for its own data, `opensmarters-core` for everything
  provider-related, argon2 (passwords), chacha20poly1305 (secrets at rest), the built UI
  embedded at compile time (e.g. `rust-embed`) so the image ships one binary. It is part of
  the workspace and passes the same gates (fmt, clippy `-D warnings`, tests, `cargo deny`).
  `#![forbid(unsafe_code)]`.
- **UI:** a new folder `web/`: Vite + TypeScript (strict) + Preact + `@preact/signals`,
  hls.js for playback, no CDN (everything bundled; fonts and icons self-hosted). Builds to
  `web/dist`, which the server embeds.
- **Streaming pipeline (per playback):** the server builds the provider URL from the
  stored credentials, fetches it **itself** with an SSRF-safe client (§5), and pipes the
  bytes into `ffmpeg -i pipe:0` (for an HLS source, the server walks the playlist with
  `crates/core`'s HLS parser and pipes the segments). ffmpeg only ever reads stdin and
  writes HLS (`-f hls`, short segments, a sliding window for live, an event playlist for
  VOD) into a per-stream temp folder. The browser fetches
  `/api/streams/<sid>/index.m3u8` and its segments with its session cookie. A stream with
  no request for 30 s is stopped: ffmpeg is killed, the provider connection closed, the
  folder deleted. VOD seeking past what's transcoded restarts the pipeline at the new
  position (`-ss`); resume works the same way.
- **Limits:** one active stream per user by default (`OPENSMARTERS_STREAMS_PER_USER`), a
  global cap (`OPENSMARTERS_MAX_STREAMS`), and the provider account's own limit
  (`max_connections`): starting a new stream for the same user stops their previous one
  first (zapping must not leak provider connections; remember the real provider counts a
  closed connection for 1–3 minutes).
- **Demo mode:** `opensmarters-web --demo` starts the fake panel in-process and creates a
  demo user, like the Windows app's `--demo`, so the UI can be built and screenshotted
  without a real provider.

## 4. API contract (`/api`, JSON, UTF-8)

All endpoints except `GET /api/status`, `POST /api/setup`, `POST /api/login` and `POST
/api/invites/accept` need the session cookie. Every non-GET request must carry the header
`X-Opensmarters: 1` (CSRF guard, with `SameSite=Strict` cookies). Errors are
`{"error": {"code": "<snake_case>", "message": "<plain English for the user>"}}` with a
fitting status (400, 401, 403, 404, 409, 429, 502 provider error, 503 limit reached).
Timestamps are Unix seconds (UTC). Ids are strings. Lists that can be long are returned
whole (the UI virtualises them) unless noted.

### Session and users
| Method, path | Body → response |
|---|---|
| `GET /api/status` | → `{version, setup_required: bool, user: null \| {id, name, is_admin}}` |
| `POST /api/setup` | `{setup_code, name, password}` → `{user}`; only while no user exists; sets the session |
| `POST /api/login` | `{name, password}` → `{user}`; 401 `bad_credentials` (same message for any reason); 429 after repeated failures |
| `POST /api/logout` | → `{}` |
| `POST /api/password` | `{current, new}` → `{}` |
| `GET /api/admin/users` (admin) | → `[{id, name, is_admin, disabled, created_at, last_seen_at}]` |
| `POST /api/admin/invites` (admin) | `{}` → `{url, expires_at}` (one use, 72 h) |
| `POST /api/admin/users/{id}/disable` / `enable` / `DELETE /api/admin/users/{id}` (admin) | → `{}` (an admin can't remove themselves) |
| `POST /api/invites/accept` | `{token, name, password}` → `{user}`; sets the session |

Passwords: at least 10 characters. Names: 2–32 characters, letters, digits, `.`, `_`, `-`.

### Provider accounts (each user has their own)
| Method, path | Body → response |
|---|---|
| `GET /api/accounts` | → `[Account]` |
| `POST /api/accounts` | `{name, kind: "xtream", server, username, password}` or `{name, kind: "m3u", m3u_url, epg_url?}` → `Account` (the server checks it against the provider first: 400 `provider_rejected` / 502 `provider_unreachable` with a plain message) |
| `PATCH /api/accounts/{id}` | same fields, all optional → `Account` |
| `DELETE /api/accounts/{id}` | → `{}` |
| `POST /api/accounts/{id}/refresh` | → `Account` (drops the cached lists) |

`Account = {id, name, kind, host, status: "active"|"expired"|"banned"|"disabled"|"unknown", expires_at: number|null, max_connections: number|null, active_connections: number|null}`. `host` is the server's host name only (never a URL with credentials, never the M3U URL).

### Catalogue (`/api/accounts/{id}/…`)
| Path | Response |
|---|---|
| `GET …/live/categories` | `[{id, name, count}]` in provider order |
| `GET …/live/channels?category=<id>\|all\|favourites` | `[Channel]` where `Channel = {key, name, number: number\|null, logo: string\|null, categories: [id], archive_days: number\|null}` |
| `GET …/epg/now?keys=<k1,k2,…>` (≤ 200 keys) | `{"<key>": [Programme, Programme]}` (now and next) |
| `GET …/epg/channel/{key}?from=<ts>&to=<ts>` (≤ 48 h) | `[Programme]` where `Programme = {start, end, title, description: string\|null}` |
| `GET …/movies/categories`, `…/series/categories` | `[{id, name}]` (Xtream only; M3U accounts: `[]`) |
| `GET …/movies?category=<id>\|all` | `[Movie]` where `Movie = {id, name, poster: string\|null, year: number\|null, rating: number\|null, added: number\|null}` |
| `GET …/movies/{id}` | `{id, name, poster, backdrop, year, rating, duration_secs, genre, plot, cast, director, progress: {position, duration}\|null}` |
| `GET …/series?category=<id>\|all` | `[Series]` (`{id, name, poster, year, rating, updated}`) |
| `GET …/series/{id}` | `{id, name, poster, backdrop, year, rating, genre, plot, cast, seasons: [{number, name, episodes: [{id, number, title, plot, duration_secs, still, progress}]}]}` |
| `GET …/search?q=<text>` (≥ 2 chars) | `{channels: [Channel], movies: [Movie], series: [Series]}` (≤ 50 each) |
| `GET …/favourites` | `{channels: [Channel], movies: [Movie], series: [Series]}` |
| `PUT` / `DELETE …/favourites/{kind}/{item}` (`kind`: `channel`, `movie`, `series`) | → `{}` |
| `GET …/continue` | `[{kind: "movie"\|"episode", id, series_id: string\|null, name, poster, position, duration, updated}]` newest first, ≤ 20 |
| `PUT …/progress` | `{kind: "movie"\|"episode", id, series_id?, position, duration}` → `{}` (finished items, ≥ 95 %, leave Continue watching) |

Text from the provider is returned as plain text (names and descriptions capped, control
characters removed). **Every image URL is the server's** (`/api/img/<signed token>`), never
the provider's.

### Playback
| Method, path | Body → response |
|---|---|
| `POST /api/play` | `{account, kind: "live"\|"movie"\|"episode"\|"catchup", id, start?: seconds, catchup_start?: ts, catchup_minutes?: n, hevc: bool}` → `{stream: sid, url: "/api/streams/<sid>/index.m3u8", live: bool, duration: number\|null, start: number}`; 503 `limit_reached` ("You're already watching on another device" / "The provider allows N streams"), 502 `provider_error` with a plain message (403 from the panel: "Your provider refused the stream. Too many connections may be open; wait a minute and try again.") |
| `GET /api/streams/{sid}/{file}` | the HLS playlist or segment (owner only; `Cache-Control: no-store`) |
| `DELETE /api/streams/{sid}` | stops it → `{}` (the UI calls it when the player closes; the 30 s idle stop is the safety net) |

### Settings
| Method, path | Body → response |
|---|---|
| `GET` / `PUT /api/settings` | `{time_format: "24h"\|"12h", live_format: "ts"\|"m3u8", autoplay_next: bool}` (per user) |
| `GET /api/about` | `{version, source_url, transcode_video: "never"\|"hevc"\|"always", streams_per_user, max_streams}` |

## 5. Security rules (in addition to SECURITY.md §3)

- **Secrets at rest:** provider passwords, M3U URLs and EPG URLs are encrypted with
  XChaCha20-Poly1305 under a key derived from `OPENSMARTERS_SECRET_KEY` (32 random bytes,
  base64; the container refuses to start without it, or reads `OPENSMARTERS_SECRET_KEY_FILE`
  for Docker secrets), with the user id and account id as associated data. They're
  decrypted only to build a request and never logged (`opensmarters_core::Redactor`), never
  returned by the API, never put in a URL the browser sees.
- **Users:** argon2id password hashes; session tokens are 256-bit random, stored hashed,
  `HttpOnly; Secure; SameSite=Strict; Path=/`, 30-day sliding expiry, rotated at sign-in,
  all of a user's sessions ended on password change or disable; login rate limit per IP and
  per name with a growing delay; the setup code is single use and printed once to the log;
  invite tokens are single use, hashed, 72 h.
- **SSRF:** users type server and playlist addresses, and playlists and HLS manifests
  contain more addresses. Every outbound request (API, playlist, EPG, image, stream,
  segment, every redirect) goes through one client that allows only `http`/`https`,
  resolves the host itself and **refuses loopback, private, link-local, CGNAT, multicast,
  unspecified and the Docker host's addresses** (IPv4 and IPv6, including IPv4-mapped), with
  connect/read/total timeouts and size caps. An instance setting
  `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS=1` relaxes it for a provider on the owner's LAN, off
  by default.
- **ffmpeg** reads only `pipe:0` (`-protocol_whitelist pipe,file` for its output only),
  never a URL; it runs as the container's unprivileged user with a CPU-time-free but
  wall-clock watchdog, and its folder is under a temp dir the server owns.
- **Image proxy:** only URLs the server itself put in a response (an HMAC-signed token, so
  it isn't an open proxy), `image/*` only, ≤ 5 MB, decoded size checked, disk cache with a
  size cap and expiry, `Content-Security-Policy: default-src 'none'` on the response.
- **Web:** a strict CSP on the SPA (`default-src 'self'; img-src 'self' data: blob:;
  media-src 'self' blob:; connect-src 'self'; style-src 'self'; script-src 'self';
  frame-ancestors 'none'; base-uri 'none'; form-action 'self'`), no inline scripts,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, provider text always
  rendered as text (never `innerHTML` / `dangerouslySetInnerHTML`).
- **Isolation:** every query is scoped by the signed-in user; a user can never reach
  another user's accounts, favourites, progress or streams (tests prove it).
- **Behind a proxy:** client IPs for rate limiting come from `X-Forwarded-For` only when
  `OPENSMARTERS_TRUSTED_PROXY` is set (Traefik); the server listens on plain HTTP inside the
  container and relies on Traefik for TLS (documented); `Secure` cookies stay on.
- **Container:** non-root user, read-only root filesystem except `/data` and `/tmp`, no
  extra capabilities, a `HEALTHCHECK` through the binary's own `--healthcheck` flag (no curl
  in the image), `tini` as PID 1 so ffmpeg children are reaped.

## 6. UI (web/)

- **Design:** DESIGN.md is the contract; start from `site/styles.css` tokens (colours,
  type scale, radii, focus ring 3 px + 3 px gap + glow, motion). Fonts and Phosphor icons
  self-hosted (subset what's used, as the landing page does, or import the needed SVGs from
  `@phosphor-icons/core`). Dark only.
- **Shell:** side rail (Home, Live TV, Guide, Movies, Series, Search, Favourites, then the
  account switcher and Settings), clock, the current account's name; a compact layout below
  960 px (bottom bar or collapsed rail).
- **Focus engine:** spatial arrow-key navigation like `crates/app/src/focus` (pure, unit
  tested), Enter activates, Esc and Backspace go back (Backspace never navigates the
  browser history away from the app), focus restored when returning to a screen.
- **Screens:**
  - **Landing (signed out):** hero with the Afterglow art and the Sign in panel.
  - **First run (setup code) and Accept invite.**
  - **Add provider:** the choice of Xtream Codes or M3U playlist; plain explanations; the
    unencrypted-HTTP warning, as in the Windows app.
  - **Home:** Continue watching, Live favourites, Recently added movies and series.
  - **Live TV:** categories column (All, Favourites, then the provider's), a virtualised
    channel list with logos, numbers and now/next with progress, an inline preview, Enter to
    go fullscreen, PageUp/PageDown or Ctrl+↑/↓ to zap, digits for a channel number.
  - **Guide:** channels with their programmes for the next hours (a simple timeline).
  - **Movies / Series:** categories column, a virtualised poster grid (sort: provider order,
    name, recently added, rating), a detail page with Play / Resume / Favourite; seasons and
    episodes; next episode at the end.
  - **Search** across channels, movies and series; **Favourites**.
  - **Player:** fullscreen `<video>` with hls.js (native HLS on Safari), an OSD with title,
    time, progress (VOD), play/pause, ±10 s, audio and subtitle tracks when present,
    fullscreen (F), mute (M), and plain error messages (stream refused, format not
    supported, connection lost with automatic retry for live).
  - **Settings:** Providers (add, edit, remove, refresh, status and expiry), Playback (live
    format, time format, autoplay next episode), Account (change password, sign out),
    Users (admin: invite link, list, disable, remove), About (version, source link, what the
    server sends where).
- Loading skeletons, empty states and errors in plain English; no jargon.
- **Install as an app:** `manifest.webmanifest` (standalone, Afterglow colours, icons from
  `assets/launch/web`), and a minimal service worker that caches only the app shell (never
  API responses or video).

## 7. Deployment (Dokploy)

- `deploy/web/Dockerfile` (build context: the repository root): stage 1 `node` builds
  `web/`; stage 2 the Rust toolchain from `rust-toolchain.toml` builds
  `opensmarters-web --release --locked` with the UI embedded; stage 3 a slim Debian runtime
  with `ffmpeg`, `ca-certificates` and `tini`, user `opensmarters` (uid 10001), `/data`
  volume, `EXPOSE 8080`, `HEALTHCHECK`. A root `.dockerignore` keeps `target/`, `dist/`,
  `vendor/` and `node_modules/` out of the context.
- `deploy/web/compose.yaml`: the same service for Dokploy's Compose type or a local
  `docker compose up` (with the env vars and the `/data` volume).
- **docs/WEB_DEPLOY.md:** step by step on Dokploy (create the application from the GitHub
  repository, build type Dockerfile, Dockerfile path `deploy/web/Dockerfile`, domain and
  HTTPS through Traefik, environment variables, a volume mounted at `/data`, deploy, read
  the setup code in the logs, invite people, update by redeploying), sizing (bandwidth and
  CPU per stream), backups of `/data`, and the private-instance rule.
- CI (`.github/workflows/ci.yml`): build and test the UI (typecheck, unit tests, build),
  test `crates/web` on Linux, and build the Docker image (without pushing).

## 8. Definition of done

- All gates pass: the workspace gates (fmt, clippy both ways, tests, `cargo deny`) and the
  UI's (typecheck, lint, unit tests, production build).
- `crates/web` integration tests against `crates/fake-panel`: setup, login, invites,
  isolation between two users, add account (Xtream and M3U), every catalogue endpoint,
  favourites and progress, SSRF refusals (loopback, private, redirect to private, playlist
  pointing at a private address), the image proxy's signature and limits, and a real
  stream: `POST /api/play` on a fake channel produces a playable HLS playlist and segments
  through ffmpeg, and the idle stop kills ffmpeg and closes the provider connection.
- The UI checked in a real browser against `opensmarters-web --demo` with keyboard only, at
  1920×1080, 1280×720 and 390×844: every screen screenshotted and looked at, playback of a
  demo live channel and a demo movie with resume.
- The Docker image builds, starts with only the documented env vars, passes its health
  check, and serves the demo when run with `--demo`.
- Docs: WEB_DEPLOY.md, a "Web edition" section in README and APP_ARCHITECTURE (or a
  WEB_ARCHITECTURE.md), SECURITY.md §4.12 for the web edition's threats and mitigations,
  CHANGELOG under `## [Unreleased]`.
