//! JS string-semantics helpers local to provider-core.
//!
//! Upstream measures strings in UTF-16 code units and trims with
//! `String.prototype.trim`. `turin_contracts::js_trim` covers trimming; these
//! cover the rest. When `turin-shared` lands (`js_string`), prefer its
//! versions; these are intentionally tiny and equivalent.

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
}
