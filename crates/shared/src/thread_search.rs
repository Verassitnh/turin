//! Port of upstream/packages/shared/src/threadSearch.ts

use crate::js_string::{index_of_utf16, slice_utf16, utf16_len};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSearchTextPart {
    pub text: String,
    pub highlighted: bool,
    pub start: usize,
}

/// JS `text.toLowerCase().replace(/ς/g, "σ")`, falling back to a per-character
/// fold (keeping the original character) whenever lowering changes a string's
/// UTF-16 length — e.g. `İ` lowers to `i` + combining dot.
fn fold_thread_search_case(text: &str) -> String {
    let lowered = text.to_lowercase().replace('ς', "σ");
    if utf16_len(&lowered) == utf16_len(text) {
        return lowered;
    }

    let mut folded = String::new();
    for character in text.chars() {
        let lowered_character: String = character.to_lowercase().collect();
        let lowered_character = lowered_character.replace('ς', "σ");
        if utf16_len(&lowered_character) == character.len_utf16() {
            folded.push_str(&lowered_character);
        } else {
            folded.push(character);
        }
    }
    folded
}

fn scan_thread_search_occurrences(
    text: &str,
    query: &str,
    mut offsets: Option<&mut Vec<usize>>,
) -> usize {
    if text.is_empty() || query.is_empty() {
        return 0;
    }

    let folded_text = fold_thread_search_case(text);
    let folded_query = fold_thread_search_case(query);
    let text_len = utf16_len(&folded_text);
    let query_len = utf16_len(&folded_query);
    let mut count = 0usize;
    let mut cursor = 0usize;

    while cursor + query_len <= text_len {
        let Some(offset) = index_of_utf16(&folded_text, &folded_query, cursor) else {
            break;
        };
        if let Some(offsets) = offsets.as_deref_mut() {
            offsets.push(offset);
        }
        count += 1;
        cursor = offset + query_len;
    }
    count
}

pub fn count_thread_search_occurrences(text: &str, query: &str) -> usize {
    scan_thread_search_occurrences(text, query, None)
}

pub fn find_thread_search_occurrences(text: &str, query: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    scan_thread_search_occurrences(text, query, Some(&mut offsets));
    offsets
}

pub fn split_thread_search_text(text: &str, query: &str) -> Vec<ThreadSearchTextPart> {
    let normalized_query = turin_contracts::base_schemas::js_trim(query);
    if normalized_query.is_empty() {
        return vec![ThreadSearchTextPart {
            text: text.to_owned(),
            highlighted: false,
            start: 0,
        }];
    }

    let query_len = utf16_len(normalized_query);
    let text_len = utf16_len(text);
    let mut parts: Vec<ThreadSearchTextPart> = Vec::new();
    let mut cursor = 0usize;
    for offset in find_thread_search_occurrences(text, normalized_query) {
        if offset > cursor {
            parts.push(ThreadSearchTextPart {
                text: slice_utf16(text, cursor, offset).to_owned(),
                highlighted: false,
                start: cursor,
            });
        }
        parts.push(ThreadSearchTextPart {
            text: slice_utf16(text, offset, offset + query_len).to_owned(),
            highlighted: true,
            start: offset,
        });
        cursor = offset + query_len;
    }
    if cursor < text_len {
        parts.push(ThreadSearchTextPart {
            text: slice_utf16(text, cursor, text_len).to_owned(),
            highlighted: false,
            start: cursor,
        });
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_non_overlapping_occurrences_without_invalidating_unicode_offsets() {
        assert_eq!(find_thread_search_occurrences("Deploy the Deployment", "deploy"), vec![0, 11]);
        assert_eq!(find_thread_search_occurrences("aaaa", "aa"), vec![0, 2]);
        assert_eq!(find_thread_search_occurrences("İİİ needle", "needle"), vec![4]);
    }

    #[test]
    fn splits_search_text_into_highlighted_and_unhighlighted_parts() {
        let parts = split_thread_search_text("one TWO three two", " two ");
        assert_eq!(
            parts,
            vec![
                ThreadSearchTextPart { text: "one".into(), highlighted: false, start: 0 },
                ThreadSearchTextPart { text: "TWO".into(), highlighted: true, start: 4 },
                ThreadSearchTextPart { text: " three ".into(), highlighted: false, start: 7 },
                ThreadSearchTextPart { text: "two".into(), highlighted: true, start: 14 },
            ]
        );
    }

    #[test]
    fn matches_both_greek_sigma_forms_without_changing_source_offsets() {
        assert_eq!(find_thread_search_occurrences("ΟΣ ος οσ", "Σ"), vec![1, 4, 7]);
        assert_eq!(find_thread_search_occurrences("ΟΣ ος οσ", "ς"), vec![1, 4, 7]);
        assert_eq!(find_thread_search_occurrences("İ ΟΣ", "Σ"), vec![3]);
    }
}
