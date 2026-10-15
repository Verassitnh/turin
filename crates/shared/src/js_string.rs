//! JS string-semantics helpers shared by ported modules.
//!
//! Upstream (`packages/shared/src`) indexes and slices strings with JavaScript
//! semantics: UTF-16 code units, `String.prototype.indexOf`, and
//! `String.prototype.trim` (which trims a Unicode whitespace set that differs
//! from Rust's `str::trim`). `turin_contracts::base_schemas::js_trim` covers
//! trimming; the helpers here cover UTF-16 indexing and searching so the ported
//! logic keeps upstream's offsets and edge cases.

use turin_contracts::base_schemas::JS_WHITESPACE_CHARS;

/// Length of `value` in UTF-16 code units (`String.prototype.length`).
pub fn utf16_len(value: &str) -> usize {
    value.chars().map(|c| c.len_utf16()).sum()
}

/// `String.prototype.charCodeAt(index)`: the UTF-16 code unit at `index`, if any.
///
/// A surrogate-pair half comes back as the lone surrogate, as in JS.
pub fn char_code_unit_at(value: &str, index: usize) -> Option<u16> {
    value.encode_utf16().nth(index)
}

/// `String.prototype.slice(start, end)` with UTF-16 indices (clamped like JS:
/// out-of-range start/end are clamped, `start > end` yields the empty string).
///
/// Deviation from JS: a start or end landing *inside* a surrogate pair returns
/// no code units from that pair, because a Rust `&str` cannot hold the lone
/// surrogate JavaScript would produce. Ported upstream code never slices into
/// a pair on the paths that matter (all offsets come from matching whole
/// needles or ASCII delimiters).
pub fn slice_utf16(value: &str, start: usize, end: usize) -> &str {
    let mut byte_start = value.len();
    let mut byte_end = value.len();
    let mut units = 0usize;
    for (byte, c) in value.char_indices() {
        let len = c.len_utf16();
        if byte_start == value.len() && units >= start {
            byte_start = byte;
        }
        if units + len > end {
            byte_end = byte;
            break;
        }
        units += len;
    }
    if byte_start >= byte_end {
        return "";
    }
    &value[byte_start..byte_end]
}

/// `String.prototype.indexOf(searchElement, fromIndex)` with UTF-16 indices.
pub fn index_of_utf16(value: &str, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(utf16_len(value)));
    }
    let value_len = utf16_len(value);
    let needle_len = utf16_len(needle);
    if needle_len > value_len {
        return None;
    }
    let mut units = 0usize;
    let mut char_starts: Vec<(usize, usize)> = Vec::new(); // (unit index, byte index)
    for (byte, c) in value.char_indices() {
        if units >= from {
            char_starts.push((units, byte));
        }
        units += c.len_utf16();
    }
    for (unit_index, byte) in char_starts {
        if unit_index + needle_len > value_len {
            break;
        }
        if value[byte..].starts_with(needle) {
            return Some(unit_index);
        }
    }
    None
}

/// `String.prototype.lastIndexOf(searchElement, fromIndex)` with UTF-16
/// indices; searches backwards starting at `from` (inclusive).
///
/// JS `lastIndexOf(s, pos)` finds the last occurrence starting at or before
/// unit index `pos`. `from_index` of `usize::MAX` means "search from the end".
pub fn last_index_of_utf16(value: &str, needle: &str, from_index: usize) -> Option<usize> {
    let value_len = utf16_len(value);
    let needle_len = utf16_len(needle);
    if needle_len > value_len {
        return None;
    }
    let from = from_index.min(value_len);
    let last_start = value_len.saturating_sub(needle_len).min(from);
    let mut units = 0usize;
    let mut starts: Vec<(usize, usize)> = Vec::new();
    for (byte, c) in value.char_indices() {
        if units + needle_len <= value_len {
            starts.push((units, byte));
        }
        units += c.len_utf16();
    }
    for (unit_index, byte) in starts.into_iter().rev() {
        if unit_index > last_start {
            continue;
        }
        if value[byte..].starts_with(needle) {
            return Some(unit_index);
        }
    }
    None
}

