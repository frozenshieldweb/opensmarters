# Engineering conventions

These rules apply to every contributor, human or agent. Read [BUILD_PROMPT.md](BUILD_PROMPT.md) for the goals and the non-negotiable security rules, and [PROTOCOL.md](PROTOCOL.md) for wire behaviour.

## Workspace

| Crate | Package / lib | Role | `unsafe` |
|---|---|---|---|
| `crates/core` | `opensmarters-core` / `opensmarters_core` | Models, lenient Xtream deserialisers, Xtream client, M3U and XMLTV parsers, URL builders, catch-up formats, log redaction. No UI, no FFI. | forbidden |
| `crates/store` | `opensmarters-store` / `opensmarters_store` | SQLite persistence (profiles without secrets, favourites, resume points, caches), keychain wrapper, Argon2 PIN | forbidden |
| `crates/player` | `opensmarters-player` / `opensmarters_player` | Safe libmpv wrapper and egui video widget | allowed **only** here; every `unsafe` block needs a `// SAFETY:` comment |
| `crates/app` | `opensmarters` (bin) | eframe/egui desktop app | forbidden |
| `crates/fake-panel` | `fake-panel` (bin + lib) | Dev-only fake Xtream/M3U/XMLTV server with synthetic data and generated test streams | forbidden |
| `crates/afterglow` | `opensmarters-afterglow` / `afterglow` | The Afterglow design system for egui (tokens, fonts, icons, focus/gradient painting, motion), shared by the app and the installer | forbidden |

## Rules

- **Toolchain:** Rust stable 1.97, edition 2024.
- **Gates:** before you report done, run `cargo fmt`, then `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo test -p <crate>`. All must pass.
- **Errors:** use `thiserror` in libraries and `anyhow` only in binaries.
  - Do not `unwrap()` or `expect()` outside tests. Where a case is truly unreachable, say why in an `expect("…")` message.
  - Never panic on input that comes from a provider.
- **Logging:**
  - Use `tracing`. Never log credentials.
  - Never log a stream URL or API URL before it has gone through `opensmarters_core::redact`.
  - The player crate does not log URLs at all; it forwards mpv log lines as events, and the app redacts them.
- **Dependencies:**
  - Declare them in your own crate's `Cargo.toml`. Do not edit the root `Cargo.toml`; ask the coordinator if you need a workspace-level change.
  - Prefer well-maintained crates. Use `rustls` for TLS, never `native-tls`/OpenSSL.
- **Data:**
  - Tests and fixtures use **synthetic** data only.
  - Never use real provider names, hosts, channel lists or credentials.
  - Never copy anything from `_reference/`.
- **Parallel work:**
  - Several agents may build at once, so each uses its own target directory: `CARGO_TARGET_DIR=target/<crate-name>`.
  - Edit only your own crate (plus any files your task names).
  - Do not run `git commit`; the coordinator commits.
- **Docs:** each crate has crate-level rustdoc that explains its public API and how to use it. The next agent depends on it.
