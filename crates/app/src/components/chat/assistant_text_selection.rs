//! Port of the pure text half of upstream/apps/web/src/lib/assistantTextSelection.ts:
//! `createAssistantTextSelector` and `findAssistantCitationText`.
//!
//! Positions are UTF-16 code-unit offsets into the whitespace-normalized text, as in
//! upstream, so a saved citation keeps its meaning across JS and Rust. The DOM half
//! (`captureAssistantTextSelection`, `findAssistantCitationSourceAnchor`,
//! `resolveAssistantCitationRange`) reads a live document and is not ported here.
//!
//! Port of upstream/apps/web/src/lib/assistantTextSelection.ts (pure text half).

use turin_contracts::assistant_citations::ASSISTANT_CITATION_CONTEXT_LENGTH;
use turin_contracts::base_schemas::js_trim;
use turin_shared::js_string::replace_js_whitespace_runs;

/// A quote plus the normalized context around it. Positions are UTF-16 code units. Upstream
/// stores them as JS numbers, so they stay `f64` here: a stored citation can carry any number.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTextSelector {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub prefix: String,
    pub suffix: String,
}

fn normalize_whitespace(text: &str) -> Vec<u16> {
    replace_js_whitespace_runs(text, " ")
        .encode_utf16()
        .collect()
}

fn is_js_whitespace_unit(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(turin_contracts::base_schemas::is_js_whitespace)
}

/// Whether `offset` falls between the two halves of a surrogate pair.
fn splits_surrogate_pair(text: &[u16], offset: usize) -> bool {
    if offset == 0 || offset >= text.len() {
        return false;
    }
    let before = text[offset - 1];
    let after = text[offset];
    (0xd800..=0xdbff).contains(&before) && (0xdc00..=0xdfff).contains(&after)
}

fn find_units(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return (from <= haystack.len()).then_some(from);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len())
        .find(|&index| haystack[index..index + needle.len()] == *needle)
}

/// JS `text.slice(start, end)` over UTF-16 units, clamped to the text.
fn slice_units(text: &[u16], start: usize, end: usize) -> &[u16] {
    let end = end.min(text.len());
    let start = start.min(end);
    &text[start..end]
}

/// Keeps the exact captured text while storing normalized UTF-16 positions and context.
/// `raw_start` and `raw_end` are UTF-16 offsets into `text`.
pub fn create_assistant_text_selector(
    text: &str,
    raw_start: usize,
    raw_end: usize,
) -> Option<AssistantTextSelector> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let quote = String::from_utf16_lossy(slice_units(&units, raw_start, raw_end));
    if js_trim(&quote).is_empty() {
        return None;
    }

    let normalized = normalize_whitespace(text);
    let mut start =
        normalize_whitespace(&String::from_utf16_lossy(slice_units(&units, 0, raw_start))).len();
    // A selection starting inside a whitespace run includes its normalized space.
    if raw_start > 0
        && is_js_whitespace_unit(units[raw_start - 1])
        && units
            .get(raw_start)
            .is_some_and(|&unit| is_js_whitespace_unit(unit))
    {
        start -= 1;
    }
    let end =
        normalize_whitespace(&String::from_utf16_lossy(slice_units(&units, 0, raw_end))).len();
    let context = ASSISTANT_CITATION_CONTEXT_LENGTH;
    let mut prefix_start = start.saturating_sub(context);
    let mut suffix_end = (end + context).min(normalized.len());
    // A split pair becomes a replacement character when the context enters a URL.
    if splits_surrogate_pair(&normalized, prefix_start) {
        prefix_start += 1;
    }
    if splits_surrogate_pair(&normalized, suffix_end) {
        suffix_end -= 1;
    }
    Some(AssistantTextSelector {
        text: quote,
        start: start as f64,
        end: end as f64,
        prefix: String::from_utf16_lossy(slice_units(&normalized, prefix_start, start)),
        suffix: String::from_utf16_lossy(slice_units(&normalized, end, suffix_end)),
    })
}

