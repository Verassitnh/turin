//! Thread search from upstream Sidebar.tsx: the query state, keyboard
//! handling (`handleThreadSearchKeyDown`), server message matches
//! (`useThreadSearch`, debounced with a two-character floor) and the
//! `SidebarSearchResultRow` listbox.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};

use super::Sidebar;
use super::sidebar_logic::{
    pull_request_matches_search_query, search_sidebar_threads, thread_search_match_key,
};
use super::sidebar_thread_row::thread_time_label;
use super::thread_hover_card::SidebarThreadTooltip;
use super::thread_status_indicators::link_search_terms;
use crate::components::project_favicon::ProjectFavicon;
use crate::components::thread_search_match::ThreadSearchMatchExcerpt;
use crate::state::models::EnvironmentThreadShell;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};

/// `useThreadSearch`'s debounce.
const THREAD_SEARCH_DEBOUNCE_MS: u64 = 150;

impl Sidebar {
    pub(crate) fn is_searching(&self) -> bool {
        !self.search_query.trim().is_empty()
    }

    pub(crate) fn set_search_query(&mut self, value: String, cx: &mut Context<Self>) {
        if value == self.search_query {
            return;
        }
        self.search_query = value.clone();
        self.active_search_index = 0;
        let query = value.trim().to_string();
        if query.chars().count() < 2 {
            self.search_matches.clear();
            self.search_pending = false;
            self.search_task = None;
            cx.notify();
            return;
        }
        self.search_pending = true;
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(THREAD_SEARCH_DEBOUNCE_MS))
                .await;
            let Ok(task) = this.update(cx, |this, cx| this.backend.search_messages(query, cx))
            else {
                return;
            };
            let matches = task.await;
            this.update(cx, |this, cx| {
                this.search_matches = matches;
                this.search_pending = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Results in lifecycle order, with whether each is settled.
    pub(crate) fn search_results(
        &mut self,
        cx: &gpui_kit::App,
    ) -> Vec<(EnvironmentThreadShell, bool)> {
        let (partition, _) = self.compute_sections(cx);
        let settled: HashSet<String> = partition
            .settled
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let searchable: Vec<&EnvironmentThreadShell> = partition
            .pinned
            .iter()
            .chain(&partition.active)
            .chain(&partition.working)
            .chain(&partition.snoozed)
            .chain(&partition.settled)
            .collect();
        let content_keys: HashSet<String> = self
            .search_matches
            .iter()
            .map(|hit| thread_search_match_key(hit.environment_id.as_str(), hit.thread_id.as_str()))
            .collect();
        search_sidebar_threads(&searchable, &self.search_query, &content_keys)
            .into_iter()
            .map(|thread| {
                let is_settled = settled.contains(&thread.key());
                (thread.clone(), is_settled)
            })
            .collect()
    }

    pub(crate) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.set_search_query(String::new(), cx);
    }

    pub(crate) fn select_active_search_result(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let results = self.search_results(cx);
        if let Some((thread, _)) = results.get(self.active_search_index) {
            let key = thread.key();
            self.clear_search(window, cx);
            self.navigate_to_thread(&key, cx);
        }
    }

    /// Arrow keys move the highlight, Escape clears, Enter selects (the
    /// input reports Enter itself).
    pub(crate) fn search_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        if key == "escape" && self.is_searching() {
            cx.stop_propagation();
            self.clear_search(window, cx);
            return;
        }
        let count = self.search_results(cx).len();
        if count == 0 {
            return;
        }
        match key {
            "down" => {
                cx.stop_propagation();
                self.active_search_index = (self.active_search_index + 1) % count;
                cx.notify();
            }
            "up" => {
                cx.stop_propagation();
                self.active_search_index = (self.active_search_index + count - 1) % count;
                cx.notify();
            }
            _ => {}
        }
    }

