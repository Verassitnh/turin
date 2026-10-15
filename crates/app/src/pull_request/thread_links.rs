//! Port of upstream/apps/web/src/components/pullRequest/PullRequestThreadLinks.tsx: how a pull
//! request relates to threads — the count of threads it is linked from (`display="count"`), the
//! "Link to this thread" / "Unlink from this thread" menu item (`display="menu-item"`) and the
//! thread picker dialog (`display="picker"`).
//!
//! Not ported: the capability gate (`threadPullRequestLinkMode`), the command palette hand-off of
//! the count (the count reports [`ThreadPickerEvent`]-style events to whoever hosts it) and the
//! error toast on a failed link (the picker stays open instead).

use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClickEvent, Context, CursorStyle, Entity, EventEmitter, FontWeight,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rems,
};
use turin_contracts::ThreadId;
use turin_contracts::pull_request::PullRequestRef;

use super::presentation_view::with_tooltip;
use super::view_model::{LinkedThread, PullRequestStore, ThreadChoice};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::dialog::dialog_overlay;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, scroll_area};

/// `linkedThreadsLabel`.
pub fn linked_threads_label(count: usize) -> String {
    if count > 0 {
        format!(
            "Linked from {count} {}",
            if count == 1 { "thread" } else { "threads" }
        )
    } else {
        "Linked threads".to_owned()
    }
}

/// `display="count"`: a ghost `xs` button with the message glyph and the number of linked threads.
/// Shown when there are linked threads, or when the read failed (`?`).
pub fn linked_threads_count(
    threads: &[LinkedThread],
    failed: bool,
    on_open: Rc<dyn Fn(&mut Window, &mut App)>,
) -> Option<impl IntoElement> {
    if threads.is_empty() && !failed {
        return None;
    }
    let label = linked_threads_label(threads.len());
    let count = if threads.is_empty() {
        "?".to_owned()
    } else {
        threads.len().to_string()
    };
    Some(with_tooltip(
        "pr-linked-threads-count",
        format!("{label}. Search in the command palette."),
        Button::new("pr-linked-threads")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Xs)
            .child(Icon::new(IconName::MessageSquare).size(rems(0.875)))
            .child(count)
            .on_click(move |_, window, cx| on_open(window, cx)),
    ))
}

/// `display="menu-item"`: the link / unlink row of the detail's actions menu.
pub fn link_menu_item(
    linked_here: bool,
    has_current_thread: bool,
    pending: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    PopupMenuItem::new(if linked_here {
        "Unlink from this thread"
    } else if has_current_thread {
        "Link to this thread"
    } else {
        "Link to thread"
    })
    .icon(gpui_kit::component::Icon::new(if linked_here {
        IconName::Unlink2
    } else {
        IconName::Link2
    }))
    .disabled(pending)
    .on_click(on_click)
}

/// Threads matching `query` in title or project name, newest first (`candidates` in
/// `ThreadPicker`).
pub fn thread_picker_candidates<'a>(threads: &'a [ThreadChoice], query: &str) -> Vec<&'a ThreadChoice> {
    let search = turin_contracts::js_trim(query).to_lowercase();
    let mut candidates: Vec<&ThreadChoice> = threads
        .iter()
        .filter(|thread| {
            format!("{} {}", thread.title, thread.project_title)
                .to_lowercase()
                .contains(&search)
        })
        .collect();
    candidates.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    candidates
}

#[derive(Clone, Debug)]
pub enum ThreadPickerEvent {
    /// The reader chose a thread to link the pull request to.
    Select(ThreadId),
    Dismiss,
}

/// `display="picker"`: a dialog with a search field and the active threads, linked ones checked.
pub struct ThreadPickerDialog {
    store: Entity<PullRequestStore>,
    reference: PullRequestRef,
    linked: Vec<ThreadId>,
    input: Entity<InputState>,
    query: String,
    pending: bool,
    _subscription: Subscription,
}

impl EventEmitter<ThreadPickerEvent> for ThreadPickerDialog {}

impl ThreadPickerDialog {
    pub fn new(
        store: Entity<PullRequestStore>,
        reference: PullRequestRef,
        linked: Vec<ThreadId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads or projects..."));
        let subscription = cx.subscribe_in(&input, window, |this, state, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = state.read(cx).value().to_string();
                cx.notify();
            }
        });
        let focus = input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        Self {
            store,
            reference,
            linked,
            input,
            query: String::new(),
            pending: false,
            _subscription: subscription,
        }
    }

    pub fn reference(&self) -> &PullRequestRef {
        &self.reference
    }
}

