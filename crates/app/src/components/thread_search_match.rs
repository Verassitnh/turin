//! Port of upstream/apps/web/src/components/ThreadSearchMatch.tsx
//!
//! `ThreadSearchMatchExcerpt`: the "You:/Agent:" message excerpt under a
//! search result, with every occurrence of the query emphasized.

use gpui_kit::{
    App, FontWeight, HighlightStyle, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, StyledText, Window, div,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;

/// `foldAsciiCase`: lowercase A–Z only, so byte offsets stay aligned.
fn fold_ascii_case(value: &str) -> String {
    value.to_ascii_lowercase()
}

/// `HighlightedSearchText` as `(range, highlighted)` byte ranges over `text`.
pub fn highlighted_search_ranges(text: &str, query: &str) -> Vec<(std::ops::Range<usize>, bool)> {
    let query = query.trim();
    if query.is_empty() {
        return vec![(0..text.len(), false)];
    }
    let normalized_text = fold_ascii_case(text);
    let normalized_query = fold_ascii_case(query);
    let mut parts = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(found) = normalized_text[cursor..].find(&normalized_query) else {
            parts.push((cursor..text.len(), false));
            break;
        };
        let start = cursor + found;
        if start > cursor {
            parts.push((cursor..start, false));
        }
        let end = start + query.len();
        parts.push((start..end, true));
        cursor = end;
    }
    parts
}

/// Which side wrote the matched message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMatchSource {
    User,
    Assistant,
}

#[derive(IntoElement)]
pub struct ThreadSearchMatchExcerpt {
    pub source: SearchMatchSource,
    pub snippet: SharedString,
    pub query: SharedString,
}

impl RenderOnce for ThreadSearchMatchExcerpt {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let is_user = self.source == SearchMatchSource::User;
        let prefix = if is_user { "You:" } else { "Agent:" };
        let prefix_color = if is_user {
            theme.info_foreground
        } else {
            theme.success_foreground
        };
        // `<mark className="bg-transparent font-semibold text-foreground">`.
        let highlight = HighlightStyle {
            color: Some(theme.foreground),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..Default::default()
        };
        let text = format!("{prefix} {}", self.snippet);
        let offset = prefix.len() + 1;
        let mut highlights = vec![(
            0..prefix.len(),
            HighlightStyle {
                color: Some(prefix_color),
                ..Default::default()
            },
        )];
        highlights.extend(
            highlighted_search_ranges(&self.snippet, &self.query)
                .into_iter()
                .filter(|(_, highlighted)| *highlighted)
                .map(|(range, _)| (range.start + offset..range.end + offset, highlight)),
        );
        div()
            .min_w_0()
            .truncate()
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .text_color(with_alpha(theme.muted_foreground, 0.85))
            .child(StyledText::new(text).with_highlights(highlights))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(text: &str, query: &str) -> Vec<(&'static str, bool)> {
        highlighted_search_ranges(text, query)
            .into_iter()
            .map(|(range, highlighted)| (&*text[range].to_string().leak(), highlighted))
            .collect()
    }

    #[test]
    fn emphasizes_every_case_insensitive_occurrence() {
        assert_eq!(
            parts("Fix the Sidebar, then sidebar tests", "sidebar"),
            vec![
                ("Fix the ", false),
                ("Sidebar", true),
                (", then ", false),
                ("sidebar", true),
                (" tests", false)
            ]
        );
    }

    #[test]
    fn leaves_text_plain_for_a_blank_query_or_no_match() {
        assert_eq!(parts("hello", "  "), vec![("hello", false)]);
        assert_eq!(parts("hello", "xyz"), vec![("hello", false)]);
        assert_eq!(parts("abc", "abc"), vec![("abc", true)]);
    }
}
