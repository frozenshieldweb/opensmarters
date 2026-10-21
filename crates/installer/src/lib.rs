// SPDX-License-Identifier: GPL-3.0-or-later
//! **opensmarters setup** (`crates/installer`): a one-click, per-user Windows installer
//! that also updates, repairs and uninstalls. Design and decisions:
//! [ADR 0002](../../docs/adr/0002-installer.md); spec: `docs/LAUNCH_PROMPT.md` Part B.
//!
//! The library holds all the logic so integration tests can drive it without a window:
//!
//! | Module | What |
//! |---|---|
//! | [`brand`] | every name shared with the app (product, folders, keychain service), in one place |
//! | [`env`] | [`env::Environment`]: every location we touch; `real()` or a test `sandbox(dir)` |
//! | [`pack`] / [`manifest`] / [`payload`] | the embedded xz payload, its SHA-256 manifest, verified extraction |
//! | [`paths`] | install-folder validation (system folders, network paths, drive roots, links) |
//! | [`version`] | version order and the install / update / repair / downgrade decision |
//! | [`install`] | the atomic install transaction with rollback |
//! | [`uninstall`] | removal of listed files only; opt-in data and password purge |
//! | [`registry`], [`shortcuts`], [`keychain`] | Apps & features entry, `.lnk` files, Credential Manager |
//! | [`fsops`], [`system`] | "in use" checks, listed-file removal, the lock, the temp-copy relaunch |
//! | [`cli`], [`run`], `ui` | the command line, the program flow, the egui window |
//!
//! Everything here is Windows-only; on other platforms the crate is empty and the binary
//! says so. `unsafe` is forbidden (workspace lint); the only `unsafe` the installer runs is
//! `opensmarters_player::harden_dll_search_path` (audited, in the player crate).
//!
//! **Testing without touching the machine:** build an [`env::Environment::sandbox`] in a
//! temp folder. It uses the test registry root `HKCU\Software\opensmartersTest\<id>` and a
//! file-backed keychain; the default build embeds a small synthetic payload.

#![forbid(unsafe_code)]

#[cfg(windows)]
pub mod brand;
#[cfg(windows)]
pub mod cli;
#[cfg(windows)]
pub mod env;
#[cfg(windows)]
pub mod fsops;
#[cfg(windows)]
pub mod install;
#[cfg(windows)]
pub mod keychain;
#[cfg(windows)]
pub mod log;
#[cfg(windows)]
pub mod manifest;
#[cfg(windows)]
pub mod pack;
#[cfg(windows)]
pub mod paths;
#[cfg(windows)]
pub mod payload;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod run;
#[cfg(windows)]
pub mod shortcuts;
#[cfg(windows)]
pub mod system;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
pub mod uninstall;
#[cfg(windows)]
pub mod version;
