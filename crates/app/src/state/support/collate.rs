//! `String#localeCompare` with the default (root) collation, for the id and
//! title tiebreaks upstream sorts with. Not an upstream module.
//!
//! ICU orders whitespace and punctuation before digits before letters,
//! compares letters case-insensitively first, and only then puts lowercase
//! before uppercase. That is enough for ids, branch names and project
//! titles; full UCA weights are not needed for a stable, upstream-matching
//! order of ASCII-ish keys. Non-ASCII letters compare by lowercase scalar.

use std::cmp::Ordering;

fn primary(c: char) -> (u8, u32) {
    if c.is_alphabetic() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        (2, lower as u32)
    } else if c.is_numeric() {
        (1, c.to_digit(10).unwrap_or(c as u32))
    } else {
        (0, c as u32)
    }
}

/// `left.localeCompare(right)` as an [`Ordering`].
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let by_primary = left.chars().map(primary).cmp(right.chars().map(primary));
    if by_primary != Ordering::Equal {
        return by_primary;
    }
    // Tertiary: lowercase sorts before uppercase at the first difference.
    for (a, b) in left.chars().zip(right.chars()) {
        if a != b {
            return match (a.is_lowercase(), b.is_lowercase()) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => a.cmp(&b),
            };
        }
    }
    left.len().cmp(&right.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_like_icu_root() {
        assert_eq!(locale_compare("a", "B"), Ordering::Less);
        assert_eq!(locale_compare("b", "B"), Ordering::Less);
        assert_eq!(locale_compare("B", "c"), Ordering::Less);
        assert_eq!(locale_compare("thread-1", "thread-2"), Ordering::Less);
        assert_eq!(locale_compare("1", "a"), Ordering::Less);
        assert_eq!(locale_compare("-", "1"), Ordering::Less);
        assert_eq!(locale_compare("abc", "abc"), Ordering::Equal);
        assert_eq!(locale_compare("ab", "abc"), Ordering::Less);
    }
}
