//! Port of JavaScript `String.prototype.localeCompare` as upstream calls it
//! with no locale argument: ICU root collation, with punctuation not ignored.
//!
//! Turin has no ICU collator, so this implements the root levels for ASCII and
//! Latin-1 letters:
//! - primary: whitespace < ASCII punctuation < other symbols < digits < letters,
//!   with letters compared case- and accent-insensitively;
//! - secondary: an unaccented letter before its accented form;
//! - tertiary: lowercase before uppercase.
//!
//! Letters outside Latin-1 sort after Latin by code point, and `ß` and `æ`
//! collate as one letter, not their ICU expansions. Those are the known gaps.

use std::cmp::Ordering;

/// ICU root order of ASCII punctuation and symbols.
const ASCII_PUNCTUATION_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

const WHITESPACE_GROUP: u32 = 1 << 24;
const PUNCTUATION_GROUP: u32 = 2 << 24;
const SYMBOL_GROUP: u32 = 3 << 24;
const DIGIT_GROUP: u32 = 4 << 24;
const LATIN_LETTER_GROUP: u32 = 5 << 24;
const OTHER_LETTER_GROUP: u32 = 6 << 24;

#[derive(Clone, Copy)]
struct CollationKey {
    primary: u32,
    secondary: u8,
    tertiary: u8,
}

/// Orders `left` and `right` like JS `left.localeCompare(right)`.
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    left.chars()
        .map(|ch| collation_key(ch).primary)
        .cmp(right.chars().map(|ch| collation_key(ch).primary))
        .then_with(|| {
            left.chars()
                .map(|ch| collation_key(ch).secondary)
                .cmp(right.chars().map(|ch| collation_key(ch).secondary))
        })
        .then_with(|| {
            left.chars()
                .map(|ch| collation_key(ch).tertiary)
                .cmp(right.chars().map(|ch| collation_key(ch).tertiary))
        })
}

fn collation_key(ch: char) -> CollationKey {
    if ch.is_whitespace() {
        return plain(WHITESPACE_GROUP | ch as u32);
    }
    if let Some(index) = ASCII_PUNCTUATION_ORDER.find(ch) {
        return plain(PUNCTUATION_GROUP | index as u32);
    }
    if ch.is_ascii_digit() {
        return plain(DIGIT_GROUP | (ch as u32 - '0' as u32));
    }
    if ch.is_ascii_alphabetic() {
        return CollationKey {
            primary: LATIN_LETTER_GROUP | (ch.to_ascii_lowercase() as u32 - 'a' as u32),
            secondary: 0,
            tertiary: u8::from(ch.is_ascii_uppercase()),
        };
    }
    if ch.is_alphabetic() {
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        let tertiary = u8::from(ch.is_uppercase());
        if let Some(base) = latin1_base_letter(lower) {
            return CollationKey {
                primary: LATIN_LETTER_GROUP | (base as u32 - 'a' as u32),
                secondary: 1,
                tertiary,
            };
        }
        return CollationKey {
            primary: OTHER_LETTER_GROUP | lower as u32,
            secondary: 0,
            tertiary,
        };
    }
    plain(SYMBOL_GROUP | ch as u32)
}

fn plain(primary: u32) -> CollationKey {
    CollationKey {
        primary,
        secondary: 0,
        tertiary: 0,
    }
}

/// The unaccented Latin letter an accented Latin-1 letter sorts with.
fn latin1_base_letter(lower: char) -> Option<char> {
    Some(match lower {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'æ' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'ß' => 's',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(values: &[&str]) -> Vec<String> {
        let mut owned: Vec<String> = values.iter().map(|value| value.to_string()).collect();
        owned.sort_by(|left, right| locale_compare(left, right));
        owned
    }

    #[test]
    fn orders_like_local_compare_for_mixed_case_paths() {
        // Byte order would put "B.ts" and "Zed" before "_x"; this is the order Node gives.
        assert_eq!(
            sorted(&["B.ts", "a.ts", "_x", "Zed", "e.ts", "émile"]),
            ["_x", "a.ts", "B.ts", "e.ts", "émile", "Zed"]
        );
    }

    #[test]
    fn breaks_ties_by_case_after_letters_match() {
        assert_eq!(sorted(&["b", "B", "a", "A"]), ["a", "A", "b", "B"]);
        assert_eq!(locale_compare("ab", "Ab"), Ordering::Less);
    }

    #[test]
    fn puts_accents_after_their_plain_letter() {
        assert_eq!(
            sorted(&["résumé", "resume", "resumes"]),
            ["resume", "résumé", "resumes"]
        );
    }

    #[test]
    fn sorts_punctuation_before_digits_before_letters() {
        assert_eq!(locale_compare("file-2", "file10"), Ordering::Less);
        assert_eq!(locale_compare("1", "a"), Ordering::Less);
        assert_eq!(locale_compare("_", "-"), Ordering::Less);
        assert_eq!(locale_compare("same", "same"), Ordering::Equal);
    }
}
