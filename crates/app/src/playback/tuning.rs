// SPDX-License-Identifier: GPL-3.0-or-later
//! How each stream is buffered (Settings › Playback "Buffering" and "Rewind live TV"),
//! extra buffering for live channels that stalled, and the disk cache folder.
//!
//! Policy ([`stream_tuning`]):
//! - **Live** starts after a short buffer (fast zapping: 1 s with Auto). With Auto, a
//!   channel that stalled starts with more next time: 1 s → 3 s → 6 s
//!   ([`ChannelBoosts`], remembered per profile and channel). Live streams always get a
//!   seekable cache (pause, rewind, "save the last minutes"); with "Rewind live TV" on, the
//!   cache behind *and* ahead of the playback position holds the window (data received
//!   while paused sits ahead of it), sized for 8 Mbit/s, on disk.
//! - **Films, episodes, catch-up** start after a longer buffer and read far ahead into a
//!   large disk-backed cache, so short network drops never show.
//! - Without a data folder (`--ephemeral`) everything stays in memory, capped.
//!
//! The disk cache folder (`<data>/cache/player`) is emptied when the app starts
//! ([`init_disk_cache`]); mpv deletes each stream's file when the stream closes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use opensmarters_player::{Hwdec, PlayerConfig, StreamTuning};
use opensmarters_store::ProfileId;

use super::MediaKind;
use crate::services::Prefs;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// The "Buffering" choice (Settings › Playback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Buffering {
    /// Short for live (fast zapping, more for channels that stalled), large for films.
    #[default]
    Auto,
    /// Least delay and memory.
    Small,
    /// Fixed middle values.
    Standard,
    /// Rides out longer network drops; channels take longer to start.
    Large,
}

impl Buffering {
    /// In settings order (the stored index).
    pub const ALL: [Buffering; 4] = [Self::Auto, Self::Small, Self::Standard, Self::Large];

    /// From the stored index (clamped).
    pub fn from_index(i: u8) -> Self {
        Self::ALL[usize::from(i).min(Self::ALL.len() - 1)]
    }

    /// The stored index.
    pub fn index(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Small => 1,
            Self::Standard => 2,
            Self::Large => 3,
        }
    }

    /// Settings label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Small => "Small",
            Self::Standard => "Standard",
            Self::Large => "Large",
        }
    }

    fn values(self) -> Preset {
        match self {
            Self::Small => Preset {
                live_start: Duration::from_millis(500),
                vod_start: Duration::from_secs(1),
                live_ahead: 32 * MIB,
                vod_ahead_disk: 256 * MIB,
                vod_ahead_ram: 32 * MIB,
                vod_back_disk: 64 * MIB,
                vod_back_ram: 8 * MIB,
            },
            Self::Auto | Self::Standard => Preset {
                live_start: EXTRA_START[0],
                vod_start: Duration::from_secs(3),
                live_ahead: 64 * MIB,
                vod_ahead_disk: GIB,
                vod_ahead_ram: 96 * MIB,
                vod_back_disk: 256 * MIB,
                vod_back_ram: 16 * MIB,
            },
            Self::Large => Preset {
                live_start: Duration::from_secs(3),
                vod_start: Duration::from_secs(8),
                live_ahead: 128 * MIB,
                vod_ahead_disk: 2 * GIB,
                vod_ahead_ram: 256 * MIB,
                vod_back_disk: 512 * MIB,
                vod_back_ram: 64 * MIB,
            },
        }
    }
}

struct Preset {
    live_start: Duration,
    vod_start: Duration,
    live_ahead: u64,
    vod_ahead_disk: u64,
    vod_ahead_ram: u64,
    vod_back_disk: u64,
    vod_back_ram: u64,
}

/// Live initial buffer by extra level (Auto): 1 s, then 3 s and 6 s for channels that
/// stalled.
pub const EXTRA_START: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(6),
];
/// Highest extra level.
pub const MAX_EXTRA_LEVEL: u8 = 2;

