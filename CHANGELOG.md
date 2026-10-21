# Changelog

All notable changes to opensmarters. Versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- **Check for updates** in Settings › About. It asks GitHub for the latest version number and, when there is a newer one, offers **Get the update** (opens the release page in your browser) or **Not now**. The app never downloads or installs anything: you run the new setup yourself, and it keeps your accounts, favourites and settings. A quiet **Update available** sign next to the clock leads to it. **Check automatically** (off by default) asks once a week. GitHub sees your IP address; nothing else is sent.

### Fixed
- **Multiscreen:** two channels now stay side by side (1×2) at full size. The grid only grows to 2×2 when you add a third channel, with the new **Add channel** button or the A key, and goes back if you cancel. Before, an empty "Add a channel" slot forced 2×2 as soon as two channels played.
- **The display no longer turns off during a film.** While video plays the display stays on (and the PC awake). While a recording or download runs, or a scheduled recording starts within 15 minutes, the PC stays awake but the display may turn off. Paused or stopped, Windows' normal power settings apply.

## [0.2.0] - 2026-10-21

The first public release.

### Watch
- **Live TV** with categories, All and Favourites, a virtualised channel list that stays smooth with 10,000+ channels, now/next with progress, an inline preview, a zap banner and channel-number entry.
- **Guide**: an EPG grid from the provider's API or XMLTV (including `url-tvg`).
- **Catch-up**: 14 days of archive, grouped by day on your clock, with a fallback to the other timeshift link format when a provider rejects the first.
- **Movies and Series**, loaded per category: detail pages, seasons, **resume**, **Continue watching** and **next episode**.
- **Search** across channels, films, series and cached programmes, and **Favourites** with reordering.

### Record and keep
- **Download** films and episodes to watch offline. Downloads resume after an interruption and step aside when you need the connection to watch something.
- **Record live TV** now, or on a schedule from the Guide or Search (TS and HLS streams).
- **Save the last minutes** of a live channel as a clip.
- A **Recordings** screen. Saved items follow your parental-control locks. They go to `Videos\opensmarters` by default, and uninstalling never touches them there.

### Smoother streams
- **Pause and rewind live TV.**
- Per-stream buffering that tunes itself to each channel, fewer stalls, and bounded reconnects.
- A **stream details** overlay: video and audio format, decoding, bitrate, buffer, download speed, dropped frames and reconnects.
- Everything respects your provider's connection limit: viewing can pause a download, never a recording.

### Comfort
- **Multiscreen** 1×2 and 2×2, with the sound following the focused tile.
- **Sleep timer**: stop after this episode, or after 30 or 60 minutes, with a fade and "Still watching?".
- **Programme reminders** from the Guide: get notified, or switch channel when it starts.
- Keyboard and remote first: spatial navigation everywhere, a TV mode (10-foot UI) and full mouse support.

### Private and secure
- A native UI (no web engine), a locked-down libmpv, and checked stream links.
- Passwords and playlist URLs stay in Windows Credential Manager, never in files or logs.
- A parental PIN (4–8 digits, Argon2-hashed and rate-limited) with whole-word category locks enforced everywhere.
- No telemetry and no accounts. Movie-database lookups are off by default.

### Install
- **`opensmarters-0.2.0-windows-x64-setup.exe`**: installs for your Windows account only (no administrator rights), with update, repair and uninstall from Windows Settings → Apps.
- **`opensmarters-0.2.0-windows-x64.zip`**: the portable version. Unzip it anywhere and run `opensmarters.exe`.
- Requires Windows 10 or 11 (x64) with a current graphics driver.

### Known limitations
- The builds aren't code-signed yet, so Windows SmartScreen may warn about an unknown publisher (*More info → Run anyway*).
- Recording, downloads and multiscreen were tested against the built-in synthetic provider. How many streams a real panel allows at once, and how quickly it frees them, varies by provider.
- Some providers refuse every catch-up link format; the app then says the programme isn't available.

[0.2.0]: https://github.com/frozenshieldweb/opensmarters/releases/tag/v0.2.0
