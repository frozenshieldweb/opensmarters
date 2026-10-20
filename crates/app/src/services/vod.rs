// SPDX-License-Identifier: GPL-3.0-or-later
//! Movies and series presentation logic (pure, unit-tested), shared by Home, the browse
//! screens and the detail page:
//!
//! - **Titles:** provider names look like `"FR | The Disappearance of Haruhi Suzumiya (2010)"`.
//!   [`display_title`] splits off the tag (shown as a chip) and the trailing year (shown
//!   in the meta line); the full name stays available for search.
//! - **Years:** [`release_year`] uses the release date (`releasedate` / `releaseDate` /
//!   `release_date`) first, then a trailing `"(YYYY)"` in the name, else nothing. The
//!   `added` timestamp is **never** a release year.
//! - **Meta lines** (`"FR · 2010 · ★ 7.4"`): [`movie_card_meta`], [`series_card_meta`].
//!   The star is a Phosphor glyph that [`crate::ui::text`] always draws with the icon
//!   font.
//! - **Sorting** options ([`VodSort`]), **seasons** ([`season_label`], specials and gaps),
//!   **episodes** ([`episode_title`], [`episode_code`], [`next_episode_id`]), the series
//!   **continue target** ([`continue_target`]) and **trailers** ([`youtube_id`]).

use opensmarters_core::catalog::SortOrder;
use opensmarters_core::{Episode, Series, SeriesInfo, VodStream};
use opensmarters_store::{PlaybackProgress, ResumeThresholds, next_episode};

use crate::services::catalog::split_tag;
use crate::ui::icons;
use crate::ui::text;

/// A provider name split for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayTitle<'a> {
    /// Short language/region tag (`"FR"`), shown as a chip.
    pub tag: Option<&'a str>,
    /// The title without tag and trailing year.
    pub title: &'a str,
    /// A trailing `"(YYYY)"` from the name.
    pub year: Option<u16>,
}

/// Splits a provider name into tag, title and trailing year.
pub fn display_title(raw: &str) -> DisplayTitle<'_> {
    let (tag, rest) = split_tag(raw);
    let (title, year) = split_year(rest);
    DisplayTitle { tag, title, year }
}

fn plausible_year(y: u32) -> Option<u16> {
    (1880..=2100)
        .contains(&y)
        .then(|| u16::try_from(y).ok())
        .flatten()
}

/// Splits a trailing `"(YYYY)"` (or `"[YYYY]"`) off `s`: `"Title (2004)"` →
/// (`"Title"`, `Some(2004)`). Anything else is returned unchanged.
pub fn split_year(s: &str) -> (&str, Option<u16>) {
    let t = s.trim();
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(inner) = t.strip_suffix(close)
            && let Some(pos) = inner.rfind(open)
        {
            let digits = inner[pos + open.len_utf8()..].trim();
            let title = inner[..pos].trim_end_matches([' ', '-', '–']).trim();
            if digits.len() == 4
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !title.is_empty()
                && let Some(y) = digits.parse().ok().and_then(plausible_year)
            {
                return (title, Some(y));
            }
        }
    }
    (t, None)
}

/// The first plausible 4-digit year in a provider date (`"2010-05-01"`, `"2010"`,
/// `"01/05/2010"`).
pub fn year_from_date(s: &str) -> Option<u16> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == 4
                && let Some(y) = s[start..i].parse().ok().and_then(plausible_year)
            {
                return Some(y);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Release year: the release date first, then a trailing `"(YYYY)"` in the name.
pub fn release_year(release_date: Option<&str>, name: &str) -> Option<u16> {
    release_date
        .and_then(year_from_date)
        .or_else(|| display_title(name).year)
}

/// A rating out of 10 as `"★ 7.4"` (`None` when unrated).
pub fn rating_label(rating10: Option<f64>) -> Option<String> {
    let r = rating10.filter(|r| r.is_finite() && *r > 0.0)?.min(10.0);
    Some(format!("{} {r:.1}", icons::STAR))
}

/// Joins non-empty parts with `" · "`.
pub fn meta_line<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = String::new();
    for p in parts {
        let p = p.as_ref().trim();
        if p.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(p);
    }
    out
}

/// Poster meta for a movie list item: `"FR · 2004 · ★ 7.4"`.
pub fn movie_card_meta(m: &VodStream) -> String {
    let t = display_title(&m.name);
    meta_line(
        [
            t.tag.map(str::to_owned),
            t.year.map(|y| y.to_string()),
            rating_label(m.rating_out_of_5().map(|r| r * 2.0)),
        ]
        .into_iter()
        .flatten(),
    )
}

