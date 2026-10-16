//! Port of upstream/apps/web/src/components/ChatView.tsx (layout only).
//!
//! The chat column: the 52px drag-region header, the messages timeline and
//! the composer docked at the bottom of the `--chat-content-max-width` lane,
//! with pending approvals / questions attached above it. Find and the thread
//! details panel are not ported yet; the terminal drawer is cut.

use gpui_kit::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, px, rems,
};

use super::chat::chat_canvas::{ChatCanvas, ChatCanvasMeasurements};
use super::chat::chat_canvas_layout::ChatCanvasDetailsCard;
use super::chat::chat_git_host::ChatGitHost;
use super::chat::chat_header::ChatHeader;
use super::chat::chat_overlay_banners::{ProviderStatusBanner, ThreadErrorBanner};
use super::chat::composer_pending_panels::{
    ComposerPendingApprovalBanner, ComposerPendingUserInputBanner,
};
use super::chat::messages_timeline::MessagesTimeline;
use super::chat::provider_status_banner::{
    get_provider_status_banner_key, should_show_provider_status_banner,
};
use super::chat::thread_details_card::ThreadDetailsCard;
use super::chat::thread_details_card_layout::{
    ThreadDetailsCardContainer, ThreadDetailsCardLane, ThreadDetailsCardLayout,
    ThreadDetailsCardLayoutInput, resolve_thread_details_card_layout,
};
use super::chat::thread_details_panel::ThreadDetailsPanel;
use super::chat::thread_error_banner::{
    dismiss_thread_error_banner_for_session, get_thread_error_banner_key,
    is_thread_error_banner_dismissed_for_session, should_show_thread_error_banner,
};
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
    /// The branch toolbar and thread details card (`BranchToolbar`,
    /// `GitActionsControl`), hosted with the state upstream's ChatView owns.
    pub git_host: Entity<ChatGitHost>,
    /// The thread details card is shown (the titlebar toggle).
    pub thread_details_open: bool,
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

        let overlay_banners = chat_overlay_banners(window, cx);
        let composer_strip = self.git_host.read(cx).composer_strip();
        // `TURIN_CHAT_DEMO=thread-details` swaps in the lineage demo card.
        // `ChatCanvas` measures its container (ResizeObserver upstream); the
        // size feeds the lane and the details card layout on the next frame.
        let canvas_size =
            window.use_keyed_state("chat-canvas-size", cx, |_, _| CanvasSize::default());
        let measured = canvas_size.read(cx).0;
        let (canvas_width, canvas_height) = if f32::from(measured.width) > 0. {
            (
                f32::from(measured.width) as f64,
                f32::from(measured.height) as f64,
            )
        } else {
            (1200.0, 800.0)
        };
        let demo_card = thread_details_card(window, cx, canvas_width, canvas_height);
        let demo_lane_card = demo_card.as_ref().map(|(layout, _)| ChatCanvasDetailsCard {
            left: layout.x,
            right: layout.x + layout.width,
            bottom: layout.y + layout.height,
        });
        let selection_toolbar = selection_toolbar_demo();
        let details_card = demo_card.map(|(_, card)| card).or_else(|| {
            self.thread_details_open
                .then(|| self.git_host.read(cx).details_card(cx))
                .flatten()
        });
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
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(measure_canvas(canvas_size))
                    .child(ChatCanvas::from_measurements(
                        ChatCanvasMeasurements {
                            width: canvas_width,
                            height: canvas_height,
                            ..Default::default()
                        },
                        demo_lane_card,
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
                                            .child(composer)
                                            // The strip hangs under the composer, joined by the glass seam.
                                            .children(composer_strip),
                                    )
                                    // Bottom spacer: `h-[calc(env(safe-area-inset-bottom)+1.25rem)]`.
                                    .child(div().h(rems(1.25))),
                            )
                            .into_any_element(),
                    ))
                    // `ThreadDetailsCard` (inline): top right of the canvas.
                    .children(details_card)
                    .children(selection_toolbar),
            )
    }
}

/// The canvas's measured size; a change re-renders the column.
#[derive(Default)]
struct CanvasSize(gpui_kit::Size<gpui_kit::Pixels>);

