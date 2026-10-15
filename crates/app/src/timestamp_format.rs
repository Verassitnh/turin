//! Port of upstream/apps/web/src/timestampFormat.ts (the parts the sidebar
//! reads: parsing, relative labels, short wall-clock times).
//!
//! Timestamps stay ISO strings end to end (PORTING: `IsoDateTime` is passed
//! through untouched); this module is where they become numbers. `Date.parse`
//! becomes [`parse_timestamp_ms`], `Intl.DateTimeFormat` becomes chrono's
//! local-time formatting with the en-US shapes upstream renders by default.
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};

/// `Settings → General → Timestamp format` (contracts `TimestampFormat`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimestampFormat {
    /// The locale default (en-US: 12-hour).
    #[default]
    Locale,
    TwelveHour,
    TwentyFourHour,
}

/// `Date.parse` for the ISO strings the app stores: RFC 3339 with an offset,
/// a zone-less date-time (local time, as JS reads it) or a bare date (UTC).
/// `None` stands in for `NaN`.
pub fn parse_timestamp_ms(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.timestamp_millis());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, format) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|local| local.timestamp_millis());
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc().timestamp_millis())
}

/// `parseTimestampDate`.
pub fn parse_timestamp_date(value: &str) -> Option<DateTime<Utc>> {
    parse_timestamp_ms(value).and_then(DateTime::from_timestamp_millis)
}

/// `Date#toISOString()`: `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub fn to_iso_string(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `new Date().toISOString()`.
pub fn now_iso() -> String {
    to_iso_string(now_ms())
}

/// `Date.now()`.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// `formatShortTimestamp`: wall-clock time without seconds (`9:05 AM`, `09:05`).
pub fn format_short_timestamp(iso: &str, format: TimestampFormat) -> String {
    match parse_timestamp_date(iso) {
        Some(date) => format_time_of_day(&date.with_timezone(&Local), format),
        None => String::new(),
    }
}

/// The `hour: "numeric", minute: "2-digit"` shape for a local time.
pub fn format_time_of_day<Tz: TimeZone>(date: &DateTime<Tz>, format: TimestampFormat) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match format {
        TimestampFormat::Locale | TimestampFormat::TwelveHour => {
            date.format("%-I:%M %p").to_string()
        }
        TimestampFormat::TwentyFourHour => date.format("%H:%M").to_string(),
    }
}

/// `formatRelativeTime` as `(value, suffix)`.
pub fn format_relative_time(iso: &str, now_ms: i64) -> Option<(String, Option<&'static str>)> {
    let date = parse_timestamp_ms(iso)?;
    let diff_ms = now_ms - date;
    if diff_ms < 0 {
        return Some(("just now".into(), None));
    }
    let seconds = diff_ms / 1000;
    if seconds < 60 {
        return Some(("just now".into(), None));
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return Some((format!("{minutes}m"), Some("ago")));
    }
    let hours = minutes / 60;
    if hours < 24 {
        return Some((format!("{hours}h"), Some("ago")));
    }
    Some((format!("{}d", hours / 24), Some("ago")))
}

/// `formatRelativeTimeLabel`: `"just now"`, `"5m ago"`, `"3d ago"`.
pub fn format_relative_time_label(iso: &str, now_ms: i64) -> String {
    match format_relative_time(iso, now_ms) {
        None => String::new(),
        Some((value, Some(suffix))) => format!("{value} {suffix}"),
        Some((value, None)) => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_strings_like_date_parse() {
        assert_eq!(
            parse_timestamp_ms("2026-03-09T10:00:00.000Z"),
            Some(1_773_050_400_000)
        );
        assert_eq!(
            parse_timestamp_ms("2026-03-09T11:00:00+01:00"),
            Some(1_773_050_400_000)
        );
        assert_eq!(parse_timestamp_ms("invalid"), None);
        assert_eq!(parse_timestamp_ms(""), None);
        assert_eq!(parse_timestamp_ms("2026-03-09"), Some(1_773_014_400_000));
    }

    #[test]
    fn iso_round_trip() {
        assert_eq!(to_iso_string(1_773_050_400_000), "2026-03-09T10:00:00.000Z");
    }

    #[test]
    fn relative_labels() {
        let now = parse_timestamp_ms("2026-03-09T10:00:00.000Z").unwrap();
        assert_eq!(
            format_relative_time_label("2026-03-09T09:59:30.000Z", now),
            "just now"
        );
        assert_eq!(
            format_relative_time_label("2026-03-09T10:00:30.000Z", now),
            "just now"
        );
        assert_eq!(
            format_relative_time_label("2026-03-09T09:55:00.000Z", now),
            "5m ago"
        );
        assert_eq!(
            format_relative_time_label("2026-03-09T07:00:00.000Z", now),
            "3h ago"
        );
        assert_eq!(
            format_relative_time_label("2026-03-06T10:00:00.000Z", now),
            "3d ago"
        );
        assert_eq!(format_relative_time_label("nope", now), "");
    }

    #[test]
    fn time_of_day_shapes() {
        let date = Utc.with_ymd_and_hms(2026, 3, 9, 9, 5, 0).unwrap();
        assert_eq!(
            format_time_of_day(&date, TimestampFormat::Locale),
            "9:05 AM"
        );
        assert_eq!(
            format_time_of_day(&date, TimestampFormat::TwentyFourHour),
            "09:05"
        );
    }
}
