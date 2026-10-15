//! JS string-semantics helpers shared by ported modules.
//!
//! Upstream (`packages/shared/src`) indexes and slices strings with JavaScript
//! semantics: UTF-16 code units, `String.prototype.indexOf`, and
//! `String.prototype.trim` (which trims a Unicode whitespace set that differs
//! from Rust's `str::trim`). `turin_contracts::base_schemas::js_trim` covers
//! trimming; the helpers here cover UTF-16 indexing and searching so the ported
//! logic keeps upstream's offsets and edge cases.

/// Length of `value` in UTF-16 code units (`String.prototype.length`).
pub fn utf16_len(value: &str) -> usize {
    value.chars().map(|c| c.len_utf16()).sum()
}

/// `String.prototype[index]`: the UTF-16 code unit at `index`, if any.
///
/// A code unit from a surrogate pair is returned as its lone surrogate (as a
/// `char`, since all UTF-16 code units fit in a `char`), exactly like the
/// broken substring JavaScript hands back.
pub fn char_code_unit_at(value: &str, index: usize) -> Option<char> {
    let mut units = 0usize;
    for c in value.chars() {
        let len = c.len_utf16();
        if units == index {
            // Return the first code unit of this char (for a surrogate pair
            // this is the high surrogate, matching JS).
            return Some(if len == 2 { high_surrogate(c) } else { c });
        }
        if units + len > index {
            // Index landed inside a surrogate pair: impossible for real UTF-16
            // (units advance by 2), so this arm is unreachable.
            return None;
        }
        units += len;
    }
    None
}

/// The high surrogate of an astral char (call only when `c.len_utf16() == 2`).
fn high_surrogate(c: char) -> char {
    let code = c as u32;
    let high = 0xd800 + ((code - 0x1_0000) >> 10);
    char::from_u32(high).expect("valid high surrogate")
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
    let end = end.min(utf16_len(value));
    let start = start.min(end);
    let mut unit_start = 0usize;
    let mut byte_start = None;
    let mut byte_end = None;
    let mut units = 0usize;
    for (byte, c) in value.char_indices() {
        if units == start {
            byte_start = Some(byte);
        }
        units += c.len_utf16();
        if units == end {
            byte_end = Some(byte + c.len_utf8());
            break;
        }
    }
    match (byte_start, byte_end) {
        (Some(a), Some(b)) => &value[a..b],
        (Some(a), None) => &value[a..],
        _ => "",
    }
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

/// `String.prototype.localeCompare` fallback used by upstream when inputs are
/// malformed: plain code-unit comparison (JS `<`/`>` on strings).
pub fn code_unit_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    left.cmp(&right)
}

/// JS `\s` whitespace test on a `char` (Unicode whitespace, including BOM).
pub fn is_js_whitespace_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// JS `decodeURIComponent`: every `%` must start a two-hex-digit escape and the
/// decoded bytes must be valid UTF-8; otherwise this fails (JS throws
/// `URIError`, which callers catch).
pub fn decode_uri_component(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3).ok_or(())?;
            let high = (hex[0] as char).to_digit(16).ok_or(())?;
            let low = (hex[1] as char).to_digit(16).ok_or(())?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

/// JS `encodeURI`: escapes everything except RFC 2396 unreserved and reserved
/// characters (`A-Za-z0-9 - _ . ! ~ * ' ( ) ; , / ? : @ & = + $ #`).
pub fn encode_uri(value: &str) -> String {
    const ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_len_counts_surrogate_pairs_as_two() {
        assert_eq!(utf16_len("aİ"), 3);
        assert_eq!(utf16_len("a😀"), 3);
    }

    #[test]
    fn char_code_unit_at_returns_high_surrogate_inside_pair() {
        // "😀" is U+1F600, one char but two UTF-16 units.
        let value = "a😀";
        assert_eq!(char_code_unit_at(value, 0), Some('a'));
        assert_eq!(char_code_unit_at(value, 1), Some('\u{d83d}'));
        assert_eq!(char_code_unit_at(value, 2), None);
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
    fn code_unit_cmp_orders_by_units() {
        // "invalid-a" > "invalid-B" because 'a' (0x61) > 'B' (0x42).
        assert_eq!(code_unit_cmp("invalid-a", "invalid-B"), std::cmp::Ordering::Greater);
        assert_eq!(code_unit_cmp("invalid-B", "invalid-a"), std::cmp::Ordering::Less);
        assert_eq!(code_unit_cmp("same", "same"), std::cmp::Ordering::Equal);
    }
}
