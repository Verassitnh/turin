//! Timestamp helpers for the view-model layer.
//!
//! Upstream keeps timestamps as ISO strings (`DateTime.formatIso`) and turns
//! them into numbers with `Date.parse` / `DateTime.toEpochMillis`. Contracts'
//! [`UtcDateTime`] already encodes `YYYY-MM-DDTHH:MM:SS.sssZ`, so presentation
//! just formats it.

use crate::state::contracts::UtcDateTime;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};

/// `DateTime.formatIso`.
pub fn iso(value: &UtcDateTime) -> String {
    value.to_string()
}

/// `nullableIso`.
pub fn nullable_iso(value: Option<&UtcDateTime>) -> Option<String> {
    value.map(iso)
}

/// `DateTime.toEpochMillis`.
pub fn epoch_millis(value: &UtcDateTime) -> i64 {
    value.as_datetime().timestamp_millis()
}

/// JS `Date.parse` for the ISO strings the app stores: RFC 3339 with an
/// offset, a zone-less date-time (local time, as JS reads it) or a bare date
/// (UTC). `None` stands in for `NaN`.
pub fn parse_ms(value: &str) -> Option<i64> {
    let value = crate::state::contracts::js_trim(value);
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

/// `Date.now()`.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `new Date(ms).toISOString()`.
pub fn to_iso_string(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_date_parse() {
        assert_eq!(
            parse_ms("2026-03-09T10:00:00.000Z"),
            Some(1_773_050_400_000)
        );
        assert_eq!(parse_ms("2026-03-09"), Some(1_773_014_400_000));
        assert_eq!(parse_ms("nope"), None);
        assert_eq!(parse_ms("   "), None);
    }

    #[test]
    fn formats_iso_with_millis() {
        assert_eq!(to_iso_string(1_773_050_400_123), "2026-03-09T10:00:00.123Z");
    }
}
