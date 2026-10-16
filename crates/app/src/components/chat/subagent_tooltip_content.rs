//! Port of upstream/apps/web/src/components/chat/SubagentTooltipContent.tsx

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div, px, rems,
};

use crate::state::contracts::OrchestrationV2TurnItemStatus;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Icon, IconName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentTooltipStatusTone {
    Info,
    Error,
    Success,
    Muted,
}

pub fn subagent_tooltip_status_tone(
    status: OrchestrationV2TurnItemStatus,
) -> SubagentTooltipStatusTone {
    if matches!(
        status,
        OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Waiting
    ) {
        return SubagentTooltipStatusTone::Info;
    }
    if status == OrchestrationV2TurnItemStatus::Failed {
        return SubagentTooltipStatusTone::Error;
    }
    if status == OrchestrationV2TurnItemStatus::Completed {
        return SubagentTooltipStatusTone::Success;
    }
    SubagentTooltipStatusTone::Muted
}

pub fn subagent_tooltip_status_label(status: OrchestrationV2TurnItemStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
        .replace('_', " ")
}

/// Rich hover card body (`ThreadHoverCard` + metadata rows).
#[derive(IntoElement)]
pub struct SubagentTooltipContent {
    title: SharedString,
    model_label: SharedString,
    status: OrchestrationV2TurnItemStatus,
    preview: Option<SharedString>,
    elapsed: Option<AnyElement>,
}

impl SubagentTooltipContent {
    pub fn new(title: impl Into<SharedString>, status: OrchestrationV2TurnItemStatus) -> Self {
        Self {
            title: title.into(),
            model_label: "Subagent".into(),
            status,
            preview: None,
            elapsed: None,
        }
    }

    pub fn model_label(mut self, label: impl Into<SharedString>) -> Self {
        self.model_label = label.into();
        self
    }

    pub fn preview(mut self, preview: Option<String>) -> Self {
        self.preview = preview.map(Into::into);
        self
    }

    pub fn elapsed(mut self, elapsed: Option<AnyElement>) -> Self {
        self.elapsed = elapsed;
        self
    }
}

impl RenderOnce for SubagentTooltipContent {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let tone = subagent_tooltip_status_tone(self.status);
        let status_color = match tone {
            SubagentTooltipStatusTone::Info => theme.info,
            SubagentTooltipStatusTone::Error => theme.error,
            SubagentTooltipStatusTone::Success => theme.success,
            SubagentTooltipStatusTone::Muted => theme.muted_foreground,
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .max_w(rems(20.))
            .px(px(4.))
            .py_2()
            .child(
                div()
                    .truncate()
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(self.title),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Bot)
                            .size(rems(0.75))
                            .color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(self.model_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(status_color)
                            .child(subagent_tooltip_status_label(self.status)),
                    )
                    .children(self.elapsed),
            )
            .when(self.preview.is_some(), |this| {
                this.child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Terminal)
                                .size(rems(0.75))
                                .color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(self.preview.unwrap()),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_active_statuses_to_info_tone() {
        assert_eq!(
            subagent_tooltip_status_tone(OrchestrationV2TurnItemStatus::Running),
            SubagentTooltipStatusTone::Info
        );
    }

    #[test]
    fn humanizes_status_labels() {
        assert_eq!(
            subagent_tooltip_status_label(OrchestrationV2TurnItemStatus::Running),
            "running"
        );
    }
}
