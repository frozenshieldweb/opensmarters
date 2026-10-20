// SPDX-License-Identifier: GPL-3.0-or-later
//! Track preferences (Settings → Playback): preferred audio and subtitle languages and
//! language-code normalisation. Buffering lives in [`super::tuning`].
//!
//! Containers and HLS playlists tag tracks with ISO 639-1 (`fr`), ISO 639-2/B (`fre`),
//! ISO 639-2/T (`fra`) or even English names (`French`). [`canonical`] maps all of these
//! to one ISO 639-2/B code so a preference matches whichever form a stream uses.
//!
//! Policy (applied by the controller once a stream's tracks are known):
//! - **Audio:** if a preferred language is set and the stream has a track in it, select
//!   that track (the first one, preferring a container-default track in that language);
//!   otherwise keep mpv's choice.
//! - **Subtitles:** live channels always start with subtitles off (providers embed closed
//!   captions that mpv would otherwise show). For films, episodes and catch-up, a
//!   preferred language selects a matching track (forced tracks last); no preference or
//!   no match = off.

use opensmarters_player::{Track, TrackKind, TrackSelection};

/// A language offered in the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// ISO 639-2/B code (what the settings store).
    pub code: &'static str,
    /// English name shown in the settings.
    pub name: &'static str,
    /// Other spellings found in streams: ISO 639-1, ISO 639-2/T, names.
    pub aliases: &'static [&'static str],
}

macro_rules! lang {
    ($code:literal, $name:literal, [$($alias:literal),* $(,)?]) => {
        Language { code: $code, name: $name, aliases: &[$($alias),*] }
    };
}

/// Languages offered for audio/subtitle preferences (common IPTV languages).
pub const LANGUAGES: &[Language] = &[
    lang!("eng", "English", ["en", "english"]),
    lang!(
        "fre",
        "French",
        ["fr", "fra", "french", "francais", "français"]
    ),
    lang!(
        "spa",
        "Spanish",
        ["es", "spanish", "espanol", "español", "castellano"]
    ),
    lang!("ger", "German", ["de", "deu", "german", "deutsch"]),
    lang!("ita", "Italian", ["it", "italian", "italiano"]),
    lang!(
        "por",
        "Portuguese",
        ["pt", "portuguese", "portugues", "português"]
    ),
    lang!("dut", "Dutch", ["nl", "nld", "dutch", "nederlands"]),
    lang!("ara", "Arabic", ["ar", "arabic"]),
    lang!("tur", "Turkish", ["tr", "turkish", "türkçe"]),
    lang!("pol", "Polish", ["pl", "polish", "polski"]),
    lang!("rus", "Russian", ["ru", "russian"]),
    lang!("ukr", "Ukrainian", ["uk", "ukrainian"]),
    lang!("rum", "Romanian", ["ro", "ron", "romanian"]),
    lang!("gre", "Greek", ["el", "ell", "greek"]),
    lang!("swe", "Swedish", ["sv", "swedish", "svenska"]),
    lang!(
        "nor",
        "Norwegian",
        ["no", "nb", "nob", "norwegian", "norsk"]
    ),
    lang!("dan", "Danish", ["da", "danish", "dansk"]),
    lang!("fin", "Finnish", ["fi", "finnish", "suomi"]),
    lang!("hin", "Hindi", ["hi", "hindi"]),
    lang!("urd", "Urdu", ["ur", "urdu"]),
    lang!("per", "Persian", ["fa", "fas", "persian", "farsi"]),
    lang!("heb", "Hebrew", ["he", "hebrew"]),
    lang!("chi", "Chinese", ["zh", "zho", "chinese"]),
    lang!("jpn", "Japanese", ["ja", "japanese"]),
    lang!("kor", "Korean", ["ko", "korean"]),
    lang!("alb", "Albanian", ["sq", "sqi", "albanian"]),
    lang!("srp", "Serbian", ["sr", "scc", "serbian"]),
    lang!("hrv", "Croatian", ["hr", "croatian"]),
    lang!("hun", "Hungarian", ["hu", "hungarian", "magyar"]),
    lang!("cze", "Czech", ["cs", "ces", "czech"]),
];

/// The ISO 639-2/B code for a track/stream language tag in any common form
/// (`"fr"`, `"fra"`, `"fre"`, `"French"`, `"fr-CA"`), or `None` if unknown.
pub fn canonical(tag: &str) -> Option<&'static str> {
    let t = tag.trim().to_lowercase();
    // "fr-CA", "pt_BR" → the primary subtag.
    let primary = t.split(['-', '_']).next().unwrap_or("");
    if primary.is_empty() || primary.len() > 16 {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|l| l.code == primary || l.aliases.contains(&primary))
        .map(|l| l.code)
}

/// The English name of a stored code, for the settings.
pub fn language_name(code: &str) -> Option<&'static str> {
    let c = canonical(code)?;
    LANGUAGES.iter().find(|l| l.code == c).map(|l| l.name)
}

