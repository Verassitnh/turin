//! Port of upstream/apps/web/src/components/ChatView.tsx (layout only).
//!
//! The chat column: the 52px drag-region header, the messages timeline and
//! the composer docked at the bottom of the `--chat-content-max-width` lane,
//! with pending approvals / questions attached above it. Find and the thread
//! details panel are not ported yet; the terminal drawer is cut.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div, px, rems,
};

use super::chat::chat_canvas::{ChatCanvas, ChatCanvasMeasurements};
use super::chat::chat_header::ChatHeader;
use super::chat::chat_overlay_banners::{ProviderStatusBanner, ThreadErrorBanner};
use super::chat::composer_pending_panels::{
    ComposerPendingApprovalBanner, ComposerPendingUserInputBanner,
};
use super::chat::messages_timeline::MessagesTimeline;
use super::chat::provider_status_banner::should_show_provider_status_banner;
use super::chat::thread_error_banner::should_show_thread_error_banner;
use super::chat::timeline::session_logic::{derive_pending_approvals, derive_pending_user_inputs};
use super::chat::timeline::source::MockTimelineSource;
use super::drag_region::DragRegion as _;
use super::workspace_titlebar::workspace_titlebar_content_left;
use crate::composer::catalog::{
    CompatibilityAdvisory, CompatibilityAdvisoryStatus, ProviderInstanceEntry,
};
use crate::composer::mock_source::MockComposerSource;
use crate::composer::views::chat_composer::ChatComposer;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{CHAT_CONTENT_MAX_WIDTH, WORKSPACE_TOPBAR_HEIGHT};

#[derive(IntoElement)]
pub struct ChatView {
    pub sidebar_open: bool,
    pub right_panel_open: bool,
    /// The routed thread's `(project, title)` (sidebar selection).
    pub thread: Option<(SharedString, SharedString)>,
}

impl RenderOnce for ChatView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        // `px-3 sm:px-5`; with the sidebar collapsed the title clears the
        // traffic lights and the fixed sidebar toggle.
        let header_left = if self.sidebar_open {
            px(20.)
        } else {
            workspace_titlebar_content_left(window)
        };
        // `.chat-composer-lane` / timeline `padding-inline: 3rem` at ≥640px.
        let lane_inset = rems(3.);
        let timeline = window.use_keyed_state("messages-timeline", cx, |window, cx| {
            let store = MockTimelineSource::start(cx);
            MessagesTimeline::new(store, window, cx)
        });
        let composer = window.use_keyed_state("chat-composer", cx, |window, cx| {
            let store = MockComposerSource::start(cx);
            ChatComposer::new(store, window, cx)
        });
        // The composer's top drawer: the oldest pending approval, else the
        // oldest pending question (ChatComposer.tsx `showComposerTopDrawer`).
        let store = timeline.read(cx).store().clone();
        let snapshot = store.read(cx).snapshot().clone();
        let source = store.read(cx).source();
        let approvals = derive_pending_approvals(&snapshot.pending_approvals);
        let inputs = derive_pending_user_inputs(&snapshot.pending_user_inputs);
        let drawer = if let Some(approval) = approvals.first().cloned() {
            Some(
                ComposerPendingApprovalBanner {
                    approval,
                    pending_count: approvals.len(),
                    source,
                }
                .into_any_element(),
            )
        } else {
            inputs
                .first()
                .cloned()
                .map(|prompt| ComposerPendingUserInputBanner { prompt, source }.into_any_element())
        };

        let overlay_banners = chat_overlay_banners();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(theme.background)
            .child(
                div()
                    .id("chat-header")
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h(WORKSPACE_TOPBAR_HEIGHT)
                    .pl(header_left)
                    .pr(px(20.))
                    .bg(theme.background)
                    .children(self.thread.clone().map(|(project, title)| ChatHeader {
                        project,
                        title,
                        right_panel_open: self.right_panel_open,
                    }))
                    .drag_region(window, cx),
            )
            .child(ChatCanvas::from_measurements(
                ChatCanvasMeasurements {
                    width: 1200.0,
                    ..Default::default()
                },
                None,
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .children(overlay_banners)
                    .child(timeline)
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(lane_inset)
                            .pt_2()
                            .child(
                                div()
                                    .mx_auto()
                                    .w_full()
                                    .max_w(CHAT_CONTENT_MAX_WIDTH)
                                    .children(drawer)
                                    .child(composer),
                            )
                            .child(div().h(rems(1.25))),
                    )
                    .into_any_element(),
            ))
    }
}

fn chat_overlay_banners() -> Vec<gpui_kit::AnyElement> {
    let Ok(demo) = std::env::var("TURIN_CHAT_DEMO") else {
        return Vec::new();
    };
    let mut banners = Vec::new();
    if demo.contains("provider-status") {
        let mut entry = ProviderInstanceEntry::new("codex-work", "codex", "Codex");
        entry.version = Some("1.0.0".into());
        entry.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Unsupported,
            message: Some("Unsupported version. Use 2.0.0.".into()),
        });
        if should_show_provider_status_banner(&entry, None) {
            banners.push(ProviderStatusBanner { entry }.into_any_element());
        }
    }
    if demo.contains("thread-error") {
        let error = "The first error line\ncontinues on a second line";
        if should_show_thread_error_banner("demo:thread", Some(error), false) {
            banners.push(
                ThreadErrorBanner {
                    error: error.into(),
                    usage_limit: false,
                }
                .into_any_element(),
            );
        }
    }
    if demo.contains("usage-limit-error") {
        let error = "Usage limit reached for this provider.";
        if should_show_thread_error_banner("demo:thread", Some(error), false) {
            banners.push(
                ThreadErrorBanner {
                    error: error.into(),
                    usage_limit: true,
                }
                .into_any_element(),
            );
        }
    }
    banners
}
