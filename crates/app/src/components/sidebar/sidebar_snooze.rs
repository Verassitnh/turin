//! Port of upstream/apps/web/src/components/Sidebar.snooze.ts
//!
//! The shared snooze presets with times in the user's clock preference, and
//! the human wake description for menus and toasts.
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use chrono::{DateTime, Local, NaiveTime, TimeZone};

pub use super::runtime::thread_settled::SnoozePreset;
use super::runtime::thread_settled::resolve_snooze_presets as resolve_shared_snooze_presets;
use crate::timestamp_format::{TimestampFormat, format_time_of_day, parse_timestamp_ms};

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// `resolveSnoozePresets(now, timestampFormat)`: "Next week" pairs its
/// weekday with the time; the others are time-only.
pub fn resolve_snooze_presets<Tz: TimeZone>(
    now: &DateTime<Tz>,
    timestamp_format: TimestampFormat,
) -> Vec<SnoozePreset>
where
    Tz::Offset: std::fmt::Display,
{
    resolve_shared_snooze_presets(now, timestamp_format)
}

/// Presets for the current local time.
pub fn resolve_snooze_presets_now(timestamp_format: TimestampFormat) -> Vec<SnoozePreset> {
    resolve_snooze_presets(&Local::now(), timestamp_format)
}

/// `snoozeWakeDescription`: "17:30" today, "tomorrow 9:00", "Mon 9:00",
/// "Apr 20, 9:00".
pub fn snooze_wake_description<Tz: TimeZone>(
    snoozed_until: &str,
    now: &DateTime<Tz>,
    timestamp_format: TimestampFormat,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(wake_ms) = parse_timestamp_ms(snoozed_until) else {
        return String::new();
    };
    let tz = now.timezone();
    let Some(wake) = tz.timestamp_millis_opt(wake_ms).single() else {
        return String::new();
    };
    let time = format_time_of_day(&wake, timestamp_format);
    let start_of_today = tz
        .from_local_datetime(&now.date_naive().and_time(NaiveTime::MIN))
        .earliest()
        .map_or(now.timestamp_millis(), |start| start.timestamp_millis());
    let day_delta = (wake_ms - start_of_today).div_euclid(DAY_MS);
    match day_delta {
        0 => time,
        1 => format!("tomorrow {time}"),
        2..=6 => format!("{} {time}", wake.format("%a")),
        _ => format!("{}, {time}", wake.format("%b %-d")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    fn local_date(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .earliest()
            .unwrap()
    }

    fn wake(preset: &SnoozePreset) -> DateTime<Local> {
        Local
            .timestamp_millis_opt(parse_timestamp_ms(&preset.snoozed_until).unwrap())
            .unwrap()
    }

    fn find<'a>(presets: &'a [SnoozePreset], id: &str) -> &'a SnoozePreset {
        presets.iter().find(|p| p.id.as_str() == id).unwrap()
    }

    #[test]
    fn offers_one_hour_three_hours_evening_tomorrow_and_next_week_in_the_morning() {
        let presets =
            resolve_snooze_presets(&local_date(2026, 4, 8, 10, 0), TimestampFormat::Locale);
        assert_eq!(
            presets.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["hour", "three-hours", "evening", "tomorrow", "next-week"]
        );
        assert_eq!(wake(find(&presets, "three-hours")).hour(), 13);
        assert_eq!(wake(find(&presets, "evening")).hour(), 18);
        let tomorrow = wake(find(&presets, "tomorrow"));
        assert_eq!((tomorrow.day(), tomorrow.hour()), (9, 9));
        let next_week = wake(find(&presets, "next-week"));
        assert_eq!(next_week.weekday().num_days_from_sunday(), 1);
        assert_eq!(next_week.day(), 13);
    }

    #[test]
    fn when_label_complements_the_label_instead_of_repeating_it() {
        let presets =
            resolve_snooze_presets(&local_date(2026, 4, 8, 10, 0), TimestampFormat::Locale);
        for preset in &presets {
            assert!(!preset.when_label.to_lowercase().contains("tomorrow"));
        }
        assert!(find(&presets, "tomorrow").when_label.contains('9'));
        assert!(find(&presets, "next-week").when_label.contains("Mon"));
    }

    #[test]
    fn drops_the_evening_preset_once_evening_is_near_or_past() {
        for hour in [(17, 30), (21, 0)] {
            assert_eq!(
                resolve_snooze_presets(
                    &local_date(2026, 4, 8, hour.0, hour.1),
                    TimestampFormat::Locale
                )
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
                vec!["hour", "three-hours", "tomorrow", "next-week"]
            );
        }
    }

    #[test]
    fn puts_next_week_a_full_week_out_when_today_is_monday() {
        let presets =
            resolve_snooze_presets(&local_date(2026, 4, 6, 10, 0), TimestampFormat::Locale);
        let next_week = wake(find(&presets, "next-week"));
        assert_eq!(next_week.weekday().num_days_from_sunday(), 1);
        assert_eq!(next_week.day(), 13);
    }

    #[test]
    fn formats_preset_times_with_the_selected_clock_preference() {
        let now = local_date(2026, 4, 8, 10, 0);
        let twelve = resolve_snooze_presets(&now, TimestampFormat::TwelveHour);
        let twenty_four = resolve_snooze_presets(&now, TimestampFormat::TwentyFourHour);
        assert!(find(&twelve, "evening").when_label.contains("PM"));
        assert_eq!(find(&twenty_four, "evening").when_label, "18:00");
    }

    // describe("snoozeWakeDescription")
    fn iso(date: DateTime<Local>) -> String {
        crate::timestamp_format::to_iso_string(date.timestamp_millis())
    }

    #[test]
    fn uses_bare_time_today_tomorrow_next_day_weekday_within_the_week() {
        let now = local_date(2026, 4, 8, 10, 0);
        assert!(
            !snooze_wake_description(
                &iso(local_date(2026, 4, 8, 18, 0)),
                &now,
                TimestampFormat::Locale
            )
            .contains("tomorrow")
        );
        assert!(
            snooze_wake_description(
                &iso(local_date(2026, 4, 9, 9, 0)),
                &now,
                TimestampFormat::Locale
            )
            .contains("tomorrow")
        );
        assert!(
            snooze_wake_description(
                &iso(local_date(2026, 4, 13, 9, 0)),
                &now,
                TimestampFormat::Locale
            )
            .contains("Mon")
        );
    }

    #[test]
    fn formats_wake_descriptions_with_the_selected_clock_preference() {
        let now = local_date(2026, 4, 8, 10, 0);
        let evening = iso(local_date(2026, 4, 8, 18, 0));
        assert!(
            snooze_wake_description(&evening, &now, TimestampFormat::TwelveHour).contains("PM")
        );
        assert_eq!(
            snooze_wake_description(&evening, &now, TimestampFormat::TwentyFourHour),
            "18:00"
        );
    }
}
