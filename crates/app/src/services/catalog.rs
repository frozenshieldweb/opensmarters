// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider-independent catalogue models used by the screens: live channels (Xtream or
//! M3U) and EPG programmes. Provider strings are capped here, once.

use std::collections::HashMap;
use std::fmt;

use opensmarters_core::catalog::{CatalogItem, CategorySelection};
use opensmarters_core::chrono::{DateTime, Utc};
use opensmarters_core::m3u::{EntryKind, M3uEntry, Playlist};
use opensmarters_core::{Category, CategoryId, EpgListing, LiveStream};

use crate::services::stable_hash;
use crate::ui::text::{MAX_LINE_CHARS, cap};

/// Where a live channel's stream comes from.
#[derive(Clone, PartialEq, Eq)]
pub enum ChannelSource {
    /// Xtream: the URL is built from `stream_id`.
    Xtream {
        /// `stream_id`.
        stream_id: u64,
    },
    /// M3U entry. **The URL may contain credentials**: never log or display it.
    M3u {
        /// Stream URL (secret).
        url: String,
        /// `#EXTVLCOPT:http-user-agent`.
        user_agent: Option<String>,
        /// `#EXTVLCOPT:http-referrer` (applied only for the stream's own host).
        referrer: Option<String>,
    },
}

impl fmt::Debug for ChannelSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelSource::Xtream { stream_id } => write!(f, "Xtream({stream_id})"),
            ChannelSource::M3u { .. } => f.write_str("M3u(<redacted>)"),
        }
    }
}

/// A live channel (one row of the Live TV list).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveChannel {
    /// Stable, non-secret id for favourites/recents: the Xtream `stream_id` as a string,
    /// or `m3u:<hash>` for playlist entries.
    pub key: String,
    /// Channel number (`num`, `tvg-chno`, or the playlist position).
    pub number: Option<u64>,
    /// Display name (capped).
    pub name: String,
    /// Logo URL (provider-controlled).
    pub logo: Option<String>,
    /// EPG channel id (`epg_channel_id` / `tvg-id`).
    pub epg_id: Option<String>,
    /// Categories (Xtream ids, or the M3U group title).
    pub categories: Vec<CategoryId>,
    /// Catch-up available (`tv_archive` / `catchup`).
    pub archive: bool,
    /// Catch-up depth in days.
    pub archive_days: Option<u32>,
    /// When added (Xtream).
    pub added: Option<DateTime<Utc>>,
    /// Flagged adult by the provider.
    pub is_adult: bool,
    /// Stream source.
    pub source: ChannelSource,
}

impl LiveChannel {
    /// The Xtream `stream_id`, if any.
    pub fn stream_id(&self) -> Option<u64> {
        match self.source {
            ChannelSource::Xtream { stream_id } => Some(stream_id),
            ChannelSource::M3u { .. } => None,
        }
    }

    /// From an Xtream live stream.
    pub fn from_xtream(s: &LiveStream) -> Self {
        let mut categories = Vec::new();
        if let Some(c) = &s.category_id {
            categories.push(c.clone());
        }
        for c in &s.category_ids {
            if !categories.contains(c) {
                categories.push(c.clone());
            }
        }
        LiveChannel {
            key: s.stream_id.to_string(),
            number: s.num,
            name: cap(&s.name, MAX_LINE_CHARS).trim().to_owned(),
            logo: s.stream_icon.clone().filter(|u| looks_like_http(u)),
            epg_id: s.epg_channel_id.clone().filter(|e| !e.trim().is_empty()),
            categories,
            archive: s.tv_archive,
            archive_days: s.tv_archive_duration,
            added: s.added,
            is_adult: s.is_adult,
            source: ChannelSource::Xtream {
                stream_id: s.stream_id,
            },
        }
    }

