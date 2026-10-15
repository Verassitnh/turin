//! JS string and date semantics local to provider-core.
//!
//! Upstream measures strings in UTF-16 code units and trims with
//! `String.prototype.trim`. `turin_contracts::js_trim` covers trimming; these
//! cover the rest. When `turin-shared` lands (`js_string`), prefer its
//! versions; these are intentionally tiny and equivalent.

use chrono::TimeZone;

/// ECMAScript WhiteSpace + LineTerminator, i.e. JS `\s` and what `trim` strips.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `String.prototype.trimStart`.
pub fn js_trim_start(value: &str) -> &str {
    value.trim_start_matches(is_js_whitespace)
}

/// `String.prototype.length`: UTF-16 code units.
pub fn utf16_len(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

/// `value.slice(0, end)` in UTF-16 units. A cut inside a surrogate pair drops
/// the pair's lone half (JS would keep a lone surrogate, which Rust cannot hold).
pub fn take_utf16(value: &str, end: usize) -> &str {
    let mut units = 0usize;
    for (byte, c) in value.char_indices() {
        let next = units + c.len_utf16();
        if next > end {
            return &value[..byte];
        }
        units = next;
    }
    value
}

/// `value.replaceAll(/\s+/g, " ")` (not trimmed).
pub fn collapse_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_space = false;
    for ch in value.chars() {
        if is_js_whitespace(ch) {
            in_space = true;
        } else {
            if in_space {
                out.push(' ');
                in_space = false;
            }
            out.push(ch);
        }
    }
    if in_space {
        out.push(' ');
    }
    out
}

/// `Date.parse` for the forms upstream timestamps take: an ISO date-time with
/// an offset, a date-only string (UTC midnight), or a date-time without an
/// offset (local time, as V8 reads it). Other legacy forms are not accepted,
/// and the caller decides what `None` means.
pub fn date_parse_ms(text: &str) -> Option<i64> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(parsed.timestamp_millis());
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    const LOCAL_FORMATS: [&str; 6] = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ];
    LOCAL_FORMATS.iter().find_map(|format| {
        let naive = chrono::NaiveDateTime::parse_from_str(text, format).ok()?;
        let local = chrono::Local.from_local_datetime(&naive).earliest()?;
        Some(local.timestamp_millis())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_helpers_match_js() {
        assert_eq!(utf16_len("a🚀"), 3);
        assert_eq!(take_utf16("a🚀b", 2), "a");
        assert_eq!(take_utf16("a🚀b", 3), "a🚀");
        assert_eq!(take_utf16("abc", 10), "abc");
        assert_eq!(collapse_whitespace(" a \n\t b "), " a b ");
        assert_eq!(js_trim_start("\u{feff} x "), "x ");
    }

    #[test]
    fn date_parse_accepts_the_forms_upstream_timestamps_take() {
        assert_eq!(date_parse_ms("2026-10-10"), Some(1_791_590_400_000));
        assert_eq!(
            date_parse_ms("2026-10-10T23:31:10Z"),
            Some(1_791_675_070_000)
        );
        let local = chrono::Local
            .with_ymd_and_hms(2026, 10, 10, 23, 31, 10)
            .earliest()
            .unwrap()
            .timestamp_millis();
        assert_eq!(date_parse_ms("2026-10-10T23:31:10"), Some(local));
        assert_eq!(date_parse_ms("2026-10-10 23:31:10"), Some(local));
        assert_eq!(date_parse_ms("not a date"), None);
    }
}
