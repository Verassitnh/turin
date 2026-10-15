//! JS string semantics the timeline ports share (no upstream file).
//!
//! Upstream leans on `\s`, `.replace(/\s+/g, " ")` and UTF-16 `.length` /
//! `.slice`; these helpers reproduce them so each port does not re-derive
//! them. Trimming uses `turin_contracts::base_schemas::js_trim`.

pub use turin_contracts::base_schemas::js_trim;

/// JS `\s`: the set `String.prototype.trim` strips (U+FEFF included,
/// U+0085 not), which is what the regex class matches too.
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

/// `text.replace(/\s+/g, " ")`.
pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if is_js_whitespace(ch) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

/// `text.length` (UTF-16 code units).
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text.slice(0, max_units)` on UTF-16 units, cut back to a char boundary
/// when the limit splits a surrogate pair.
pub fn utf16_prefix(text: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        units += ch.len_utf16();
        if units > max_units {
            return &text[..index];
        }
    }
    text
}

/// `value.charAt(0).toUpperCase() + value.slice(1)`.
pub fn capitalize_first(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `value.charAt(0).toLowerCase() + value.slice(1)`.
pub fn lowercase_first(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_and_slices_like_js() {
        assert_eq!(collapse_whitespace("a \n\t b\u{feff}c"), "a b c");
        assert_eq!(utf16_len("a😀"), 3);
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
        assert_eq!(capitalize_first("read file"), "Read file");
        assert_eq!(lowercase_first("Ran 2 commands"), "ran 2 commands");
    }
}