    /// From an M3U entry at `position` (0-based) in the playlist.
    pub fn from_m3u(e: &M3uEntry, position: usize) -> Self {
        let group = e.group_title.clone().filter(|g| !g.trim().is_empty());
        LiveChannel {
            key: m3u_key(e),
            number: e
                .tvg_chno
                .map(u64::from)
                .or_else(|| u64::try_from(position + 1).ok()),
            name: cap(
                e.tvg_name
                    .as_deref()
                    .filter(|_| e.title.trim().is_empty())
                    .unwrap_or(&e.title),
                MAX_LINE_CHARS,
            )
            .trim()
            .to_owned(),
            logo: e.tvg_logo.clone().filter(|u| looks_like_http(u)),
            epg_id: e.tvg_id.clone().filter(|t| !t.trim().is_empty()),
            categories: group.map(CategoryId::from).into_iter().collect(),
            archive: e.catchup.is_some() || e.catchup_days.is_some_and(|d| d > 0),
            archive_days: e.catchup_days,
            added: None,
            is_adult: false,
            source: ChannelSource::M3u {
                url: e.url.clone(),
                user_agent: e.user_agent.clone(),
                referrer: e.referrer.clone(),
            },
        }
    }

    /// Up to two alphanumeric characters for the monogram tile (§6.4).
    pub fn monogram(&self) -> String {
        monogram(&self.name)
    }
}

/// A provider name split into an optional short language/region tag and the rest:
/// `"FR | Le Titre (2003)"` → (`Some("FR")`, `"Le Titre (2003)"`). Recognises
/// `XX | rest`, `XX: rest`, `XX ┃ rest`, `[XX] rest` and `|XX| rest` with a 2–4 character
/// uppercase/digit tag containing a letter. Anything else is returned unchanged.
pub fn split_tag(raw: &str) -> (Option<&str>, &str) {
    let s = raw.trim();
    let is_tag = |t: &str| {
        (2..=4).contains(&t.chars().count())
            && t.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && t.chars().any(|c| c.is_ascii_uppercase())
    };
    // [XX] rest  /  |XX| rest
    for (open, close) in [('[', ']'), ('|', '|')] {
        if let Some(rest) = s.strip_prefix(open)
            && let Some(end) = rest.find(close)
        {
            let tag = rest[..end].trim();
            let name = rest[end + close.len_utf8()..]
                .trim_start_matches(['-', ':', ' '])
                .trim();
            if is_tag(tag) && !name.is_empty() {
                return (Some(tag), name);
            }
        }
    }
    // XX | rest  /  XX: rest  /  XX ┃ rest
    if let Some(pos) = s.find(['|', ':', '┃', '•']) {
        let tag = s[..pos].trim();
        let sep_len = s[pos..].chars().next().map_or(1, char::len_utf8);
        let name = s[pos + sep_len..].trim();
        if is_tag(tag) && !name.is_empty() {
            return (Some(tag), name);
        }
    }
    (None, s)
}

/// First 1–2 alphanumeric characters of `name`, uppercased ("Cairnwell" → "CA").
pub fn monogram(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().filter(|c| c.is_alphanumeric()).take(2) {
        out.extend(c.to_uppercase());
    }
    out
}

