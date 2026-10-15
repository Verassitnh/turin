//! Port of upstream/packages/shared/src/dateTime.ts

use std::cmp::Ordering;

use chrono::{Datelike, NaiveDate};

use crate::js_string::code_unit_cmp;

/// Upstream pattern:
/// `/^(?:\d{4}|[+-]\d{6})-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])T(?:(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d(?:\.\d+)?)?|24:00(?::00(?:\.0+)?)?)(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/`
/// checked with `Schema.isTrimmed()`, so surrounding whitespace fails too.
fn zoned_iso_date_time_regex() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"^(?P<year>\d{4}|[+-]\d{6})-(?P<month>0[1-9]|1[0-2])-(?P<day>0[1-9]|[12]\d|3[01])T(?P<time>(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d(?:\.\d+)?)?|24:00(?::00(?:\.0+)?)?)(?P<zone>Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$",
        )
        .expect("static regex is valid")
    })
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

/// Milliseconds since the Unix epoch, or `None` for anything the upstream
/// schema or its calendar round-trip would reject.
fn parse_timestamp(value: &str) -> Option<i64> {
    if value != turin_contracts::base_schemas::js_trim(value) {
        return None;
    }
    let captures = zoned_iso_date_time_regex().captures(value)?;

    // Engines can normalize invalid calendar dates instead of rejecting them,
    // so round-trip the date part like upstream (`DateTime.make` + parts
    // comparison) before trusting the match.
    let year_text = captures.name("year")?.as_str();
    let year: i32 = if let Some(rest) = year_text.strip_prefix('+') {
        rest.parse().ok()?
    } else if let Some(rest) = year_text.strip_prefix('-') {
        -rest.parse::<i32>().ok()?
    } else {
        year_text.parse().ok()?
    };
    let month: u32 = captures.name("month")?.as_str().parse().ok()?;
    let day: u32 = captures.name("day")?.as_str().parse().ok()?;
    if day > days_in_month(year, month) {
        return None;
    }
    let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else {
        return None;
    };

    let time = captures.name("time")?.as_str();
    let (hour_text, rest) = time.split_once(':')?;
    let hour: i64 = hour_text.parse().ok()?;
    let (minute_text, rest) = rest.split_once(':').unwrap_or((rest, ""));
    let minute: i64 = if minute_text.len() == 2 { minute_text.parse().ok()? } else { 0 };
    let (second_text, fraction) = match rest.split_once('.') {
        Some((second_text, fraction)) => (second_text, Some(fraction)),
        None => (rest, None),
    };
    let second: i64 = if second_text.is_empty() {
        0
    } else {
        second_text.parse().ok()?
    };
    // JS Date keeps milliseconds; longer fractions truncate toward zero.
    let milliseconds: i64 = match fraction {
        Some(fraction) => {
            let digits: String = fraction.chars().take(3).collect();
            let padded = format!("{digits:0<3}");
            padded.parse().ok()?
        }
        None => 0,
    };

    let zone = captures.name("zone")?.as_str();
    let offset_millis: i64 = match zone {
        "Z" => 0,
        offset => {
            let sign: i64 = if offset.starts_with('-') { -1 } else { 1 };
            let (offset_hour, offset_minute) = offset[1..].split_once(':')?;
            sign
                * (offset_hour.parse::<i64>().ok()? * 60 + offset_minute.parse::<i64>().ok()?)
                * 60_000
        }
    };

    let days_since_epoch = NaiveDate::from_ymd_opt(1970, 1, 1)?
        .signed_duration_since(date)
        .num_days();
    let time_of_day_millis =
        (hour * 60 + minute) * 60_000 + second * 1_000 + milliseconds;
    Some(days_since_epoch * 86_400_000 + time_of_day_millis - offset_millis)
}

