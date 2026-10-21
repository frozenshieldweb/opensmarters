// SPDX-License-Identifier: GPL-3.0-or-later
//! App preferences (Settings → Appearance / Language / Playback / About) persisted in the store's
//! settings table. Read once at startup; changes are written in the background.
//!
//! These are app-wide (every profile on this computer). Per-profile playback settings
//! (stream format, User-Agent…) live on the profile in the store.

use opensmarters_store::settings::{self, Setting, TimeFormat};
use opensmarters_store::{Store, StoreError};

/// TV mode (zoom ≈ 1.75 at 1080p, §3).
pub const TV_MODE: Setting<bool> = Setting::new("app.tv_mode", || false);
/// Reduced motion (§7).
pub const REDUCED_MOTION: Setting<bool> = Setting::new("app.reduced_motion", || false);
/// Index into `VideoFit::PRESETS` (aspect/zoom persists, §4).
pub const VIDEO_FIT: Setting<u8> = Setting::new("app.video_fit", || 0);
/// Player volume 0..=100.
pub const VOLUME: Setting<u8> = Setting::new("app.volume", || 100);
/// Reduced motion follows the Windows "Animation effects" setting (default) instead of
/// [`REDUCED_MOTION`].
pub const MOTION_FOLLOW_SYSTEM: Setting<bool> = Setting::new("app.motion_follow_system", || true);
/// Preferred audio language (ISO 639-2 code, e.g. `"fre"`); `None` = the stream's default.
pub const AUDIO_LANGUAGE: Setting<Option<String>> = Setting::new("app.audio_language", || None);
/// Subtitles for films, series and catch-up: a preferred language (ISO 639-2), or `None` =
/// off. Live TV subtitles always start off.
pub const SUBTITLE_LANGUAGE: Setting<Option<String>> =
    Setting::new("app.subtitle_language", || None);
/// Hardware video decoding (`hwdec=auto-safe`); off = software decoding. Applies at once.
pub const HW_DECODING: Setting<bool> = Setting::new("app.hw_decoding", || true);
/// Buffering ([`crate::playback::Buffering`] index: 0 = Auto (default), 1 = Small,
/// 2 = Standard, 3 = Large). Applies at once. (Replaces `app.network_buffer`, whose three
/// presets applied at the next launch.)
pub const BUFFERING: Setting<u8> = Setting::new("app.buffering", || 0);
/// "Rewind live TV" window in minutes: one of [`crate::playback::REWIND_CHOICES`]
/// (0 = off; default 30).
pub const LIVE_REWIND: Setting<u8> = Setting::new("app.live_rewind", || 30);
/// Live channels that stalled get more initial buffering next time (Buffering: Auto):
/// `"<profile id>:<channel key>"` → (extra level 1..=2, unix time of the last stall).
/// Capped ([`crate::playback::tuning::MAX_BOOSTED_CHANNELS`], least recently stalled
/// dropped first). Channel keys are catalogue ids or `m3u:<hash>`, never URLs. Owned by
/// the playback controller (read and written there, not through [`Prefs`]).
pub const CHANNEL_BUFFERING: Setting<std::collections::BTreeMap<String, (u8, u64)>> =
    Setting::new("app.channel_buffering", std::collections::BTreeMap::new);

/// Settings › About › "Check automatically": ask GitHub for the latest version number
/// about once a week (off by default; see [`crate::services::updates`]).
pub const CHECK_UPDATES_AUTOMATICALLY: Setting<bool> =
    Setting::new("app.check_updates_automatically", || false);

/// UI size choices (Settings → Appearance), applied as an extra zoom in desktop mode.
pub const UI_SCALES: [f32; 4] = [0.9, 1.0, 1.1, 1.25];

/// The preferences the UI reads every frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// TV/couch density.
    pub tv_mode: bool,
    /// Reduced motion.
    pub reduced_motion: bool,
    /// 12/24 h clock.
    pub time_format: TimeFormat,
    /// Aspect preset index.
    pub video_fit: u8,
    /// Volume 0..=100.
    pub volume: u8,
    /// Reduced motion follows Windows (then `reduced_motion` is ignored).
    pub motion_follow_system: bool,
    /// Extra zoom in desktop mode (one of [`UI_SCALES`]).
    pub ui_scale: f32,
    /// Preferred audio language (ISO 639-2), `None` = stream default.
    pub audio_language: Option<String>,
    /// Preferred subtitle language (ISO 639-2), `None` = off.
    pub subtitle_language: Option<String>,
    /// Hardware decoding.
    pub hw_decoding: bool,
    /// Buffering index (0 = Auto, 1 = Small, 2 = Standard, 3 = Large).
    pub buffering: u8,
    /// Live rewind window in minutes (0 = off).
    pub live_rewind: u8,
    /// Check for updates about once a week (off by default).
    pub check_updates_automatically: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            tv_mode: false,
            reduced_motion: false,
            time_format: TimeFormat::H24,
            video_fit: 0,
            volume: 100,
            motion_follow_system: true,
            ui_scale: 1.0,
            audio_language: None,
            subtitle_language: None,
            hw_decoding: true,
            buffering: 0,
            live_rewind: 30,
            check_updates_automatically: false,
        }
    }
}

