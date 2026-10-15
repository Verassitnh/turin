//! Port of `formatRelativeTime` / `formatRelativeTimeLabel` in upstream/apps/web/src/timestampFormat.ts.
//!
//! Lives here until the shared timestamp module grows them (the sidebar task needs the same
//! helpers); the chat formatters in `components::chat::timestamp_format` only cover day-aware stamps.

use crate::state::time::parse_ms;

/// `{ value: "20s", suffix: "ago" }` or `{ value: "just now", suffix: null }`, so callers can
/// style the numeric portion independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeTimeParts {
    pub value: String,
    pub suffix: Option<&'static str>,
}

/// `formatRelativeTime`, against an explicit clock.
pub fn format_relative_time_at(iso: &str, now_ms: i64) -> Option<RelativeTimeParts> {
    let then = parse_ms(iso)?;
    let diff_ms = now_ms - then;
    if diff_ms < 0 {
        return Some(RelativeTimeParts {
            value: "just now".into(),
            suffix: None,
        });
    }
    let seconds = diff_ms / 1000;
    if seconds < 60 {
        return Some(RelativeTimeParts {
            value: "just now".into(),
            suffix: None,
        });
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

/// `formatRelativeTime`.
pub fn format_relative_time(iso: &str) -> Option<RelativeTimeParts> {
    format_relative_time_at(iso, chrono::Utc::now().timestamp_millis())
}

/// `formatRelativeTimeLabel`: `"5m ago"`, `"just now"`, or `""` for an unparseable stamp.
pub fn format_relative_time_label(iso: &str) -> String {
    match format_relative_time(iso) {
        None => String::new(),
        Some(RelativeTimeParts {
            value,
            suffix: Some(suffix),
        }) => format!("{value} {suffix}"),
        Some(parts) => parts.value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_760_000_000_000;

    fn at(offset_ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(NOW - offset_ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn label(offset_ms: i64) -> Option<(String, Option<&'static str>)> {
        format_relative_time_at(&at(offset_ms), NOW).map(|parts| (parts.value, parts.suffix))
    }

    #[test]
    fn buckets_like_upstream() {
        assert_eq!(label(-5_000), Some(("just now".into(), None)));
        assert_eq!(label(59_000), Some(("just now".into(), None)));
        assert_eq!(label(60_000), Some(("1m".into(), Some("ago"))));
        assert_eq!(label(59 * 60_000), Some(("59m".into(), Some("ago"))));
        assert_eq!(label(3_600_000), Some(("1h".into(), Some("ago"))));
        assert_eq!(label(86_400_000 * 3), Some(("3d".into(), Some("ago"))));
    }

    #[test]
    fn invalid_stamps_have_no_label() {
        assert_eq!(format_relative_time_at("not a date", NOW), None);
        assert_eq!(format_relative_time_label("not a date"), "");
    }
}