/// Poster meta for a series list item: `"FR · 2010 · ★ 8.1"`.
pub fn series_card_meta(s: &Series) -> String {
    let t = display_title(&s.name);
    meta_line(
        [
            t.tag.map(str::to_owned),
            release_year(s.release_date.as_deref(), &s.name).map(|y| y.to_string()),
            rating_label(s.rating_out_of_5().map(|r| r * 2.0)),
        ]
        .into_iter()
        .flatten(),
    )
}

/// Quality markers in a name (`"4K"`, `"HDR"`, `"HD"`), for outline tags on the detail page.
pub fn quality_tags(name: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for tok in name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        let tag = match tok.to_ascii_uppercase().as_str() {
            "4K" | "UHD" | "2160P" => "4K",
            "HDR" | "HDR10" | "DV" => "HDR",
            "FHD" | "1080P" | "HD" | "720P" => "HD",
            _ => continue,
        };
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    if out.contains(&"4K") {
        out.retain(|t| *t != "HD");
    }
    out
}

/// `"01:45:00"` / `"45:00"` / `"95"` (minutes) → seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    let nums: Option<Vec<u64>> = parts.iter().map(|p| p.trim().parse::<u64>().ok()).collect();
    let nums = nums?;
    let secs = match nums.as_slice() {
        [m] => m.checked_mul(60)?,
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    (secs > 0).then_some(secs)
}

/// Browse sort options (§8.4): server order, name, recently added, rating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VodSort {
    /// Provider order.
    #[default]
    Server,
    /// Name A→Z (natural).
    Name,
    /// Recently added first.
    Recent,
    /// Highest rated first.
    Rating,
}

impl VodSort {
    /// All options in menu order.
    pub const ALL: [VodSort; 4] = [
        VodSort::Server,
        VodSort::Name,
        VodSort::Recent,
        VodSort::Rating,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            VodSort::Server => "Provider order",
            VodSort::Name => "Name",
            VodSort::Recent => "Recently added",
            VodSort::Rating => "Rating",
        }
    }

    /// The catalogue sort order.
    pub fn order(self) -> SortOrder {
        match self {
            VodSort::Server => SortOrder::Server,
            VodSort::Name => SortOrder::NameAsc,
            VodSort::Recent => SortOrder::RecentlyAdded,
            VodSort::Rating => SortOrder::TopRated,
        }
    }
}

/// Tab label of a season: 0 → "Specials".
pub fn season_label(season: u32) -> String {
    if season == 0 {
        "Specials".to_owned()
    } else {
        format!("Season {season}")
    }
}

/// The episode number used for display and ordering: `episode_num`, else the position
/// in the season (1-based).
pub fn episode_number(ep: &Episode, position: usize) -> u32 {
    ep.episode_num
        .unwrap_or_else(|| u32::try_from(position + 1).unwrap_or(u32::MAX))
}

/// `"S1·E3"`, or `"Special 2"` for season 0.
pub fn episode_code(season: u32, number: u32) -> String {
    if season == 0 {
        format!("Special {number}")
    } else {
        format!("S{season}·E{number}")
    }
}

/// A clean episode title: drops a leading series name and an `SxxEyy` code
/// (`"Show - S01E02 - Pilot"` → `"Pilot"`), else `"Episode N"` when nothing is left.
pub fn episode_title(raw: &str, series_name: &str, number: u32) -> String {
    let capped = text::cap(raw, text::MAX_LINE_CHARS);
    let mut t: &str = capped.trim();
    let series = display_title(series_name).title.trim();
    for prefix in [series_name.trim(), series] {
        if !prefix.is_empty()
            && t.len() > prefix.len()
            && t.is_char_boundary(prefix.len())
            && t[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            t = t[prefix.len()..].trim_start_matches([' ', '-', '–', ':', '|', '·']);
            break;
        }
    }
    // A leading "S01E02" / "S1 E2" / "1x02" code.
    let b = t.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > s
    };
    let code_end = if b.first().is_some_and(|c| c.eq_ignore_ascii_case(&b's')) {
        i = 1;
        if digits(&mut i) {
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if b.get(i).is_some_and(|c| c.eq_ignore_ascii_case(&b'e')) {
                i += 1;
                digits(&mut i).then_some(i)
            } else {
                None
            }
        } else {
            None
        }
    } else if digits(&mut i) && b.get(i).is_some_and(|c| c.eq_ignore_ascii_case(&b'x')) {
        i += 1;
        digits(&mut i).then_some(i)
    } else {
        None
    };
    if let Some(end) = code_end
        && b.get(end).is_none_or(|c| !c.is_ascii_alphanumeric())
    {
        t = t[end..].trim_start_matches([' ', '-', '–', ':', '|', '·', '.']);
    }
    let t = t.trim();
    if t.is_empty() || t.eq_ignore_ascii_case(series) {
        format!("Episode {number}")
    } else {
        t.to_owned()
    }
}