fn looks_like_http(u: &str) -> bool {
    let l = u.trim_start().get(..8).unwrap_or(u).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// The stable favourites id of an M3U entry: never the URL (it may carry credentials).
pub fn m3u_key(e: &M3uEntry) -> String {
    let id = e.tvg_id.as_deref().unwrap_or_default();
    let group = e.group_title.as_deref().unwrap_or_default();
    format!("m3u:{:016x}", stable_hash(&[id, &e.title, group]))
}

impl CatalogItem for LiveChannel {
    type Key = String;

    fn key(&self) -> String {
        self.key.clone()
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn number(&self) -> Option<u64> {
        self.number
    }
    fn added(&self) -> Option<DateTime<Utc>> {
        self.added
    }
    fn in_category(&self, category: &CategoryId) -> bool {
        self.categories.contains(category)
    }
}

/// All live channels of a profile plus their categories.
#[derive(Debug, Clone, Default)]
pub struct LiveCatalog {
    /// Channels in provider order.
    pub channels: Vec<LiveChannel>,
    /// Categories in provider order (M3U: groups in first-seen order).
    pub categories: Vec<Category>,
    counts: HashMap<CategoryId, usize>,
    by_key: HashMap<String, usize>,
}

impl LiveCatalog {
    /// Builds the index.
    pub fn new(channels: Vec<LiveChannel>, mut categories: Vec<Category>) -> Self {
        let mut counts: HashMap<CategoryId, usize> = HashMap::new();
        let mut by_key = HashMap::with_capacity(channels.len());
        for (i, c) in channels.iter().enumerate() {
            by_key.entry(c.key.clone()).or_insert(i);
            for cat in &c.categories {
                *counts.entry(cat.clone()).or_default() += 1;
            }
        }
        for c in &mut categories {
            c.name = cap(&c.name, MAX_LINE_CHARS).trim().to_owned();
        }
        Self {
            channels,
            categories,
            counts,
            by_key,
        }
    }

    /// From an Xtream category list and the full ("All") stream list.
    pub fn from_xtream(categories: &[Category], streams: &[LiveStream]) -> Self {
        let channels = streams.iter().map(LiveChannel::from_xtream).collect();
        Self::new(channels, categories.to_vec())
    }

    /// From a parsed M3U playlist (live entries only).
    pub fn from_playlist(p: &Playlist) -> Self {
        let channels: Vec<LiveChannel> = p
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Live)
            .enumerate()
            .map(|(i, e)| LiveChannel::from_m3u(e, i))
            .collect();
        let mut categories: Vec<Category> = Vec::new();
        for ch in &channels {
            for cat in &ch.categories {
                if !categories.iter().any(|c| &c.id == cat) {
                    categories.push(Category {
                        id: cat.clone(),
                        name: cat.as_str().to_owned(),
                        parent_id: None,
                    });
                }
            }
        }
        Self::new(channels, categories)
    }

    /// Channel by key.
    pub fn get(&self, key: &str) -> Option<&LiveChannel> {
        self.by_key.get(key).and_then(|&i| self.channels.get(i))
    }

    /// Index of a channel by key.
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.by_key.get(key).copied()
    }

    /// First channel with this number.
    pub fn by_number(&self, number: u64) -> Option<&LiveChannel> {
        self.channels.iter().find(|c| c.number == Some(number))
    }

    /// Number of channels in a category.
    pub fn count(&self, category: &CategoryId) -> usize {
        self.counts.get(category).copied().unwrap_or(0)
    }

    /// Category display name.
    pub fn category_name(&self, id: &CategoryId) -> Option<&str> {
        self.categories
            .iter()
            .find(|c| &c.id == id)
            .map(|c| c.name.as_str())
    }

    /// Indices of the channels matching a selection.
    pub fn select(
        &self,
        selection: &CategorySelection,
        favourites: &std::collections::HashSet<String>,
    ) -> Vec<usize> {
        self.channels
            .iter()
            .enumerate()
            .filter(|(_, c)| match selection {
                CategorySelection::All => true,
                CategorySelection::Favourites => favourites.contains(&c.key),
                CategorySelection::Category(id) => c.in_category(id),
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// One EPG programme, from Xtream short EPG or XMLTV.
#[derive(Debug, Clone, PartialEq)]
pub struct Programme {
    /// Title (decoded, capped).
    pub title: String,
    /// Description (decoded, capped).
    pub description: String,
    /// Start.
    pub start: DateTime<Utc>,
    /// End.
    pub stop: DateTime<Utc>,
    /// Archived (catch-up available).
    pub has_archive: bool,
}

impl Programme {
    /// From an Xtream listing.
    pub fn from_listing(l: &EpgListing) -> Self {
        Self {
            title: cap(&l.title, MAX_LINE_CHARS).trim().to_owned(),
            description: crate::ui::text::cap_paragraph(&l.description, 1200),
            start: l.start,
            stop: l.stop,
            has_archive: l.has_archive,
        }
    }

    /// 0..1 elapsed at `now`.
    pub fn progress(&self, now: DateTime<Utc>) -> f32 {
        let total = (self.stop - self.start).num_seconds().max(1) as f32;
        let done = (now - self.start).num_seconds() as f32;
        (done / total).clamp(0.0, 1.0)
    }

    /// On air at `now`.
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        self.start <= now && now < self.stop
    }
}

/// The programme on air at `now` and the one after it, from a sorted list.
pub fn now_and_next(
    list: &[Programme],
    now: DateTime<Utc>,
) -> (Option<&Programme>, Option<&Programme>) {
    let idx = list.partition_point(|p| p.start <= now);
    let current = idx
        .checked_sub(1)
        .and_then(|i| list.get(i))
        .filter(|p| now < p.stop);
    let next = list.get(idx);
    (current, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_core::chrono::TimeZone;
    use opensmarters_core::m3u::{M3uLimits, parse_str};

    #[test]
    fn m3u_channels_get_stable_non_url_keys_and_positions() {
        let p = parse_str(
            "#EXTM3U\n#EXTINF:-1 tvg-id=\"a.1\" group-title=\"News\",Alpha\nhttp://h/live/u/p/1.ts\n#EXTINF:-1,Beta\nhttp://h/2.ts\n",
            &M3uLimits::default(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let cat = LiveCatalog::from_playlist(&p);
        assert_eq!(cat.channels.len(), 2);
        let a = &cat.channels[0];
        assert!(a.key.starts_with("m3u:") && !a.key.contains("http"));
        assert_eq!(a.number, Some(1));
        assert_eq!(cat.channels[1].number, Some(2));
        assert_eq!(cat.categories.len(), 1);
        assert_eq!(cat.count(&CategoryId::from("News")), 1);
        assert_eq!(format!("{:?}", a.source), "M3u(<redacted>)");
        assert_eq!(a.monogram(), "AL");
    }

    #[test]
    fn provider_name_tags() {
        assert_eq!(
            split_tag("FR | Le Titre (2003)"),
            (Some("FR"), "Le Titre (2003)")
        );
        assert_eq!(split_tag("EN: News 24"), (Some("EN"), "News 24"));
        assert_eq!(split_tag("[UK] Sky Thing"), (Some("UK"), "Sky Thing"));
        assert_eq!(split_tag("|CA| Hockey"), (Some("CA"), "Hockey"));
        assert_eq!(split_tag("4K | Nature"), (Some("4K"), "Nature"));
        assert_eq!(
            split_tag("Mission: Impossible"),
            (None, "Mission: Impossible")
        );
        assert_eq!(split_tag("CANADA FRENCH"), (None, "CANADA FRENCH"));
        assert_eq!(split_tag("AB | "), (None, "AB |"));
        assert_eq!(split_tag("12 | Twelve"), (None, "12 | Twelve"));
    }

    #[test]
    fn now_next_lookup() {
        let t = |h| {
            Utc.with_ymd_and_hms(2026, 1, 1, h, 0, 0)
                .single()
                .unwrap_or_default()
        };
        let p = |a, b| Programme {
            title: format!("{a}"),
            description: String::new(),
            start: t(a),
            stop: t(b),
            has_archive: false,
        };
        let list = vec![p(8, 9), p(9, 10), p(10, 11)];
        let (n, x) = now_and_next(
            &list,
            t(9) + opensmarters_core::chrono::Duration::minutes(30),
        );
        assert_eq!(n.map(|p| p.title.as_str()), Some("9"));
        assert_eq!(x.map(|p| p.title.as_str()), Some("10"));
        let (n, x) = now_and_next(&list, t(12));
        assert!(n.is_none() && x.is_none());
        assert!(
            (list[1].progress(t(9) + opensmarters_core::chrono::Duration::minutes(30)) - 0.5).abs()
                < 0.01
        );
    }
}