/// True if a track's language tag means the wanted language: equal canonical codes, or
/// (for languages outside [`LANGUAGES`]) the same tag ignoring case.
fn lang_matches(track: &Track, want: &str) -> bool {
    let Some(tag) = track.lang.as_deref() else {
        return false;
    };
    match (canonical(tag), canonical(want)) {
        (Some(a), Some(b)) => a == b,
        _ => tag.trim().eq_ignore_ascii_case(want.trim()),
    }
}

/// The audio track to switch to for the preference, or `None` to keep mpv's choice
/// (no preference, no match, or a matching track is already selected).
pub fn pick_audio(tracks: &[Track], preferred: Option<&str>) -> Option<i64> {
    let want = preferred.map(str::trim).filter(|w| !w.is_empty())?;
    let audio: Vec<&Track> = tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .collect();
    if audio.iter().any(|t| t.selected && lang_matches(t, want)) {
        return None;
    }
    let matching: Vec<&&Track> = audio.iter().filter(|t| lang_matches(t, want)).collect();
    matching
        .iter()
        .find(|t| t.default)
        .or_else(|| matching.first())
        .map(|t| t.id)
}

/// The subtitle selection for a new stream: off for live, else the preferred language
/// (non-forced tracks first), else off.
pub fn pick_subtitle(tracks: &[Track], preferred: Option<&str>, live: bool) -> TrackSelection {
    if live {
        return TrackSelection::Off;
    }
    let Some(want) = preferred.map(str::trim).filter(|w| !w.is_empty()) else {
        return TrackSelection::Off;
    };
    let mut subs: Vec<&Track> = tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Subtitle && lang_matches(t, want))
        .collect();
    subs.sort_by_key(|t| (t.forced, !t.default));
    subs.first()
        .map_or(TrackSelection::Off, |t| TrackSelection::Id(t.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, kind: TrackKind, lang: Option<&str>) -> Track {
        Track {
            id,
            kind,
            title: None,
            lang: lang.map(str::to_owned),
            codec: None,
            default: false,
            forced: false,
            selected: false,
            external: false,
            channels: None,
        }
    }

    #[test]
    fn canonicalises_every_common_form() {
        for tag in [
            "fr",
            "fre",
            "fra",
            "French",
            " FR ",
            "fr-CA",
            "fr_FR",
            "Français",
        ] {
            assert_eq!(canonical(tag), Some("fre"), "{tag}");
        }
        assert_eq!(canonical("deu"), Some("ger"));
        assert_eq!(canonical("en-GB"), Some("eng"));
        assert_eq!(canonical("und"), None);
        assert_eq!(canonical(""), None);
        assert_eq!(canonical(&"x".repeat(100)), None);
        assert_eq!(language_name("fra"), Some("French"));
        // Codes are unique and canonical to themselves.
        for l in LANGUAGES {
            assert_eq!(canonical(l.code), Some(l.code), "{}", l.code);
        }
    }

    #[test]
    fn audio_follows_the_preference() {
        let mut eng = track(1, TrackKind::Audio, Some("eng"));
        eng.selected = true;
        let fra = track(2, TrackKind::Audio, Some("fra"));
        let mut fre_default = track(3, TrackKind::Audio, Some("fr"));
        fre_default.default = true;
        let tracks = vec![eng.clone(), fra.clone()];
        assert_eq!(pick_audio(&tracks, Some("fre")), Some(2));
        assert_eq!(
            pick_audio(&tracks, None),
            None,
            "no preference: mpv's choice"
        );
        assert_eq!(
            pick_audio(&tracks, Some("ger")),
            None,
            "no match: mpv's choice"
        );
        assert_eq!(pick_audio(&tracks, Some("eng")), None, "already selected");
        let tracks = vec![eng, fra, fre_default];
        assert_eq!(
            pick_audio(&tracks, Some("fre")),
            Some(3),
            "default track first"
        );
    }

    #[test]
    fn subtitles_off_for_live_and_without_preference() {
        let mut forced = track(1, TrackKind::Subtitle, Some("eng"));
        forced.forced = true;
        let full = track(2, TrackKind::Subtitle, Some("en"));
        let fr = track(3, TrackKind::Subtitle, Some("fre"));
        let tracks = vec![forced, full, fr];
        assert_eq!(
            pick_subtitle(&tracks, Some("eng"), true),
            TrackSelection::Off
        );
        assert_eq!(pick_subtitle(&tracks, None, false), TrackSelection::Off);
        assert_eq!(
            pick_subtitle(&tracks, Some("eng"), false),
            TrackSelection::Id(2),
            "full subtitles before forced ones"
        );
        assert_eq!(
            pick_subtitle(&tracks, Some("fr"), false),
            TrackSelection::Id(3)
        );
        assert_eq!(
            pick_subtitle(&tracks, Some("spa"), false),
            TrackSelection::Off
        );
        // A language outside the list still matches its exact tag (track-sheet picks).
        let vie = track(4, TrackKind::Subtitle, Some("vie"));
        assert_eq!(
            pick_subtitle(&[vie], Some("VIE"), false),
            TrackSelection::Id(4)
        );
    }
}
