//! Port of upstream/apps/server/src/workspace/fileHeaderMimeType.ts
//!
//! A presentation hint for extensionless files. Never a validation of contents
//! or safety.

use std::sync::LazyLock;

use regex::Regex;

/// Formats whose headers give extensionless files a useful icon.
pub fn file_header_mime_type(bytes: &[u8]) -> Option<&'static str> {
    let starts_with = |signature: &[u8], offset: usize| {
        bytes
            .get(offset..offset + signature.len())
            .is_some_and(|slice| slice == signature)
    };
    if starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a], 0) {
        return Some("image/png");
    }
    if starts_with(&[0xff, 0xd8, 0xff], 0) {
        return Some("image/jpeg");
    }
    if starts_with(&[0x50, 0x4b, 0x03, 0x04], 0) {
        return Some("application/zip");
    }
    if starts_with(&[0x52, 0x49, 0x46, 0x46], 0) && starts_with(&[0x57, 0x45, 0x42, 0x50], 8) {
        return Some("image/webp");
    }
    // TextDecoder's default is lossy UTF-8, same as `from_utf8_lossy`.
    let header = String::from_utf8_lossy(bytes);
    if header.starts_with("GIF87a") || header.starts_with("GIF89a") {
        return Some("image/gif");
    }
    if header.starts_with("%PDF-") {
        return Some("application/pdf");
    }
    if header.starts_with("SQLite format 3\0") {
        return Some("application/vnd.sqlite3");
    }
    if !header.starts_with("#!") {
        return None;
    }
    // `split("\n", 1)` keeps only the first line.
    let interpreter = header.split('\n').next().unwrap_or("");
    // These patterns are not unicode-aware in upstream (`\d` and `\b` are ASCII).
    static PYTHON: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?-u:\bpython[0-9.]*\b)").unwrap());
    static NODE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?-u:\b(?:node|nodejs)\b)").unwrap());
    static RUBY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u:\bruby\b)").unwrap());
    static SHELL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?-u:\b(?:sh|bash|zsh|fish)\b)").unwrap());
    if PYTHON.is_match(interpreter) {
        return Some("text/x-python");
    }
    if NODE.is_match(interpreter) {
        return Some("text/javascript");
    }
    if RUBY.is_match(interpreter) {
        return Some("text/x-ruby");
    }
    if SHELL.is_match(interpreter) {
        return Some("text/x-shellscript");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::file_header_mime_type;

    #[test]
    fn classifies_binary_headers() {
        let cases: &[(&[u8], Option<&str>)] = &[
            (
                &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
                Some("image/png"),
            ),
            (&[0xff, 0xd8, 0xff], Some("image/jpeg")),
            (&[0x50, 0x4b, 0x03, 0x04], Some("application/zip")),
            (
                &[
                    0x52, 0x49, 0x46, 0x46, 0xc2, 0xa0, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50,
                ],
                Some("image/webp"),
            ),
            (&[0x89, 0x50, 0x4e], None),
        ];
        for (header, expected) in cases {
            assert_eq!(file_header_mime_type(header), *expected, "{header:?}");
        }
    }

    #[test]
    fn classifies_text_headers() {
        let cases: &[(&str, Option<&str>)] = &[
            ("%PDF-1.7", Some("application/pdf")),
            ("GIF89a", Some("image/gif")),
            ("SQLite format 3\0", Some("application/vnd.sqlite3")),
            ("RIFF1234WEBP", Some("image/webp")),
            ("#!/usr/bin/env python3\n", Some("text/x-python")),
            ("#!/usr/bin/node\n", Some("text/javascript")),
            ("#!/bin/bash\n", Some("text/x-shellscript")),
            ("#!/usr/bin/env ruby\n", Some("text/x-ruby")),
            ("hello", None),
            ("", None),
            ("#!/usr/bin/env custom\n", None),
        ];
        for (header, expected) in cases {
            assert_eq!(
                file_header_mime_type(header.as_bytes()),
                *expected,
                "{header:?}"
            );
        }
    }
}
