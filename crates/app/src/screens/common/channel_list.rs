// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel lists for the screens that show live channels (Guide, Catch-up, Search; Live
//! TV does the same and can move here): channels of locked categories are left out
//! (every category of a channel counts, see [`super::locks`]), then the list is sorted.
//!
//! The lock check is a closure so the logic is testable without a session;
//! [`session_lock`] builds the real one.

use std::collections::HashMap;

use opensmarters_core::CategoryId;
use opensmarters_core::catalog::{SortOrder, sorted_indices};
use opensmarters_store::ContentKind;

use super::locks::any_locked;
use crate::services::Session;
use crate::services::catalog::{LiveCatalog, LiveChannel};

/// A "is this live category locked?" check for `session` (`None` while parental control
/// is off, so callers skip the work). Category names are looked up once, not per channel.
pub fn session_lock<'a>(
    session: &'a Session,
    cat: &'a LiveCatalog,
) -> Option<impl Fn(&CategoryId) -> bool + 'a> {
    if !session.parental_enabled() {
        return None;
    }
    let names: HashMap<&CategoryId, &str> = cat
        .categories
        .iter()
        .map(|c| (&c.id, c.name.as_str()))
        .collect();
    Some(move |id: &CategoryId| {
        session.is_category_locked(ContentKind::Live, id, names.get(id).copied().unwrap_or(""))
    })
}

/// True when `ch` sits in a category that `is_locked` says is locked.
pub fn is_channel_locked(ch: &LiveChannel, is_locked: impl Fn(&CategoryId) -> bool) -> bool {
    any_locked(&ch.categories, is_locked)
}

/// The channels of `picked` (in order) without the locked ones. `is_locked = None`
/// (parental control off, or the user unlocked the one category being shown) keeps all.
pub fn unlocked<'a>(
    picked: impl IntoIterator<Item = &'a LiveChannel>,
    is_locked: Option<&dyn Fn(&CategoryId) -> bool>,
) -> Vec<LiveChannel> {
    picked
        .into_iter()
        .filter(|c| is_locked.is_none_or(|f| !is_channel_locked(c, f)))
        .cloned()
        .collect()
}

/// The channels of `picked` that `session` doesn't lock, in order.
pub fn unlocked_in_session<'a>(
    session: &Session,
    cat: &LiveCatalog,
    picked: impl IntoIterator<Item = &'a LiveChannel>,
) -> Vec<LiveChannel> {
    match session_lock(session, cat) {
        Some(f) => unlocked(picked, Some(&f)),
        None => unlocked(picked, None),
    }
}

/// `channels` sorted by `order` (stable: equal keys keep their order).
pub fn sorted(channels: Vec<LiveChannel>, order: SortOrder) -> Vec<LiveChannel> {
    if order == SortOrder::Server {
        return channels;
    }
    let idx = sorted_indices(&channels, order);
    let mut slots: Vec<Option<LiveChannel>> = channels.into_iter().map(Some).collect();
    idx.into_iter()
        .filter_map(|i| slots.get_mut(i).and_then(Option::take))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::catalog::ChannelSource;

    fn ch(key: &str, number: Option<u64>, cats: &[&str]) -> LiveChannel {
        LiveChannel {
            key: key.to_owned(),
            number,
            name: format!("Channel {key}"),
            logo: None,
            epg_id: None,
            categories: cats.iter().map(|c| CategoryId::from(*c)).collect(),
            archive: false,
            archive_days: None,
            added: None,
            is_adult: false,
            source: ChannelSource::Xtream { stream_id: 1 },
        }
    }

    #[test]
    fn locked_in_any_category_is_left_out() {
        let list = [
            ch("a", Some(3), &["news"]),
            ch("b", Some(1), &["news", "adult"]),
            ch("c", None, &["kids"]),
            ch("d", Some(2), &[]),
        ];
        let lock = |id: &CategoryId| id.as_str() == "adult";
        let keys = |v: &[LiveChannel]| v.iter().map(|c| c.key.clone()).collect::<Vec<_>>();
        let kept = unlocked(&list, Some(&lock));
        assert_eq!(
            keys(&kept),
            ["a", "c", "d"],
            "b is also in a locked category"
        );
        assert_eq!(keys(&unlocked(&list, None)), ["a", "b", "c", "d"]);
        let by_number = sorted(kept, SortOrder::NumberAsc);
        assert_eq!(keys(&by_number), ["d", "a", "c"], "missing numbers last");
        assert_eq!(
            keys(&sorted(list.to_vec(), SortOrder::Server)),
            ["a", "b", "c", "d"]
        );
    }
}
