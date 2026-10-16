//! Port of upstream/apps/web/src/components/chat/ProviderSubagentBar.tsx
//!
//! Stands in for the composer on a provider-native subagent thread. The provider runs that
//! conversation, so there is nothing to send; the bar says which model is working, for how long,
//! and leads back to the parent.
//!
//! The status label is computed on each render; upstream ticks a live bar through DOM writes
//! once a second, and here the owner's refresh does (as for `AgentElapsed`).

use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div, rems,
};

use super::thread_details_pr_row::OpenHandler;
use crate::assets::provider_icon_path;
use crate::composer::catalog::ProviderInstanceEntry;
use crate::state::thread_execution::{ProviderSubagentStatus, format_provider_subagent_status};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_SM;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

#[derive(IntoElement)]
pub struct ProviderSubagentBar {
    /// The provider running the subagent; no icon while its catalog loads.
    pub provider: Option<ProviderInstanceEntry>,
    pub model_label: SharedString,
    /// Reasoning effort as the composer names it, when the subagent has one.
    pub effort_label: Option<SharedString>,
    /// `None` until the subagent's root turn arrives.
    pub status: Option<ProviderSubagentStatus>,
    pub on_open_parent: Option<OpenHandler>,
}

/// The screen-reader line: announced once per transition, so it carries no ticking time.
pub fn provider_subagent_announcement(
    model_label: &str,
    effort_label: Option<&str>,
    status: Option<&ProviderSubagentStatus>,
) -> String {
    let model_description = match effort_label {
        Some(effort) => format!("{model_label}, {effort}"),
        None => model_label.to_owned(),
    };
    let announced = status.map(|status| ProviderSubagentStatus {
        started_at: None,
        ..status.clone()
    });
    format!(
        "{model_description} subagent: {}",
        format_provider_subagent_status(announced.as_ref(), 0.0)
    )
}

impl RenderOnce for ProviderSubagentBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let now_ms = chrono::Utc::now().timestamp_millis() as f64;
        let status_label = format_provider_subagent_status(self.status.as_ref(), now_ms);
        let glyph = self
            .provider
            .as_ref()
            .and_then(|provider| provider_icon_path(&provider.driver_kind))
            .map(|path| {
                Icon::path(path)
                    .size(rems(1.))
                    .color(theme.muted_foreground)
            });
        div()
            .flex()
            .min_h(rems(3.))
            .items_center()
            .gap_3()
            .rounded_3xl()
            .py_2()
            // `ps-5 pe-2`.
            .pl(rems(1.25))
            .pr_2()
            .text_size(TEXT_SM.size)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .children(glyph)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(self.model_label),
                    )
                    .children(self.effort_label.map(|effort| {
                        div()
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(effort)
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(status_label),
            )
            .child(
                div()
                    .ml_auto()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child("Runs on its own"),
            )
            .children(self.on_open_parent.map(|on_open| {
                Button::new("open-parent")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .icon(IconName::ArrowUpLeft)
                    .icon_size(rems(1.))
                    .label("Open parent")
                    .on_click(move |event, window, cx| on_open(event, window, cx))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2ExecutionNodeStatus;

    fn status(status: OrchestrationV2ExecutionNodeStatus) -> ProviderSubagentStatus {
        ProviderSubagentStatus {
            status,
            started_at: Some("2026-10-15T10:00:00.000Z".into()),
            completed_at: None,
        }
    }

    #[test]
    fn announces_the_model_effort_and_status_without_a_ticking_time() {
        let running = status(OrchestrationV2ExecutionNodeStatus::Running);
        // The live status would carry the elapsed time; the announcement has none.
        assert_eq!(
            provider_subagent_announcement("GPT-5.4", Some("High"), Some(&running)),
            "GPT-5.4, High subagent: Working"
        );
        assert_eq!(
            provider_subagent_announcement("GPT-5.4", None, None),
            "GPT-5.4 subagent: Starting"
        );
    }
}
