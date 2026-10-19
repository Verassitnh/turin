//! Port of upstream/apps/web/src/components/chat/ThreadFindBar.tsx
//!
//! The find bar over the top right of the chat canvas: the query field, the
//! `n/total` count, retry on a failed search, previous and next match, and close.
//! `ThreadFindBar` here is the stateless view; `ThreadFind` (the provider) owns the
//! input and the search. The label rules are plain functions so they are tested
//! without a window.

use std::rc::Rc;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    App, ClickEvent, Entity, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, px, rems,
};

use super::thread_details_card_layout::THREAD_DETAILS_CARD_GAP;
use super::thread_find::format_thread_find_count;
use super::thread_find_state::ThreadFindStatus;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS, THREAD_DETAILS_PANEL_WIDTH};
use crate::ui::input_group::{
    InputGroup, InputGroupAddon, InputGroupAlign, InputGroupSize, InputGroupVariant,
};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

/// Height of the bar: `h-9`.
const THREAD_FIND_BAR_HEIGHT: f64 = 36.0;

/// Room the open bar takes at the top of the chat canvas: its `h-9` height plus the
/// shared 12px gap below it, so the details card starts on the same rhythm.
pub const THREAD_FIND_BAR_RESERVED_HEIGHT: f64 = THREAD_FIND_BAR_HEIGHT + THREAD_DETAILS_CARD_GAP;

/// Upstream's input `maxLength`.
pub const THREAD_FIND_QUERY_MAX_LENGTH: usize = 200;

/// What the count slot shows: `n/total`, `1/…` while the server still counts, or the
/// loading and error text.
pub fn thread_find_bar_label(
    query: &str,
    counting: bool,
    status: Option<ThreadFindStatus>,
    active_index: u64,
    match_count: u64,
) -> String {
    match status {
        Some(ThreadFindStatus::Loading) => return "Searching…".to_owned(),
        Some(ThreadFindStatus::Error) => return "Search failed".to_owned(),
        None => {}
    }
    if turin_contracts::base_schemas::js_trim(query).is_empty() {
        return String::new();
    }
    if counting {
        "1/…".to_owned()
    } else {
        format_thread_find_count(active_index, match_count)
    }
}

/// A query with no hits in a settled search shows its count as an error.
pub fn thread_find_bar_no_results(
    query: &str,
    status: Option<ThreadFindStatus>,
    match_count: u64,
) -> bool {
    !turin_contracts::base_schemas::js_trim(query).is_empty()
        && status.is_none()
        && match_count == 0
}

/// Previous and next are off with nothing to step to, while searching, or while counting.
pub fn thread_find_bar_navigation_disabled(
    match_count: u64,
    status: Option<ThreadFindStatus>,
    counting: bool,
) -> bool {
    match_count == 0 || status == Some(ThreadFindStatus::Loading) || counting
}

type Action = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ThreadFindBar {
    pub input: Entity<InputState>,
    pub query: String,
    pub match_count: u64,
    pub counting: bool,
    pub status: Option<ThreadFindStatus>,
    pub active_index: u64,
    pub on_retry: Action,
    pub on_next: Action,
    pub on_previous: Action,
    pub on_close: Action,
}

fn click(action: &Action) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let action = action.clone();
    move |_, window, cx| action(window, cx)
}

