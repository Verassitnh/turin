//! Port of upstream/apps/web/src/components/ThreadHoverCard.tsx and the
//! `SidebarThreadTooltip` in Sidebar.tsx.
//!
//! The details card a row shows on hover: title, then project, branch,
//! provider and model, hand-offs and the last error, on the glass tooltip
//! surface (`max-w-80 text-left`). Environment rows are cut (one machine);
//! the branch-mismatch warning needs VCS status (T22 wiring) and terminal
//! processes are cut.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, FontWeight, Hsla, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, div, px, rems,
};

use crate::state::models::{
    EnvironmentProject, EnvironmentThreadShell, ProviderFailureClass, PullRequestState,
    ThreadPullRequestLink,
};
use super::thread_status_indicators::pull_request_state_presentation;
use super::view_model::SidebarSnapshot;
use crate::components::project_favicon::ProjectFavicon;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_MD, TEXT_XS};
use crate::ui::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::ui::{Icon, IconName};

/// Everything the card shows, captured when the tooltip opens.
#[derive(Clone, Debug)]
pub struct SidebarThreadTooltip {
    pub title: SharedString,
    pub project: Option<EnvironmentProject>,
    pub project_display_name: Option<SharedString>,
    pub branch: Option<SharedString>,
    /// `${modelLabel} · ${provider}` or the model label alone.
    pub model_label: Option<SharedString>,
    pub previous_providers: Vec<String>,
    /// `Some(true)` for a usage-limit stop.
    pub error: Option<bool>,
    pub pull_requests: Vec<ThreadPullRequestLink>,
}

impl SidebarThreadTooltip {
    pub fn from_thread(thread: &EnvironmentThreadShell, snapshot: &SidebarSnapshot) -> Self {
        let project_key = thread.project_key();
        let model_instance = thread
            .runtime
            .as_ref()
            .map(|runtime| runtime.provider_instance_id.clone())
            .unwrap_or_else(|| thread.model_selection.instance_id.clone());
        let entry = snapshot.provider_entry(&model_instance);
        let model_label = entry
            .and_then(|entry| {
                entry
                    .models
                    .iter()
                    .find(|(slug, _)| *slug == thread.model_selection.model)
                    .map(|(_, label)| label.clone())
            })
            .unwrap_or_else(|| thread.model_selection.model.clone());
        // `showInstanceBadge` only matters with several instances of one
        // driver; Turin has one instance per driver, so the name joins in
        // when the label alone does not say the provider.
        let model_label = entry.map(|entry| {
            if model_label.is_empty() {
                entry.display_name.clone()
            } else {
                format!("{model_label} · {}", entry.display_name)
            }
        });
        let previous_providers = thread
            .provider_instance_history
            .iter()
            .filter(|instance| **instance != model_instance)
            .map(|instance| {
                snapshot
                    .provider_entry(instance)
                    .map_or_else(|| instance.clone(), |entry| entry.display_name.clone())
            })
            .collect();
        let error = thread.runtime.as_ref().and_then(|runtime| {
            runtime
                .last_error
                .as_ref()
                .map(|_| runtime.last_error_class == Some(ProviderFailureClass::UsageLimit))
        });
        Self {
            title: thread.title.clone().into(),
            project: snapshot.project(&project_key).cloned(),
            project_display_name: snapshot
                .project_group_for(&project_key)
                .map(|group| group.display_name.clone().into()),
            branch: thread.branch.clone().map(Into::into),
            model_label: model_label.map(Into::into),
            previous_providers,
            error,
            pull_requests: thread
                .pull_requests
                .iter()
                .filter(|link| !link.dismissed)
                .cloned()
                .collect(),
        }
    }

    pub fn build(self, _: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| HoverCardView(self)).into()
    }
}

struct HoverCardView(SidebarThreadTooltip);

fn detail_row(icon: impl IntoElement, text: impl IntoElement, color: Hsla) -> AnyElement {
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(icon)
        .child(div().min_w_0().truncate().text_color(color).child(text))
        .into_any_element()
}

impl Render for HoverCardView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        let card = &self.0;
        let muted = theme.muted_foreground;
        let body: Hsla = with_alpha(theme.foreground, 0.75).into();
        let small = rems(0.75);
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(name) = &card.project_display_name {
            let favicon = card
                .project
                .as_ref()
                .map(|project| ProjectFavicon::new(project).size(small));
            rows.push(detail_row(
                div().flex_shrink_0().children(favicon),
                name.clone(),
                body,
            ));
        }
        if let Some(branch) = &card.branch {
            rows.push(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::GitBranch).size(small).color(muted))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle()
                            .text_color(body)
                            .child(branch.clone()),
                    )
                    .into_any_element(),
            );
        }
        if let Some(model) = &card.model_label {
            rows.push(detail_row(
                Icon::new(IconName::Bot)
                    .size(small)
                    .color(with_alpha(muted, 0.6).into()),
                model.clone(),
                body,
            ));
        }
        if !card.previous_providers.is_empty() {
            rows.push(detail_row(
                Icon::new(IconName::ArrowRightLeft).size(small).color(muted),
                format!("Handed off from {}", card.previous_providers.join(", ")),
                body,
            ));
        }
        if let Some(usage_limit) = card.error {
            let color = if usage_limit {
                theme.warning
            } else {
                theme.error
            };
            rows.push(detail_row(
                Icon::new(IconName::CircleAlert).size(small).color(color),
                if usage_limit {
                    "Usage limit reached"
                } else {
                    "Error occurred"
                },
                color,
            ));
        }
        let footer = (!card.pull_requests.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .border_t_1()
                .border_color(with_alpha(theme.border, 0.6))
                .pt_2()
                .pl(px(2.))
                .children(card.pull_requests.iter().map(|link| {
                    let (label, tone, icon) = pull_request_state_presentation(
                        link.state.or(Some(PullRequestState::Open)),
                        link.is_draft,
                        theme,
                    );
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_1p5()
                        .child(Icon::new(icon).size(small).color(tone))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(tone)
                                .child(format!("#{}", link.number)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(muted)
                                .child(link.title.clone().unwrap_or_else(|| label.to_string())),
                        )
                }))
        });
        // Offset from the pointer, as the anchored tooltip's `sideOffset`.
        div().pl(px(4.)).pt(px(4.)).child(
            div()
                .rounded(RADIUS_MD)
                .border_1()
                .border_color(dropdown_glass_border(theme))
                .bg(dropdown_glass_background(theme))
                .shadow(tokens::shadow_xl(0.25))
                .font_family(theme.fonts.sans.clone())
                .px_2()
                .py_1()
                .max_w(rems(20.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .max_w(rems(20.))
                        .gap_2()
                        .px_1()
                        .py_2()
                        .text_size(TEXT_XS.size)
                        .line_height(TEXT_XS.line_height)
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                // `leading-tight` (1.25 × 12px), not the 1rem row height.
                                .line_height(px(15.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(card.title.clone()),
                        )
                        .when(!rows.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(rems(0.375))
                                    .pl(px(2.))
                                    .text_color(muted)
                                    .children(rows),
                            )
                        })
                        .children(footer),
                ),
        )
    }
}
