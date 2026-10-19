//! Port of the pure helpers in upstream/apps/web/src/lib/utils.ts that the
//! command palette and branch toolbar read.
//!
//! Port of upstream/apps/web/src/lib/utils.ts (pure helpers the app uses).

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

/// `normalizeSearchText`: NFKD, marks stripped, lower-cased, whitespace
/// collapsed and trimmed. (`to_lowercase` is locale independent, as the
/// upstream test requires.)
pub fn normalize_search_text(value: &str) -> String {
    let stripped: String = value.nfkd().filter(|c| !is_combining_mark(*c)).collect();
    stripped
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_marks_case_and_extra_whitespace() {
        assert_eq!(
            normalize_search_text("  Thè\u{1ab0}mes   Café "),
            "themes cafe"
        );
        assert_eq!(normalize_search_text("GIT"), "git");
    }
}