/// An invisible element that records the size of the box it is placed in.
fn measure_canvas(state: Entity<CanvasSize>) -> impl IntoElement {
    gpui_kit::canvas(
        move |bounds, _, cx| {
            if state.read(cx).0 != bounds.size {
                state.update(cx, |state, cx| {
                    state.0 = bounds.size;
                    cx.notify();
                });
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

fn thread_details_card(
    window: &mut Window,
    cx: &mut App,
    width: f64,
    height: f64,
) -> Option<(ThreadDetailsCardLayout, gpui_kit::AnyElement)> {
    let panel = ThreadDetailsPanel::from_demo(window, cx)?;
    let layout = resolve_thread_details_card_layout(ThreadDetailsCardLayoutInput {
        container: ThreadDetailsCardContainer { width, height },
        lane: ThreadDetailsCardLane {
            padding: 48.0,
            min_chat_width: 640.0,
        },
        top_inset: 0.0,
    })?;
    Some((
        layout,
        ThreadDetailsCard {
            layout,
            children: panel.into_any_element(),
        }
        .into_any_element(),
    ))
}

/// `TURIN_CHAT_DEMO=selection-toolbar`: the cite button over a made-up selection, until a GPUI
/// text selection in the assistant markdown reports captures.
fn selection_toolbar_demo() -> Option<gpui_kit::AnyElement> {
    use super::chat::assistant_selection_toolbar::AssistantSelectionToolbar;
    use super::chat::selection_actions::SelectionActionPoint;
    if !std::env::var("TURIN_CHAT_DEMO").is_ok_and(|demo| demo.contains("selection-toolbar")) {
        return None;
    }
    let long =
        std::env::var("TURIN_CHAT_DEMO").is_ok_and(|demo| demo.contains("selection-too-long"));
    Some(
        AssistantSelectionToolbar {
            citation_text: if long {
                "word ".repeat(2_000).into()
            } else {
                "The timeline keeps following the bottom".into()
            },
            position: SelectionActionPoint { x: 640.0, y: 360.0 },
            on_cite: std::rc::Rc::new(|_, _| true),
            on_dismiss: std::rc::Rc::new(|_, _| {}),
            on_clear_selection: std::rc::Rc::new(|_, _| {}),
        }
        .into_any_element(),
    )
}

/// The alerts above the lane, with their dismissals: the provider status banner remembers the
/// key it was dismissed under (a changed status shows again), the thread error banner the
/// session-wide `thread key + message`.
fn chat_overlay_banners(window: &mut Window, cx: &mut App) -> Vec<gpui_kit::AnyElement> {
    use std::rc::Rc;

    let Ok(demo) = std::env::var("TURIN_CHAT_DEMO") else {
        return Vec::new();
    };
    let mut banners = Vec::new();
    if demo.contains("provider-status") {
        let dismissed =
            window.use_keyed_state("provider-status-dismissed", cx, |_, _| None::<String>);
        let mut entry = ProviderInstanceEntry::new("codex-work", "codex", "Codex");
        entry.version = Some("1.0.0".into());
        entry.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Unsupported,
            message: Some("Unsupported version. Use 2.0.0.".into()),
        });
        if should_show_provider_status_banner(&entry, dismissed.read(cx).as_deref()) {
            let key = get_provider_status_banner_key(&entry);
            banners.push(
                ProviderStatusBanner::new(entry)
                    .on_dismiss(Rc::new(move |_, cx| {
                        dismissed.update(cx, |dismissed, cx| {
                            *dismissed = key.clone();
                            cx.notify();
                        });
                    }))
                    .into_any_element(),
            );
        }
    }
    let thread_error = |window: &mut Window,
                        cx: &mut App,
                        error: &'static str,
                        usage_limit: bool|
     -> Option<gpui_kit::AnyElement> {
        let tick = window.use_keyed_state("thread-error-dismissed", cx, |_, _| 0_u32);
        let _ = tick.read(cx);
        let key = get_thread_error_banner_key("demo:thread", Some(error));
        let dismissed = is_thread_error_banner_dismissed_for_session(key.as_deref());
        should_show_thread_error_banner("demo:thread", Some(error), dismissed).then(|| {
            ThreadErrorBanner::new(error, usage_limit)
                .on_dismiss(Rc::new(move |_, cx| {
                    dismiss_thread_error_banner_for_session(key.as_deref());
                    tick.update(cx, |tick, cx| {
                        *tick += 1;
                        cx.notify();
                    });
                }))
                .into_any_element()
        })
    };
    if demo.contains("thread-error") {
        banners.extend(thread_error(
            window,
            cx,
            "The first error line\ncontinues on a second line",
            false,
        ));
    }
    if demo.contains("usage-limit-error") {
        banners.extend(thread_error(
            window,
            cx,
            "Usage limit reached for this provider.",
            true,
        ));
    }
    banners
}