impl Render for ThreadPickerDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let page = cx.entity().downgrade();
        let threads = self.store.read(cx).threads().to_vec();
        let candidates = thread_picker_candidates(&threads, &self.query);
        let rows: Vec<_> = candidates
            .iter()
            .map(|thread| {
                let linked = self.linked.contains(&thread.id);
                let id = thread.id.clone();
                let page = page.clone();
                let title: SharedString = if thread.title.is_empty() {
                    "Untitled thread".into()
                } else {
                    thread.title.clone().into()
                };
                let disabled = self.pending || linked;
                div()
                    .id(SharedString::from(format!("pr-thread-pick-{}", thread.id.as_str())))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .px_2()
                    .py(rems(0.375))
                    .text_size(TEXT_SM.size)
                    .when(!disabled, |this| {
                        this.cursor(CursorStyle::PointingHand)
                            .hover(|style| style.bg(theme.accent))
                            .on_click(move |_, _, cx| {
                                page.update(cx, |_, cx| cx.emit(ThreadPickerEvent::Select(id.clone())))
                                    .ok();
                            })
                    })
                    .when(disabled, |this| this.opacity(0.64))
                    .child(Icon::new(IconName::MessageSquare).size(rems(1.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(title))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(thread.project_title.clone())),
                            ),
                    )
                    .when(linked, |this| {
                        this.child(Icon::new(IconName::Check).size(rems(0.875)))
                            .child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child("Linked"),
                            )
                    })
            })
            .collect();
        let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = {
            let page = page.clone();
            Rc::new(move |_, cx| {
                page.update(cx, |_, cx| cx.emit(ThreadPickerEvent::Dismiss)).ok();
            })
        };
        dialog_overlay(
            "pr-thread-picker",
            px(448.),
            dismiss,
            div()
                .flex()
                .flex_col()
                .child(
                    // `CommandInput`: the search field with a divider under it.
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .h(rems(2.75))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            Icon::new(IconName::Search)
                                .size(rems(1.))
                                .color(with_alpha(theme.foreground, 0.8).into()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.input).appearance(false).bordered(false)),
                        ),
                )
                .child(
                    div().max_h(rems(20.)).child(
                        scroll_area("pr-thread-picker-list").child(
                            div().flex().flex_col().p(rems(0.375)).map(|list| {
                                if rows.is_empty() {
                                    list.child(
                                        div()
                                            .px_3()
                                            .py(rems(1.5))
                                            .text_center()
                                            .text_size(TEXT_SM.size)
                                            .font_weight(FontWeight::NORMAL)
                                            .text_color(theme.muted_foreground)
                                            .child("No active threads found."),
                                    )
                                } else {
                                    list.children(rows)
                                }
                            }),
                        ),
                    ),
                ),
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, title: &str, project: &str, updated_at: &str) -> ThreadChoice {
        ThreadChoice {
            id: ThreadId::new(id).unwrap(),
            title: title.to_owned(),
            project_title: project.to_owned(),
            updated_at: updated_at.to_owned(),
        }
    }

    #[test]
    fn labels_count_threads() {
        assert_eq!(linked_threads_label(0), "Linked threads");
        assert_eq!(linked_threads_label(1), "Linked from 1 thread");
        assert_eq!(linked_threads_label(3), "Linked from 3 threads");
    }

    #[test]
    fn picker_matches_title_or_project_newest_first() {
        let threads = vec![
            thread("a", "Fix composer", "turin", "2026-01-01T00:00:00.000Z"),
            thread("b", "Review sidebar", "t3code", "2026-01-03T00:00:00.000Z"),
            thread("c", "", "turin", "2026-01-02T00:00:00.000Z"),
        ];
        let ids = |query: &str| {
            thread_picker_candidates(&threads, query)
                .iter()
                .map(|thread| thread.id.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(""), ["b", "c", "a"]);
        assert_eq!(ids("  TURIN "), ["c", "a"]);
        assert_eq!(ids("sidebar"), ["b"]);
        assert_eq!(ids("zzz"), Vec::<String>::new());
    }
}
