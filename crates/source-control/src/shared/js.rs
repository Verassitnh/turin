//! JS string semantics used by ported code.

/// JS `String.prototype.trim`: Unicode White_Space plus U+FEFF (Rust's `str::trim` omits U+FEFF).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// JS `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    // Unreserved by encodeURIComponent: A-Z a-z 0-9 - _ . ! ~ * ' ( )
    const SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    utf8_percent_encode(s, SET).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encode_matches_js() {
        assert_eq!(encode_uri_component("a b/c?d=é"), "a%20b%2Fc%3Fd%3D%C3%A9");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    }
    #[test]
    fn trim_strips_bom() {
        assert_eq!(js_trim("\u{feff} a \n"), "a");
    }
}
