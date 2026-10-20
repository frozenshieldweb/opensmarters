// SPDX-License-Identifier: GPL-3.0-or-later
//! Catch-up day tabs: archived programmes grouped by the **viewer's** calendar date (the
//! times on screen are the viewer's too), newest day first and, inside a day, newest
//! programme first, labelled "Today", "Yesterday", then weekday + date.
//!
//! The server's wall clock only matters for the timeshift URL
//! ([`ArchiveProgramme::start_local`]): grouping by it made "Today" start at 16:00
//! yesterday for a viewer six hours behind a +01:00 panel.

use opensmarters_core::ArchiveProgramme;
use opensmarters_core::chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc, Weekday};
use opensmarters_core::epg::ServerTimeZone;

use crate::services::session::guide::EpgRow;

/// One day of archive.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    /// The viewer's calendar date.
    pub date: NaiveDate,
    /// Tab label.
    pub label: String,
    /// Programmes of that day, newest first.
    pub programmes: Vec<ArchiveProgramme>,
}

fn weekday_short(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

/// "Today", "Yesterday", else "Sun 18 Oct" (relative to the viewer's `today`).
pub fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    match (today - date).num_days() {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        -1 => "Tomorrow".to_owned(),
        _ => format!(
            "{} {} {}",
            weekday_short(date.weekday()),
            date.day(),
            date.format("%b")
        ),
    }
}

