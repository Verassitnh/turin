//! Port of upstream/apps/web/src/components/pullRequest/pullRequestDiff.logic.ts.

use std::collections::HashSet;
use turin_contracts::pull_request::PullRequestDiffSide;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffHunkRange {
    pub deletion_start: i64,
    pub deletion_count: i64,
    pub addition_start: i64,
    pub addition_count: i64,
}

/// A thread naming a file is not necessarily in a hunk the host returned.
pub fn is_line_in_file_diff(hunks: &[DiffHunkRange], side: PullRequestDiffSide, line: i64) -> bool {
    hunks.iter().any(|hunk| match side {
        PullRequestDiffSide::Left => {
            line >= hunk.deletion_start && line < hunk.deletion_start + hunk.deletion_count
        }
        PullRequestDiffSide::Right => {
            line >= hunk.addition_start && line < hunk.addition_start + hunk.addition_count
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffFoldOverride {
    Expanded,
    Folded,
}

/// Reader toggles are differences from the toolbar choice so later diff slices inherit it.
pub fn is_file_diff_collapsed(
    file_key: &str,
    fold_override: Option<DiffFoldOverride>,
    toggled_file_keys: &HashSet<String>,
) -> bool {
    let folded_by_default = fold_override == Some(DiffFoldOverride::Folded);
    if toggled_file_keys.contains(file_key) {
        !folded_by_default
    } else {
        folded_by_default
    }
}

/// A viewed tick moves the fold as if the reader pressed the chevron.
pub fn toggle_file_diff_fold_for_viewed(
    file_key: &str,
    viewed: bool,
    fold_override: Option<DiffFoldOverride>,
    toggled_file_keys: &HashSet<String>,
) -> HashSet<String> {
    if is_file_diff_collapsed(file_key, fold_override, toggled_file_keys) == viewed {
        return toggled_file_keys.clone();
    }
    let mut next = toggled_file_keys.clone();
    if !next.remove(file_key) {
        next.insert(file_key.to_owned());
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn file() -> Vec<DiffHunkRange> {
        vec![
            DiffHunkRange {
                deletion_start: 10,
                deletion_count: 3,
                addition_start: 10,
                addition_count: 5,
            },
            DiffHunkRange {
                deletion_start: 40,
                deletion_count: 0,
                addition_start: 42,
                addition_count: 2,
            },
        ]
    }
    fn set(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }
    #[test]
    fn places_a_line_inside_a_hunk_on_the_side_that_hunk_counts() {
        assert!(is_line_in_file_diff(
            &file(),
            PullRequestDiffSide::Right,
            12
        ));
        assert!(is_line_in_file_diff(&file(), PullRequestDiffSide::Left, 11));
    }
    #[test]
    fn includes_first_line_and_excludes_one_past_last() {
        for (side, line, want) in [
            (PullRequestDiffSide::Right, 10, true),
            (PullRequestDiffSide::Right, 14, true),
            (PullRequestDiffSide::Right, 15, false),
            (PullRequestDiffSide::Left, 9, false),
            (PullRequestDiffSide::Left, 12, true),
            (PullRequestDiffSide::Left, 13, false),
        ] {
            assert_eq!(is_line_in_file_diff(&file(), side, line), want);
        }
    }
    #[test]
    fn keeps_two_sides_apart() {
        assert!(is_line_in_file_diff(
            &file(),
            PullRequestDiffSide::Right,
            43
        ));
        assert!(!is_line_in_file_diff(
            &file(),
            PullRequestDiffSide::Left,
            40
        ));
    }
    #[test]
    fn empty_hunks_place_nothing() {
        assert!(!is_line_in_file_diff(&[], PullRequestDiffSide::Right, 1));
    }
    #[test]
    fn opens_before_touch() {
        assert!(!is_file_diff_collapsed("a.ts", None, &set(&[])));
        assert!(!is_file_diff_collapsed("b.ts", None, &set(&[])));
    }
    #[test]
    fn opens_after_toolbar_expand() {
        assert!(!is_file_diff_collapsed(
            "a.ts",
            Some(DiffFoldOverride::Expanded),
            &set(&[])
        ));
        assert!(!is_file_diff_collapsed(
            "b.ts",
            Some(DiffFoldOverride::Expanded),
            &set(&[])
        ));
    }
    #[test]
    fn folds_after_toolbar_fold() {
        assert!(is_file_diff_collapsed(
            "a.ts",
            Some(DiffFoldOverride::Folded),
            &set(&[])
        ));
        assert!(is_file_diff_collapsed(
            "b.ts",
            Some(DiffFoldOverride::Folded),
            &set(&[])
        ));
    }
    #[test]
    fn keeps_folded_file_as_slice_arrives() {
        assert!(is_file_diff_collapsed("b.ts", None, &set(&["b.ts"])));
        assert!(!is_file_diff_collapsed("c.ts", None, &set(&["b.ts"])));
    }
    #[test]
    fn toggles_after_either_toolbar_press() {
        assert!(is_file_diff_collapsed(
            "a.ts",
            Some(DiffFoldOverride::Expanded),
            &set(&["a.ts"])
        ));
        assert!(!is_file_diff_collapsed(
            "a.ts",
            Some(DiffFoldOverride::Folded),
            &set(&["a.ts"])
        ));
    }
    #[test]
    fn tick_puts_file_away() {
        assert_eq!(
            toggle_file_diff_fold_for_viewed("a.ts", true, None, &set(&[])),
            set(&["a.ts"])
        );
    }
    #[test]
    fn untick_brings_file_back() {
        assert_eq!(
            toggle_file_diff_fold_for_viewed("a.ts", false, None, &set(&["a.ts"])),
            set(&[])
        );
    }
    #[test]
    fn matching_fold_is_unchanged() {
        assert_eq!(
            toggle_file_diff_fold_for_viewed("a.ts", true, None, &set(&["a.ts"])),
            set(&["a.ts"])
        );
    }
    #[test]
    fn moves_against_toolbar() {
        assert_eq!(
            toggle_file_diff_fold_for_viewed(
                "a.ts",
                true,
                Some(DiffFoldOverride::Expanded),
                &set(&[])
            ),
            set(&["a.ts"])
        );
        assert_eq!(
            toggle_file_diff_fold_for_viewed(
                "a.ts",
                false,
                Some(DiffFoldOverride::Expanded),
                &set(&[])
            )
            .len(),
            0
        );
    }
    #[test]
    fn touches_only_ticked_file() {
        assert_eq!(
            toggle_file_diff_fold_for_viewed("a.ts", false, None, &set(&["a.ts", "b.ts"])),
            set(&["b.ts"])
        );
    }
}
