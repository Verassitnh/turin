//! Port of upstream/apps/web/src/components/ui/middle-truncate.tsx
//!
//! Truncates in the middle, the way Finder does, for strings that carry
//! meaning at both ends: branch names, paths, worktree names, shas. CSS has no
//! middle ellipsis, so the string is split into a head that truncates and a
//! tail that does not. Selection/copy give the whole string upstream; GPUI
//! text is not selectable, so the full value is only the tooltip.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

const DEFAULT_TAIL: usize = 10;
const MAX_SEGMENT_TAIL: usize = 16;

/// Where to cut. A path keeps its last segment when that is short enough to be
/// the useful part; anything else keeps a fixed count. Nothing is split when
/// the tail would be most of the string, since then the head could never show
/// enough to be worth an ellipsis.
pub fn split_for_middle_truncate(value: &str, tail: Option<usize>) -> Option<(String, String)> {
    // Code points, not UTF-16 units: a cut inside a surrogate pair would render
    // two broken glyphs where an emoji or a CJK extension character used to be.
    let chars: Vec<char> = value.chars().collect();
    let mut keep = tail.unwrap_or(DEFAULT_TAIL);
    if tail.is_none()
        && let Some(slash) = chars.iter().rposition(|c| *c == '/')
        && slash > 0
        && slash < chars.len() - 1
    {
        let segment = chars.len() - slash - 1;
        keep = if segment <= MAX_SEGMENT_TAIL {
            segment
        } else {
            DEFAULT_TAIL
        };
    }
    if keep == 0 || chars.len() <= keep + 4 {
        return None;
    }
    let cut = chars.len() - keep;
    Some((chars[..cut].iter().collect(), chars[cut..].iter().collect()))
}

#[derive(IntoElement)]
pub struct MiddleTruncate {
    value: SharedString,
    tail: Option<usize>,
}

impl MiddleTruncate {
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            tail: None,
        }
    }

    /// Characters kept at the end. Defaults to the last path segment, capped, or 10.
    pub fn tail(mut self, tail: usize) -> Self {
        self.tail = Some(tail);
        self
    }
}

impl RenderOnce for MiddleTruncate {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // `inline-flex min-w-0 max-w-full overflow-hidden whitespace-nowrap`.
        let container = div()
            .flex()
            .min_w_0()
            .max_w_full()
            .overflow_hidden()
            .whitespace_nowrap();
        match split_for_middle_truncate(&self.value, self.tail) {
            Some((head, tail)) => container
                .child(div().min_w_0().truncate().child(head))
                .child(div().flex_shrink_0().child(tail)),
            None => container.child(div().min_w_0().truncate().child(self.value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_short_last_path_segment() {
        assert_eq!(
            split_for_middle_truncate("feature/some-long-prefix/fix-bug", None),
            Some(("feature/some-long-prefix/".into(), "fix-bug".into()))
        );
    }

    #[test]
    fn falls_back_to_ten_characters_for_a_long_segment_or_no_slash() {
        let value = "fix-cache-main-20260918-180825";
        assert_eq!(
            split_for_middle_truncate(value, None),
            Some(("fix-cache-main-20260".into(), "918-180825".into()))
        );
    }

    #[test]
    fn does_not_split_when_the_tail_would_be_most_of_the_string() {
        assert_eq!(split_for_middle_truncate("main", None), None);
        assert_eq!(split_for_middle_truncate("fourteen-chars", Some(10)), None);
    }

    #[test]
    fn cuts_on_code_points() {
        let value = "ab😀😀😀😀cdefghij";
        let (head, tail) = split_for_middle_truncate(value, Some(3)).unwrap();
        assert_eq!(format!("{head}{tail}"), value);
        assert_eq!(tail, "hij");
    }
}
