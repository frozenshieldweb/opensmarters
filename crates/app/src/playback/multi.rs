// SPDX-License-Identifier: GPL-3.0-or-later
//! Multiscreen players (stub). Phase 7 fills it in: up to four tile players, each with its
//! own connection lease (`Purpose::Tile(n)`), audio following the focus.

/// The multiscreen tiles (`cx.multi`). They live here, not in the screen, so the app can
/// free their GL resources at exit ([`MultiView::destroy`]).
#[derive(Debug, Default)]
pub struct MultiView {
    _todo: (),
}

impl MultiView {
    /// Frees the tiles' render contexts. Call with the GL context current (app exit).
    pub fn destroy(&mut self, _gl: &eframe::glow::Context) {}
}
