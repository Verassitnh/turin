//! Port of upstream/apps/web/src/composer-editor-mentions.ts
//!
//! Splits a prompt into text and inline-token segments: file mentions,
//! skills, assistant citations and composer context references. Offsets are
//! UTF-16 code units, like upstream.

use turin_contracts::assistant_citations::AssistantCitation;
use turin_shared::assistant_citations::collect_assistant_citations;
use turin_shared::composer_context_references::collect_composer_context_references;

use crate::composer::inline_tokens::{
    CollectComposerInlineTokensOptions, ComposerInlineToken, ComposerInlineTokenKind,
    collect_composer_inline_tokens,
};
use crate::composer::js_string::{is_js_whitespace_char, utf16_len};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerPromptSegment {
    Text {
        text: String,
    },
    Mention {
        path: String,
        source: String,
    },
    Skill {
        name: String,
        source: String,
    },
    Citation {
        citation: AssistantCitation,
        source: String,
    },
    ContextReference {
        kind: String,
        context_id: String,
        label: String,
        source: String,
    },
}

impl ComposerPromptSegment {
    pub fn is_inline_token(&self) -> bool {
        !matches!(self, Self::Text { .. })
    }

    /// The prompt text this segment stands for.
    pub fn source(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Mention { source, .. }
            | Self::Skill { source, .. }
            | Self::Citation { source, .. }
            | Self::ContextReference { source, .. } => source,
        }
    }

    /// Length of [`Self::source`] in UTF-16 code units.
    pub fn source_len(&self) -> usize {
        utf16_len(self.source())
    }
}

/// One match of `collectComposerPromptInlineTokens`: a mention or skill
/// token, or a citation / context-reference link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerPromptToken {
    Inline(ComposerInlineToken),
    Citation {
        citation: AssistantCitation,
        source: String,
        start: usize,
        end: usize,
    },
    ContextReference {
        kind: String,
        context_id: String,
        label: String,
        source: String,
        start: usize,
        end: usize,
    },
}

impl ComposerPromptToken {
    /// UTF-16 offset of the token's first unit.
    pub fn start(&self) -> usize {
        match self {
            Self::Inline(token) => token.start,
            Self::Citation { start, .. } | Self::ContextReference { start, .. } => *start,
        }
    }

    /// UTF-16 offset just past the token.
    pub fn end(&self) -> usize {
        match self {
            Self::Inline(token) => token.end,
            Self::Citation { end, .. } | Self::ContextReference { end, .. } => *end,
        }
    }

    /// A file mention or a skill (`type === "mention" || type === "skill"`).
    pub fn is_mention_or_skill(&self) -> bool {
        matches!(self, Self::Inline(_))
    }
}

/// `collectComposerPromptInlineTokens`.
pub fn collect_composer_prompt_inline_tokens(text: &str) -> Vec<ComposerPromptToken> {
    let tokens =
        collect_composer_inline_tokens(text, CollectComposerInlineTokensOptions::default());
    // The shared collectors report byte offsets; upstream's are UTF-16.
    let utf16_at = |byte: usize| utf16_len(&text[..byte]);
    let mut links: Vec<ComposerPromptToken> = collect_assistant_citations(text)
        .into_iter()
        .map(|found| ComposerPromptToken::Citation {
            start: utf16_at(found.start),
            end: utf16_at(found.end),
            citation: found.citation,
            source: found.source,
        })
        .collect();
    links.extend(
        collect_composer_context_references(text)
            .into_iter()
            .map(|found| ComposerPromptToken::ContextReference {
                start: utf16_at(found.start),
                end: utf16_at(found.end),
                kind: found.kind.as_str().to_owned(),
                context_id: found.context_id.as_str().to_owned(),
                label: found.label,
                source: found.source,
            }),
    );
    if links.is_empty() {
        return tokens
            .into_iter()
            .map(ComposerPromptToken::Inline)
            .collect();
    }
    // An unfinished @ mention can otherwise consume the start of a link label.
    let mut all: Vec<ComposerPromptToken> = tokens
        .into_iter()
        .filter(|token| {
            !links
                .iter()
                .any(|link| token.start < link.end() && token.end > link.start())
        })
        .map(ComposerPromptToken::Inline)
        .collect();
    all.extend(links);
    // Stable, like `Array.prototype.sort`.
    all.sort_by_key(ComposerPromptToken::start);
    all
}

fn push_text_segment(segments: &mut Vec<ComposerPromptSegment>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(ComposerPromptSegment::Text { text: last }) = segments.last_mut() {
        last.push_str(text);
        return;
    }
    segments.push(ComposerPromptSegment::Text {
        text: text.to_owned(),
    });
}