/// Finds the quote the selector describes. The saved position is only a hint: a match needs
/// the quote and all supplied context, and repeated quotes must not be ambiguous.
pub fn find_assistant_citation_text(
    text: &str,
    selector: &AssistantTextSelector,
) -> Option<(usize, usize)> {
    let normalized = normalize_whitespace(text);
    let quote = normalize_whitespace(&selector.text);
    if js_trim(&selector.text).is_empty() {
        return None;
    }
    let quote_len = quote.len();

    let prefix = normalize_whitespace(&selector.prefix);
    let suffix = normalize_whitespace(&selector.suffix);
    let matches_context = |start: usize, end: usize| {
        slice_units(&normalized, start.saturating_sub(prefix.len()), start) == prefix.as_slice()
            && slice_units(&normalized, end, end + suffix.len()) == suffix.as_slice()
    };

    // Upstream rejects anything that is not a safe non-negative integer span of the quote's length.
    let hint_is_valid = |start: f64, end: f64| {
        start.is_finite()
            && end.is_finite()
            && start.fract() == 0.0
            && end.fract() == 0.0
            && start >= 0.0
            && end - start == quote_len as f64
    };
    let mut hint = None;
    if hint_is_valid(selector.start, selector.end) {
        let (start, end) = (selector.start as usize, selector.end as usize);
        if slice_units(&normalized, start, end) == quote.as_slice() && matches_context(start, end) {
            hint = Some((start, end));
        }
    }
    let mut matched = hint;
    let mut only_quote = None;
    let mut quote_count = 0;

    let mut occurrence = find_units(&normalized, &quote, 0);
    while let Some(start) = occurrence {
        let end = start + quote_len;
        quote_count += 1;
        only_quote = Some((start, end));
        if matches_context(start, end) {
            if matched.is_some_and(|(matched_start, _)| matched_start != start) {
                return None;
            }
            matched = Some((start, end));
        }
        occurrence = find_units(&normalized, &quote, start + 1);
    }

    matched.or(if quote_count == 1 { only_quote } else { None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16_index_of(text: &str, needle: &str) -> usize {
        let index = text.find(needle).expect("needle in text");
        text[..index].encode_utf16().count()
    }

    fn selector(
        text: &str,
        start: f64,
        end: f64,
        prefix: &str,
        suffix: &str,
    ) -> AssistantTextSelector {
        AssistantTextSelector {
            text: text.to_string(),
            start,
            end,
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
        }
    }

    /// Quote with no saved position, so only the text and context decide.
    fn bare(text: &str) -> AssistantTextSelector {
        selector(text, -1.0, -1.0, "", "")
    }

    #[test]
    fn keeps_exact_selected_whitespace_while_normalizing_its_utf16_positions() {
        let text = "Before \n  quote\t \n after";
        assert_eq!(
            create_assistant_text_selector(text, "Before \n".len(), "Before \n  quote\t".len()),
            Some(selector("  quote\t", 6.0, 13.0, "Before", "after"))
        );
    }

    #[test]
    fn keeps_complete_context_code_points_within_the_context_budget() {
        for padding_length in [
            0,
            ASSISTANT_CITATION_CONTEXT_LENGTH - 2,
            ASSISTANT_CITATION_CONTEXT_LENGTH - 1,
            ASSISTANT_CITATION_CONTEXT_LENGTH,
        ] {
            let padding = "x".repeat(padding_length);
            let text = format!("😀{padding}quote{padding}🚀");
            let start = utf16_index_of(&text, "quote");
            let captured =
                create_assistant_text_selector(&text, start, start + 5).expect("selector");
            let keep_emoji = padding_length + 2 <= ASSISTANT_CITATION_CONTEXT_LENGTH;
            assert_eq!(
                captured,
                selector(
                    "quote",
                    start as f64,
                    (start + 5) as f64,
                    &format!("{}{padding}", if keep_emoji { "😀" } else { "" }),
                    &format!("{padding}{}", if keep_emoji { "🚀" } else { "" }),
                )
            );
        }
    }

    #[test]
    fn preserves_multiline_quotes_and_relocates_them_after_surrounding_text_changes() {
        let quote = "selected\r\n  code 🚀";
        let prefix = "x".repeat(ASSISTANT_CITATION_CONTEXT_LENGTH - 1);
        let suffix = "y".repeat(ASSISTANT_CITATION_CONTEXT_LENGTH - 1);
        let text = format!("{quote} elsewhere\n😀{prefix}{quote}{suffix}🚀");
        let raw_start = text.rfind(quote).expect("quote");
        let raw_start = text[..raw_start].encode_utf16().count();
        let raw_end = raw_start + quote.encode_utf16().count();
        let captured = create_assistant_text_selector(&text, raw_start, raw_end).expect("selector");
        let start = "selected code 🚀 elsewhere 😀".encode_utf16().count() + prefix.len();
        let end = start + "selected code 🚀".encode_utf16().count();
        assert_eq!(
            captured,
            selector(quote, start as f64, end as f64, &prefix, &suffix)
        );

        assert_eq!(
            find_assistant_citation_text(&text, &captured),
            Some((start, end))
        );
        let shifted = format!("Inserted paragraph.\n{text}");
        assert_eq!(
            find_assistant_citation_text(&shifted, &captured),
            Some((
                "Inserted paragraph. ".len() + start,
                "Inserted paragraph. ".len() + end
            ))
        );
    }

    #[test]
    fn does_not_relocate_to_a_replacement_character_decoy_after_clipping_the_prefix_or_suffix() {
        for side in ["prefix", "suffix"] {
            let context = ASSISTANT_CITATION_CONTEXT_LENGTH;
            let prefix = "x".repeat(context - usize::from(side == "prefix"));
            let suffix = "y".repeat(context - usize::from(side == "suffix"));
            let occurrence = |character: &str| {
                format!(
                    "{}{prefix}quote{suffix}{}",
                    if side == "prefix" { character } else { "" },
                    if side == "suffix" { character } else { "" },
                )
            };
            let text = format!("{} / {}", occurrence("😀"), occurrence("\u{FFFD}"));
            let start = utf16_index_of(&text, "quote");
            let captured =
                create_assistant_text_selector(&text, start, start + 5).expect("selector");
            assert_eq!(find_assistant_citation_text(&text, &captured), None);
        }
    }

    #[test]
    fn rejects_empty_or_whitespace_only_selections() {
        for text in ["", " \n\t\r\n"] {
            assert_eq!(create_assistant_text_selector(text, 0, text.len()), None);
        }
    }

    #[test]
    fn resolves_an_exact_selection_with_its_saved_position_and_context() {
        assert_eq!(
            find_assistant_citation_text(
                "Before the selected text after.",
                &selector("selected text", 11.0, 24.0, "Before the ", " after."),
            ),
            Some((11, 24))
        );
    }

    #[test]
    fn finds_a_unique_quote_when_insertion_before_it_shifts_its_offsets() {
        assert_eq!(
            find_assistant_citation_text(
                "An inserted paragraph. Before the selected text after.",
                &selector("selected text", 11.0, 24.0, "Before the ", " after."),
            ),
            Some((34, 47))
        );
    }

    #[test]
    fn still_finds_a_unique_quote_after_its_surrounding_text_changes() {
        assert_eq!(
            find_assistant_citation_text(
                "New selected text nearby.",
                &selector("selected text", 11.0, 24.0, "Before the ", " after."),
            ),
            Some((4, 17))
        );
    }

    #[test]
    fn uses_both_context_sides_when_each_side_alone_matches_several_repeated_quotes() {
        assert_eq!(
            find_assistant_citation_text(
                "left quote one; other quote two; left quote two",
                &selector("quote", 5.0, 10.0, "left ", " two"),
            ),
            Some((38, 43))
        );
    }

    #[test]
    fn does_not_trust_stale_offsets_that_now_point_at_another_occurrence() {
        assert_eq!(
            find_assistant_citation_text(
                "wrong quote here; right quote there",
                &selector("quote", 6.0, 11.0, "right ", " there"),
            ),
            Some((24, 29))
        );
    }

    #[test]
    fn allows_a_single_context_side_to_disambiguate() {
        assert_eq!(
            find_assistant_citation_text(
                "first quote / quote last",
                &selector("quote", -1.0, -1.0, "first ", "")
            ),
            Some((6, 11))
        );
        assert_eq!(
            find_assistant_citation_text(
                "first quote / quote last",
                &selector("quote", -1.0, -1.0, "", " last")
            ),
            Some((14, 19))
        );
    }

    #[test]
    fn does_not_guess_between_quotes_without_context_even_at_the_saved_position() {
        assert_eq!(
            find_assistant_citation_text("quote / quote", &bare("quote")),
            None
        );
    }

    #[test]
    fn does_not_use_distance_or_saved_offsets_to_break_a_context_tie() {
        assert_eq!(
            find_assistant_citation_text(
                "same quote end / same quote end",
                &selector("quote", 5.0, 10.0, "same ", " end"),
            ),
            None
        );
    }

    #[test]
    fn rejects_repeated_quotes_when_neither_occurrence_matches_all_supplied_context() {
        assert_eq!(
            find_assistant_citation_text(
                "left quote wrong / wrong quote right",
                &selector("quote", -1.0, -1.0, "left ", " right"),
            ),
            None
        );
    }

    #[test]
    fn counts_overlapping_occurrences_when_checking_ambiguity() {
        assert_eq!(
            find_assistant_citation_text("banana", &selector("ana", 1.0, 4.0, "", "")),
            None
        );
    }

    #[test]
    fn matches_multiline_code_after_indentation_tabs_and_line_endings_change() {
        assert_eq!(
            find_assistant_citation_text(
                "Example:\nif (ready) {\n\trun();\n}\nDone.",
                &selector(
                    "if (ready) {\r\n    run();\r\n}",
                    -1.0,
                    -1.0,
                    "Example:\n",
                    "\nDone."
                ),
            ),
            Some((9, 30))
        );
    }

    #[test]
    fn normalizes_nonbreaking_spaces_and_keeps_selected_boundary_whitespace() {
        assert_eq!(
            find_assistant_citation_text(
                "before\u{a0}\n quoted\t text \nafter",
                &bare("\nquoted  text\t")
            ),
            Some((6, 19))
        );
    }

    #[test]
    fn does_not_trim_the_documents_leading_whitespace_out_of_its_offsets() {
        assert_eq!(
            find_assistant_citation_text("\n\t  quote", &bare("quote")),
            Some((1, 6))
        );
    }

    #[test]
    fn uses_utf16_offsets_for_emoji_and_combining_characters() {
        assert_eq!(
            find_assistant_citation_text("😀 cafe\u{301} 🚀 done", &bare("cafe\u{301} 🚀")),
            Some((3, 11))
        );
    }

    #[test]
    fn uses_up_to_32_utf16_context_units_without_requiring_the_entire_surrounding_text() {
        let prefix = format!("{} ", "x".repeat(ASSISTANT_CITATION_CONTEXT_LENGTH - 1));
        let suffix = format!(" {}", "y".repeat(ASSISTANT_CITATION_CONTEXT_LENGTH - 1));
        let text = format!("unrelated quote / {prefix}quote{suffix} changed further away");
        let start = text.rfind("quote").expect("quote");
        let start = text[..start].encode_utf16().count();
        assert_eq!(
            find_assistant_citation_text(&text, &selector("quote", -1.0, -1.0, &prefix, &suffix)),
            Some((start, start + 5))
        );
    }

    #[test]
    fn rejects_empty_or_missing_literal_text() {
        for quote in ["", " \n\t\r\n", "absent", "QUOTE", "qu.te"] {
            assert_eq!(
                find_assistant_citation_text("quote", &bare(quote)),
                None,
                "quote {quote:?}"
            );
        }
    }

    #[test]
    fn treats_invalid_stored_offsets_as_unusable_hints() {
        for (start, end) in [
            (-1.0, 4.0),
            (2.5, 7.5),
            (f64::NAN, f64::NAN),
            (f64::INFINITY, f64::INFINITY),
            (8.0, 3.0),
        ] {
            assert_eq!(
                find_assistant_citation_text("a quote", &selector("quote", start, end, "", "")),
                Some((2, 7)),
                "offsets {start} / {end}"
            );
        }
    }
}
