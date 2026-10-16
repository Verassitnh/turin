//! Port of upstream/apps/web/src/components/chat/AssistantCitationSource.tsx
//!
//! The scroll offset, the pulse keyframes, and the comment-highlight registry
//! follow the web observer. GPUI has no CSS Highlight or DOM Range, so the
//! row paints the pulse opacity and the registry is the ownership model the
//! tests pin down (a disposed editor must not erase a replacement highlight).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::{MixSpace, color, color_mix};

/// `CITATION_PULSE_DURATION_MS`.
pub const CITATION_PULSE_DURATION_MS: f64 = 650.0;
/// `CITATION_HIGHLIGHT_HOLD_MS`.
pub const CITATION_HIGHLIGHT_HOLD_MS: f64 = 1575.0;
/// `CITATION_HIGHLIGHT_FADE_MS`.
pub const CITATION_HIGHLIGHT_FADE_MS: f64 = 450.0;
/// `CITATION_HIGHLIGHT_TOTAL_MS`.
pub const CITATION_HIGHLIGHT_TOTAL_MS: f64 =
    1.5 * CITATION_PULSE_DURATION_MS + CITATION_HIGHLIGHT_HOLD_MS + CITATION_HIGHLIGHT_FADE_MS;
/// `CITATION_HIGHLIGHT_PEAK`.
pub const CITATION_HIGHLIGHT_PEAK: f64 = 0.45;

const COMMENT_HIGHLIGHT_NAME: &str = "t3-assistant-citation-comment";
const NAVIGATION_HIGHLIGHT_NAME: &str = "t3-assistant-citation";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HighlightKeyframe {
    pub offset: f64,
    pub opacity: f64,
}

/// The `element.animate(...)` keyframes, including the reduced-motion hold.
pub fn citation_highlight_keyframes(reduced_motion: bool) -> Vec<HighlightKeyframe> {
    let at = |milliseconds: f64| milliseconds / CITATION_HIGHLIGHT_TOTAL_MS;
    let hold_end = at(CITATION_HIGHLIGHT_TOTAL_MS - CITATION_HIGHLIGHT_FADE_MS);
    if reduced_motion {
        vec![
            HighlightKeyframe {
                offset: 0.0,
                opacity: CITATION_HIGHLIGHT_PEAK,
            },
            HighlightKeyframe {
                offset: hold_end,
                opacity: CITATION_HIGHLIGHT_PEAK,
            },
            HighlightKeyframe {
                offset: 1.0,
                opacity: 0.0,
            },
        ]
    } else {
        vec![
            HighlightKeyframe {
                offset: 0.0,
                opacity: 0.0,
            },
            HighlightKeyframe {
                offset: at(CITATION_PULSE_DURATION_MS * 0.5),
                opacity: CITATION_HIGHLIGHT_PEAK,
            },
            HighlightKeyframe {
                offset: at(CITATION_PULSE_DURATION_MS),
                opacity: 0.0,
            },
            HighlightKeyframe {
                offset: at(CITATION_PULSE_DURATION_MS * 1.5),
                opacity: CITATION_HIGHLIGHT_PEAK,
            },
            HighlightKeyframe {
                offset: hold_end,
                opacity: CITATION_HIGHLIGHT_PEAK,
            },
            HighlightKeyframe {
                offset: 1.0,
                opacity: 0.0,
            },
        ]
    }
}

/// Opacity at `elapsed_ms`, sampled across the keyframes. A pulse past the
/// deadline is gone.
pub fn citation_highlight_opacity(elapsed_ms: f64, reduced_motion: bool) -> f64 {
    if elapsed_ms >= CITATION_HIGHLIGHT_TOTAL_MS {
        return 0.0;
    }
    let at = (elapsed_ms / CITATION_HIGHLIGHT_TOTAL_MS).clamp(0.0, 1.0);
    let frames = citation_highlight_keyframes(reduced_motion);
    let mut previous = frames[0];
    for frame in frames.iter().skip(1) {
        if at <= frame.offset {
            let span = (frame.offset - previous.offset).max(f64::EPSILON);
            let t = (at - previous.offset) / span;
            return previous.opacity + (frame.opacity - previous.opacity) * t;
        }
        previous = *frame;
    }
    0.0
}

