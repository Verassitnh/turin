//! Port of upstream/apps/web/src/components/CommandPaletteResults.tsx
//!
//! `CommandPaletteVirtualizedResults`: only the visible rows are rendered
//! (GPUI's [`ListState`] in place of LegendList), group headings and result
//! rows share one flat list. The parent tracks the highlight itself, so a
//! row is "active" by value, never by hover state.
//!
//! Row anatomy, as `CommandPaletteResultRow`: icon, a one- or two-line body
//! (title with leading/trailing addons; optional message excerpt; optional
//! description), the relative timestamp, the shortcut label and a chevron for
//! submenus. Disabled rows render dimmed and are skipped by the keyboard.

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, Styled as _, Window, div, list, px, rems,
};

use super::CommandPalette;
use super::command_palette_logic::{
    CommandPaletteItem, CommandPaletteRow, PaletteAction, PaletteAddon, PaletteDescription,
    PaletteIcon, PaletteTitle, ThemePreviewSpec,
};
use super::shortcut_labels::default_shortcut_label_for_command;
use crate::components::project_favicon::ProjectFavicon;
use crate::components::sidebar::sidebar_logic::{
    ThreadStatusPill, ThreadStatusPillLabel, resolve_thread_last_visited_at,
    resolve_thread_status_pill,
};
use crate::components::thread_command_subtitle::{ThreadCommandSubtitle, command_palette_meta_dot};
use crate::components::thread_search_match::ThreadSearchMatchExcerpt;
use crate::theme::color::{hex, with_alpha};
use crate::theme::tokens::{TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::command::{CommandShortcut, command_item_frame, command_list_heading};
use crate::ui::icon::{Icon, IconName};

/// `ITEM_ICON_CLASS`: `size-4 text-icon-muted`.
fn item_icon(icon: PaletteIcon, project_icon: Option<AnyElement>, theme: &AppTheme) -> AnyElement {
    match icon {
        PaletteIcon::None => div().into_any_element(),
        PaletteIcon::Named(name) => Icon::new(name)
            .size(rems(1.))
            .color(theme.icon_muted)
            .into_any_element(),
        PaletteIcon::ProjectFavicon => project_icon.unwrap_or_else(|| {
            Icon::new(IconName::Folder)
                .color(theme.icon_muted)
                .into_any_element()
        }),
    }
}

/// The color of a thread status pill: `text-sky-600 dark:text-sky-300/80`
/// and friends.
fn status_pill_color(pill: &ThreadStatusPill, theme: &AppTheme) -> Hsla {
    let dark = theme.is_dark();
    let tone = |light: u32, dark_color: u32, alpha: f32| -> Hsla {
        if dark {
            with_alpha(hex(dark_color), alpha).into()
        } else {
            hex(light).into()
        }
    };
    match pill.label {
        ThreadStatusPillLabel::PendingApproval => tone(0xe17100, 0xffd230, 0.9),
        ThreadStatusPillLabel::AwaitingInput => tone(0x4f39f6, 0xa3b3ff, 0.9),
        ThreadStatusPillLabel::Working | ThreadStatusPillLabel::Connecting => {
            tone(0x0084d1, 0x74d4ff, 0.8)
        }
        ThreadStatusPillLabel::Waiting => theme.sidebar_muted_foreground,
        ThreadStatusPillLabel::PlanReady => tone(0x7f22fe, 0xc4b4ff, 0.9),
        ThreadStatusPillLabel::Completed => tone(0x009966, 0x5ee9b5, 0.9),
    }
}

impl CommandPalette {
    /// `CommandPaletteVirtualizedResults` (and its empty state).
    pub(crate) fn render_results(
        &mut self,
        empty_message: Option<&'static str>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        if self.rows.rows.is_empty() {
            // `CommandPaletteEmptyState`: `py-10 text-center text-sm text-muted-foreground`.
            let message = empty_message.unwrap_or(if self.is_actions_only() {
                "No matching actions."
            } else {
                "No matching commands, projects, or threads."
            });
            return div()
                .py(rems(2.5))
                .flex()
                .justify_center()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child(message)
                .into_any_element();
        }
        // The panel hugs its rows up to the dialog's `max-h-105` less the input
        // row and footer; the list scrolls inside. Row heights are estimated
        // (heading 28px, one-line row 32px, two-line row 48px).
        let estimate: f32 = 16.
            + self
                .rows
                .rows
                .iter()
                .map(|row| match row {
                    CommandPaletteRow::Label { first, .. } => 28. + if *first { 0. } else { 6. },
                    CommandPaletteRow::Item { item, .. } => {
                        if item.description.is_some() || item.thread_content_match.is_some() {
                            48.
                        } else {
                            32.
                        }
                    }
                })
                .sum::<f32>();
        let max_height = 420. - 46. - 42. - self.panel_extra_height;
        div()
            .relative()
            .h(px(estimate.min(max_height.max(96.))))
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, window, cx| this.render_result_row(ix, window, cx)),
                )
                .size_full(),
            )
            .vertical_scrollbar(&self.list)
            .into_any_element()
    }

    fn render_result_row(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let last = ix + 1 == self.rows.rows.len();
        // `contentContainerClassName="px-2"`, list `py-2`.
        let wrap = |element: AnyElement| {
            div()
                .px_2()
                .when(ix == 0, |this| this.pt_2())
                .when(last, |this| this.pb_2())
                .child(element)
                .into_any_element()
        };
        match row {
            CommandPaletteRow::Label { label, first, .. } => wrap(
                div()
                    .when(!first, |this| this.pt(rems(0.375)))
                    .child(command_list_heading(label, cx))
                    .into_any_element(),
            ),
            CommandPaletteRow::Item {
                item, item_index, ..
            } => {
                let element = if item_index.is_none() {
                    self.render_item(&item, false, ix, cx)
                } else {
                    self.render_item(&item, true, ix, cx)
                };
                wrap(element)
            }
        }
    }

    /// `CommandPaletteResultRow` / `DisabledCommandPaletteResultRow`.
    fn render_item(
        &mut self,
        item: &CommandPaletteItem,
        enabled: bool,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let active_value = if self.auto_highlights_first_row() && !self.highlight_cleared {
            self.effective_highlight()
        } else {
            self.highlighted.clone()
        };
        let active = enabled && active_value.as_deref() == Some(item.value.as_str());
        let sidebar = self.snapshot.as_ref().map(|snapshot| &snapshot.sidebar);

        let project_for_action = match item.run() {
            Some(PaletteAction::OpenProject { project_key })
            | Some(PaletteAction::NewThreadInProject { project_key }) => sidebar
                .and_then(|snapshot| snapshot.project(project_key))
                .map(|project| ProjectFavicon::new(project).into_any_element()),
            _ => None,
        };
        let icon = item_icon(item.icon, project_for_action, &theme);

        let has_body_detail = item.description.is_some() || item.thread_content_match.is_some();
        let title_text = match &item.title {
            PaletteTitle::Text(text) => div().truncate().child(text.clone()).into_any_element(),
            PaletteTitle::NewThreadIn { project } => div()
                .truncate()
                .child(
                    div().flex().gap(rems(0.25)).child("New thread in").child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(project.clone()),
                    ),
                )
                .into_any_element(),
        };
        let title_row = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(rems(0.375))
            .text_size(TEXT_SM.size)
            .text_color(theme.foreground)
            .children(
                item.title_leading_content
                    .as_ref()
                    .map(|addon| self.render_addon(addon, cx)),
            )
            .child(title_text);

        let body = if has_body_detail {
            let mut column = div().flex().flex_col().min_w_0().flex_1().child(title_row);
            if let Some(hit) = &item.thread_content_match {
                column = column.child(ThreadSearchMatchExcerpt {
                    source: hit.source,
                    snippet: SharedString::from(hit.snippet.clone()),
                    query: SharedString::from(hit.query.clone()),
                });
            }
            if let Some(description) = &item.description {
                column = column.child(
                    div()
                        .min_w_0()
                        .text_size(TEXT_XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.7))
                        .child(self.render_description(description, cx)),
                );
            }
            column.into_any_element()
        } else {
            div()
                .flex()
                .min_w_0()
                .flex_1()
                .child(title_row)
                .into_any_element()
        };

        let trailing: Vec<AnyElement> = item
            .title_trailing_content
            .iter()
            .map(|addon| self.render_addon(addon, cx))
            .collect();

        let shortcut_label = item
            .shortcut_command
            .as_deref()
            .and_then(|command| default_shortcut_label_for_command(command, true));

        let mut row = command_item_frame(active, !enabled, cx)
            .id(("command-palette-item", ix))
            .when(enabled, |this| this.cursor(CursorStyle::PointingHand))
            .child(icon)
            .child(body)
            .children(trailing);

        if let Some(timestamp) = &item.timestamp {
            // `min-w-12 shrink-0 text-right text-xs tabular-nums text-muted-foreground/70`.
            row = row.child(
                div()
                    .min_w(rems(3.))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .text_size(TEXT_XS.size)
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child(timestamp.clone()),
            );
        }
        if let Some(label) = shortcut_label {
            row = row.child(CommandShortcut::new(label));
        }
        if item.is_submenu() {
            // `-me-0.5 ms-auto size-4 shrink-0 text-muted-foreground/70`.
            row = row.child(
                div().ml_auto().mr(px(-2.)).child(
                    Icon::new(IconName::ChevronRight)
                        .color(with_alpha(theme.muted_foreground, 0.7).into()),
                ),
            );
        }

        if enabled {
            let value = item.value.clone();
            let click_item = item.clone();
            row = row
                // The pointer moves the highlight, as Base UI does.
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if this.highlighted.as_deref() != Some(value.as_str()) {
                        this.highlighted = Some(value.clone());
                        this.highlight_cleared = false;
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.execute_item(&click_item, window, cx);
                    }),
                );
        }
        row.into_any_element()
    }

    fn render_description(
        &mut self,
        description: &PaletteDescription,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match description {
            PaletteDescription::Text(text) => div().child(text.clone()).into_any_element(),
            PaletteDescription::Thread(subtitle) => {
                let favicon = subtitle.project_key.as_ref().and_then(|key| {
                    self.snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.sidebar.project(key))
                        .map(ProjectFavicon::new)
                });
                ThreadCommandSubtitle::new(subtitle.clone(), favicon).into_any_element()
            }
            PaletteDescription::ProjectLocation {
                location_label,
                workspace_root,
            } => div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(div().truncate().child(location_label.clone()))
                .child(command_palette_meta_dot(cx))
                .child(div().truncate().child(workspace_root.clone()))
                .into_any_element(),
            PaletteDescription::ProjectEnvironments(labels) => {
                div().truncate().child(labels.clone()).into_any_element()
            }
        }
    }

    fn render_addon(&mut self, addon: &PaletteAddon, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        match addon {
            PaletteAddon::Current => div()
                .flex_shrink_0()
                .text_size(TEXT_XS.size)
                .text_color(with_alpha(theme.muted_foreground, 0.7))
                .child("Current")
                .into_any_element(),
            PaletteAddon::ChevronRight => div()
                .ml_auto()
                .mr(px(-2.))
                .child(
                    Icon::new(IconName::ChevronRight)
                        .color(with_alpha(theme.muted_foreground, 0.7).into()),
                )
                .into_any_element(),
            PaletteAddon::Check => div()
                .ml_auto()
                .child(
                    Icon::new(IconName::Check)
                        .color(with_alpha(theme.muted_foreground, 0.7).into()),
                )
                .into_any_element(),
            PaletteAddon::ThreadLeadingStatus { thread_key } => {
                self.render_thread_leading_status(thread_key, &theme)
            }
            PaletteAddon::ThreadTrailing {
                thread_key,
                query,
                settled,
            } => self.render_thread_trailing(thread_key, query, *settled, &theme, cx),
            PaletteAddon::ThemePreview { current, previews } => div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .when(*current, |this| {
                    this.child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .child("Current"),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(previews.iter().map(theme_preview_circle)),
                )
                .into_any_element(),
        }
    }

    /// `ThreadRowLeadingStatus`: the change-request glyph and the thread
    /// status label (`inline-flex items-center gap-1 text-3xs`).
    fn render_thread_leading_status(&self, thread_key: &str, theme: &AppTheme) -> AnyElement {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return div().into_any_element();
        };
        let Some(thread) = snapshot
            .sidebar
            .threads
            .iter()
            .find(|thread| thread.key() == thread_key)
        else {
            return div().into_any_element();
        };
        let local = snapshot
            .sidebar
            .local_last_visited_at
            .get(thread_key)
            .map(String::as_str);
        let last_visited = resolve_thread_last_visited_at(
            thread
                .last_visited_at
                .as_ref()
                .map(|value| value.as_deref()),
            local,
        );
        let Some(pill) = resolve_thread_status_pill(thread, last_visited.as_deref()) else {
            return div().into_any_element();
        };
        let color = status_pill_color(&pill, theme);
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .text_size(TEXT_3XS.size)
            .text_color(color)
            .child(div().size(rems(0.375)).rounded_full().bg(color))
            .child(pill.label.as_str())
            .into_any_element()
    }

    /// `ThreadSearchPullRequestNumber` + `ThreadRowTrailingStatus` (the latter
    /// shows terminal and remote-environment glyphs; both are cut).
    fn render_thread_trailing(
        &self,
        thread_key: &str,
        query: &str,
        settled: bool,
        theme: &AppTheme,
        _: &mut Context<Self>,
    ) -> AnyElement {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return div().into_any_element();
        };
        let Some(thread) = snapshot
            .sidebar
            .threads
            .iter()
            .find(|thread| thread.key() == thread_key)
        else {
            return div().into_any_element();
        };
        let matched = thread
            .pull_requests
            .iter()
            .filter(|link| !link.dismissed)
            .find(|link| {
                crate::components::sidebar::sidebar_logic::pull_request_matches_search_query(
                    &crate::components::sidebar::thread_status_indicators::link_search_terms(
                        std::slice::from_ref(*link),
                    ),
                    query,
                )
            });
        match matched {
            Some(link) => div()
                .flex_shrink_0()
                .text_size(TEXT_XS.size)
                .text_color(if settled {
                    theme.secondary_label
                } else {
                    use crate::components::sidebar::thread_status_indicators::pull_request_state_tone;
                    use crate::state::support::thread_pull_requests::PullRequestBadgeState;
                    pull_request_state_tone(
                        match link.state {
                            Some(crate::state::models::PullRequestState::Merged) => {
                                PullRequestBadgeState::Merged
                            }
                            Some(crate::state::models::PullRequestState::Closed) => {
                                PullRequestBadgeState::Closed
                            }
                            _ if link.is_draft => PullRequestBadgeState::Draft,
                            _ => PullRequestBadgeState::Open,
                        },
                        theme,
                    )
                })
                .child(format!("#{}", link.number))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }
}

/// `ThemePreviewCircle` at `size-3 border-0`: the canvas disc with the accent
/// and message-action glows reduced to two dots.
fn theme_preview_circle(preview: &ThemePreviewSpec) -> AnyElement {
    let canvas: Hsla = crate::theme::color::color(&preview.canvas);
    let accent: Hsla = crate::theme::color::color(&preview.accent);
    let action: Hsla = crate::theme::color::color(&preview.message_action);
    div()
        .relative()
        .size(rems(0.75))
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .bg(canvas)
        .child(
            div()
                .absolute()
                .top(px(-1.))
                .right(px(-1.))
                .size(px(7.))
                .rounded_full()
                .bg(accent),
        )
        .child(
            div()
                .absolute()
                .bottom(px(1.))
                .left(px(1.))
                .size(px(4.))
                .rounded_full()
                .bg(action),
        )
        .into_any_element()
}