impl RenderOnce for ThreadFindBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let label = thread_find_bar_label(
            &self.query,
            self.counting,
            self.status,
            self.active_index,
            self.match_count,
        );
        let no_results = thread_find_bar_no_results(&self.query, self.status, self.match_count);
        let navigation_disabled =
            thread_find_bar_navigation_disabled(self.match_count, self.status, self.counting);
        let retry = (self.status == Some(ThreadFindStatus::Error)).then(|| {
            Button::new("thread-find-retry")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Xs)
                .label("Retry")
                .on_click(click(&self.on_retry))
        });
        // `absolute top-3 right-3 z-40 w-[min(var(--thread-details-panel-width),calc(100%-1.5rem))]`
        // An inset track the bar right-aligns in: `min(panel width, 100% - 1.5rem)` wide.
        div()
            .id("thread-find-bar")
            .absolute()
            .top(px(THREAD_DETAILS_CARD_GAP as f32))
            .left(px(THREAD_DETAILS_CARD_GAP as f32))
            .right(px(THREAD_DETAILS_CARD_GAP as f32))
            .flex()
            .justify_end()
            .child(
                div().w(THREAD_DETAILS_PANEL_WIDTH).max_w_full().child(
                    InputGroup::new()
                        .variant(InputGroupVariant::Popover)
                        .size(InputGroupSize::Lg)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pl(px(11.))
                                .text_size(TEXT_SM.size)
                                .child(
                                    Input::new(&self.input)
                                        .appearance(false)
                                        .bordered(false)
                                        .text_size(TEXT_SM.size),
                                ),
                        )
                        .child(
                            InputGroupAddon::new(InputGroupAlign::InlineEnd)
                                .child(
                                    div()
                                        .min_w(rems(2.5))
                                        .text_center()
                                        .text_size(TEXT_XS.size)
                                        .text_color(if no_results {
                                            theme.destructive
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(SharedString::from(label)),
                                )
                                .children(retry)
                                .child(
                                    Button::new("thread-find-previous")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::IconXs)
                                        .icon(IconName::ChevronUp)
                                        .disabled(navigation_disabled)
                                        .tooltip(Tooltip::new("Previous match"))
                                        .on_click(click(&self.on_previous)),
                                )
                                .child(
                                    Button::new("thread-find-next")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::IconXs)
                                        .icon(IconName::ChevronDown)
                                        .disabled(navigation_disabled)
                                        .tooltip(Tooltip::new("Next match"))
                                        .on_click(click(&self.on_next)),
                                )
                                .child(
                                    Button::new("thread-find-close")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::IconXs)
                                        .icon(IconName::X)
                                        .tooltip(Tooltip::new("Close find"))
                                        .on_click(click(&self.on_close)),
                                ),
                        ),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_height_is_the_bar_plus_the_shared_gap() {
        assert_eq!(THREAD_FIND_BAR_RESERVED_HEIGHT, 48.0);
    }

    #[test]
    fn a_blank_query_shows_no_count() {
        assert_eq!(thread_find_bar_label("  ", false, None, 0, 0), "");
    }

    #[test]
    fn shows_the_one_based_position_over_the_total() {
        assert_eq!(thread_find_bar_label("foo", false, None, 1, 4), "2/4");
        assert_eq!(thread_find_bar_label("foo", false, None, 0, 0), "0/0");
    }

    #[test]
    fn counting_shows_an_open_total() {
        assert_eq!(thread_find_bar_label("foo", true, None, 0, 3), "1/…");
    }

    #[test]
    fn loading_and_failed_searches_replace_the_count() {
        assert_eq!(
            thread_find_bar_label("foo", false, Some(ThreadFindStatus::Loading), 0, 0),
            "Searching…"
        );
        assert_eq!(
            thread_find_bar_label("foo", true, Some(ThreadFindStatus::Error), 0, 5),
            "Search failed"
        );
        // Upstream sets the status text after the query test, so it shows even for a blank query.
        assert_eq!(
            thread_find_bar_label("", false, Some(ThreadFindStatus::Loading), 0, 0),
            "Searching…"
        );
    }

    #[test]
    fn only_a_settled_empty_search_reads_as_no_results() {
        assert!(thread_find_bar_no_results("foo", None, 0));
        assert!(!thread_find_bar_no_results("foo", None, 2));
        assert!(!thread_find_bar_no_results(
            "foo",
            Some(ThreadFindStatus::Loading),
            0
        ));
        assert!(!thread_find_bar_no_results(
            "foo",
            Some(ThreadFindStatus::Error),
            0
        ));
        assert!(!thread_find_bar_no_results("   ", None, 0));
    }

    #[test]
    fn stepping_is_off_without_a_settled_result() {
        assert!(thread_find_bar_navigation_disabled(0, None, false));
        assert!(thread_find_bar_navigation_disabled(
            3,
            Some(ThreadFindStatus::Loading),
            false
        ));
        assert!(thread_find_bar_navigation_disabled(3, None, true));
        assert!(!thread_find_bar_navigation_disabled(3, None, false));
        // An error with an old count keeps stepping, as upstream's rule only names loading.
        assert!(!thread_find_bar_navigation_disabled(
            3,
            Some(ThreadFindStatus::Error),
            false
        ));
    }
}