/// The list offset that brings the quote into view, clamped to the scroll range.
/// `rect_top` and `scroll_rect_top` are viewport coordinates, as
/// `getBoundingClientRect` reports them.
pub fn citation_scroll_offset(
    state_scroll: f64,
    rect_top: f64,
    scroll_rect_top: f64,
    scroll_height: f64,
    client_height: f64,
) -> f64 {
    let max_scroll = (scroll_height - client_height).max(0.0);
    let inset = 120.0_f64.min(client_height / 3.0);
    let raw = state_scroll + rect_top - scroll_rect_top - inset;
    raw.clamp(0.0, max_scroll)
}

/// Whether that offset is far enough from the current scroll to start a scroll.
pub fn citation_scroll_needed(state_scroll: f64, offset: f64) -> bool {
    (offset - state_scroll).abs() > 1.0
}

/// One CSS Highlight set. Identity is by `Rc`, matching `registry.get(name) === highlight`.
#[derive(Clone)]
pub struct HighlightSet {
    ranges: Rc<RefCell<Vec<u64>>>,
}

impl HighlightSet {
    fn new(ranges: Vec<u64>) -> Self {
        Self {
            ranges: Rc::new(RefCell::new(ranges)),
        }
    }

    pub fn len(&self) -> usize {
        self.ranges.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, range: u64) -> bool {
        self.ranges.borrow().contains(&range)
    }

    fn add(&self, range: u64) {
        let mut ranges = self.ranges.borrow_mut();
        if !ranges.contains(&range) {
            ranges.push(range);
        }
    }

    fn delete(&self, range: u64) {
        self.ranges.borrow_mut().retain(|current| *current != range);
    }

    fn same(&self, other: &HighlightSet) -> bool {
        Rc::ptr_eq(&self.ranges, &other.ranges)
    }
}

#[derive(Default)]
pub struct HighlightRegistry {
    sets: HashMap<String, HighlightSet>,
}

impl HighlightRegistry {
    pub fn get(&self, name: &str) -> Option<HighlightSet> {
        self.sets.get(name).cloned()
    }

    pub fn set(&mut self, name: &str, set: HighlightSet) {
        self.sets.insert(name.to_owned(), set);
    }

    pub fn contains_name(&self, name: &str) -> bool {
        self.sets.contains_key(name)
    }
}

/// One comment editor's hold on the shared comment highlight.
pub struct CommentSourceObserver {
    range: u64,
    highlight: HighlightSet,
    registry: Rc<RefCell<HighlightRegistry>>,
    stopped: bool,
    unavailable_calls: u32,
}

impl CommentSourceObserver {
    /// `observeAssistantCitationCommentSource` at mount, before any mutation.
    /// A disconnected source never registers a highlight.
    pub fn mount(
        registry: Rc<RefCell<HighlightRegistry>>,
        range: u64,
        connected: bool,
        in_viewport: bool,
    ) -> Self {
        let mut observer = Self {
            range,
            highlight: HighlightSet::new(Vec::new()),
            registry,
            stopped: false,
            unavailable_calls: 0,
        };
        if !observer.validate(connected, in_viewport, false, true) {
            return observer;
        }
        let highlight = {
            let mut registry = observer.registry.borrow_mut();
            if let Some(existing) = registry.get(COMMENT_HIGHLIGHT_NAME) {
                existing.add(range);
                existing
            } else {
                let set = HighlightSet::new(vec![range]);
                registry.set(COMMENT_HIGHLIGHT_NAME, set.clone());
                set
            }
        };
        observer.highlight = highlight;
        observer
    }

    pub fn unavailable_calls(&self) -> u32 {
        self.unavailable_calls
    }

    pub fn registered(&self) -> bool {
        !self.highlight.is_empty() || self.highlight.contains(self.range)
    }

    /// A mutation of the source. `repair` is `resolveAssistantCitationRange`:
    /// `None` means the quote can no longer be found.
    pub fn on_source_changed(
        &mut self,
        connected: bool,
        in_viewport: bool,
        range_broken: bool,
        repair: Option<u64>,
    ) {
        if self.stopped {
            return;
        }
        if !self.validate(connected, in_viewport, range_broken, repair.is_some()) {
            return;
        }
        if range_broken && let Some(repaired) = repair {
            self.highlight.delete(self.range);
            self.range = repaired;
            self.highlight.add(repaired);
        }
    }

    pub fn dispose(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.highlight.delete(self.range);
        if self.highlight.is_empty()
            && self
                .registry
                .borrow()
                .get(COMMENT_HIGHLIGHT_NAME)
                .is_some_and(|current| current.same(&self.highlight))
        {
            self.registry
                .borrow_mut()
                .sets
                .remove(COMMENT_HIGHLIGHT_NAME);
        }
    }