impl Prefs {
    /// Loads from the store (startup only, before the UI loop).
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn load(store: &Store) -> Result<Self, StoreError> {
        let s = store.settings();
        Ok(Self {
            tv_mode: s.get(&TV_MODE)?,
            reduced_motion: s.get(&REDUCED_MOTION)?,
            time_format: s.get(&settings::TIME_FORMAT)?,
            video_fit: s.get(&VIDEO_FIT)?,
            volume: s.get(&VOLUME)?.min(100),
            motion_follow_system: s.get(&MOTION_FOLLOW_SYSTEM)?,
            ui_scale: sanitize_scale(s.get(&settings::UI_SCALE)?),
            audio_language: s.get(&AUDIO_LANGUAGE)?.and_then(|l| sanitize_lang(&l)),
            subtitle_language: s.get(&SUBTITLE_LANGUAGE)?.and_then(|l| sanitize_lang(&l)),
            hw_decoding: s.get(&HW_DECODING)?,
            buffering: s.get(&BUFFERING)?.min(3),
            live_rewind: crate::playback::sanitize_rewind(s.get(&LIVE_REWIND)?),
            check_updates_automatically: s.get(&CHECK_UPDATES_AUTOMATICALLY)?,
        })
    }

    /// Writes every preference (blocking; call off the UI thread).
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn save(&self, store: &Store) -> Result<(), StoreError> {
        let s = store.settings();
        s.set(&TV_MODE, &self.tv_mode)?;
        s.set(&REDUCED_MOTION, &self.reduced_motion)?;
        s.set(&settings::TIME_FORMAT, &self.time_format)?;
        s.set(&VIDEO_FIT, &self.video_fit)?;
        s.set(&VOLUME, &self.volume)?;
        s.set(&MOTION_FOLLOW_SYSTEM, &self.motion_follow_system)?;
        s.set(&settings::UI_SCALE, &self.ui_scale)?;
        s.set(&AUDIO_LANGUAGE, &self.audio_language)?;
        s.set(&SUBTITLE_LANGUAGE, &self.subtitle_language)?;
        s.set(&HW_DECODING, &self.hw_decoding)?;
        s.set(&BUFFERING, &self.buffering)?;
        s.set(&LIVE_REWIND, &self.live_rewind)?;
        s.set(
            &CHECK_UPDATES_AUTOMATICALLY,
            &self.check_updates_automatically,
        )?;
        Ok(())
    }

    /// Whether motion is reduced, given the system's animation setting (`Some(true)` =
    /// Windows animations on; `None` = unknown).
    pub fn effective_reduced_motion(&self, system_animations: Option<bool>) -> bool {
        match (self.motion_follow_system, system_animations) {
            (true, Some(animations_on)) => !animations_on,
            _ => self.reduced_motion,
        }
    }

    /// Formats a local time per the clock preference ("19:59" / "7:59 PM").
    pub fn format_time<Tz: opensmarters_core::chrono::TimeZone>(
        &self,
        t: &opensmarters_core::chrono::DateTime<Tz>,
    ) -> String
    where
        Tz::Offset: std::fmt::Display,
    {
        match self.time_format {
            TimeFormat::H24 => t.format("%H:%M").to_string(),
            TimeFormat::H12 => t.format("%-I:%M %p").to_string(),
        }
    }
}

/// Clamps a stored UI scale to the nearest offered choice (a hand-edited database can't
/// make the UI unusable).
pub fn sanitize_scale(v: f32) -> f32 {
    if !v.is_finite() {
        return 1.0;
    }
    UI_SCALES
        .iter()
        .copied()
        .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
        .unwrap_or(1.0)
}

/// Accepts a 2–3 letter ASCII language code (lower-cased).
fn sanitize_lang(code: &str) -> Option<String> {
    let c = code.trim().to_ascii_lowercase();
    ((2..=3).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_lowercase())).then_some(c)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use opensmarters_store::{MemorySecretStore, StoreOptions};

    use super::*;

    #[test]
    fn prefs_round_trip_through_the_store() {
        let store = Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        assert_eq!(Prefs::load(&store).unwrap(), Prefs::default());
        let p = Prefs {
            tv_mode: true,
            reduced_motion: true,
            time_format: TimeFormat::H12,
            video_fit: 2,
            volume: 40,
            motion_follow_system: false,
            ui_scale: 1.25,
            audio_language: Some("fre".into()),
            subtitle_language: Some("eng".into()),
            hw_decoding: false,
            buffering: 3,
            live_rewind: 60,
            check_updates_automatically: true,
        };
        p.save(&store).unwrap();
        assert_eq!(Prefs::load(&store).unwrap(), p);
    }

    #[test]
    fn hostile_values_are_sanitised() {
        let store = Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        let s = store.settings();
        s.set(&settings::UI_SCALE, &40.0).unwrap();
        s.set(&AUDIO_LANGUAGE, &Some("../../etc".to_owned()))
            .unwrap();
        s.set(&BUFFERING, &200).unwrap();
        s.set(&LIVE_REWIND, &44).unwrap();
        let p = Prefs::load(&store).unwrap();
        assert!((p.ui_scale - 1.25).abs() < f32::EPSILON);
        assert_eq!(p.audio_language, None);
        assert_eq!(p.buffering, 3);
        assert_eq!(p.live_rewind, 30, "nearest offered window");
        assert!((sanitize_scale(f32::NAN) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn reduced_motion_follows_windows_unless_overridden() {
        let mut p = Prefs::default();
        assert!(p.motion_follow_system);
        assert!(
            p.effective_reduced_motion(Some(false)),
            "Windows animations off"
        );
        assert!(!p.effective_reduced_motion(Some(true)));
        assert!(
            !p.effective_reduced_motion(None),
            "unknown: manual value (off)"
        );
        p.motion_follow_system = false;
        p.reduced_motion = true;
        assert!(
            p.effective_reduced_motion(Some(true)),
            "manual override wins"
        );
    }
}