/// UTF-16 code-unit order, i.e. JS `<` / `>` on strings.
pub fn code_unit_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// ICU root-collation order of ASCII punctuation and symbols (after
/// whitespace, before digits), as `localeCompare` sees it in en-US.
const COLLATION_PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// Approximates `String.prototype.localeCompare` in the default en-US locale
/// (ICU root collation) without an ICU dependency.
///
/// Three levels, like ICU: base characters (whitespace < punctuation <
/// digits < letters, letters case-insensitively), then accents, then case
/// (lowercase before uppercase). Characters outside ASCII letters/digits fall
/// back to code point order after the letters.
pub fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    use unicode_normalization::UnicodeNormalization;

    struct Key {
        primary: Vec<u32>,
        secondary: Vec<u32>,
        tertiary: Vec<u8>,
    }

    fn key(value: &str) -> Key {
        let mut primary = Vec::new();
        let mut secondary = Vec::new();
        let mut tertiary = Vec::new();
        for c in value.nfd() {
            if ('\u{0300}'..='\u{036f}').contains(&c) {
                // Accent: attach to the previous base character.
                if let Some(last) = secondary.last_mut() {
                    *last = *last * 1000 + (c as u32 - 0x2ff);
                }
                continue;
            }
            let lowered = c.to_lowercase().next().unwrap_or(c);
            let weight = if let Some(rank) = JS_WHITESPACE_CHARS.iter().position(|ws| *ws == c) {
                // Above raw control code points (all < 200), below punctuation.
                200 + rank as u32
            } else if c.is_control() {
                c as u32
            } else if let Some(index) = COLLATION_PUNCTUATION.find(c) {
                1_000 + index as u32
            } else if c.is_ascii_digit() {
                2_000 + (c as u32 - '0' as u32)
            } else if lowered.is_ascii_lowercase() {
                3_000 + (lowered as u32 - 'a' as u32)
            } else {
                4_000 + lowered as u32
            };
            primary.push(weight);
            secondary.push(0);
            tertiary.push(u8::from(c != lowered));
        }
        Key {
            primary,
            secondary,
            tertiary,
        }
    }

    let (left, right) = (key(left), key(right));
    left.primary
        .cmp(&right.primary)
        .then_with(|| left.secondary.cmp(&right.secondary))
        .then_with(|| left.tertiary.cmp(&right.tertiary))
}

/// Body of a regex character class equal to JS `\s`. Rust's `\s` differs
/// (it has U+0085 and lacks U+FEFF), so patterns ported from JS splice this in:
/// `format!("[{JS_WS}]")` for `\s`, `format!("[^{JS_WS}]")` for `\S`.
pub const JS_WS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// JS `URIError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("URI malformed")]
pub struct UriError;

/// JS `decodeURIComponent`: every `%` must start a two-hex-digit escape and the
/// decoded bytes must be valid UTF-8; otherwise this fails (JS throws
/// `URIError`, which callers catch).
pub fn decode_uri_component(value: &str) -> Result<String, UriError> {
    let bytes = value.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3).ok_or(UriError)?;
            let high = (hex[0] as char).to_digit(16).ok_or(UriError)?;
            let low = (hex[1] as char).to_digit(16).ok_or(UriError)?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| UriError)
}

/// JS `encodeURI`: escapes everything except RFC 2396 unreserved and reserved
/// characters (`A-Za-z0-9 - _ . ! ~ * ' ( ) ; , / ? : @ & = + $ #`).
pub fn encode_uri(value: &str) -> String {
    const ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'%')
        .add(b'[')
        .add(b']')
        .add(b'"')
        .add(b'<')
        .add(b'>')
        .add(b'`')
        .add(b'{')
        .add(b'}')
        .add(b'|')
        .add(b'\\')
        .add(b'^');
    percent_encoding::utf8_percent_encode(value, ESCAPE).to_string()
}