/// Compare date-time strings by absolute time, with stable handling for
/// malformed stored values.
pub fn compare_date_time_strings(left: &str, right: &str) -> Ordering {
    let left_timestamp = parse_timestamp(left);
    let right_timestamp = parse_timestamp(right);

    match (left_timestamp, right_timestamp) {
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (Some(left_timestamp), Some(right_timestamp)) => {
            left_timestamp.cmp(&right_timestamp)
        }
        (None, None) => code_unit_cmp(left, right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_valid_date_time_strings_by_absolute_time() {
        assert_eq!(
            compare_date_time_strings("2026-09-01T12:00:00.000Z", "2026-09-01T05:00:00.000-07:00"),
            Ordering::Equal
        );
        assert_eq!(
            compare_date_time_strings("2026-09-01T12:00:01.000Z", "2026-09-01T12:00:00.000Z"),
            Ordering::Greater
        );
    }

    #[test]
    fn sorts_malformed_values_before_valid_values() {
        assert_eq!(
            compare_date_time_strings("invalid", "2026-09-01T12:00:00.000Z"),
            Ordering::Less
        );
        assert_eq!(
            compare_date_time_strings("2026-09-01T12:00:00.000Z", "invalid"),
            Ordering::Greater
        );
    }

    #[test]
    fn treats_calendar_and_time_edge_cases_as_malformed() {
        let valid = "1970-01-01T00:00:00.000Z";
        let malformed = [
            "2014-02-30",
            "2014-03-02",
            "2014-03-02T00:00:00",
            "2014-03-02T00:00:00.000",
            "03/02/2014",
            "March 2, 2014",
            "Sun, 02 Mar 2014 00:00:00 GMT",
            "2014-03-02T00:00:00.000Z\n",
            "2014-02-30T00:00:00.000Z",
            "1900-02-29T00:00:00.000-07:00",
            "2024-04-31T00:00:00.000+05:30",
            "2024-03-02T12:00:00.000+24:00",
            "2026-09-01T24:01:00Z",
            "2026-09-01T24:00:01Z",
            "2026-09-01T24:00:00.0001Z",
            "2026-09-01T24:01Z",
            "2026-09-01T25:00Z",
        ];
        for value in malformed {
            assert_eq!(compare_date_time_strings(value, valid), Ordering::Less, "{value}");
            assert_eq!(compare_date_time_strings(valid, value), Ordering::Greater, "{value}");
            assert_eq!(compare_date_time_strings(value, "invalid"), Ordering::Less, "{value}");
            assert_eq!(compare_date_time_strings("invalid", value), Ordering::Greater, "{value}");
            assert_eq!(compare_date_time_strings(value, value), Ordering::Equal, "{value}");
        }
    }

    #[test]
    fn preserves_equal_iso_instants_table() {
        let equal_pairs = [
            ("2024-02-29T12:00:00Z", "2024-02-29T17:30:00+05:30"),
            ("2000-02-29T00:00:00.100Z", "2000-02-28T20:30:00.1-03:30"),
            ("0000-01-01T00:00:00.000Z", "+000000-01-01T00:00:00.000+00:00"),
            ("+010000-01-01T00:00:00.000Z", "9999-12-31T23:00:00.000-01:00"),
            ("2026-09-01T24:00:00Z", "2026-09-02T00:00:00Z"),
            ("2026-09-01T24:00:00.0000Z", "2026-09-02T00:00:00.000Z"),
            ("2024-02-29T24:00:00+05:30", "2024-03-01T00:00:00+05:30"),
            ("2026-12-31T24:00:00-07:00", "2027-01-01T07:00:00Z"),
            ("2026-09-01T12:00Z", "2026-09-01T12:00:00.000Z"),
            ("2026-09-01T05:00-07:00", "2026-09-01T12:00:00Z"),
            ("2026-09-01T24:00Z", "2026-09-02T00:00:00Z"),
            ("2026-09-01T24:00+05:30", "2026-09-02T00:00:00+05:30"),
        ];
        for (left, right) in equal_pairs {
            assert_eq!(compare_date_time_strings(left, right), Ordering::Equal, "{left} vs {right}");
        }
    }

    #[test]
    fn uses_code_unit_order_for_malformed_date_time_strings() {
        assert_eq!(
            compare_date_time_strings("invalid-a", "invalid-B"),
            Ordering::Greater
        );
        assert_eq!(
            compare_date_time_strings("invalid-B", "invalid-a"),
            Ordering::Less
        );
    }

    #[test]
    fn returns_zero_for_equal_malformed_date_time_strings() {
        assert_eq!(compare_date_time_strings("invalid", "invalid"), Ordering::Equal);
    }

    #[test]
    fn gives_every_permutation_of_mixed_values_the_same_order() {
        let early = "2026-09-01T12:00:00.000+14:00";
        let late = "2026-09-01T00:00:00.000-12:00";
        let malformed = "2026-09-01T06:invalid";
        let expected = [malformed, early, late];

        let permutations = [
            [early, late, malformed],
            [early, malformed, late],
            [late, early, malformed],
            [late, malformed, early],
            [malformed, early, late],
            [malformed, late, early],
        ];

        for values in permutations {
            let mut sorted = values;
            sorted.sort_by(compare_date_time_strings);
            assert_eq!(sorted, expected);
        }
    }
}
