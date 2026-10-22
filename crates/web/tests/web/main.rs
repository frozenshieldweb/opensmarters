// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests of `opensmarters-web` against `crates/fake-panel` (one binary; see
//! `common` for the helpers).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_debug_implementations
)]

mod accounts;
mod auth;
mod catalog;
mod common;
mod images;
mod ssrf;
mod streams;