    pub(crate) fn render_search_results(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let results = self.search_results(cx);
        if results.is_empty() {
            return div()
                .px_2()
                .py_6()
                .flex()
                .justify_center()
                .text_size(TEXT_XS.size)
                .text_color(scope.sidebar_muted_foreground)
                .child(if self.search_pending {
                    "Searching thread messages…"
                } else {
                    "No threads found"
                })
                .into_any_element();
        }
        let snapshot = self.snapshot(cx).clone();
        let route = self.route_thread_key().map(str::to_string);
        let query = self.search_query.clone();
        let active_index = self.active_search_index.min(results.len() - 1);
        let rows: Vec<AnyElement> = results
            .into_iter()
            .enumerate()
            .map(|(index, (thread, is_settled))| {
                let key = thread.key();
                let highlighted = index == active_index;
                let is_route = route.as_deref() == Some(key.as_str());
                let lit = highlighted || is_route;
                let project = snapshot.project(&thread.project_key()).cloned();
                let hit = snapshot_match(self, &thread);
                let pr_number = thread
                    .pull_requests
                    .iter()
                    .filter(|link| !link.dismissed)
                    .find(|link| {
                        pull_request_matches_search_query(
                            &link_search_terms(std::slice::from_ref(*link)),
                            &query,
                        )
                    })
                    .map(|link| link.number);
                let card = SidebarThreadTooltip::from_thread(&thread, &snapshot);
                let title_color = if lit {
                    scope.foreground
                } else if is_settled {
                    with_alpha(theme.secondary_label, 0.7).into()
                } else {
                    scope.foreground
                };
                let open_key = key.clone();
                div()
                    .id(SharedString::from(format!(
                        "sidebar-thread-search-result-{index}"
                    )))
                    .flex()
                    .min_h_9()
                    .w_full()
                    .items_center()
                    .gap(rems(0.625))
                    .rounded(RADIUS_MD)
                    .px(rems(0.625))
                    .py_1()
                    .cursor(CursorStyle::PointingHand)
                    .text_size(TEXT_SM.size)
                    .map(|this| {
                        if lit {
                            this.bg(scope.sidebar_row_active)
                                .text_color(scope.sidebar_foreground)
                        } else {
                            this.text_color(with_alpha(scope.sidebar_muted_foreground, 0.75))
                                .hover(move |style| {
                                    style
                                        .bg(scope.sidebar_row_hover)
                                        .text_color(scope.sidebar_foreground)
                                })
                        }
                    })
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.active_search_index != index {
                            this.active_search_index = index;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.clear_search(window, cx);
                        this.navigate_to_thread(&open_key, cx);
                    }))
                    .tooltip(move |window, cx| card.clone().build(window, cx))
                    .children(project.as_ref().map(|project| {
                        let dim = is_settled && !lit;
                        div()
                            .flex_shrink_0()
                            .when(dim, |this| this.opacity(0.4))
                            .child(ProjectFavicon::new(project).grayscale(dim))
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap(rems(0.625))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .font_weight(if is_settled && !lit {
                                                FontWeight::NORMAL
                                            } else {
                                                FontWeight::MEDIUM
                                            })
                                            .text_color(title_color)
                                            .when(thread.title_regeneration.is_some(), |this| {
                                                this.opacity(0.55)
                                            })
                                            .child(thread.title.clone()),
                                    )
                                    .children(pr_number.map(|number| {
                                        div()
                                            .flex_shrink_0()
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.muted_foreground)
                                            .child(format!("#{number}"))
                                    }))
                                    .child(
                                        div()
                                            .w_8()
                                            .flex_shrink_0()
                                            .flex()
                                            .justify_end()
                                            .text_size(TEXT_XS.size)
                                            .text_color(with_alpha(theme.muted_foreground, 0.55))
                                            .child(thread_time_label(&thread)),
                                    ),
                            )
                            .children(hit.map(|hit| ThreadSearchMatchExcerpt {
                                source: hit.source,
                                snippet: hit.snippet.into(),
                                query: query.clone().into(),
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .id("sidebar-thread-search-results")
            .flex()
            .flex_col()
            .gap(px(1.))
            .children(rows)
            .into_any_element()
    }
}

fn snapshot_match(
    sidebar: &Sidebar,
    thread: &EnvironmentThreadShell,
) -> Option<super::view_model::ThreadSearchMatch> {
    sidebar
        .search_matches
        .iter()
        .find(|hit| hit.environment_id == thread.environment_id && hit.thread_id == thread.id)
        .cloned()
}
