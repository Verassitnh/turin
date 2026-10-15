//! Port of upstream/packages/shared/src/gitPatchPath.ts
//!
//! A patch header stops at a tab or a space. Names that contain those bytes, or
//! a quote, a backslash, or a control character, travel quoted the way git writes
//! them, so a reader can put the name back together.

const QUOTE: char = '"';
const DELETE_CHARACTER: u32 = 0x7f;
const LOWEST_PRINTABLE: u32 = 0x20;
const BACKSLASH: u8 = 0x5c;

fn escape_for(character: char) -> Option<&'static str> {
    Some(match character {
        '"' => "\\\"",
        '\\' => "\\\\",
        '\u{0007}' => "\\a",
        '\u{0008}' => "\\b",
        '\t' => "\\t",
        '\n' => "\\n",
        '\u{000b}' => "\\v",
        '\u{000c}' => "\\f",
        '\r' => "\\r",
        _ => return None,
    })
}

fn character_for_escape(escaped: char) -> Option<u8> {
    Some(match escaped {
        '"' => 0x22,
        '\\' => 0x5c,
        'a' => 0x07,
        'b' => 0x08,
        'f' => 0x0c,
        'n' => 0x0a,
        'r' => 0x0d,
        't' => 0x09,
        'v' => 0x0b,
        _ => return None,
    })
}

/// A name as a patch header can carry it. Pass the `a/` or `b/` prefix in with
/// the name: git quotes the whole token.
pub fn quote_git_patch_path(path: &str) -> String {
    let mut body = String::new();
    let mut quoting = false;
    for character in path.chars() {
        if let Some(escape) = escape_for(character) {
            body.push_str(escape);
            quoting = true;
            continue;
        }
        let code = character as u32;
        if code < LOWEST_PRINTABLE || code == DELETE_CHARACTER {
            body.push('\\');
            body.push_str(&format!("{code:03o}"));
            quoting = true;
            continue;
        }
        body.push(character);
    }
    if quoting {
        format!("{QUOTE}{body}{QUOTE}")
    } else {
        body
    }
}

fn unescape_body(body: &str) -> String {
    if !body.contains('\\') {
        return body.to_owned();
    }
    let mut bytes = Vec::new();
    let mut literal = String::new();
    let flush = |literal: &mut String, bytes: &mut Vec<u8>| {
        if literal.is_empty() {
            return;
        }
        bytes.extend(literal.as_bytes());
        literal.clear();
    };
    let chars: Vec<char> = body.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let character = chars[at];
        if character != '\\' {
            literal.push(character);
            at += 1;
            continue;
        }
        if at + 1 >= chars.len() {
            flush(&mut literal, &mut bytes);
            bytes.push(BACKSLASH);
            break;
        }
        let escaped = chars[at + 1];
        if let Some(named) = character_for_escape(escaped) {
            flush(&mut literal, &mut bytes);
            bytes.push(named);
            at += 2;
            continue;
        }
        if at + 3 < chars.len() {
            let octal: String = chars[at + 1..at + 4].iter().collect();
            if octal.len() == 3 && octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
                flush(&mut literal, &mut bytes);
                bytes.push(u8::from_str_radix(&octal, 8).unwrap_or(0));
                at += 4;
                continue;
            }
        }
        literal.push(escaped);
        at += 2;
    }
    flush(&mut literal, &mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One header token as the name it stands for. Quotes may already be gone.
pub fn unquote_git_patch_path(token: &str) -> String {
    if token.len() >= 2 && token.starts_with(QUOTE) && token.ends_with(QUOTE) {
        unescape_body(&token[1..token.len() - 1])
    } else {
        unescape_body(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_an_unambiguous_name_alone() {
        assert_eq!(quote_git_patch_path("src/app.ts"), "src/app.ts");
        assert_eq!(quote_git_patch_path("with space.txt"), "with space.txt");
        assert_eq!(quote_git_patch_path("café/résumé.ts"), "café/résumé.ts");
        assert_eq!(quote_git_patch_path("🚀.ts"), "🚀.ts");
    }

    #[test]
    fn quotes_characters_a_header_would_split_on() {
        assert_eq!(quote_git_patch_path("tab\tfile.txt"), "\"tab\\tfile.txt\"");
        assert_eq!(
            quote_git_patch_path("line\nfile.txt"),
            "\"line\\nfile.txt\""
        );
        assert_eq!(quote_git_patch_path("quo\"te.txt"), "\"quo\\\"te.txt\"");
        assert_eq!(
            quote_git_patch_path("back\\slash.txt"),
            "\"back\\\\slash.txt\""
        );
        assert_eq!(quote_git_patch_path("ring\u{0007}.txt"), "\"ring\\a.txt\"");
        assert_eq!(
            quote_git_patch_path("unit\u{001f}.txt"),
            "\"unit\\037.txt\""
        );
        assert_eq!(quote_git_patch_path("del\u{007f}.txt"), "\"del\\177.txt\"");
    }

    #[test]
    fn round_trips_quoted_and_unwrapped_names() {
        let names = [
            "src/app.ts",
            "with space.txt",
            "café/résumé.ts",
            "🚀.ts",
            "tab\tfile.txt",
            "line\nfile.txt",
            "carriage\rfile.txt",
            "quo\"te.txt",
            "back\\slash.txt",
            "ring\u{0007}.txt",
            "unit\u{001f}.txt",
            "del\u{007f}.txt",
            "every\t\n\r\"\\\u{0007}.txt",
        ];
        for name in names {
            let written = quote_git_patch_path(name);
            assert_eq!(unquote_git_patch_path(&written), name);
            let unwrapped = if written.starts_with('"') {
                &written[1..written.len() - 1]
            } else {
                written.as_str()
            };
            assert_eq!(unquote_git_patch_path(unwrapped), name);
        }
        let written = quote_git_patch_path("tab\tfile.txt");
        assert!(!written.contains('\t'));
        assert!(!written.contains('\n'));
        assert_ne!(unquote_git_patch_path(&written), "tab");
    }

    #[test]
    fn reads_octal_and_partial_escapes() {
        assert_eq!(unquote_git_patch_path("src/app.ts"), "src/app.ts");
        assert_eq!(unquote_git_patch_path(""), "");
        assert_eq!(unquote_git_patch_path("\""), "\"");
        assert_eq!(
            unquote_git_patch_path("\"caf\\303\\251/r\\303\\251sum\\303\\251.ts\""),
            "café/résumé.ts"
        );
        assert_eq!(
            unquote_git_patch_path("\"\\360\\237\\232\\200.ts\""),
            "🚀.ts"
        );
        assert_eq!(unquote_git_patch_path("tab\\tfile.txt"), "tab\tfile.txt");
        assert_eq!(unquote_git_patch_path("caf\\303\\251.ts"), "café.ts");
        assert_eq!(unquote_git_patch_path("odd\\zname.txt"), "oddzname.txt");
        assert_eq!(unquote_git_patch_path("trailing\\"), "trailing\\");
        assert_eq!(unquote_git_patch_path("short\\12.txt"), "short12.txt");
    }
}
