// SPDX-License-Identifier: GPL-3.0-or-later
//! App preferences (Settings → Appearance / Playback) persisted in the store's settings
//! table. Read once at startup; changes are written in the background.

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
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            tv_mode: false,
            reduced_motion: false,
            time_format: TimeFormat::H24,
            video_fit: 0,
            volume: 100,
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
        Ok(())
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
