//! Port of upstream/packages/shared/src/Array.ts

/// Runs `compare` on a copy of `values` (JS `[...values].sort(compareFn)`).
///
/// JS `Array.prototype.sort` is stable (V8 since Node 11); `sort_by` is too.
pub fn copy_sorted<T: Ord + Clone, F>(values: &[T], mut compare: F) -> Vec<T>
where
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    let mut copy = values.to_vec();
    copy.sort_by(|left, right| compare(left, right));
    copy
}

/// Trims, then keeps at most `max_length` characters and appends `"..."`.
///
/// Upstream declares `maxLength = 50`; Rust callers pass the length explicitly
/// (Turin call sites pass `50` where upstream used the default). Upstream
/// trims with JS `String.prototype.trim`; the length and slice are in UTF-16
/// code units. When the cut lands inside a surrogate pair the port drops the
/// partial pair (a lone surrogate cannot exist in a Rust string).
pub fn truncate(text: &str, max_length: usize) -> String {
    let trimmed = turin_contracts::base_schemas::js_trim(text);
    if crate::js_string::utf16_len(trimmed) <= max_length {
        return trimmed.to_owned();
    }
    let cut: String = trimmed
        .chars()
        .by_ref()
        .scan(0usize, |units, c| {
            *units += c.len_utf16();
            Some((*units <= max_length).then_some(c))
        })
        .flatten()
        .collect();
    format!("{cut}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn sorts_a_copy_without_mutating_the_source() {
        let source = [3, 1, 2];
        let sorted = copy_sorted(&source, |left, right| left.cmp(right));
        assert_eq!(sorted, vec![1, 2, 3]);
        assert_eq!(source, [3, 1, 2]);
    }

    #[test]
    fn truncate_defaults_to_50_and_appends_ellipsis() {
        assert_eq!(truncate("hello", 50), "hello");
        let long = "x".repeat(60);
        let truncated = truncate(&long, 50);
        assert_eq!(truncated.len(), 53);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_trims_before_measuring() {
        assert_eq!(truncate("   hi   ", 50), "hi");
    }

    #[test]
    fn truncate_keeps_surrogate_pairs_whole() {
        // 27 emoji = 54 UTF-16 units, cut at 50 would split pair #25.
        let emoji: String = "😀".repeat(27);
        let truncated = truncate(&emoji, 50);
        assert_eq!(truncated.chars().count(), 25);
        assert!(truncated.ends_with("..."));
    }
}
