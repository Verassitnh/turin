//! JS string helpers the settings logic leans on: `localeCompare` (ICU root
//! collation, close enough for ASCII identifiers and labels) and upstream's
//! `normalizeSearchText` (apps/web/src/lib/utils.ts).

use std::cmp::Ordering;

use unicode_normalization::UnicodeNormalization as _;

/// `normalizeSearchText`: NFKD, drop combining marks, lowercase, collapse
/// whitespace, trim.
pub fn normalize_search_text(value: &str) -> String {
    let stripped: String = value
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect();
    stripped
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// ICU root collation groups: whitespace < punctuation < symbols < digits <
/// letters. Within punctuation the order is `_ - , ; : ! ? . ' " ( ) [ ] { }
/// @ * / \ & # % ` ^ + < = > | ~ $`.
fn primary_weight(c: char) -> (u8, u32) {
    const PUNCT: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
    if c.is_whitespace() {
        return (0, c as u32);
    }
    if let Some(index) = PUNCT.find(c) {
        return (1, index as u32);
    }
    if c.is_ascii_digit() {
        return (2, c as u32);
    }
    if c.is_alphabetic() {
        return (3, c.to_lowercase().next().map_or(c as u32, |l| l as u32));
    }
    (1, 1000 + c as u32)
}

/// `String.prototype.localeCompare`: letters compare case-insensitively
/// first; ties put lowercase before uppercase.
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let primary = left
        .chars()
        .map(primary_weight)
        .cmp(right.chars().map(primary_weight));
    if primary != Ordering::Equal {
        return primary;
    }
    for (l, r) in left.chars().zip(right.chars()) {
        if l != r {
            // lowercase sorts before uppercase at the tertiary level.
            return if l.is_lowercase() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_search_text() {
        assert_eq!(normalize_search_text("  Crème   BRÛLÉE "), "creme brulee");
    }

    #[test]
    fn compares_like_locale_compare() {
        assert_eq!(locale_compare("chat.new", "chat.newLocal"), Ordering::Less);
        assert_eq!(locale_compare("a", "B"), Ordering::Less);
        assert_eq!(locale_compare("a", "A"), Ordering::Less);
        assert_eq!(locale_compare("usage", "usage"), Ordering::Equal);
        assert_eq!(locale_compare("sidebar.toggle", "script.a"), Ordering::Greater);
    }
}