    fn validate(
        &mut self,
        connected: bool,
        in_viewport: bool,
        _range_broken: bool,
        repairable: bool,
    ) -> bool {
        if self.stopped {
            return false;
        }
        if !connected || !in_viewport {
            self.unavailable();
            return false;
        }
        if !repairable {
            self.unavailable();
            return false;
        }
        true
    }

    fn unavailable(&mut self) {
        if self.stopped {
            return;
        }
        self.dispose();
        self.unavailable_calls += 1;
    }
}

/// Navigation highlight ownership: clearing only removes the set this observer installed.
pub fn clear_navigation_highlight(registry: &mut HighlightRegistry, owned: &HighlightSet) {
    if registry
        .get(NAVIGATION_HIGHLIGHT_NAME)
        .is_some_and(|current| current.same(owned))
    {
        registry.sets.remove(NAVIGATION_HIGHLIGHT_NAME);
    }
}

/// The source row. `opacity` is the pulse strength, 0 when idle.
#[derive(IntoElement)]
pub struct AssistantCitationSource {
    pub message_id: SharedString,
    pub environment_id: Option<SharedString>,
    pub thread_id: Option<SharedString>,
    pub opacity: f32,
    pub child: AnyElement,
}

impl RenderOnce for AssistantCitationSource {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let accent = color("oklch(0.62 0.16 259)");
        let wash: gpui_kit::Hsla = color_mix(
            MixSpace::Oklab,
            accent,
            self.opacity.clamp(0.0, 1.0),
            theme.background,
        )
        .into();
        div()
            .id(SharedString::from(format!(
                "assistant-citation-source-{}",
                self.message_id
            )))
            .w_full()
            .min_w_0()
            .rounded(gpui_kit::px(6.))
            .when(self.opacity > 0.0, |this| this.bg(wash))
            .child(self.child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn pulse_keyframes_match_the_web_animation() {
        let full = citation_highlight_keyframes(false);
        let at = |milliseconds: f64| milliseconds / CITATION_HIGHLIGHT_TOTAL_MS;
        assert_eq!(CITATION_HIGHLIGHT_TOTAL_MS, 3000.0);
        approx(full[0].offset, 0.0);
        approx(full[0].opacity, 0.0);
        approx(full[1].offset, at(325.0));
        approx(full[1].opacity, 0.45);
        approx(full[2].offset, at(650.0));
        approx(full[2].opacity, 0.0);
        approx(full[3].offset, at(975.0));
        approx(full[3].opacity, 0.45);
        approx(full[4].opacity, 0.45);
        approx(full[5].offset, 1.0);
        approx(full[5].opacity, 0.0);

        let reduced = citation_highlight_keyframes(true);
        approx(reduced[0].opacity, 0.45);
        approx(reduced[1].opacity, 0.45);
        approx(reduced[2].opacity, 0.0);
        assert!(citation_highlight_opacity(3100.0, false) == 0.0);
    }

    #[test]
    fn scroll_offset_matches_the_measured_fixture() {
        // scrollTop 8586, scrollHeight 9334, clientHeight 748, sourceTop 200.
        let offset = citation_scroll_offset(8586.0, 200.0 - 8586.0, 0.0, 9334.0, 748.0);
        approx(offset, 80.0);
        assert!(citation_scroll_needed(8586.0, offset));
        // After the first scroll settles at 4200 and the source moves to 260.
        let next = citation_scroll_offset(4200.0, 260.0 - 4200.0, 0.0, 9334.0, 748.0);
        approx(next, 140.0);
        // Clamped to the beginning when the quote is already near the top.
        let clamped = citation_scroll_offset(8586.0, 40.0 - 8586.0, 0.0, 9334.0, 748.0);
        approx(clamped, 0.0);
    }

    #[test]
    fn a_zero_height_range_does_not_count_as_in_view() {
        // rect.height <= 0 skips scrolling. The offset helper is not asked.
        assert!(!citation_scroll_needed(80.0, 80.0));
    }

    fn registry() -> Rc<RefCell<HighlightRegistry>> {
        Rc::new(RefCell::new(HighlightRegistry::default()))
    }

    #[test]
    fn comment_highlight_survives_until_cleanup_and_spares_the_navigation_set() {
        let registry = registry();
        let navigation = HighlightSet::new(vec![9]);
        registry
            .borrow_mut()
            .set(NAVIGATION_HIGHLIGHT_NAME, navigation.clone());
        let mut comment = CommentSourceObserver::mount(registry.clone(), 1, true, true);
        assert_eq!(
            registry.borrow().get(COMMENT_HIGHLIGHT_NAME).unwrap().len(),
            1
        );
        comment.dispose();
        comment.dispose();
        assert!(registry.borrow().get(COMMENT_HIGHLIGHT_NAME).is_none());
        assert!(
            registry
                .borrow()
                .get(NAVIGATION_HIGHLIGHT_NAME)
                .unwrap()
                .same(&navigation)
        );
        assert_eq!(comment.unavailable_calls(), 0);
    }

    #[test]
    fn repairs_a_broken_range_in_place() {
        let registry = registry();
        let mut comment = CommentSourceObserver::mount(registry.clone(), 1, true, true);
        comment.on_source_changed(true, true, true, Some(7));
        let set = registry.borrow().get(COMMENT_HIGHLIGHT_NAME).unwrap();
        assert!(set.contains(7));
        assert!(!set.contains(1));
        assert_eq!(comment.unavailable_calls(), 0);
    }

    #[test]
    fn closes_once_when_the_quote_cannot_be_repaired() {
        let registry = registry();
        let mut comment = CommentSourceObserver::mount(registry.clone(), 1, true, true);
        comment.on_source_changed(true, true, true, None);
        comment.on_source_changed(true, true, true, None);
        comment.dispose();
        assert_eq!(comment.unavailable_calls(), 1);
        assert!(registry.borrow().get(COMMENT_HIGHLIGHT_NAME).is_none());
    }

    #[test]
    fn closes_once_when_the_source_disconnects_or_leaves_the_viewport() {
        for (connected, in_viewport) in [(false, true), (true, false)] {
            let registry = registry();
            let mut comment = CommentSourceObserver::mount(registry.clone(), 1, true, true);
            comment.on_source_changed(connected, in_viewport, false, Some(1));
            comment.on_source_changed(connected, in_viewport, false, Some(1));
            comment.dispose();
            assert_eq!(comment.unavailable_calls(), 1);
            assert!(registry.borrow().get(COMMENT_HIGHLIGHT_NAME).is_none());
        }
    }

    #[test]
    fn rejects_an_already_detached_source_without_registering_a_highlight() {
        let registry = registry();
        let mut comment = CommentSourceObserver::mount(registry.clone(), 1, false, true);
        assert_eq!(comment.unavailable_calls(), 1);
        assert!(registry.borrow().get(COMMENT_HIGHLIGHT_NAME).is_none());
        comment.on_source_changed(false, true, true, None);
        comment.dispose();
        assert_eq!(comment.unavailable_calls(), 1);
    }

    #[test]
    fn distinct_editors_share_one_highlight_and_leave_each_other_alone() {
        let registry = registry();
        let mut first = CommentSourceObserver::mount(registry.clone(), 1, true, true);
        let mut second = CommentSourceObserver::mount(registry.clone(), 2, true, true);
        assert_eq!(
            registry.borrow().get(COMMENT_HIGHLIGHT_NAME).unwrap().len(),
            2
        );
        first.dispose();
        first.dispose();
        let set = registry.borrow().get(COMMENT_HIGHLIGHT_NAME).unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains(2));
        assert_eq!(second.unavailable_calls(), 0);
        second.dispose();
        assert!(registry.borrow().get(COMMENT_HIGHLIGHT_NAME).is_none());
    }

