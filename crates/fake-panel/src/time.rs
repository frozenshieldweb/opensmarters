//! Server clock helpers: the panel pretends to live in `Europe/Paris`.
//!
//! The EU daylight-saving rule (last Sunday of March 01:00 UTC to last Sunday of October
//! 01:00 UTC) is implemented directly so no timezone database is needed.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, Utc};

/// The timezone the fake panel reports in `server_info.timezone`.
pub const TIMEZONE: &str = "Europe/Paris";

const CET: i32 = 3600;
const CEST: i32 = 7200;

/// Current time as epoch seconds.
pub(crate) fn now() -> i64 {
    Utc::now().timestamp()
}

/// Current time as epoch milliseconds.
pub(crate) fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Epoch seconds of the last Sunday of `month` in `year`, at 01:00 UTC.
fn last_sunday_0100_utc(year: i32, month: u32) -> Option<i64> {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let last_day = NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?;
    let back = i64::from(last_day.weekday().num_days_from_sunday());
    let sunday = last_day - Duration::days(back);
    Some(sunday.and_hms_opt(1, 0, 0)?.and_utc().timestamp())
}

/// UTC offset (seconds) of Europe/Paris at the given instant.
pub(crate) fn paris_offset(utc: i64) -> i32 {
    let Some(dt) = DateTime::<Utc>::from_timestamp(utc, 0) else {
        return CET;
    };
    let year = dt.year();
    match (
        last_sunday_0100_utc(year, 3),
        last_sunday_0100_utc(year, 10),
    ) {
        (Some(start), Some(end)) if utc >= start && utc < end => CEST,
        _ => CET,
    }
}

fn local_datetime(utc: i64) -> Option<DateTime<FixedOffset>> {
    let off = FixedOffset::east_opt(paris_offset(utc))?;
    Some(DateTime::<Utc>::from_timestamp(utc, 0)?.with_timezone(&off))
}

/// `YYYY-MM-DD HH:MM:SS` in server-local time (what panels put in EPG `start`/`end`).
pub(crate) fn local_string(utc: i64) -> String {
    local_datetime(utc)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// XMLTV timestamp: `YYYYMMDDHHMMSS +HHMM` in server-local time.
pub(crate) fn xmltv_string(utc: i64) -> String {
    local_datetime(utc)
        .map(|d| d.format("%Y%m%d%H%M%S %z").to_string())
        .unwrap_or_default()
}

/// Convert a server-local wall-clock time to epoch seconds.
///
/// Ambiguous times (the repeated hour in October) resolve to the earlier instant; times in
/// the spring-forward gap are interpreted with the winter offset.
pub(crate) fn local_to_utc(local: NaiveDateTime) -> i64 {
    let wall = local.and_utc().timestamp();
    for off in [CEST, CET] {
        let candidate = wall - i64::from(off);
        if paris_offset(candidate) == off {
            return candidate;
        }
    }
    wall - i64::from(CET)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dst_boundaries_2026() {
        // 2026-03-29 01:00 UTC switch to CEST, 2026-10-25 01:00 UTC back to CET.
        let spring = 1_774_746_000; // 2026-03-29T01:00:00Z
        assert_eq!(paris_offset(spring - 1), CET);
        assert_eq!(paris_offset(spring), CEST);
        let autumn = 1_792_890_000; // 2026-10-25T01:00:00Z
        assert_eq!(paris_offset(autumn - 1), CEST);
        assert_eq!(paris_offset(autumn), CET);
    }

    #[test]
    fn formats() {
        let t = 1_792_486_800; // 2026-10-20T09:00:00Z
        assert_eq!(local_string(t), "2026-10-20 11:00:00");
        assert_eq!(xmltv_string(t), "20261020110000 +0200");
        let back = NaiveDateTime::parse_from_str("2026-10-20 11:00:00", "%Y-%m-%d %H:%M:%S")
            .map(local_to_utc);
        assert_eq!(back.ok(), Some(t));
    }
}