/// `splitPromptIntoComposerSegments`.
pub fn split_prompt_into_composer_segments(prompt: &str) -> Vec<ComposerPromptSegment> {
    let mut segments = Vec::new();
    if prompt.is_empty() {
        return segments;
    }
    let units: Vec<u16> = prompt.encode_utf16().collect();
    let slice = |from: usize, to: usize| String::from_utf16_lossy(&units[from..to]);
    let mut cursor = 0usize;
    for token in collect_composer_prompt_inline_tokens(prompt) {
        if token.start() < cursor {
            continue;
        }
        if token.start() > cursor {
            push_text_segment(&mut segments, &slice(cursor, token.start()));
        }
        cursor = token.end();
        segments.push(match token {
            ComposerPromptToken::Citation {
                citation, source, ..
            } => ComposerPromptSegment::Citation { citation, source },
            ComposerPromptToken::ContextReference {
                kind,
                context_id,
                label,
                source,
                ..
            } => ComposerPromptSegment::ContextReference {
                kind,
                context_id,
                label,
                source,
            },
            ComposerPromptToken::Inline(token) => match token.kind {
                ComposerInlineTokenKind::Mention => ComposerPromptSegment::Mention {
                    path: token.value,
                    source: token.source,
                },
                ComposerInlineTokenKind::Skill => ComposerPromptSegment::Skill {
                    name: token.value,
                    source: token.source,
                },
            },
        });
    }
    if cursor < units.len() {
        push_text_segment(&mut segments, &slice(cursor, units.len()));
    }
    segments
}

fn range_includes_index(start: usize, end: usize, index: usize) -> bool {
    start <= index && index < end
}

