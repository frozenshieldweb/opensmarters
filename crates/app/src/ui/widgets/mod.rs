// SPDX-License-Identifier: GPL-3.0-or-later
//! Reusable Afterglow widgets. Every interactive widget registers with the focus engine
//! (one focus, ring + glow + lift, Enter/click activation) and returns a
//! [`crate::focus::Focus`] (or a richer response).
//!
//! | Module | Widgets |
//! |---|---|
//! | [`button`] | [`Button`] (primary/secondary/ghost/danger, large), [`IconButton`] |
//! | [`chip`] | [`Chip`], [`Segmented`], [`Toggle`], [`Tabs`] |
//! | [`field`] | [`TextField`] (label, password reveal, error), [`SearchField`] |
//! | [`callout`] | [`Callout`] (warning/danger/info/success), [`Badge`] |
//! | [`card`] | [`PosterCard`], [`BackdropCard`], [`ChannelTile`] |
//! | [`logo`] | [`ChannelLogo`] (logo plate / monogram), [`Avatar`] |
//! | [`rail`] | [`Rail`] (horizontal carousel with paint allowance + focus memory) |
//! | [`list`] | [`VirtualList`] (virtualised, keyboard-scrolled), [`SettingRow`], [`MenuRow`] |
//! | [`grid`] | [`VirtualGrid`] (row-virtualised card grid) |
//! | [`dialog`] | [`Dialog`] (modal with focus trap) |
//! | [`toast`] | [`Toasts`] |
//! | [`feedback`] | skeletons, spinner, progress bar, [`EmptyState`], coming-soon placeholder |
//! | [`pin_pad`] | [`PinPad`] |
//! | [`clock`] | [`clock`](clock::clock) |
//! | [`dropdown`] | [`Dropdown`] (sort menu with focus trap) |
//! | [`episode_row`] | [`EpisodeRow`] (still, progress, title, synopsis, watched toggle) |
//! | [`up_next`] | [`UpNextPrompt`] (next-episode countdown) |

pub mod button;
pub mod callout;
pub mod card;
pub mod chip;
pub mod clock;
pub mod dialog;
pub mod dropdown;
pub mod episode_row;
pub mod feedback;
pub mod field;
pub mod grid;
pub mod list;
pub mod logo;
pub mod pin_pad;
pub mod rail;
pub mod toast;
pub mod up_next;

pub use button::{Button, ButtonKind, IconButton};
pub use callout::{Badge, Callout, CalloutKind};
pub use card::{BackdropCard, ChannelTile, PosterCard};
pub use chip::{Chip, Segmented, Tabs, Toggle};
pub use dialog::Dialog;
pub use feedback::{EmptyState, coming_soon};
pub use field::{SearchField, TextField, TextFieldResponse};
pub use grid::VirtualGrid;
pub use list::{MenuRow, SettingRow, VirtualList};
pub use logo::{Avatar, ChannelLogo};
pub use pin_pad::{PinPad, PinPadEvent};
pub use rail::Rail;
pub use toast::{ToastKind, Toasts};

pub use dropdown::Dropdown;
pub use episode_row::{EpisodeRow, EpisodeRowResponse};
pub use up_next::{UpNextAction, UpNextPrompt};
