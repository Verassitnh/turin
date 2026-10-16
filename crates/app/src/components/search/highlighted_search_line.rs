//! Port of upstream/apps/web/src/components/search/HighlightedSearchLine.tsx
#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock};
use std::thread;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, Hsla, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
    rems,
};
use parking_lot::Mutex;
use turin_contracts::project::ProjectContentMatch;
use turin_shared::js_string::{slice_utf16, utf16_len};

use crate::components::chat::markdown::code_highlight;
use crate::diff::highlight::language_for_path;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHighlightRange {
    pub start: usize,
    pub end: usize,
}

fn utf16_unit_to_byte_offset(line: &str, unit: usize) -> usize {
    slice_utf16(line, 0, unit).len()
}

pub fn normalize_search_ranges(match_: &ProjectContentMatch) -> Vec<SearchHighlightRange> {
    let line = &match_.line_content;
    let utf16_len_line = utf16_len(line);
    let mut ranges: Vec<SearchHighlightRange> = match_
        .match_ranges
        .iter()
        .map(|range| {
            let start = utf16_unit_to_byte_offset(line, (range.start as usize).min(utf16_len_line));
            let end = utf16_unit_to_byte_offset(line, (range.end as usize).min(utf16_len_line));
            SearchHighlightRange { start, end }
        })
        .filter(|range| range.end > range.start)
        .collect();
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<SearchHighlightRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

#[derive(Clone, Debug)]
pub struct CodeTokenSpan {
    pub content: String,
    pub offset: usize,
    pub color: Option<Hsla>,
    pub italic: bool,
    pub bold: bool,
    pub underline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSegment {
    pub content: String,
    pub is_match: bool,
    pub color: Option<Hsla>,
    pub italic: bool,
    pub bold: bool,
    pub underline: bool,
}

pub fn split_token_into_segments(
    line: &str,
    token: &CodeTokenSpan,
    ranges: &[SearchHighlightRange],
) -> Vec<HighlightSegment> {
    let token_end = token.offset + token.content.len();
    let mut cursor = token.offset;
    let mut segments = Vec::new();
    for range in ranges {
        if range.end <= cursor {
            continue;
        }
        if range.start >= token_end {
            break;
        }
        let match_start = cursor.max(range.start);
        if match_start > cursor {
            segments.push(segment_from(&line[cursor..match_start], false, token));
        }
        let match_end = token_end.min(range.end);
        segments.push(segment_from(&line[match_start..match_end], true, token));
        cursor = match_end;
    }
    if cursor < token_end {
        segments.push(segment_from(&line[cursor..token_end], false, token));
    }
    segments
}

fn segment_from(slice: &str, is_match: bool, token: &CodeTokenSpan) -> HighlightSegment {
    HighlightSegment {
        content: slice.to_owned(),
        is_match,
        color: token.color,
        italic: token.italic,
        bold: token.bold,
        underline: token.underline,
    }
}

pub fn fallback_token(line: &str) -> CodeTokenSpan {
    CodeTokenSpan {
        content: line.to_owned(),
        offset: 0,
        color: None,
        italic: false,
        bold: false,
        underline: false,
    }
}

pub fn code_tokens_for_line(line: &str, path: &str, dark: bool) -> Vec<CodeTokenSpan> {
    let language = language_for_path(path);
    let highlighted = code_highlight::highlight_code(line, language, dark);
    if highlighted.tokens.is_empty() {
        return vec![fallback_token(line)];
    }
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let mut cursor = 0usize;
    for (range, color) in &highlighted.tokens {
        if range.start >= range.end || range.start >= bytes.len() {
            continue;
        }
        let end = range.end.min(bytes.len());
        if range.start > cursor {
            spans.push(CodeTokenSpan {
                content: line[cursor..range.start].to_owned(),
                offset: cursor,
                color: Some(highlighted.foreground),
                italic: false,
                bold: false,
                underline: false,
            });
        }
        spans.push(CodeTokenSpan {
            content: line[range.start..end].to_owned(),
            offset: range.start,
            color: Some(*color),
            italic: false,
            bold: false,
            underline: false,
        });
        cursor = cursor.max(end);
    }
    if cursor < line.len() {
        spans.push(CodeTokenSpan {
            content: line[cursor..].to_owned(),
            offset: cursor,
            color: Some(highlighted.foreground),
            italic: false,
            bold: false,
            underline: false,
        });
    }
    if spans.is_empty() {
        vec![CodeTokenSpan {
            content: line.to_owned(),
            offset: 0,
            color: Some(highlighted.foreground),
            italic: false,
            bold: false,
            underline: false,
        }]
    } else {
        spans
    }
}

pub fn highlight_segments_for_match(
    match_: &ProjectContentMatch,
    path: &str,
    dark: bool,
) -> Vec<HighlightSegment> {
    let ranges = normalize_search_ranges(match_);
    let tokens = code_tokens_for_line(&match_.line_content, path, dark);
    tokens
        .iter()
        .flat_map(|token| split_token_into_segments(&match_.line_content, token, &ranges))
        .collect()
}

fn fallback_highlight_segments(match_: &ProjectContentMatch) -> Vec<HighlightSegment> {
    let ranges = normalize_search_ranges(match_);
    split_token_into_segments(
        &match_.line_content,
        &fallback_token(&match_.line_content),
        &ranges,
    )
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct HighlightCacheKey {
    path: String,
    line: String,
    dark: bool,
    ranges: Vec<(usize, usize)>,
}

struct InFlightGuard {
    key: HighlightCacheKey,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        HIGHLIGHT_CACHE.lock().in_flight.remove(&self.key);
    }
}

fn cache_key(match_: &ProjectContentMatch, path: &str, dark: bool) -> HighlightCacheKey {
    HighlightCacheKey {
        path: path.to_owned(),
        line: match_.line_content.clone(),
        dark,
        ranges: normalize_search_ranges(match_)
            .into_iter()
            .map(|range| (range.start, range.end))
            .collect(),
    }
}

struct HighlightCacheState {
    entries: HashMap<HighlightCacheKey, Arc<Vec<HighlightSegment>>>,
    order: VecDeque<HighlightCacheKey>,
    in_flight: HashSet<HighlightCacheKey>,
}

impl HighlightCacheState {
    const MAX_ENTRIES: usize = 512;
}

static HIGHLIGHT_CACHE: LazyLock<Mutex<HighlightCacheState>> = LazyLock::new(|| {
    Mutex::new(HighlightCacheState {
        entries: HashMap::new(),
        order: VecDeque::new(),
        in_flight: HashSet::new(),
    })
});

/// Warms syntax highlighting off the UI thread for visible rows (upstream Suspense).
pub struct SearchLineHighlightCache;

impl SearchLineHighlightCache {
    pub fn warm(visible: &[ProjectContentMatch], all: &[ProjectContentMatch], dark: bool) {
        let _ = all;
        for match_ in visible {
            Self::schedule(match_, match_.path.as_str(), dark);
        }
    }

    fn schedule(match_: &ProjectContentMatch, path: &str, dark: bool) {
        let key = cache_key(match_, path, dark);
        let mut cache = HIGHLIGHT_CACHE.lock();
        if cache.entries.contains_key(&key) || cache.in_flight.contains(&key) {
            return;
        }
        cache.in_flight.insert(key.clone());
        let match_ = match_.clone();
        let path = path.to_owned();
        drop(cache);
        thread::spawn(move || {
            let _guard = InFlightGuard { key: key.clone() };
            let segments = Arc::new(highlight_segments_for_match(&match_, &path, dark));
            let mut cache = HIGHLIGHT_CACHE.lock();
            if cache.entries.len() >= HighlightCacheState::MAX_ENTRIES
                && let Some(oldest) = cache.order.pop_front()
            {
                cache.entries.remove(&oldest);
            }
            cache.order.push_back(key.clone());
            cache.entries.insert(key, segments);
        });
    }

    fn cached(match_: &ProjectContentMatch, path: &str, dark: bool) -> Arc<Vec<HighlightSegment>> {
        let key = cache_key(match_, path, dark);
        if let Some(segments) = HIGHLIGHT_CACHE.lock().entries.get(&key).cloned() {
            return segments;
        }
        Self::schedule(match_, path, dark);
        Arc::new(fallback_highlight_segments(match_))
    }
}

#[derive(IntoElement)]
pub struct HighlightedSearchLine {
    pub match_: ProjectContentMatch,
    pub path: String,
    pub dark: bool,
}

impl RenderOnce for HighlightedSearchLine {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let primary_match_bg = with_alpha(theme.primary, 0.25);
        let segments = SearchLineHighlightCache::cached(&self.match_, &self.path, self.dark);
        div()
            .flex()
            .flex_wrap()
            .children(segments.iter().cloned().map(move |segment| {
                let mut style = div().child(segment.content);
                if segment.italic {
                    style = style.italic();
                }
                if segment.bold {
                    style = style.font_weight(FontWeight::BOLD);
                }
                if segment.underline {
                    style = style.underline();
                }
                if segment.is_match {
                    style
                        .rounded(rems(0.125))
                        .bg(primary_match_bg)
                        .when_some(segment.color, |this, color| this.text_color(color))
                } else {
                    style.when_some(segment.color, |this, color| this.text_color(color))
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::PositiveInt;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::project::ProjectContentMatchRange;

    fn sample_match(line: &str, ranges: &[(u64, u64)]) -> ProjectContentMatch {
        ProjectContentMatch {
            path: TrimmedNonEmptyString::new("src/main.rs").unwrap(),
            line_number: PositiveInt::new(1).unwrap(),
            line_content: line.to_owned(),
            match_ranges: ranges
                .iter()
                .map(|(start, end)| ProjectContentMatchRange {
                    start: *start,
                    end: *end,
                })
                .collect(),
        }
    }

    #[test]
    fn merges_overlapping_ranges_and_clamps_to_the_line() {
        let normalized =
            normalize_search_ranges(&sample_match("hello", &[(1, 3), (2, 5), (10, 12)]));
        assert_eq!(normalized, vec![SearchHighlightRange { start: 1, end: 5 }]);
    }

    #[test]
    fn maps_utf16_match_ranges_to_byte_slices_without_panicking() {
        let normalized = normalize_search_ranges(&sample_match("café", &[(1, 2)]));
        assert_eq!(normalized.len(), 1);
        let range = &normalized[0];
        assert_eq!(
            &sample_match("café", &[(1, 2)]).line_content[range.start..range.end],
            "a"
        );
    }

    #[test]
    fn cache_keys_differ_when_match_ranges_change() {
        let path = "src/main.rs";
        let line = "hello";
        let left = cache_key(&sample_match(line, &[(1, 2)]), path, false);
        let right = cache_key(&sample_match(line, &[(2, 3)]), path, false);
        assert_ne!(left, right);
    }

    #[test]
    fn uses_fallback_segments_without_syntax_while_cache_warms() {
        let match_ = sample_match("hello", &[(1, 4)]);
        let segments = SearchLineHighlightCache::cached(&match_, "src/main.rs", false);
        assert!(segments.iter().any(|segment| segment.is_match));
        assert!(segments.iter().any(|segment| !segment.is_match));
    }

    #[test]
    fn splits_token_text_around_match_ranges() {
        let token = CodeTokenSpan {
            content: "hello".to_owned(),
            offset: 0,
            color: None,
            italic: false,
            bold: false,
            underline: false,
        };
        let segments = split_token_into_segments(
            "hello",
            &token,
            &[SearchHighlightRange { start: 1, end: 4 }],
        );
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].content, "h");
        assert!(!segments[0].is_match);
        assert_eq!(segments[1].content, "ell");
        assert!(segments[1].is_match);
    }
}