/// Groups the replayable programmes of `row` (archive flag set, already started) into
/// day tabs on the `viewer`'s clock (the app passes `Local`), newest day and newest
/// programme first. `server` only fills in the timeshift start of each programme.
pub fn archive_days<Z: TimeZone>(
    row: &EpgRow,
    server: &ServerTimeZone,
    now: DateTime<Utc>,
    viewer: &Z,
) -> Vec<Day> {
    let mut archived: Vec<ArchiveProgramme> = (0..row.programmes.len())
        .filter_map(|i| row.archive(i, server, now))
        .collect();
    archived.sort_by(|a, b| b.start.cmp(&a.start));
    let today = now.with_timezone(viewer).date_naive();
    let mut days: Vec<Day> = Vec::new();
    for p in archived {
        let date = p.start.with_timezone(viewer).date_naive();
        match days.last_mut() {
            Some(d) if d.date == date => d.programmes.push(p),
            _ => days.push(Day {
                date,
                label: day_label(date, today),
                programmes: vec![p],
            }),
        }
    }
    days
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::services::catalog::Programme;
    use opensmarters_core::chrono::FixedOffset;
    use opensmarters_core::chrono_tz::America;

    fn utc(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, d, h, m, 0)
            .single()
            .unwrap_or_default()
    }

    fn prog(a: DateTime<Utc>, b: DateTime<Utc>, archive: bool) -> Programme {
        Programme {
            title: format!("{a}"),
            description: String::new(),
            start: a,
            stop: b,
            has_archive: archive,
        }
    }

    fn hours(h: i32) -> FixedOffset {
        FixedOffset::east_opt(h * 3600).expect("valid offset")
    }

    /// `(label, ["DD HH:MM" on the viewer's clock, …])` per day.
    fn summary<Z: TimeZone>(days: &[Day], viewer: &Z) -> Vec<(String, Vec<String>)> {
        days.iter()
            .map(|d| {
                (
                    d.label.clone(),
                    d.programmes
                        .iter()
                        .map(|p| {
                            p.start
                                .with_timezone(viewer)
                                .naive_local()
                                .format("%d %H:%M")
                                .to_string()
                        })
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn labels() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap_or_default();
        let d = |n| NaiveDate::from_ymd_opt(2026, 10, n).unwrap_or_default();
        assert_eq!(day_label(d(20), today), "Today");
        assert_eq!(day_label(d(19), today), "Yesterday");
        assert_eq!(day_label(d(21), today), "Tomorrow");
        assert_eq!(day_label(d(18), today), "Sun 18 Oct");
        assert_eq!(day_label(d(5), today), "Mon 5 Oct");
    }

    #[test]
    fn utc_server_north_american_viewer() {
        // The provider runs on UTC; the viewer is in Chicago (CDT, UTC−5 in October).
        // Now: 15:00Z = 10:00 local on the 20th.
        let server = ServerTimeZone::Utc;
        let viewer = America::Chicago;
        let now = utc(20, 15, 0);
        let row = EpgRow::from_programmes(vec![
            prog(utc(19, 3, 0), utc(19, 4, 0), true), // 18th 22:00 local
            prog(utc(19, 23, 0), utc(20, 3, 0), true), // 19th 18:00 local
            prog(utc(20, 3, 0), utc(20, 4, 30), true), // 19th 22:00 local, 20th on the server
            prog(utc(20, 5, 0), utc(20, 6, 0), true), // 20th 00:00 local
            prog(utc(20, 6, 0), utc(20, 7, 0), false), // no archive
            prog(utc(20, 14, 30), utc(20, 15, 30), true), // on air
            prog(utc(20, 16, 0), utc(20, 17, 0), true), // future
        ]);
        let days = archive_days(&row, &server, now, &viewer);
        let got = summary(&days, &viewer);
        assert_eq!(
            got,
            vec![
                (
                    "Today".to_owned(),
                    vec!["20 09:30".into(), "20 00:00".into()]
                ),
                (
                    "Yesterday".to_owned(),
                    vec!["19 22:00".into(), "19 18:00".into()]
                ),
                ("Sun 18 Oct".to_owned(), vec!["18 22:00".into()]),
            ]
        );
        // The timeshift start stays on the server's clock (UTC here): the 22:00 local
        // programme of "Yesterday" starts on the 20th for the panel.
        assert_eq!(
            days[1].programmes[0].start_local.to_string(),
            "2026-10-20 03:00:00"
        );
    }

    #[test]
    fn panel_ahead_of_the_viewer() {
        // The case from the review: a +01:00 panel and a −06:00 viewer. Now: 20:00Z =
        // 14:00 local. 16:00 local yesterday (22:00Z) is "Yesterday", not "Today".
        let server = ServerTimeZone::Fixed(hours(1));
        let viewer = hours(-6);
        let now = utc(20, 20, 0);
        let row = EpgRow::from_programmes(vec![
            prog(utc(19, 22, 0), utc(19, 23, 0), true), // 19th 16:00 local, 19th 23:00 server
            prog(utc(19, 23, 30), utc(20, 0, 30), true), // 19th 17:30 local, 20th 00:30 server
            prog(utc(20, 6, 0), utc(20, 7, 0), true),   // 20th 00:00 local
            prog(utc(20, 19, 0), utc(20, 19, 45), true), // 20th 13:00 local
        ]);
        let days = archive_days(&row, &server, now, &viewer);
        let got = summary(&days, &viewer);
        assert_eq!(
            got,
            vec![
                (
                    "Today".to_owned(),
                    vec!["20 13:00".into(), "20 00:00".into()]
                ),
                (
                    "Yesterday".to_owned(),
                    vec!["19 17:30".into(), "19 16:00".into()]
                ),
            ]
        );
        assert_eq!(
            days[1].programmes[0].start_local.to_string(),
            "2026-10-20 00:30:00",
            "server wall clock for the timeshift URL"
        );
    }

    #[test]
    fn viewer_dst_change_keeps_viewer_dates() {
        // New York leaves summer time on 1 Nov 2026 at 06:00Z (02:00 EDT → 01:00 EST).
        let server = ServerTimeZone::from_name("Europe/Paris").unwrap_or_default();
        let viewer = America::New_York;
        let now = Utc
            .with_ymd_and_hms(2026, 11, 1, 18, 0, 0)
            .single()
            .unwrap_or_default();
        // Minutes after midnight UTC on 1 Nov.
        let t = |min: i64| {
            Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
                .single()
                .unwrap_or_default()
                + opensmarters_core::chrono::Duration::minutes(min)
        };
        let row = EpgRow::from_programmes(vec![
            // 31 Oct 23:30 EDT = 1 Nov 03:30Z.
            prog(t(210), t(240), true),
            // 1 Nov 00:30 EDT = 04:30Z.
            prog(t(270), t(300), true),
            // 1 Nov 01:30 EST (the second 01:30 of the night) = 06:30Z.
            prog(t(390), t(420), true),
        ]);
        let days = archive_days(&row, &server, now, &viewer);
        let dates: Vec<String> = days.iter().map(|d| d.date.to_string()).collect();
        assert_eq!(dates, ["2026-11-01", "2026-10-31"]);
        assert_eq!(days[0].label, "Today");
        assert_eq!(days[0].programmes.len(), 2);
        assert_eq!(days[1].label, "Yesterday");
        assert_eq!(days[0].programmes[0].duration_minutes(), 30);
    }
}
