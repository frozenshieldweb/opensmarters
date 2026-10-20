// SPDX-License-Identifier: GPL-3.0-or-later
//! Logic shared by several screens (no widgets of its own):
//!
//! | Module | What |
//! |---|---|
//! | [`locks`] | Parental locks over **every** category of an item (channels, films, series) |
//! | [`channel_list`] | Channel lists with locked categories left out, sorted (Guide, Catch-up, Search; Live TV can use it too) |
//! | [`profile_data`] | Deleting a profile together with its machine-local data (search history, remembered browse categories) |

pub mod channel_list;
pub mod locks;
pub mod profile_data;
