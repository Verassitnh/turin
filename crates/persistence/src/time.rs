//! UTC timestamps stored in `*_at` columns.
//!
//! Upstream sorts and compares these as text, so the spelling is load-bearing:
//! `YYYY-MM-DDTHH:MM:SS.sssZ` (UTC, millisecond precision, `Z` suffix).

use chrono::{SecondsFormat, Utc};

/// Format `time` the way upstream's ISO date-time columns are written.
pub fn format_utc_millis(time: chrono::DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn utc_millis_now() -> String {
    format_utc_millis(Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamp_is_utc_millis_with_z() {
        let time =
            Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap() + chrono::Duration::milliseconds(6);
        assert_eq!(format_utc_millis(time), "2026-01-02T03:04:05.006Z");
    }
}
