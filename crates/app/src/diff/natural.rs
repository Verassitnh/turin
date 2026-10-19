//! `new Intl.Collator(undefined, { numeric: true, sensitivity: "base" })`
//! (`filePathCollator` in useReviewFilePatches.ts), which orders a diff's
//! files by path. The collation itself is `turin_shared::js_string`'s
//! `locale_compare_with`, shared with `localeCompare`.
//!
//! Port of upstream/apps/web/src/components/diffs/useReviewFilePatches.ts (`filePathCollator`; lazy path in mock_source.rs).

use std::cmp::Ordering;

use turin_shared::js_string::{LocaleCompareOptions, locale_compare_with};

pub fn natural_compare(a: &str, b: &str) -> Ordering {
    locale_compare_with(
        a,
        b,
        LocaleCompareOptions {
            numeric: true,
            base_sensitivity: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_numbers_numerically_and_text_without_case() {
        assert_eq!(natural_compare("file2", "file10"), Ordering::Less);
        assert_eq!(natural_compare("README.md", "readme.md"), Ordering::Equal);
        assert_eq!(natural_compare("a/b.ts", "a/B.ts"), Ordering::Equal);
        assert_eq!(natural_compare("src/a.ts", "src/b.ts"), Ordering::Less);
        let mut paths = vec!["docs/z.md", "README.md", "docs/a.md", "crates/x.rs"];
        paths.sort_by(|a, b| natural_compare(a, b));
        assert_eq!(
            paths,
            vec!["crates/x.rs", "docs/a.md", "docs/z.md", "README.md"]
        );
    }

    // Expected orders are `[...paths].sort(new Intl.Collator(undefined, { numeric: true,
    // sensitivity: "base" }).compare)` on Node v22 (en-US ICU).
    #[test]
    fn punctuation_follows_icu_root_order() {
        let mut paths = vec!["src/foo.ts", "src/foo_bar.ts", "src/foo-bar.ts"];
        paths.sort_by(|a, b| natural_compare(a, b));
        assert_eq!(
            paths,
            vec!["src/foo_bar.ts", "src/foo-bar.ts", "src/foo.ts"]
        );
        assert_eq!(natural_compare("foo_bar.ts", "foo-bar.ts"), Ordering::Less);
        assert_eq!(natural_compare("a_b", "a/b"), Ordering::Less);
    }

    #[test]
    fn punctuation_sorts_before_digits_so_file_precedes_file1() {
        let mut paths = vec!["file10.ts", "file.ts", "file2.ts", "file1.ts"];
        paths.sort_by(|a, b| natural_compare(a, b));
        assert_eq!(paths, vec!["file.ts", "file1.ts", "file2.ts", "file10.ts"]);
    }

    #[test]
    fn digit_runs_compare_by_value_and_leading_zeros_do_not_count() {
        assert_eq!(natural_compare("file9.ts", "file10.ts"), Ordering::Less);
        assert_eq!(natural_compare("a1b", "a01c"), Ordering::Less);
        assert_eq!(natural_compare("x01", "x1"), Ordering::Equal);
        assert_eq!(natural_compare("a0", "a"), Ordering::Greater);
        assert_eq!(natural_compare("1a", "1"), Ordering::Greater);
    }

    #[test]
    fn base_sensitivity_ignores_accents() {
        assert_eq!(natural_compare("\u{e9}.ts", "e.ts"), Ordering::Equal);
        assert_eq!(natural_compare("e.ts", "f.ts"), Ordering::Less);
    }
}