/// JS `encodeURIComponent`: escapes everything except unreserved characters
/// (`A-Za-z0-9 - _ . ! ~ * ' ( )`).
pub fn encode_uri_component(value: &str) -> String {
    const ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'$')
        .add(b'%')
        .add(b'&')
        .add(b'+')
        .add(b',')
        .add(b'/')
        .add(b':')
        .add(b';')
        .add(b'<')
        .add(b'=')
        .add(b'>')
        .add(b'?')
        .add(b'@')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');
    percent_encoding::utf8_percent_encode(value, ESCAPE).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_len_counts_surrogate_pairs_as_two() {
        assert_eq!(utf16_len("aİ"), 2);
        assert_eq!(utf16_len("a😀"), 3);
    }

    #[test]
    fn char_code_unit_at_returns_high_surrogate_inside_pair() {
        // "😀" is U+1F600, one char but two UTF-16 units.
        let value = "a😀";
        assert_eq!(char_code_unit_at(value, 0), Some(u16::from(b'a')));
        assert_eq!(char_code_unit_at(value, 1), Some(0xd83d));
        assert_eq!(char_code_unit_at(value, 2), Some(0xde00));
        assert_eq!(char_code_unit_at(value, 3), None);
    }

    #[test]
    fn slice_utf16_splits_on_unit_boundaries() {
        let value = "a😀b";
        assert_eq!(slice_utf16(value, 0, 1), "a");
        assert_eq!(slice_utf16(value, 1, 3), "😀");
        assert_eq!(slice_utf16(value, 3, 4), "b");
        assert_eq!(slice_utf16(value, 3, 2), "");
        assert_eq!(slice_utf16(value, 0, 99), "a😀b");
    }

    #[test]
    fn index_of_utf16_reports_unit_offsets() {
        let value = "a😀a";
        assert_eq!(index_of_utf16(value, "a", 0), Some(0));
        assert_eq!(index_of_utf16(value, "a", 1), Some(3));
        assert_eq!(index_of_utf16(value, "😀", 0), Some(1));
        assert_eq!(index_of_utf16(value, "z", 0), None);
        assert_eq!(index_of_utf16("", "a", 0), None);
        assert_eq!(index_of_utf16("ab", "", 5), Some(2));
    }

    #[test]
    fn last_index_of_utf16_searches_backwards() {
        assert_eq!(last_index_of_utf16("a\nb\nc", "\n", usize::MAX), Some(3));
        assert_eq!(last_index_of_utf16("a\nb\nc", "\n", 3), Some(3));
        assert_eq!(last_index_of_utf16("a\nb\nc", "\n", 2), Some(1));
        assert_eq!(last_index_of_utf16("a\nb\nc", "\n", 1), Some(1));
        assert_eq!(last_index_of_utf16("a\nb\nc", "\n", 0), None);
        // JS lastIndexOf clamps beyond-length start positions.
        assert_eq!(last_index_of_utf16("ab", "a", 9), Some(0));
    }

    #[test]
    fn locale_compare_matches_icu_for_common_cases() {
        use std::cmp::Ordering::{Greater, Less};
        assert_eq!(locale_compare("a", "B"), Less);
        assert_eq!(locale_compare("a", "A"), Less);
        assert_eq!(locale_compare("A", "b"), Less);
        assert_eq!(locale_compare("a-b", "ab"), Less);
        assert_eq!(locale_compare("10", "9"), Less);
        assert_eq!(locale_compare("9", "a"), Less);
        assert_eq!(locale_compare("resume", "r\u{e9}sum\u{e9}"), Less);
        assert_eq!(locale_compare("b", "r\u{e9}sum\u{e9}"), Less);
        assert_eq!(locale_compare("same", "same"), std::cmp::Ordering::Equal);
        assert_eq!(locale_compare("ab", "a"), Greater);
    }

    #[test]
    fn locale_compare_sorts_every_whitespace_before_letters() {
        use std::cmp::Ordering::{Greater, Less};
        // U+2003 and U+3000 used to take their code point as weight, which put
        // them after ASCII letters.
        assert_eq!(locale_compare("\u{2003}x", "ax"), Less);
        assert_eq!(locale_compare("\u{3000}x", "ax"), Less);
        assert_eq!(locale_compare("\u{2003}x", "-x"), Less);
        assert_eq!(locale_compare("\u{2003}x", "9x"), Less);
        assert_eq!(locale_compare("ax", "\u{2003}x"), Greater);
        assert_eq!(
            locale_compare("\u{2003}", "\u{2003}"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn code_unit_cmp_orders_by_units() {
        // "invalid-a" > "invalid-B" because 'a' (0x61) > 'B' (0x42).
        assert_eq!(
            code_unit_cmp("invalid-a", "invalid-B"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            code_unit_cmp("invalid-B", "invalid-a"),
            std::cmp::Ordering::Less
        );
        assert_eq!(code_unit_cmp("same", "same"), std::cmp::Ordering::Equal);
    }
}
