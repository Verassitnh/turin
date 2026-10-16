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

/// JS `decodeURIComponent`: `None` where JS would throw (a `%` not followed by two hex digits, or
/// a byte sequence that is not UTF-8).
pub fn decode_uri_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
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
    fn decode_matches_js() {
        assert_eq!(decode_uri_component("a%20b").as_deref(), Some("a b"));
        assert_eq!(decode_uri_component("%E0%A4%A"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        assert_eq!(decode_uri_component("%E2%82%AC").as_deref(), Some("€"));
    }
    #[test]
    fn trim_strips_bom() {
        assert_eq!(js_trim("\u{feff} a \n"), "a");
    }
}