/// The series name: `info.name`, else guessed from the first episode's title
/// (`"Show - S01E01 - Pilot"` → `"Show"`) for panels whose `info` is empty.
pub fn series_name(info: &SeriesInfo) -> String {
    if let Some(n) = info.info.name.as_deref().filter(|n| !n.trim().is_empty()) {
        return n.to_owned();
    }
    let guess = info.episodes.values().flatten().next().and_then(|ep| {
        let (head, rest) = ep.title.trim().split_once(" - ")?;
        let b = rest.trim_start().as_bytes();
        let code = b.len() >= 2 && b[0].eq_ignore_ascii_case(&b's') && b[1].is_ascii_digit();
        (code && !head.trim().is_empty()).then(|| head.trim().to_owned())
    });
    guess.unwrap_or_default()
}

/// Every episode as `(season, number, id)` in season/episode order.
pub fn episode_index(info: &SeriesInfo) -> Vec<(u32, u32, u64)> {
    let mut out = Vec::with_capacity(info.episode_count());
    for (season, eps) in &info.episodes {
        for (i, ep) in eps.iter().enumerate() {
            out.push((*season, episode_number(ep, i), ep.id));
        }
    }
    out
}

/// The season and number of episode `id`.
pub fn locate_episode(info: &SeriesInfo, id: u64) -> Option<(u32, u32, &Episode)> {
    info.episodes.iter().find_map(|(season, eps)| {
        eps.iter()
            .enumerate()
            .find(|(_, e)| e.id == id)
            .map(|(i, e)| (*season, episode_number(e, i), e))
    })
}

/// The episode after `id` (next number, then the next season; specials only chain into
/// season 1), using the store's [`next_episode`] rule.
pub fn next_episode_id(info: &SeriesInfo, id: u64) -> Option<u64> {
    let (season, number, _) = locate_episode(info, id)?;
    next_episode((season, number), episode_index(info))
}

/// The first episode to offer: season 1 (or the lowest regular season), else specials.
pub fn first_episode_id(info: &SeriesInfo) -> Option<u64> {
    info.episodes
        .iter()
        .filter(|(s, eps)| **s > 0 && !eps.is_empty())
        .chain(info.episodes.iter().filter(|(s, _)| **s == 0))
        .find_map(|(_, eps)| eps.first().map(|e| e.id))
}

/// What the series' primary button does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContinueKind {
    /// Resume this episode at the position (seconds).
    Resume(f64),
    /// The episode after the last finished one.
    Next,
    /// Start this episode (first episode, or the last one was barely started).
    Start,
    /// Everything was watched: start over.
    Again,
}

/// The series' "Continue" target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContinueTarget {
    /// Episode id.
    pub episode_id: u64,
    /// What to do.
    pub kind: ContinueKind,
}

/// Picks the "Continue S1·E3" target from the most recently watched episode of the
/// series (`last`): resume it when resumable, else the next episode when it was
/// finished, else start it; with no history the first episode.
pub fn continue_target(
    info: &SeriesInfo,
    last: Option<&PlaybackProgress>,
    t: &ResumeThresholds,
) -> Option<ContinueTarget> {
    let first = first_episode_id(info)?;
    let Some(last) = last else {
        return Some(ContinueTarget {
            episode_id: first,
            kind: ContinueKind::Start,
        });
    };
    let Some(id) = last
        .item_id
        .parse::<u64>()
        .ok()
        .filter(|id| locate_episode(info, *id).is_some())
    else {
        return Some(ContinueTarget {
            episode_id: first,
            kind: ContinueKind::Start,
        });
    };
    let done = last.finished || last.fraction().is_some_and(|f| f >= t.max_fraction);
    Some(if last.is_resumable(t) {
        ContinueTarget {
            episode_id: id,
            kind: ContinueKind::Resume(last.position_secs),
        }
    } else if done {
        match next_episode_id(info, id) {
            Some(next) => ContinueTarget {
                episode_id: next,
                kind: ContinueKind::Next,
            },
            None => ContinueTarget {
                episode_id: first,
                kind: ContinueKind::Again,
            },
        }
    } else {
        ContinueTarget {
            episode_id: id,
            kind: ContinueKind::Start,
        }
    })
}

