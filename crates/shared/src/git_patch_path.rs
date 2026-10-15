//! Port of upstream/packages/shared/src/gitPatchPath.ts
//!
//! How a file's name travels in a unified patch, which is not as itself. A
//! reader finds the name in a header by where it stops: `diff --git a/<old>
//! b/<new>` splits on a space, `--- a/<name>` stops at a tab. So a name holding
//! a tab or a newline reads back as the part of itself before that byte, or
//! fabricates a header line of its own, and git's answer is to write such a
//! name quoted with C-style escapes. Both halves live here so what one writes
//! is what the other reads.

/// What git escapes by name, as the character written and the escape written
/// for it. Not every byte git would escape: `core.quotePath` also escapes
/// anything outside ASCII, which is a setting for what a terminal can show
/// rather than anything the format needs, and a patch here is read by a diff
/// viewer.
fn escape_for_character(ch: char) -> Option<&'static str> {
    match ch {
        '"' => Some("\\\""),
        '\\' => Some("\\\\"),
        '\u{0007}' => Some("\\a"),
        '\u{0008}' => Some("\\b"),
        '\t' => Some("\\t"),
        '\n' => Some("\\n"),
        '\u{000b}' => Some("\\v"),
        '\u{000c}' => Some("\\f"),
        '\r' => Some("\\r"),
        _ => None,
    }
}

/// The escape written for a character, as the byte it stands for.
fn byte_for_escape(escaped: char) -> Option<u8> {
    match escaped {
        '"' => Some(0x22),
        '\\' => Some(0x5c),
        'a' => Some(0x07),
        'b' => Some(0x08),
        'f' => Some(0x0c),
        'n' => Some(0x0a),
        'r' => Some(0x0d),
        't' => Some(0x09),
        'v' => Some(0x0b),
        _ => None,
    }
}

const QUOTE: char = '"';
const DELETE_CHARACTER: u32 = 0x7f;
const LOWEST_PRINTABLE: u32 = 0x20;
const BACKSLASH: u8 = 0x5c;

/// A name as a patch header can carry it: itself where that is unambiguous,
/// and git's quoted form where it is not, which is any name holding a quote, a
/// backslash or a control character. A header side's `a/` or `b/` belongs
/// inside the quoting, so pass it in along with the name: what git quotes is
/// the whole token a reader takes off the line, side letter and all.
pub fn quote_git_patch_path(path: &str) -> String {
    let mut body = String::new();
    let mut quoting = false;
    for character in path.chars() {
        if let Some(escape) = escape_for_character(character) {
            body.push_str(escape);
            quoting = true;
            continue;
        }
        let code = character as u32;
        // Every character git has no name for is written as the octal of its
        // byte, and a control character is one byte in UTF-8, so the
        // character's own code point is that byte.
        if code < LOWEST_PRINTABLE || code == DELETE_CHARACTER {
            body.push_str(&format!("\\{:03o}", code));
            quoting = true;
            continue;
        }
        body.push(character);
    }
    if quoting {
        format!("{QUOTE}{body}{QUOTE}")
    } else {
        path.to_owned()
    }
}