/// "Rewind live TV" choices in minutes (0 = off).
pub const REWIND_CHOICES: [u8; 4] = [0, 15, 30, 60];
/// Bytes per minute the rewind window is sized for: 8 Mbit/s, more than most channels.
pub const REWIND_BYTES_PER_MINUTE: u64 = 60 * 1_000_000;
/// Rewind buffer cap (each way) when it must stay in memory (no data folder).
pub const RAM_REWIND_CAP: u64 = 256 * MIB;
/// Live cache behind the playback position with rewind off: enough to step back a little
/// and to save a short clip.
pub const LIVE_BACK_WITHOUT_REWIND: u64 = 32 * MIB;

/// The nearest offered rewind window (a hand-edited database can't produce odd values).
pub fn sanitize_rewind(minutes: u8) -> u8 {
    REWIND_CHOICES
        .iter()
        .copied()
        .min_by_key(|c| c.abs_diff(minutes))
        .unwrap_or(30)
}

/// Bytes reserved behind (and ahead of) the playback position for a rewind window.
pub fn rewind_bytes(minutes: u8, disk: bool) -> u64 {
    let b = u64::from(minutes) * REWIND_BYTES_PER_MINUTE;
    if disk { b } else { b.min(RAM_REWIND_CAP) }
}

/// Settings › Playback values the controller applies to every stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTuning {
    /// Hardware decoding (`hwdec=auto-safe`), else software.
    pub hw_decoding: bool,
    /// Buffering choice.
    pub buffering: Buffering,
    /// Live rewind window in minutes (0 = off).
    pub rewind_minutes: u8,
}

impl Default for PlayerTuning {
    fn default() -> Self {
        Self {
            hw_decoding: true,
            buffering: Buffering::Auto,
            rewind_minutes: 30,
        }
    }
}

impl PlayerTuning {
    /// From the preferences.
    pub fn from_prefs(p: &Prefs) -> Self {
        Self {
            hw_decoding: p.hw_decoding,
            buffering: Buffering::from_index(p.buffering),
            rewind_minutes: sanitize_rewind(p.live_rewind),
        }
    }

    /// The decoder setting.
    pub fn hwdec(&self) -> Hwdec {
        if self.hw_decoding {
            Hwdec::AutoSafe
        } else {
            Hwdec::Disabled
        }
    }

    /// The live initial buffer for a channel at `extra_level` (only Auto adapts).
    pub fn live_start(&self, extra_level: u8) -> Duration {
        match self.buffering {
            Buffering::Auto => EXTRA_START[usize::from(extra_level.min(MAX_EXTRA_LEVEL))],
            other => other.values().live_start,
        }
    }

    /// The nominal live rewind window, if on.
    pub fn rewind_window(&self) -> Option<Duration> {
        (self.rewind_minutes > 0).then(|| Duration::from_secs(u64::from(self.rewind_minutes) * 60))
    }
}

/// The player-wide part of the settings: hardware decoding, and the disk cache folder
/// when [`init_disk_cache`] prepared one. Everything else is per load
/// ([`stream_tuning`]).
pub fn configure(config: PlayerConfig, t: &PlayerTuning) -> PlayerConfig {
    let config = config.hwdec(t.hwdec());
    match disk_cache_dir() {
        Some(dir) => config.cache_dir(dir),
        None => config,
    }
}

