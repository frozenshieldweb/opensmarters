// SPDX-License-Identifier: GPL-3.0-or-later
//! `opensmarters-setup`: installs, updates, repairs and removes opensmarters for the
//! current Windows user. See the library docs (`opensmarters_installer`) and ADR 0002.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    let code = opensmarters_installer::run::main();
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("opensmarters-setup is a Windows program.");
    std::process::ExitCode::FAILURE
}
