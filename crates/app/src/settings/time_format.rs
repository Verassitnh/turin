//! The relative-time helpers of upstream/apps/web/src/timestampFormat.ts the
//! settings pages use.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeTimeParts {
    pub value: String,
    pub suffix: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelativeTimeState {
    Missing,
    Invalid,
    Relative(RelativeTimeParts),
}

fn parse(iso: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// `formatRelativeTime` against an explicit `now`.
pub fn format_relative_time(iso: &str, now: DateTime<Utc>) -> Option<RelativeTimeParts> {
    let date = parse(iso)?;
    let diff = now.signed_duration_since(date);
    let just_now = RelativeTimeParts {
        value: "just now".into(),
        suffix: None,
    };
    if diff.num_milliseconds() < 0 {
        return Some(just_now);
    }
    let seconds = diff.num_seconds();
    if seconds < 60 {
        return Some(just_now);
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return Some(RelativeTimeParts {
            value: format!("{minutes}m"),
            suffix: Some("ago"),
        });
    }
    let hours = minutes / 60;
    if hours < 24 {
        return Some(RelativeTimeParts {
            value: format!("{hours}h"),
            suffix: Some("ago"),
        });
    }
    Some(RelativeTimeParts {
        value: format!("{}d", hours / 24),
        suffix: Some("ago"),
    })
}

pub fn format_relative_time_label(iso: &str, now: DateTime<Utc>) -> String {
    match format_relative_time(iso, now) {
        None => String::new(),
        Some(RelativeTimeParts {
            value,
            suffix: Some(suffix),
        }) => format!("{value} {suffix}"),
        Some(RelativeTimeParts {
            value,
            suffix: None,
        }) => value,
    }
}

pub fn get_relative_time_state(iso: Option<&str>, now: DateTime<Utc>) -> RelativeTimeState {
    match iso {
        None | Some("") => RelativeTimeState::Missing,
        Some(iso) => match format_relative_time(iso, now) {
            Some(parts) => RelativeTimeState::Relative(parts),
            None => RelativeTimeState::Invalid,
        },
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 15, h, m, s).unwrap()
    }

    #[test]
    fn formats_relative_time_like_upstream() {
        let iso = "2026-10-15T10:00:00.000Z";
        assert_eq!(format_relative_time_label(iso, at(10, 0, 30)), "just now");
        assert_eq!(format_relative_time_label(iso, at(10, 5, 0)), "5m ago");
        assert_eq!(format_relative_time_label(iso, at(13, 0, 0)), "3h ago");
        assert_eq!(
            format_relative_time_label(iso, Utc.with_ymd_and_hms(2026, 10, 18, 10, 0, 0).unwrap()),
            "3d ago"
        );
        // A future instant reads as just now.
        assert_eq!(format_relative_time_label(iso, at(9, 0, 0)), "just now");
        assert_eq!(format_relative_time_label("nope", at(9, 0, 0)), "");
        assert_eq!(
            get_relative_time_state(None, at(9, 0, 0)),
            RelativeTimeState::Missing
        );
        assert_eq!(
            get_relative_time_state(Some("nope"), at(9, 0, 0)),
            RelativeTimeState::Invalid
        );
    }
}