    #[test]
    fn an_older_editor_does_not_delete_a_replacement_highlight() {
        let registry = registry();
        let mut comment = CommentSourceObserver::mount(registry.clone(), 1, true, true);
        let original = registry.borrow().get(COMMENT_HIGHLIGHT_NAME).unwrap();
        let replacement = HighlightSet::new(vec![9]);
        registry
            .borrow_mut()
            .set(COMMENT_HIGHLIGHT_NAME, replacement.clone());
        comment.dispose();
        assert!(original.is_empty());
        assert!(
            registry
                .borrow()
                .get(COMMENT_HIGHLIGHT_NAME)
                .unwrap()
                .same(&replacement)
        );
        assert_eq!(replacement.len(), 1);
    }

    #[test]
    fn clearing_navigation_leaves_a_replacement_highlight() {
        let mut registry = HighlightRegistry::default();
        let owned = HighlightSet::new(vec![1]);
        registry.set(NAVIGATION_HIGHLIGHT_NAME, owned.clone());
        let replacement = HighlightSet::new(vec![2]);
        registry.set(NAVIGATION_HIGHLIGHT_NAME, replacement.clone());
        clear_navigation_highlight(&mut registry, &owned);
        assert!(
            registry
                .get(NAVIGATION_HIGHLIGHT_NAME)
                .unwrap()
                .same(&replacement)
        );
    }
}