/// A YouTube video id from a provider's `youtube_trailer` (a bare id or a YouTube URL).
/// Only `[A-Za-z0-9_-]{11}` is accepted, so the browser URL can't be abused.
pub fn youtube_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    let valid = |id: &str| {
        id.len() == 11
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if valid(s) {
        return Some(s.to_owned());
    }
    let lower = s.to_ascii_lowercase();
    let start = ["youtube.com/watch?v=", "youtu.be/", "youtube.com/embed/"]
        .iter()
        .find_map(|m| lower.find(m).map(|p| p + m.len()))?;
    let id: String = s
        .get(start..)?
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    valid(&id).then_some(id)
}

/// The watch page of a validated YouTube id.
pub fn youtube_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_store::{PlaybackKind, PlaybackSnapshot, Timestamp};

    fn ep(id: u64, num: Option<u32>, title: &str) -> Episode {
        Episode {
            id,
            episode_num: num,
            title: title.to_owned(),
            container_extension: Some("mkv".into()),
            season: None,
            added: None,
            direct_source: None,
            info: Default::default(),
        }
    }

    fn info(seasons: &[(u32, &[u64])]) -> SeriesInfo {
        let mut i = SeriesInfo::default();
        for (s, ids) in seasons {
            i.episodes.insert(
                *s,
                ids.iter()
                    .enumerate()
                    .map(|(n, id)| ep(*id, Some(n as u32 + 1), "x"))
                    .collect(),
            );
        }
        i
    }

    fn progress(id: u64, pos: f64, dur: Option<f64>, finished: bool) -> PlaybackProgress {
        PlaybackProgress {
            kind: PlaybackKind::Episode,
            item_id: id.to_string(),
            position_secs: pos,
            duration_secs: dur,
            finished,
            ever_finished: finished,
            updated_at: Timestamp::EPOCH,
            snapshot: PlaybackSnapshot::default(),
        }
    }

    #[test]
    fn titles_tags_and_years() {
        let t = display_title("FR | The Disappearance of Haruhi Suzumiya (2010)");
        assert_eq!(t.tag, Some("FR"));
        assert_eq!(t.title, "The Disappearance of Haruhi Suzumiya");
        assert_eq!(t.year, Some(2010));
        let t = display_title("Pinocchio 3000 (2004)");
        assert_eq!(
            (t.tag, t.title, t.year),
            (None, "Pinocchio 3000", Some(2004))
        );
        assert_eq!(split_year("Blade Runner 2049"), ("Blade Runner 2049", None));
        assert_eq!(split_year("(2004)"), ("(2004)", None));
        assert_eq!(split_year("Title (1)"), ("Title (1)", None));
        assert_eq!(split_year("Title [1999]"), ("Title", Some(1999)));
        assert_eq!(split_year("Title (3021)"), ("Title (3021)", None));
        assert_eq!(year_from_date("2010-05-01"), Some(2010));
        assert_eq!(year_from_date("01/05/1999"), Some(1999));
        assert_eq!(year_from_date("n/a"), None);
        assert_eq!(year_from_date("12345"), None);
        // Release date first, then the name; never `added`.
        assert_eq!(release_year(Some("1999-01-01"), "X (2004)"), Some(1999));
        assert_eq!(release_year(None, "X (2004)"), Some(2004));
        assert_eq!(release_year(Some(""), "X"), None);
    }

    #[test]
    fn meta_lines_and_ratings() {
        assert_eq!(meta_line(["FR", "", " 2004 "]), "FR · 2004");
        assert_eq!(
            rating_label(Some(7.44)),
            Some(format!("{} 7.4", icons::STAR))
        );
        assert_eq!(rating_label(Some(0.0)), None);
        assert_eq!(rating_label(Some(f64::NAN)), None);
        let m: VodStream = serde_json::from_str(
            r#"{"stream_id":1,"name":"FR | Pinocchio 3000 (2004)","rating":"7.4","added":"1760000000"}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            movie_card_meta(&m),
            format!("FR · 2004 · {} 7.4", icons::STAR)
        );
        assert_eq!(quality_tags("Nature 4K UHD HDR"), vec!["4K", "HDR"]);
        assert_eq!(quality_tags("EN | Movie FHD"), vec!["HD"]);
        assert!(quality_tags("Hidden Valley").is_empty());
        assert_eq!(parse_duration("01:45:00"), Some(6300));
        assert_eq!(parse_duration("45:30"), Some(2730));
        assert_eq!(parse_duration("95"), Some(5700));
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn sort_options_map_to_catalogue_orders() {
        assert_eq!(VodSort::Recent.order(), SortOrder::RecentlyAdded);
        assert_eq!(VodSort::Rating.order(), SortOrder::TopRated);
        assert_eq!(VodSort::ALL.len(), 4);
        assert_eq!(VodSort::default(), VodSort::Server);
    }

    #[test]
    fn episode_titles_and_codes() {
        assert_eq!(
            episode_title("Show Name - S01E02 - The Pilot", "Show Name", 2),
            "The Pilot"
        );
        assert_eq!(
            episode_title("Show Name - S01E02", "FR | Show Name", 2),
            "Episode 2"
        );
        assert_eq!(episode_title("S2 E5: Return", "Other", 5), "Return");
        assert_eq!(episode_title("1x03 Night", "Other", 3), "Night");
        assert_eq!(episode_title("Sunrise", "Other", 1), "Sunrise");
        assert_eq!(episode_title("", "Other", 4), "Episode 4");
        assert_eq!(episode_code(0, 2), "Special 2");
        assert_eq!(episode_code(3, 7), "S3·E7");
        assert_eq!(season_label(0), "Specials");
        assert_eq!(season_label(2), "Season 2");
    }

    #[test]
    fn series_names_fall_back_to_episode_titles() {
        let mut i = info(&[(1, &[1])]);
        if let Some(e) = i.episodes.get_mut(&1).and_then(|v| v.first_mut()) {
            e.title = "My Show - S01E01 - Pilot".into();
        }
        assert_eq!(series_name(&i), "My Show");
        i.info.name = Some("Named".into());
        assert_eq!(series_name(&i), "Named");
        assert_eq!(series_name(&info(&[(1, &[1])])), "");
    }

    #[test]
    fn next_episode_handles_specials_and_gaps() {
        // Season 0 (specials), seasons 1 and 3 (gap at 2).
        let i = info(&[(0, &[900, 901]), (1, &[100, 101]), (3, &[300])]);
        assert_eq!(next_episode_id(&i, 100), Some(101));
        assert_eq!(next_episode_id(&i, 101), Some(300)); // across the gap
        assert_eq!(next_episode_id(&i, 300), None);
        assert_eq!(next_episode_id(&i, 901), Some(100)); // specials chain into season 1
        assert_eq!(next_episode_id(&i, 12345), None);
        assert_eq!(first_episode_id(&i), Some(100));
        assert_eq!(first_episode_id(&info(&[(0, &[7])])), Some(7));
        assert_eq!(first_episode_id(&SeriesInfo::default()), None);
        // Missing episode numbers fall back to the position.
        let mut j = SeriesInfo::default();
        j.episodes.insert(
            1,
            vec![ep(1, None, "a"), ep(2, None, "b"), ep(3, None, "c")],
        );
        assert_eq!(next_episode_id(&j, 2), Some(3));
    }

    #[test]
    fn continue_targets() {
        let t = ResumeThresholds::default();
        let i = info(&[(1, &[100, 101, 102])]);
        let c = |p: Option<PlaybackProgress>| continue_target(&i, p.as_ref(), &t);
        assert_eq!(
            c(None),
            Some(ContinueTarget {
                episode_id: 100,
                kind: ContinueKind::Start
            })
        );
        assert_eq!(
            c(Some(progress(101, 600.0, Some(2400.0), false))),
            Some(ContinueTarget {
                episode_id: 101,
                kind: ContinueKind::Resume(600.0)
            })
        );
        assert_eq!(
            c(Some(progress(101, 2400.0, Some(2400.0), true))),
            Some(ContinueTarget {
                episode_id: 102,
                kind: ContinueKind::Next
            })
        );
        assert_eq!(
            c(Some(progress(102, 2390.0, Some(2400.0), false))),
            Some(ContinueTarget {
                episode_id: 100,
                kind: ContinueKind::Again
            })
        );
        assert_eq!(
            c(Some(progress(101, 10.0, Some(2400.0), false))),
            Some(ContinueTarget {
                episode_id: 101,
                kind: ContinueKind::Start
            })
        );
        // An episode the provider no longer lists → the first one.
        assert_eq!(
            c(Some(progress(999, 600.0, None, false))).map(|c| c.episode_id),
            Some(100)
        );
        assert_eq!(continue_target(&SeriesInfo::default(), None, &t), None);
    }

    #[test]
    fn youtube_ids_are_validated() {
        assert_eq!(youtube_id("dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(
            youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            youtube_id("https://youtu.be/dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        for bad in [
            "",
            "short",
            "dQw4w9WgXcQ\" & calc",
            "javascript:alert(1)",
            "a b c d e f g",
        ] {
            assert_eq!(youtube_id(bad), None, "{bad}");
        }
        assert_eq!(
            youtube_url("dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }
}