/// The buffering for one stream. `extra_level`: the channel's extra buffering (live,
/// Auto); `disk`: the player has a disk cache folder.
pub fn stream_tuning(
    kind: MediaKind,
    t: &PlayerTuning,
    extra_level: u8,
    disk: bool,
) -> StreamTuning {
    let p = t.buffering.values();
    if kind.is_live() {
        let rewind = rewind_bytes(t.rewind_minutes, disk);
        let back = if t.rewind_minutes > 0 {
            rewind
        } else {
            LIVE_BACK_WITHOUT_REWIND
        };
        StreamTuning {
            start_buffer: t.live_start(extra_level),
            forward_bytes: p.live_ahead.max(back),
            back_bytes: back,
            seekable_cache: true,
            disk_cache: disk && t.rewind_minutes > 0,
        }
    } else {
        // Local recordings are read from disk; a cache file would only copy them.
        let disk = disk && kind != MediaKind::Recording;
        StreamTuning {
            start_buffer: p.vod_start,
            forward_bytes: if disk {
                p.vod_ahead_disk
            } else {
                p.vod_ahead_ram
            },
            back_bytes: if disk {
                p.vod_back_disk
            } else {
                p.vod_back_ram
            },
            seekable_cache: false,
            disk_cache: disk,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Extra buffering per channel
// ---------------------------------------------------------------------------------------

/// Most channels remembered with extra buffering (least recently stalled dropped first).
pub const MAX_BOOSTED_CHANNELS: usize = 300;
/// Longest channel key accepted in the map.
const MAX_KEY_LEN: usize = 200;

/// The map key for a profile's channel, or `None` for an unusable channel key.
pub fn boost_key(profile: ProfileId, channel_key: &str) -> Option<String> {
    let k = format!("{profile}:{channel_key}");
    (!channel_key.is_empty() && k.len() <= MAX_KEY_LEN && !k.chars().any(char::is_control))
        .then_some(k)
}

/// Raises a channel's level by one (max [`MAX_EXTRA_LEVEL`]), records `now` and drops the
/// least recently stalled channels beyond [`MAX_BOOSTED_CHANNELS`]. Returns the new level.
pub fn bump(map: &mut BTreeMap<String, (u8, u64)>, key: &str, now: u64) -> u8 {
    let level = map
        .get(key)
        .map_or(1, |(l, _)| l.saturating_add(1))
        .min(MAX_EXTRA_LEVEL);
    map.insert(key.to_owned(), (level, now));
    while map.len() > MAX_BOOSTED_CHANNELS {
        let oldest = map
            .iter()
            .min_by_key(|(_, (_, t))| *t)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                map.remove(&k);
            }
            None => break,
        }
    }
    level
}

/// Cleans a stored map: valid keys, levels 1..=[`MAX_EXTRA_LEVEL`], at most
/// [`MAX_BOOSTED_CHANNELS`] entries (most recent kept).
pub fn sanitize_boosts(map: BTreeMap<String, (u8, u64)>) -> BTreeMap<String, (u8, u64)> {
    let mut entries: Vec<(String, (u8, u64))> = map
        .into_iter()
        .filter(|(k, (l, _))| {
            *l > 0 && k.len() <= MAX_KEY_LEN && k.contains(':') && !k.chars().any(char::is_control)
        })
        .map(|(k, (l, t))| (k, (l.min(MAX_EXTRA_LEVEL), t)))
        .collect();
    entries.sort_by_key(|(_, (_, t))| std::cmp::Reverse(*t));
    entries.truncate(MAX_BOOSTED_CHANNELS);
    entries.into_iter().collect()
}

/// Channels that start with extra buffering, in memory (loaded from and saved to
/// [`crate::services::prefs::CHANNEL_BUFFERING`] by the controller).
#[derive(Debug, Clone, Default)]
pub struct ChannelBoosts {
    map: BTreeMap<String, (u8, u64)>,
}

impl ChannelBoosts {
    /// From the stored map.
    pub fn from_map(map: BTreeMap<String, (u8, u64)>) -> Self {
        Self {
            map: sanitize_boosts(map),
        }
    }

    /// Merges a map loaded later (the higher level and the later time win).
    pub fn merge(&mut self, loaded: BTreeMap<String, (u8, u64)>) {
        for (k, (l, t)) in sanitize_boosts(loaded) {
            let e = self.map.entry(k).or_insert((l, t));
            *e = (e.0.max(l), e.1.max(t));
        }
        self.map = sanitize_boosts(std::mem::take(&mut self.map));
    }

    /// The channel's extra level (0 = none).
    pub fn level(&self, profile: ProfileId, channel_key: &str) -> u8 {
        boost_key(profile, channel_key)
            .and_then(|k| self.map.get(&k).map(|(l, _)| *l))
            .unwrap_or(0)
    }

    /// Raises the channel's level; returns the new level (0 for an unusable key).
    pub fn bump(&mut self, profile: ProfileId, channel_key: &str, now: u64) -> u8 {
        match boost_key(profile, channel_key) {
            Some(k) => bump(&mut self.map, &k, now),
            None => 0,
        }
    }

    /// Channels with extra buffering for `profile`.
    pub fn count_for(&self, profile: ProfileId) -> usize {
        let prefix = format!("{profile}:");
        self.map.keys().filter(|k| k.starts_with(&prefix)).count()
    }

    /// Forgets `profile`'s channels; returns the remaining map (to store).
    pub fn clear_profile(&mut self, profile: ProfileId) -> BTreeMap<String, (u8, u64)> {
        let prefix = format!("{profile}:");
        self.map.retain(|k, _| !k.starts_with(&prefix));
        self.map.clone()
    }
}

// ---------------------------------------------------------------------------------------
// Disk cache folder
// ---------------------------------------------------------------------------------------

static CACHE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Prepares `<data>/cache/player` for disk-backed caches: empties it (files left by a
/// crash) and creates it. Call once at startup, before the player is created; later
/// calls return the first result. `None` (no data folder, or the folder can't be made)
/// keeps every cache in memory.
pub fn init_disk_cache(data_dir: Option<&Path>) -> Option<PathBuf> {
    CACHE_DIR
        .get_or_init(|| {
            let dir = data_dir?.join("cache").join("player");
            if let Err(e) = std::fs::remove_dir_all(&dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                // Another running copy may hold a file open; keep going.
                tracing::info!(error = %e, "couldn't empty the player cache folder");
            }
            match std::fs::create_dir_all(&dir) {
                Ok(()) if dir.is_absolute() => Some(dir),
                Ok(()) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "no player cache folder; caching in memory");
                    None
                }
            }
        })
        .clone()
}

