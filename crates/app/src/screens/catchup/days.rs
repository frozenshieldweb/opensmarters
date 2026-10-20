// SPDX-License-Identifier: GPL-3.0-or-later
//! Catch-up day tabs: archived programmes grouped by the **server-local** date (what
//! the panel's archive is organised by), newest first, labelled "Today", "Yesterday",
//! then weekday + date.

use opensmarters_core::ArchiveProgramme;
use opensmarters_core::chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use opensmarters_core::epg::ServerTimeZone;
use opensmarters_core::model::group_by_server_date;

use crate::services::session::guide::EpgRow;

/// One day of archive.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    /// Server-local date.
    pub date: NaiveDate,
    /// Tab label.
    pub label: String,
    /// Programmes of that day, oldest first.
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

/// "Today", "Yesterday", else "Sun 18 Oct" (relative to the server-local `today`).
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
/// day tabs, newest day first.
pub fn archive_days(row: &EpgRow, tz: &ServerTimeZone, now: DateTime<Utc>) -> Vec<Day> {
    let archived: Vec<ArchiveProgramme> = (0..row.programmes.len())
        .filter_map(|i| row.archive(i, tz, now))
        .collect();
    let today = tz.to_local(now).date();
    group_by_server_date(archived)
        .into_iter()
        .rev()
        .map(|(date, mut programmes)| {
            programmes.sort_by_key(|p| p.start);
            Day {
                date,
                label: day_label(date, today),
                programmes,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::catalog::Programme;
    use opensmarters_core::chrono::TimeZone;

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
    fn grouped_by_server_date_newest_first() {
        // Server in Paris (UTC+2 in October until the 25th). 22:30Z on the 19th is
        // 00:30 on the 20th in Paris: it belongs to "Today" on the server.
        let tz = ServerTimeZone::from_name("Europe/Paris").unwrap_or_default();
        let now = utc(20, 12, 0);
        let row = EpgRow::from_programmes(vec![
            prog(utc(18, 8, 0), utc(18, 9, 0), true),
            prog(utc(19, 21, 0), utc(19, 22, 30), true),
            prog(utc(19, 22, 30), utc(19, 23, 30), true),
            prog(utc(20, 9, 0), utc(20, 10, 0), false), // no archive
            prog(utc(20, 11, 30), utc(20, 12, 30), true), // on air: replayable from start
            prog(utc(20, 13, 0), utc(20, 14, 0), true), // future
        ]);
        let days = archive_days(&row, &tz, now);
        let labels: Vec<&str> = days.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["Today", "Yesterday", "Sun 18 Oct"]);
        assert_eq!(days[0].programmes.len(), 2);
        assert_eq!(days[0].programmes[0].start, utc(19, 22, 30));
        assert_eq!(
            days[0].programmes[0].start_local.to_string(),
            "2026-10-20 00:30:00"
        );
        assert_eq!(days[1].programmes.len(), 1);
    }

    #[test]
    fn dst_change_keeps_server_dates() {
        // Paris leaves summer time on 25 Oct 2026 at 01:00Z (03:00 CEST → 02:00 CET).
        let tz = ServerTimeZone::from_name("Europe/Paris").unwrap_or_default();
        let now = utc(26, 12, 0);
        let row = EpgRow::from_programmes(vec![
            // 23:30Z on the 24th = 01:30 CEST on the 25th.
            prog(utc(24, 23, 30), utc(25, 0, 30), true),
            // 23:30Z on the 25th = 00:30 CET on the 26th.
            prog(utc(25, 23, 30), utc(26, 0, 30), true),
        ]);
        let days = archive_days(&row, &tz, now);
        let dates: Vec<String> = days.iter().map(|d| d.date.to_string()).collect();
        assert_eq!(dates, ["2026-10-26", "2026-10-25"]);
        assert_eq!(days[0].label, "Today");
        assert_eq!(days[1].label, "Yesterday");
        // The timeshift start is the server wall clock, which moved back an hour.
        assert_eq!(
            days[1].programmes[0].start_local.to_string(),
            "2026-10-25 01:30:00"
        );
        assert_eq!(days[1].programmes[0].duration_minutes(), 60);
    }
}