/// The escapes inside a quoted form undone, whether or not the quotes are still
/// around them. A name holding no backslash is already itself, and an escape
/// git would never write reads the way C reads it. The escapes are per byte, so
/// a name in another alphabet arrives as a run of octal and only reads back as
/// itself once those bytes are rejoined and decoded together.
///
/// Deviation: upstream accumulates unescaped text in a JS string (UTF-16), so
/// an unescaped astral character in a body that also holds an escape becomes
/// two lone surrogates and encodes as U+FFFD. Rust strings hold the real
/// character, so such names survive intact.
fn unescape_body(body: &str) -> String {
    if !body.contains('\\') {
        return body.to_owned();
    }
    let mut bytes: Vec<u8> = Vec::new();
    // Anything left as itself is encoded a run at a time rather than a unit at
    // a time, so a character outside the basic plane keeps its bytes together.
    let mut literal = String::new();
    let flush = |literal: &mut String, bytes: &mut Vec<u8>| {
        if literal.is_empty() {
            return;
        }
        bytes.extend_from_slice(literal.as_bytes());
        literal.clear();
    };
    let units: Vec<u16> = body.encode_utf16().collect();
    let unit_char = |index: usize| -> Option<char> {
        units
            .get(index)
            .and_then(|unit| char::from_u32(*unit as u32))
    };
    let mut at = 0usize;
    while at < units.len() {
        let Some(character) = unit_char(at) else {
            // Lone surrogate: keep it out of the literal run; upstream would
            // encode it as U+FFFD, which Rust cannot represent here.
            at += 1;
            continue;
        };
        if character != '\\' {
            literal.push(character);
            at += 1;
            continue;
        }
        let Some(escaped) = unit_char(at + 1) else {
            flush(&mut literal, &mut bytes);
            bytes.push(BACKSLASH);
            break;
        };
        if let Some(named) = byte_for_escape(escaped) {
            flush(&mut literal, &mut bytes);
            bytes.push(named);
            at += 2;
            continue;
        }
        let octal: String = (1..4)
            .filter_map(|offset| unit_char(at + offset))
            .take(3)
            .collect();
        if octal.len() == 3 && octal.chars().all(|c| ('0'..='7').contains(&c)) {
            flush(&mut literal, &mut bytes);
            bytes.push(u8::from_str_radix(&octal, 8).unwrap_or(0));
            at += 4;
            continue;
        }
        literal.push(escaped);
        at += 2;
    }
    flush(&mut literal, &mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One header's name token as the name it stands for. The escapes are undone
/// whether the quotes are still there or not, because patch parsers disagree
/// about how much of the quoting they hand back: the one the clients read diffs
/// with takes the quotes off the `diff --git` line's names and leaves them on a
/// rename's.
pub fn unquote_git_patch_path(token: &str) -> String {
    let bytes = token.as_bytes();
    if token.len() >= 2 && bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') {
        unescape_body(&token[1..token.len() - 1])
    } else {
        unescape_body(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BELL: char = '\u{0007}';
    const UNIT_SEPARATOR: char = '\u{001f}';
    const DELETE: char = '\u{007f}';

    #[test]
    fn leaves_a_name_a_header_can_carry_as_itself() {
        assert_eq!(quote_git_patch_path("src/app.ts"), "src/app.ts");
        assert_eq!(quote_git_patch_path("with space.txt"), "with space.txt");
        // Outside ASCII is only quoted for what a terminal can show, and a diff viewer is not one.
        assert_eq!(quote_git_patch_path("café/résumé.ts"), "café/résumé.ts");
        assert_eq!(quote_git_patch_path("🚀.ts"), "🚀.ts");
    }

    #[test]
    fn writes_gits_escape_for_a_character_the_header_would_read_as_its_own() {
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
        assert_eq!(
            quote_git_patch_path(&format!("ring{BELL}.txt")),
            "\"ring\\a.txt\""
        );
    }

    #[test]
    fn writes_the_octal_of_a_control_character_git_has_no_name_for() {
        assert_eq!(
            quote_git_patch_path(&format!("unit{UNIT_SEPARATOR}.txt")),
            "\"unit\\037.txt\""
        );
        assert_eq!(
            quote_git_patch_path(&format!("del{DELETE}.txt")),
            "\"del\\177.txt\""
        );
    }

    #[test]
    fn a_name_written_into_a_header_reads_back_out() {
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
            assert_eq!(unquote_git_patch_path(&written), name, "{name:?}");
            // A parser that takes the quotes off itself, as the clients' one does, gets there too.
            let unwrapped = written
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(&written);
            assert_eq!(unquote_git_patch_path(unwrapped), name, "{name:?}");
        }
    }

    #[test]
    fn carries_the_whole_name_past_the_first_thing_a_header_stops_at() {
        let written = quote_git_patch_path("tab\tfile.txt");

        // The bug this guards: a header line ends at a tab and splits on a
        // space, so the name written as itself would be read as `tab`.
        assert!(!written.contains('\t'));
        assert!(!written.contains('\n'));
        assert_ne!(unquote_git_patch_path(&written), "tab");
    }

    #[test]
    fn leaves_a_token_git_saw_no_need_to_quote_alone() {
        assert_eq!(unquote_git_patch_path("src/app.ts"), "src/app.ts");
        assert_eq!(unquote_git_patch_path("with space.txt"), "with space.txt");
        assert_eq!(unquote_git_patch_path(""), "");
        assert_eq!(unquote_git_patch_path("\""), "\"");
    }

    #[test]
    fn reads_the_octal_escapes_a_host_with_core_quote_path_on_writes() {
        assert_eq!(
            unquote_git_patch_path("\"caf\\303\\251/r\\303\\251sum\\303\\251.ts\""),
            "café/résumé.ts"
        );
        assert_eq!(
            unquote_git_patch_path("\"\\360\\237\\232\\200.ts\""),
            "🚀.ts"
        );
    }

    #[test]
    fn reads_an_unwrapped_token_for_the_escapes_it_still_carries() {
        assert_eq!(unquote_git_patch_path("tab\\tfile.txt"), "tab\tfile.txt");
        assert_eq!(unquote_git_patch_path("caf\\303\\251.ts"), "café.ts");
    }

    #[test]
    fn reads_an_escape_git_would_never_write_the_way_c_reads_it() {
        assert_eq!(unquote_git_patch_path("odd\\zname.txt"), "oddzname.txt");
        assert_eq!(unquote_git_patch_path("trailing\\"), "trailing\\");
        assert_eq!(unquote_git_patch_path("short\\12.txt"), "short12.txt");
    }
}