/// The folder prepared by [`init_disk_cache`], if any.
pub fn disk_cache_dir() -> Option<PathBuf> {
    CACHE_DIR.get().cloned().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewind_choices_and_sizes() {
        assert_eq!(sanitize_rewind(0), 0);
        assert_eq!(sanitize_rewind(16), 15);
        assert_eq!(sanitize_rewind(200), 60);
        assert_eq!(rewind_bytes(30, true), 1_800_000_000);
        assert_eq!(rewind_bytes(60, false), RAM_REWIND_CAP);
        assert_eq!(rewind_bytes(0, true), 0);
        assert_eq!(Buffering::from_index(99), Buffering::Large);
        for b in Buffering::ALL {
            assert_eq!(Buffering::from_index(b.index()), b);
        }
    }

    #[test]
    fn live_is_short_and_seekable_vod_is_large() {
        let t = PlayerTuning::default();
        let live = stream_tuning(MediaKind::Live, &t, 0, true);
        assert_eq!(live.start_buffer, Duration::from_secs(1));
        assert!(live.seekable_cache && live.disk_cache);
        assert_eq!(live.back_bytes, 1_800_000_000, "30 min at 8 Mbit/s");
        assert!(
            live.forward_bytes >= live.back_bytes,
            "a pause must fit the window"
        );
        let vod = stream_tuning(MediaKind::Movie, &t, 0, true);
        assert!(vod.start_buffer > live.start_buffer);
        assert_eq!(vod.forward_bytes, GIB);
        assert!(vod.disk_cache && !vod.seekable_cache);
        // Memory only: capped.
        let ram = stream_tuning(MediaKind::Live, &t, 0, false);
        assert!(!ram.disk_cache);
        assert_eq!(ram.back_bytes, RAM_REWIND_CAP);
        assert_eq!(
            stream_tuning(MediaKind::Episode, &t, 0, false).forward_bytes,
            96 * MIB
        );
        // Rewind off: still seekable, small back buffer, in memory.
        let off = PlayerTuning {
            rewind_minutes: 0,
            ..t
        };
        let live_off = stream_tuning(MediaKind::Live, &off, 0, true);
        assert!(live_off.seekable_cache && !live_off.disk_cache);
        assert_eq!(live_off.back_bytes, LIVE_BACK_WITHOUT_REWIND);
        // Recordings play from disk without a cache file.
        assert!(!stream_tuning(MediaKind::Recording, &t, 0, true).disk_cache);
    }

    #[test]
    fn only_auto_adapts_to_stalls() {
        let auto = PlayerTuning::default();
        let starts: Vec<_> = (0..4)
            .map(|l| stream_tuning(MediaKind::Live, &auto, l, true).start_buffer)
            .collect();
        assert_eq!(
            starts,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(3),
                Duration::from_secs(6),
                Duration::from_secs(6)
            ]
        );
        let large = PlayerTuning {
            buffering: Buffering::Large,
            ..auto
        };
        assert_eq!(
            stream_tuning(MediaKind::Live, &large, 2, true).start_buffer,
            Duration::from_secs(3)
        );
        let small = PlayerTuning {
            buffering: Buffering::Small,
            ..auto
        };
        assert_eq!(
            stream_tuning(MediaKind::Live, &small, 0, true).start_buffer,
            Duration::from_millis(500)
        );
    }

    #[test]
    fn boosts_step_up_cap_and_forget() {
        let p = ProfileId::new_random();
        let other = ProfileId::new_random();
        let mut b = ChannelBoosts::default();
        assert_eq!(b.level(p, "101"), 0);
        assert_eq!(b.bump(p, "101", 10), 1);
        assert_eq!(b.bump(p, "101", 11), 2);
        assert_eq!(b.bump(p, "101", 12), 2, "capped");
        assert_eq!(b.level(p, "101"), 2);
        assert_eq!(b.level(other, "101"), 0, "per profile");
        assert_eq!(b.bump(p, "", 13), 0, "no key");
        assert_eq!(b.bump(p, &"x".repeat(500), 13), 0, "key too long");
        assert_eq!(b.count_for(p), 1);
        let left = b.clear_profile(p);
        assert!(left.is_empty());
        assert_eq!(b.level(p, "101"), 0);
    }

    #[test]
    fn boost_map_is_capped_by_recency_and_sanitised() {
        let mut m = BTreeMap::new();
        for i in 0..(MAX_BOOSTED_CHANNELS as u64 + 10) {
            bump(&mut m, &format!("1:{i}"), i);
        }
        assert_eq!(m.len(), MAX_BOOSTED_CHANNELS);
        assert!(!m.contains_key("1:0"), "oldest dropped");
        assert!(m.contains_key(&format!("1:{}", MAX_BOOSTED_CHANNELS + 9)));

        let mut hostile = BTreeMap::new();
        hostile.insert("1:ok".to_owned(), (200, 5));
        hostile.insert("1:zero".to_owned(), (0, 5));
        hostile.insert("nocolon".to_owned(), (1, 5));
        hostile.insert("1:bad\nkey".to_owned(), (1, 5));
        let clean = sanitize_boosts(hostile);
        assert_eq!(clean.len(), 1);
        assert_eq!(clean.get("1:ok"), Some(&(MAX_EXTRA_LEVEL, 5)));

        let p = ProfileId::new_random();
        let mut b = ChannelBoosts::default();
        b.bump(p, "a", 1);
        let mut loaded = BTreeMap::new();
        loaded.insert(format!("{p}:a"), (2, 0));
        loaded.insert(format!("{p}:b"), (1, 3));
        b.merge(loaded);
        assert_eq!(b.level(p, "a"), 2);
        assert_eq!(b.level(p, "b"), 1);
    }
}
