//! Port of upstream/apps/web/src/components/chat/ThreadRelationshipIcon.tsx

use gpui_kit::{
    App, Hsla, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, px,
};

use super::thread_details_panel_styles::THREAD_DETAILS_ICON_SIZE;
use crate::theme::ActiveAppTheme;
use crate::ui::{Icon, IconName};

/// Human-readable status for lineage and timeline relationship badges.
pub fn thread_relationship_status_label(status: Option<&str>) -> &'static str {
    match status.unwrap_or("") {
        "preparing" | "starting" => "Starting",
        "running" | "in_progress" => "Running",
        "pending" | "queued" => "Queued",
        "waiting" | "blocked" => "Waiting",
        "completed" => "Done",
        "failed" | "error" => "Failed",
        "cancelled" | "interrupted" => "Stopped",
        "rolled_back" => "Reverted",
        "resolved_native" => "Resolved (native)",
        "resolved_portable" => "Resolved (portable)",
        "consumed" => "Consumed",
        "superseded" => "Superseded",
        "idle" => "Idle",
        _ => "Unknown",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRelationshipBadgeTone {
    Info,
    Destructive,
    Success,
    Muted,
}

pub fn thread_relationship_badge_tone(status: Option<&str>) -> ThreadRelationshipBadgeTone {
    match status.unwrap_or("") {
        "running" | "in_progress" | "pending" | "waiting" => ThreadRelationshipBadgeTone::Info,
        "failed" | "error" => ThreadRelationshipBadgeTone::Destructive,
        "completed" => ThreadRelationshipBadgeTone::Success,
        _ => ThreadRelationshipBadgeTone::Muted,
    }
}

fn badge_color(tone: ThreadRelationshipBadgeTone, theme: &crate::theme::AppTheme) -> Hsla {
    match tone {
        ThreadRelationshipBadgeTone::Info => theme.info,
        ThreadRelationshipBadgeTone::Destructive => theme.destructive,
        ThreadRelationshipBadgeTone::Success => theme.success,
        ThreadRelationshipBadgeTone::Muted => theme.muted_foreground,
    }
}

#[derive(IntoElement)]
pub struct ThreadRelationshipIcon {
    pub status: Option<String>,
    pub fallback: IconName,
}

impl RenderOnce for ThreadRelationshipIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let tone = thread_relationship_badge_tone(self.status.as_deref());
        div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(THREAD_DETAILS_ICON_SIZE)
            .flex_shrink_0()
            .child(
                Icon::new(self.fallback)
                    .size(THREAD_DETAILS_ICON_SIZE)
                    .color(theme.muted_foreground),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(-4.))
                    .right(px(-4.))
                    .size(px(8.))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.card)
                    .bg(badge_color(tone, &theme)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_wire_statuses_to_labels() {
        assert_eq!(thread_relationship_status_label(Some("queued")), "Queued");
        assert_eq!(
            thread_relationship_status_label(Some("resolved_portable")),
            "Resolved (portable)"
        );
        assert_eq!(thread_relationship_status_label(None), "Unknown");
    }

    #[test]
    fn maps_wire_statuses_to_badge_tones() {
        assert_eq!(
            thread_relationship_badge_tone(Some("running")),
            ThreadRelationshipBadgeTone::Info
        );
        assert_eq!(
            thread_relationship_badge_tone(Some("failed")),
            ThreadRelationshipBadgeTone::Destructive
        );
    }
}
