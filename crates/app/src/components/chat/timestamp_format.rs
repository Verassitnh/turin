//! Port of upstream/apps/web/src/timestampFormat.ts (the chat formatters).
//!
//! Upstream formats with `Intl.DateTimeFormat` in the host locale. Turin
//! formats the `locale` setting as en-US (`h:mm AM/PM`, `M/D`), which is what
//! the desktop app shows for the default macOS locale; `12-hour` / `24-hour`
//! follow the setting.

use chrono::{DateTime, Datelike, Local, TimeZone as _};

/// `TimestampFormat` (settings). The 12/24-hour choices arrive with the
/// settings task.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimestampFormat {
    #[default]
    Locale,
    TwelveHour,
    TwentyFourHour,
}

/// `parseTimestampDate`.
pub fn parse_timestamp_date(iso: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|date| date.with_timezone(&Local))
}

fn format_time(date: &DateTime<Local>, format: TimestampFormat, seconds: bool) -> String {
    match format {
        TimestampFormat::TwentyFourHour => {
            if seconds {
                date.format("%H:%M:%S").to_string()
            } else {
                date.format("%H:%M").to_string()
            }
        }
        TimestampFormat::Locale | TimestampFormat::TwelveHour => {
            if seconds {
                date.format("%-I:%M:%S %p").to_string()
            } else {
                date.format("%-I:%M %p").to_string()
            }
        }
    }
}

/// `formatTimestamp` (with seconds).
#[allow(dead_code)]
pub fn format_timestamp(iso: &str, format: TimestampFormat) -> String {
    parse_timestamp_date(iso).map_or_else(String::new, |date| format_time(&date, format, true))
}

/// `formatShortTimestamp`.
pub fn format_short_timestamp(iso: &str, format: TimestampFormat) -> String {
    parse_timestamp_date(iso).map_or_else(String::new, |date| format_time(&date, format, false))
}

fn ordinal_suffix(day: u32) -> &'static str {
    let last_two = day % 100;
    if (11..=13).contains(&last_two) {
        return "th";
    }
    match day % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// `formatChatTimestampTooltip`: `12:04 PM, 4th June 2026`.
pub fn format_chat_timestamp_tooltip(iso: &str, format: TimestampFormat) -> String {
    let Some(date) = parse_timestamp_date(iso) else {
        return String::new();
    };
    let time = format_time(&date, format, false);
    let day = date.day();
    format!(
        "{time}, {day}{} {} {}",
        ordinal_suffix(day),
        date.format("%B"),
        date.year()
    )
}

fn day_diff(from: &DateTime<Local>, to: &DateTime<Local>) -> i64 {
    let start = |date: &DateTime<Local>| {
        Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
            .earliest()
            .map_or(0, |d| d.timestamp_millis())
    };
    // Round so DST-shifted 23/25 hour days still count as whole days.
    ((start(to) - start(from)) as f64 / 86_400_000.0).round() as i64
}

fn numeric_date(date: &DateTime<Local>, now: &DateTime<Local>) -> String {
    if date.year() == now.year() {
        format!("{}/{}", date.month(), date.day())
    } else {
        format!("{}/{}/{}", date.month(), date.day(), date.year())
    }
}

/// `formatDayAwareTimestamp`: today `12:34 PM`, `yesterday at 12:34 PM`,
/// later `8/13 12:34 PM`.
pub fn format_day_aware_timestamp_at(
    iso: &str,
    format: TimestampFormat,
    now: DateTime<Local>,
) -> String {
    let Some(date) = parse_timestamp_date(iso) else {
        return String::new();
    };
    let time = format_time(&date, format, false);
    let diff = day_diff(&date, &now);
    if diff <= 0 {
        return time;
    }
    if diff == 1 {
        return format!("yesterday at {time}");
    }
    format!("{} {time}", numeric_date(&date, &now))
}

pub fn format_day_aware_timestamp(iso: &str, format: TimestampFormat) -> String {
    format_day_aware_timestamp_at(iso, format, Local::now())
}

/// `formatUpcomingTimestamp`.
#[allow(dead_code)]
pub fn format_upcoming_timestamp(iso: &str, format: TimestampFormat) -> String {
    let now = Local::now();
    let Some(date) = parse_timestamp_date(iso) else {
        return String::new();
    };
    let time = format_time(&date, format, false);
    let diff = day_diff(&now, &date);
    if diff < 0 {
        return format_day_aware_timestamp_at(iso, format, now);
    }
    if diff == 0 {
        return time;
    }
    if diff == 1 {
        return format!("tomorrow at {time}");
    }
    format!("{} {time}", numeric_date(&date, &now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinal_suffixes() {
        assert_eq!(ordinal_suffix(1), "st");
        assert_eq!(ordinal_suffix(12), "th");
        assert_eq!(ordinal_suffix(23), "rd");
    }

    #[test]
    fn day_aware_formats() {
        let now = parse_timestamp_date("2026-06-04T12:00:00.000Z").unwrap();
        let same_day = now.to_rfc3339();
        assert!(
            !format_day_aware_timestamp_at(&same_day, TimestampFormat::Locale, now).contains('/')
        );
        let yesterday = (now - chrono::Duration::days(1)).to_rfc3339();
        assert!(
            format_day_aware_timestamp_at(&yesterday, TimestampFormat::Locale, now)
                .starts_with("yesterday at ")
        );
        let older = (now - chrono::Duration::days(10)).to_rfc3339();
        assert!(format_day_aware_timestamp_at(&older, TimestampFormat::Locale, now).contains('/'));
    }
}
