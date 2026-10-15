//! Port of upstream/apps/web/src/composer-editor-mentions.ts
//!
//! Splits a prompt into text and inline-token segments. Offsets are UTF-16
//! code units, like upstream.
//!
//! Assistant citations and composer context references (terminal contexts,
//! PR findings, file comments) have no producer in Turin yet; their segment
//! kinds are omitted (PORTING.md, Deviations).

use crate::composer::inline_tokens::{
    CollectComposerInlineTokensOptions, ComposerInlineToken, ComposerInlineTokenKind,
    collect_composer_inline_tokens,
};
use crate::composer::js_string::{is_js_whitespace_char, utf16_len};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerPromptSegment {
    Text { text: String },
    Mention { path: String, source: String },
    Skill { name: String, source: String },
}

impl ComposerPromptSegment {
    pub fn is_inline_token(&self) -> bool {
        !matches!(self, Self::Text { .. })
    }

    /// The prompt text this segment stands for.
    pub fn source(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Mention { source, .. } | Self::Skill { source, .. } => source,
        }
    }

    /// Length of [`Self::source`] in UTF-16 code units.
    pub fn source_len(&self) -> usize {
        utf16_len(self.source())
    }
}

/// `collectComposerPromptInlineTokens`.
pub fn collect_composer_prompt_inline_tokens(text: &str) -> Vec<ComposerInlineToken> {
    collect_composer_inline_tokens(text, CollectComposerInlineTokensOptions::default())
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
        if token.start < cursor {
            continue;
        }
        if token.start > cursor {
            push_text_segment(&mut segments, &slice(cursor, token.start));
        }
        segments.push(match token.kind {
            ComposerInlineTokenKind::Mention => ComposerPromptSegment::Mention {
                path: token.value,
                source: token.source,
            },
            ComposerInlineTokenKind::Skill => ComposerPromptSegment::Skill {
                name: token.value,
                source: token.source,
            },
        });
        cursor = token.end;
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
        .filter(|token| token.kind == ComposerInlineTokenKind::Mention)
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
                mention("path/to/package.json", "[package.json](path/to/package.json)"),
                text(" before continuing")
            ]
        );
    }

    #[test]
    fn does_not_turn_normal_web_links_into_file_mention_segments() {
        let prompt = "Read [the docs](https://example.com/docs) first";
        assert_eq!(split_prompt_into_composer_segments(prompt), vec![text(prompt)]);
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
        assert_eq!(split_prompt_into_composer_segments(prompt), vec![text(prompt)]);
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
            assert_eq!(split_prompt_into_composer_segments(prompt), vec![text(prompt)]);
        }
    }

    #[test]
    fn does_not_convert_an_incomplete_trailing_skill_token() {
        assert_eq!(
            split_prompt_into_composer_segments("Use $review-follow-up"),
            vec![text("Use $review-follow-up")]
        );
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
