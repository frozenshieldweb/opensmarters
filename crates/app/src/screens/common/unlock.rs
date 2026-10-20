// SPDX-License-Identifier: GPL-3.0-or-later
//! The flow after a request builder refused a locked item ([`AppError::Locked`]):
//!
//! 1. The item's categories are unknown (`LockedItem::category == None`): look them up
//!    with `Session::verify_item` (one cached request). Unlocked after all → retry.
//! 2. Still locked → the PIN ([`PinPrompt`]): `Session::unlock_category` for the locked
//!    category, or `Session::unlock_item` when it still can't be told → retry.
//!
//! Keep an [`UnlockFlow`] next to the action to retry, call [`UnlockFlow::show`] every
//! frame and close it from `Screen::on_back` (Esc cancels).
//!
//! ```ignore
//! match session.movie_request(..) {
//!     Ok(req) => cx.play(req, true),
//!     Err(AppError::Locked(l)) => self.unlock = Some((UnlockFlow::new(cx, l), Retry::Movie(id))),
//!     Err(e) => cx.toasts.error(e.user_message()),
//! }
//! // each frame:
//! if let Some((flow, retry)) = &mut self.unlock {
//!     match flow.show(cx) {
//!         UnlockResult::Unlocked => { let r = retry.clone(); self.unlock = None; self.retry(cx, r) }
//!         UnlockResult::Cancelled => self.unlock = None,
//!         UnlockResult::Pending => {}
//!     }
//! }
//! ```

use eframe::egui::Id;

use crate::error::{AppError, LockedItem};
use crate::screens::AppCtx;
use crate::task::Loader;
use crate::ui::text;
use crate::ui::widgets::{PinPrompt, PinPromptResult};

/// What the flow reports each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockResult {
    /// Checking, or waiting for the PIN.
    Pending,
    /// The item may be used now: retry the request.
    Unlocked,
    /// The user cancelled.
    Cancelled,
}

#[derive(Debug)]
enum Stage {
    /// `Session::verify_item` runs; `Some` = still locked (with what is known now).
    Verifying(LockedItem, Box<Loader<Option<LockedItem>>>),
    /// The PIN prompt for this lock.
    Pin(LockedItem, Box<PinPrompt>),
}

/// The unlock flow for one refused item. See the [module docs](self).
#[derive(Debug)]
pub struct UnlockFlow {
    stage: Stage,
}

impl UnlockFlow {
    /// Starts the flow for `locked` (looks the item up first when its categories are
    /// unknown).
    pub fn new(cx: &AppCtx<'_>, locked: LockedItem) -> Self {
        let Some(session) = cx.session().filter(|_| locked.is_unverified()) else {
            return Self::pin(locked);
        };
        let mut loader = Loader::new();
        let l = locked.clone();
        loader.start(cx.rt(), &cx.egui, move |_| async move {
            match session.verify_item(l.kind, &l.item).await {
                Ok(()) => Ok(None),
                Err(AppError::Locked(now)) => Ok(Some(now)),
                // The look-up failed (offline…): the PIN still unlocks this item.
                Err(_) => Ok(Some(l)),
            }
        });
        Self {
            stage: Stage::Verifying(locked, Box::new(loader)),
        }
    }

    fn pin(locked: LockedItem) -> Self {
        Self {
            stage: Stage::Pin(
                locked,
                Box::new(PinPrompt::new(Id::unique("screens.unlock.pin"))),
            ),
        }
    }

    /// Advances the flow and shows the PIN dialog when it is needed.
    pub fn show(&mut self, cx: &mut AppCtx<'_>) -> UnlockResult {
        if let Stage::Verifying(first, loader) = &mut self.stage {
            loader.poll();
            if !loader.has_finished() {
                return UnlockResult::Pending;
            }
            match loader.take() {
                Some(None) => return UnlockResult::Unlocked,
                Some(Some(now)) => *self = Self::pin(now),
                // The task failed: the PIN unlocks the item itself.
                None => *self = Self::pin(first.clone()),
            }
        }
        let Stage::Pin(locked, prompt) = &mut self.stage else {
            return UnlockResult::Pending;
        };
        let note = locked.category_name.as_deref().map(|n| {
            format!(
                "\u{201c}{}\u{201d} is locked by parental control.",
                text::cap(n, 40)
            )
        });
        let ctx = cx.egui.clone();
        match prompt.show_dialog(&ctx, cx.services, "Enter your PIN", note.as_deref()) {
            PinPromptResult::Verified(_) => {
                if let Some(s) = cx.session() {
                    match &locked.category {
                        Some(c) => s.unlock_category(locked.kind, c),
                        None => s.unlock_item(locked.kind, &locked.item),
                    }
                }
                UnlockResult::Unlocked
            }
            PinPromptResult::Cancelled | PinPromptResult::Forgot => UnlockResult::Cancelled,
            PinPromptResult::Pending => UnlockResult::Pending,
        }
    }
}