/// `selectionTouchesMentionBoundary`.
pub fn selection_touches_mention_boundary(prompt: &str, start: usize, end: usize) -> bool {
    if prompt.is_empty() || start >= end {
        return false;
    }
    let units: Vec<u16> = prompt.encode_utf16().collect();
    let is_space = |index: usize| {
        units
            .get(index)
            .and_then(|unit| char::from_u32(u32::from(*unit)))
            .is_some_and(is_js_whitespace_char)
    };
    collect_composer_prompt_inline_tokens(prompt)
        .iter()
        .filter_map(|token| match token {
            ComposerPromptToken::Inline(token)
                if token.kind == ComposerInlineTokenKind::Mention =>
            {
                Some(token)
            }
            _ => None,
        })
        .any(|token| {
            if token.start >= 1
                && is_space(token.start - 1)
                && range_includes_index(start, end, token.start - 1)
            {
                return true;
            }
            token.end < units.len()
                && is_space(token.end)
                && range_includes_index(start, end, token.end)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> ComposerPromptSegment {
        ComposerPromptSegment::Text {
            text: value.to_owned(),
        }
    }
    fn mention(path: &str, source: &str) -> ComposerPromptSegment {
        ComposerPromptSegment::Mention {
            path: path.to_owned(),
            source: source.to_owned(),
        }
    }
    fn skill(name: &str, source: &str) -> ComposerPromptSegment {
        ComposerPromptSegment::Skill {
            name: name.to_owned(),
            source: source.to_owned(),
        }
    }

    #[test]
    fn splits_mention_tokens_followed_by_whitespace_into_mention_segments() {
        assert_eq!(
            split_prompt_into_composer_segments("Inspect @AGENTS.md please"),
            vec![
                text("Inspect "),
                mention("AGENTS.md", "@AGENTS.md"),
                text(" please")
            ]
        );
    }

    #[test]
    fn does_not_convert_an_incomplete_trailing_mention_token() {
        assert_eq!(
            split_prompt_into_composer_segments("Inspect @AGENTS.md"),
            vec![text("Inspect @AGENTS.md")]
        );
    }

    #[test]
    fn keeps_newlines_around_mention_tokens() {
        assert_eq!(
            split_prompt_into_composer_segments("one\n@AGENTS.md \ntwo"),
            vec![
                text("one\n"),
                mention("AGENTS.md", "@AGENTS.md"),
                text(" \ntwo")
            ]
        );
    }

    #[test]
    fn splits_quoted_mention_tokens_containing_whitespace() {
        assert_eq!(
            split_prompt_into_composer_segments("Inspect @\"My File.md\" please"),
            vec![
                text("Inspect "),
                mention("My File.md", "@\"My File.md\""),
                text(" please")
            ]
        );
    }

    #[test]
    fn unescapes_quoted_mention_token_content() {
        assert_eq!(
            split_prompt_into_composer_segments("Inspect @\"docs/My \\\"File\\\".md\" please"),
            vec![
                text("Inspect "),
                mention("docs/My \"File\".md", "@\"docs/My \\\"File\\\".md\""),
                text(" please")
            ]
        );
    }

    #[test]
    fn splits_generated_markdown_file_links_into_mention_segments() {
        assert_eq!(
            split_prompt_into_composer_segments(
                "Inspect [package.json](path/to/package.json) before continuing"
            ),
            vec![
                text("Inspect "),
                mention(
                    "path/to/package.json",
                    "[package.json](path/to/package.json)"
                ),
                text(" before continuing")
            ]
        );
    }

    #[test]
    fn does_not_turn_normal_web_links_into_file_mention_segments() {
        let prompt = "Read [the docs](https://example.com/docs) first";
        assert_eq!(
            split_prompt_into_composer_segments(prompt),
            vec![text(prompt)]
        );
    }

    #[test]
    fn does_not_turn_scoped_package_references_into_file_mention_segments() {
        for reference in ["@expo/ui", "@jane/foo.js", "@scope/pkg/sub/path"] {
            let prompt = format!("Install {reference} next");
            assert_eq!(
                split_prompt_into_composer_segments(&prompt),
                vec![text(&prompt)]
            );
        }
    }

    #[test]
    fn keeps_ime_composed_text_containing_a_scoped_package_reference_as_text() {
        let prompt = "入力 @expo/ui　を追加";
        assert_eq!(
            split_prompt_into_composer_segments(prompt),
            vec![text(prompt)]
        );
    }

    #[test]
    fn turns_canonical_scoped_folder_links_into_file_mention_segments() {
        assert_eq!(
            split_prompt_into_composer_segments("Inspect [sub](@scope/pkg/sub) next"),
            vec![
                text("Inspect "),
                mention("@scope/pkg/sub", "[sub](@scope/pkg/sub)"),
                text(" next")
            ]
        );
    }

    #[test]
    fn decodes_reserved_path_characters_from_generated_links() {
        assert_eq!(
            split_prompt_into_composer_segments(
                "Inspect [config#draft?.json](config%23draft%3F.json) before continuing"
            ),
            vec![
                text("Inspect "),
                mention(
                    "config#draft?.json",
                    "[config#draft?.json](config%23draft%3F.json)"
                ),
                text(" before continuing")
            ]
        );
    }

    #[test]
    fn splits_skill_tokens_followed_by_whitespace_into_skill_segments() {
        assert_eq!(
            split_prompt_into_composer_segments("Use $review-follow-up please"),
            vec![
                text("Use "),
                skill("review-follow-up", "$review-follow-up"),
                text(" please")
            ]
        );
        assert_eq!(
            split_prompt_into_composer_segments("Use $2spec please"),
            vec![text("Use "), skill("2spec", "$2spec"), text(" please")]
        );
    }

    #[test]
    fn keeps_digits_only_dollar_amounts_and_compact_monetary_expressions_as_text() {
        for prompt in [
            "I'll pay $20 tomorrow",
            "Budget is $20k tomorrow",
            "Cost is $100M total",
            "Limit is $1e6 here",
        ] {
            assert_eq!(
                split_prompt_into_composer_segments(prompt),
                vec![text(prompt)]
            );
        }
    }

    #[test]
    fn does_not_convert_an_incomplete_trailing_skill_token() {
        assert_eq!(
            split_prompt_into_composer_segments("Use $review-follow-up"),
            vec![text("Use $review-follow-up")]
        );
    }

    // ── Citations and context references ────────────────────────────────

    fn citation() -> AssistantCitation {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "environmentId": "remote/環境",
            "threadId": "thread-1",
            "messageId": "message-1",
            "text": "Use @AGENTS.md, $review and \"雪 ❄️\" (carefully).",
            "start": 4,
            "end": 50,
            "prefix": "前: ",
            "suffix": " 後",
        }))
        .expect("citation")
    }

    fn citation_segment(citation: AssistantCitation, source: &str) -> ComposerPromptSegment {
        ComposerPromptSegment::Citation {
            citation,
            source: source.to_owned(),
        }
    }

    /// `formatTerminalContextReference({ id: "ctx-1", terminalLabel: "Terminal 1", lineStart: 3, lineEnd: 4 })`.
    fn terminal_reference() -> String {
        turin_shared::composer_context_references::format_composer_context_reference(
            "terminal",
            "terminal_ctx-1",
            "Terminal 1 lines 3-4",
        )
    }

    fn terminal_segment() -> ComposerPromptSegment {
        ComposerPromptSegment::ContextReference {
            kind: "terminal".to_owned(),
            context_id: "terminal_ctx-1".to_owned(),
            label: "Terminal 1 lines 3-4".to_owned(),
            source: terminal_reference(),
        }
    }

    use turin_shared::assistant_citations::serialize_assistant_citation;

    #[test]
    fn keeps_multiple_assistant_citations_atomic_next_to_punctuation_and_unicode() {
        let citation = citation();
        let source = serialize_assistant_citation(&citation);
        let other = AssistantCitation {
            message_id: turin_contracts::base_schemas::MessageId::new("message-2").expect("id"),
            text: "A second quote".to_owned(),
            ..citation.clone()
        };
        let other_source = serialize_assistant_citation(&other);
        assert_eq!(
            split_prompt_into_composer_segments(&format!("前({source}),{other_source}後")),
            vec![
                text("前("),
                citation_segment(citation, &source),
                text("),"),
                citation_segment(other, &other_source),
                text("後"),
            ]
        );
    }

    #[test]
    fn preserves_exact_citation_source_encoding_for_adjacent_chips_at_the_end_of_a_prompt() {
        let citation = citation();
        let source = serialize_assistant_citation(&citation).replace('+', "%20");
        assert_eq!(
            split_prompt_into_composer_segments(&format!("{source}{source}")),
            vec![
                citation_segment(citation.clone(), &source),
                citation_segment(citation, &source),
            ]
        );
    }

    #[test]
    fn keeps_a_citation_after_an_unfinished_mention_intact() {
        for prefix in ["@", "@AGENTS.md"] {
            let citation = citation();
            let source = serialize_assistant_citation(&citation);
            assert_eq!(
                split_prompt_into_composer_segments(&format!("{prefix}{source}")),
                vec![text(prefix), citation_segment(citation, &source)],
                "{prefix}"
            );
        }
    }

    #[test]
    fn parses_citations_alongside_file_mentions_skills_and_context_references() {
        let citation = citation();
        let source = serialize_assistant_citation(&citation);
        let reference = terminal_reference();
        assert_eq!(
            split_prompt_into_composer_segments(&format!(
                "@AGENTS.md {source}\n$review {reference}{source}"
            )),
            vec![
                mention("AGENTS.md", "@AGENTS.md"),
                text(" "),
                citation_segment(citation.clone(), &source),
                text("\n"),
                skill("review", "$review"),
                text(" "),
                terminal_segment(),
                citation_segment(citation, &source),
            ]
        );
    }

    #[test]
    fn keeps_malformed_citation_links_as_editable_text() {
        let prompt = "[Assistant quote](t3-citation://v1/env/thread/message?text=missing+metadata)";
        assert_eq!(
            split_prompt_into_composer_segments(prompt),
            vec![text(prompt)]
        );
    }

    #[test]
    fn keeps_context_references_at_their_prompt_positions() {
        assert_eq!(
            split_prompt_into_composer_segments(&format!(
                "Inspect {} @AGENTS.md please",
                terminal_reference()
            )),
            vec![
                text("Inspect "),
                terminal_segment(),
                text(" "),
                mention("AGENTS.md", "@AGENTS.md"),
                text(" please"),
            ]
        );
    }

    #[test]
    fn preserves_consecutive_context_references_without_dropping_positions() {
        let reference = terminal_reference();
        assert_eq!(
            split_prompt_into_composer_segments(&format!("{reference}{reference}tail")),
            vec![terminal_segment(), terminal_segment(), text("tail")]
        );
    }

    #[test]
    fn keeps_skill_parsing_alongside_mentions_and_context_references() {
        assert_eq!(
            split_prompt_into_composer_segments(&format!(
                "Inspect {} $review-follow-up after @AGENTS.md ",
                terminal_reference()
            )),
            vec![
                text("Inspect "),
                terminal_segment(),
                text(" "),
                skill("review-follow-up", "$review-follow-up"),
                text(" after "),
                mention("AGENTS.md", "@AGENTS.md"),
                text(" "),
            ]
        );
    }

    #[test]
    fn leaves_a_context_link_with_an_unparsable_href_as_text() {
        let prompt = "see [x](t3-context://v1/terminal/ctx 1) now";
        assert_eq!(
            split_prompt_into_composer_segments(prompt),
            vec![text(prompt)]
        );
    }

    #[test]
    fn does_not_treat_text_before_a_citation_as_an_overlapping_file_mention() {
        let prompt = format!("before @{}", serialize_assistant_citation(&citation()));
        assert!(!selection_touches_mention_boundary(&prompt, 6, 7));
    }

    #[test]
    fn mention_boundary_selection() {
        let prompt = "see @a.md here";
        assert!(selection_touches_mention_boundary(prompt, 3, 4));
        assert!(selection_touches_mention_boundary(prompt, 9, 10));
        assert!(!selection_touches_mention_boundary(prompt, 4, 7));
        assert!(!selection_touches_mention_boundary(prompt, 4, 4));
    }
}
