//! HTTP Basic auth the way both OpenCode generations decode it: UTF-8
//! `opencode:<password>`, not Latin-1 `btoa`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub const OPENCODE_USERNAME: &str = "opencode";

pub fn basic_authorization(password: &str) -> String {
    let token = STANDARD.encode(format!("{OPENCODE_USERNAME}:{password}"));
    format!("Basic {token}")
}

/// `encodeURIComponent` for the v1 `x-opencode-directory` header.
pub fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_non_ascii_passwords_as_utf8() {
        let header = basic_authorization("pässwörd");
        let expected = STANDARD.encode("opencode:pässwörd");
        assert_eq!(header, format!("Basic {expected}"));
        assert!(header.is_ascii());
    }
}
