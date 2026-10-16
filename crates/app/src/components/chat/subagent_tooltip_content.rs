//! Port of upstream/apps/web/src/components/chat/SubagentTooltipContent.tsx
//!
//! Geometry and preview limits stay identical in lineage and timeline
//! tooltips; the card body is `ThreadHoverCard` and the popup is the glass
//! tooltip surface.

use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, rems,
};

use super::agent_elapsed::AgentElapsed;
use crate::assets::provider_icon_path;
use crate::components::sidebar::thread_hover_card::{ThreadHoverCard, thread_hover_card_popup};
use crate::state::contracts::OrchestrationV2TurnItemStatus;
use crate::state::subagent_display::SubagentMetadata;
use crate::state::subagent_runtime::RuntimeSubagentStatus;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::ui::middle_truncate::MiddleTruncate;
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

/// `runtimeStatus` → the wire status the card labels.
pub fn runtime_status_to_turn_item_status(
    status: RuntimeSubagentStatus,
) -> OrchestrationV2TurnItemStatus {
    match status {
        RuntimeSubagentStatus::Pending => OrchestrationV2TurnItemStatus::Pending,
        RuntimeSubagentStatus::Running => OrchestrationV2TurnItemStatus::Running,
        RuntimeSubagentStatus::Waiting => OrchestrationV2TurnItemStatus::Waiting,
        RuntimeSubagentStatus::Completed => OrchestrationV2TurnItemStatus::Completed,
        RuntimeSubagentStatus::Failed => OrchestrationV2TurnItemStatus::Failed,
        RuntimeSubagentStatus::Cancelled => OrchestrationV2TurnItemStatus::Cancelled,
        RuntimeSubagentStatus::Interrupted => OrchestrationV2TurnItemStatus::Interrupted,
        RuntimeSubagentStatus::Idle => OrchestrationV2TurnItemStatus::Idle,
    }
}

/// CSS `capitalize`: the first letter of every word.
fn capitalize_words(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut at_word_start = true;
    for ch in value.chars() {
        if at_word_start {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
        at_word_start = ch.is_whitespace();
    }
    out
}

/// Rich hover card body for a subagent (`ThreadHoverCard` + metadata rows).
#[derive(Clone, IntoElement)]
pub struct SubagentTooltipContent {
    title: SharedString,
    metadata: SubagentMetadata,
    /// The provider driver kind (`codex`, `claudeAgent`, …) behind the glyph.
    driver: Option<SharedString>,
    /// The account name, when several instances of one driver exist.
    instance_name: Option<SharedString>,
    effort: Option<SharedString>,
    status: OrchestrationV2TurnItemStatus,
    elapsed: Option<AgentElapsed>,
    preview: Option<SharedString>,
}

impl SubagentTooltipContent {
    pub fn new(title: impl Into<SharedString>, status: OrchestrationV2TurnItemStatus) -> Self {
        Self {
            title: title.into(),
            metadata: SubagentMetadata {
                model_label: "Not reported".to_owned(),
                workspace: Vec::new(),
            },
            driver: None,
            instance_name: None,
            effort: None,
            status,
            elapsed: None,
            preview: None,
        }
    }

    pub fn metadata(mut self, metadata: SubagentMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn driver(mut self, driver: Option<impl Into<SharedString>>) -> Self {
        self.driver = driver.map(Into::into);
        self
    }

    #[allow(dead_code)]
    /// `showInstanceBadge`: the card names the account in the model label.
    pub fn instance_name(mut self, name: Option<impl Into<SharedString>>) -> Self {
        self.instance_name = name.map(Into::into);
        self
    }

    pub fn effort(mut self, effort: Option<impl Into<SharedString>>) -> Self {
        self.effort = effort.map(Into::into);
        self
    }

    pub fn preview(mut self, preview: Option<String>) -> Self {
        self.preview = preview.map(Into::into);
        self
    }

    pub fn elapsed(mut self, elapsed: Option<AgentElapsed>) -> Self {
        self.elapsed = elapsed;
        self
    }

    /// The tooltip view GPUI shows on hover.
    pub fn build(self, _: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| SubagentTooltipView(self)).into()
    }
}

/// A `Tooltip delay={200}` trigger around `child` whose popup is the subagent
/// hover card. GPUI shows tooltips below the pointer, not on a chosen side.
pub fn with_subagent_tooltip(
    id: impl Into<gpui_kit::ElementId>,
    content: SubagentTooltipContent,
    child: impl IntoElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex()
        .min_w_0()
        .child(child)
        .tooltip_show_delay(std::time::Duration::from_millis(200))
        .tooltip(move |window, cx| content.clone().build(window, cx))
}

struct SubagentTooltipView(SubagentTooltipContent);

impl Render for SubagentTooltipView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        thread_hover_card_popup(self.0.clone())
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
        let status_icon = match tone {
            SubagentTooltipStatusTone::Error => IconName::CircleX,
            SubagentTooltipStatusTone::Success => IconName::Check,
            SubagentTooltipStatusTone::Info | SubagentTooltipStatusTone::Muted => {
                IconName::CircleDashed
            }
        };
        let small = rems(0.75);
        let model_label = match &self.instance_name {
            Some(name) => format!("{} · {name}", self.metadata.model_label),
            None => self.metadata.model_label.clone(),
        };
        let glyph = match self.driver.as_deref().and_then(provider_icon_path) {
            // `size-3 shrink-0 grayscale opacity-60`.
            Some(path) => Icon::path(path)
                .size(small)
                .color(with_alpha(theme.foreground, 0.6).into()),
            None => Icon::new(IconName::Bot)
                .size(small)
                .color(theme.muted_foreground),
        };
        let mut rows: Vec<AnyElement> = vec![
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(glyph)
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .text_color(with_alpha(theme.foreground, 0.75))
                        .child(div().min_w_0().truncate().child(model_label))
                        .children(
                            self.effort
                                .map(|effort| div().flex_shrink_0().child(format!(" · {effort}"))),
                        ),
                )
                .into_any_element(),
            div()
                .flex()
                .min_w_0()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(status_color)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(Icon::new(status_icon).size(small).color(status_color))
                        .child(capitalize_words(&subagent_tooltip_status_label(
                            self.status,
                        ))),
                )
                .children(self.elapsed)
                .into_any_element(),
        ];
        for entry in self.metadata.workspace {
            let icon = if entry.label == "Branch" {
                IconName::GitBranch
            } else {
                IconName::Folder
            };
            rows.push(metadata_row(icon, entry.value));
        }
        if let Some(preview) = self.preview {
            rows.push(metadata_row(IconName::Terminal, preview.to_string()));
        }
        ThreadHoverCard {
            title: self.title,
            rows,
            footer: None,
        }
    }
}

/// `<Icon className="size-3 shrink-0" /><MiddleTruncate value />`.
fn metadata_row(icon: IconName, value: String) -> AnyElement {
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(Icon::new(icon).size(rems(0.75)))
        .child(MiddleTruncate::new(value))
        .into_any_element()
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
