//! Port of upstream/apps/web/src/components/ChatView.tsx (layout only).
//!
//! The chat column: the 52px drag-region header, the messages timeline and
//! the composer docked at the bottom of the `--chat-content-max-width` lane,
//! with pending approvals / questions attached above it. Find and the thread
//! details panel are not ported yet; the terminal drawer is cut.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, px, rems,
};

use super::chat::chat_canvas::{ChatCanvas, ChatCanvasMeasurements};
use super::chat::chat_canvas_layout::ChatCanvasDetailsCard;
use super::chat::chat_git_host::ChatGitHost;
use super::chat::chat_header::ChatHeader;
use super::chat::chat_media::chat_media_layer;
use super::chat::chat_overlay_banners::{ProviderStatusBanner, ThreadErrorBanner};
use super::chat::composer_pending_panels::{
    ComposerPendingApprovalBanner, ComposerPendingUserInputBanner,
};
use super::chat::live_chat::LiveChat;
use super::chat::live_lineage::{lineage_panel_snapshot, live_lineage_actions};
use super::chat::live_wiring::{live_overlay_banners, subagent_bar_from_state};
use super::chat::messages_timeline::MessagesTimeline;
use super::chat::provider_status_banner::{
    get_provider_status_banner_key, should_show_provider_status_banner,
};
use super::chat::provider_subagent_bar::ProviderSubagentBar;
use super::chat::thread_details_card::{ThreadDetailsCard, ThreadDetailsCardMeasurements};
use super::chat::thread_details_card_layout::{
    ThreadDetailsCardContainer, ThreadDetailsCardLane, ThreadDetailsCardLayout,
    ThreadDetailsCardLayoutInput, resolve_thread_details_card_layout,
};
use super::chat::thread_details_panel::ThreadDetailsPanel;
use super::chat::thread_details_pr_row::OpenHandler;
use super::chat::thread_error_banner::{
    dismiss_thread_error_banner_for_session, get_thread_error_banner_key,
    is_thread_error_banner_dismissed_for_session, should_show_thread_error_banner,
};
use super::chat::thread_relationships_panel::ThreadRelationshipsPanel;
use super::chat::timeline::session_logic::{derive_pending_approvals, derive_pending_user_inputs};
use super::chat::timeline::source::MockTimelineSource;
use super::chat::timeline_page_scroll::TimelinePageScrollSession;
use super::drag_region::DragRegion as _;
use super::workspace_titlebar::workspace_titlebar_content_left;
use crate::app_runtime::write::bus::CommandBus;
use crate::composer::catalog::{
    CompatibilityAdvisory, CompatibilityAdvisoryStatus, ProviderInstanceEntry,
};
use crate::composer::live_catalog::provider_entries;
use crate::composer::mock_source::MockComposerSource;
use crate::composer::views::chat_composer::ChatComposer;
use crate::state::store::ClientStore;
use crate::state::thread_execution::ProviderSubagentStatus;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{CHAT_CONTENT_MAX_WIDTH, WORKSPACE_TOPBAR_HEIGHT};
use turin_contracts::ThreadId;

/// Binds the chat column to the live client store instead of the fixtures.
#[derive(Clone)]
pub struct LiveChatTarget {
    pub client: Entity<ClientStore>,
    pub bus: CommandBus,
    /// The routed thread; `None` shows the empty state.
    pub thread: Option<ThreadId>,
}

#[derive(IntoElement)]
pub struct ChatView {
    /// `Some` in the running app, `None` for the fixture-driven demos.
    pub live: Option<LiveChatTarget>,
    pub sidebar_open: bool,
    pub right_panel_open: bool,
    /// The routed thread's `(project, title)` (sidebar selection).
    pub thread: Option<(SharedString, SharedString)>,
    /// The branch toolbar and thread details card (`BranchToolbar`,
    /// `GitActionsControl`), hosted with the state upstream's ChatView owns.
    pub git_host: Entity<ChatGitHost>,
    /// The thread details card is shown (the titlebar toggle).
    pub thread_details_open: bool,
    /// A provider-native subagent thread: the bar stands in for the composer. The owner of the
    /// live thread data (T22b) fills this from the thread's projection.
    pub subagent_bar: Option<SubagentBarData>,
}

/// What `ProviderSubagentBar` needs from the routed subagent thread.
#[derive(Clone)]
pub struct SubagentBarData {
    pub provider: Option<ProviderInstanceEntry>,
    pub model_label: SharedString,
    pub effort_label: Option<SharedString>,
    pub status: Option<ProviderSubagentStatus>,
    /// The parent thread the bar leads back to (`onOpenParent`).
    pub parent_thread_id: Option<String>,
}

impl ChatView {
    /// No thread routed: the header bar and a prompt to pick one.
    fn render_empty_state(
        &self,
        header_left: gpui_kit::Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let theme = cx.app_theme().clone();
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
                    .drag_region(window, cx),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child("Select a thread, or start a new one from a project."),
            )
    }
}

impl RenderOnce for ChatView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.render_column(window, cx)
    }
}

impl ChatView {
    fn render_column(self, window: &mut Window, cx: &mut App) -> gpui_kit::AnyElement {
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
        let live_target = self.live.clone();
        let (timeline, composer, page_scroll) = match &self.live {
            Some(LiveChatTarget { thread: None, .. }) => {
                return self
                    .render_empty_state(header_left, window, cx)
                    .into_any_element();
            }
            Some(LiveChatTarget {
                client,
                bus,
                thread: Some(thread_id),
            }) => {
                let chat = window.use_keyed_state(
                    SharedString::from(format!("live-chat-{thread_id}")),
                    cx,
                    |_, cx| LiveChat::new(thread_id.clone(), client, bus.clone(), cx),
                );
                let (timeline_store, composer_store) = {
                    let chat = chat.read(cx);
                    (chat.timeline.clone(), chat.composer.clone())
                };
                let page_scroll = window.use_keyed_state(
                    SharedString::from(format!("timeline-page-scroll-{thread_id}")),
                    cx,
                    |_, _| TimelinePageScrollSession::default(),
                );
                let scroll_for_timeline = page_scroll.clone();
                let timeline = window.use_keyed_state(
                    SharedString::from(format!("messages-timeline-{thread_id}")),
                    cx,
                    |window, cx| MessagesTimeline::new(timeline_store, window, cx),
                );
                let scroll_for_composer = page_scroll.clone();
                let timeline_for_scroll = timeline.clone();
                let composer = window.use_keyed_state(
                    SharedString::from(format!("chat-composer-{thread_id}")),
                    cx,
                    |window, cx| {
                        let mut composer = ChatComposer::new(composer_store, window, cx);
                        composer.bind_page_scroll(
                            scroll_for_composer,
                            timeline_for_scroll,
                            window,
                            cx,
                        );
                        composer
                    },
                );
                let _ = scroll_for_timeline;
                (timeline, composer, Some(page_scroll))
            }
            None => {
                let timeline = window.use_keyed_state("messages-timeline", cx, |window, cx| {
                    let store = MockTimelineSource::start(cx);
                    MessagesTimeline::new(store, window, cx)
                });
                let composer = window.use_keyed_state("chat-composer", cx, |window, cx| {
                    let store = MockComposerSource::start(cx);
                    ChatComposer::new(store, window, cx)
                });
                (timeline, composer, None)
            }
        };
        // The composer's top drawer: the oldest pending approval, else the
        // oldest pending question (ChatComposer.tsx `showComposerTopDrawer`).
        let store = timeline.read(cx).store().clone();
        let snapshot = store.read(cx).snapshot().clone();
        let source = store.read(cx).source();
        let subagent_bar_data = live_target
            .as_ref()
            .and_then(|live| {
                let thread_id = live.thread.as_ref()?;
                let state = live.client.read(cx).state();
                subagent_bar_from_state(state, &live.bus, thread_id)
            })
            .or_else(|| live_target.is_none().then(subagent_bar_demo).flatten())
            .or_else(|| {
                live_target
                    .is_none()
                    .then(|| self.subagent_bar.clone())
                    .flatten()
            });
        let subagent_bar = subagent_bar_data.map(|data| {
            let open_parent = data.parent_thread_id.clone().zip(source.clone()).map(
                |(parent, source)| -> OpenHandler {
                    Rc::new(move |_, _, cx| source.open_thread(&parent, cx))
                },
            );
            ProviderSubagentBar {
                provider: data.provider,
                model_label: data.model_label,
                effort_label: data.effort_label,
                status: data.status,
                on_open_parent: open_parent,
            }
        });
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
            inputs.first().cloned().map(|prompt| {
                ComposerPendingUserInputBanner {
                    prompt,
                    source,
                    environment_id: live_target
                        .as_ref()
                        .map(|_| crate::state::local_environment_id().to_string()),
                    thread_id: live_target
                        .as_ref()
                        .and_then(|live| live.thread.as_ref())
                        .map(|id| id.to_string()),
                }
                .into_any_element()
            })
        };

        let mut overlay_banners = live_target
            .as_ref()
            .and_then(|live| {
                let thread_id = live.thread.as_ref()?;
                Some(live_overlay_banners(
                    &live.client,
                    &live.bus,
                    thread_id,
                    window,
                    cx,
                ))
            })
            .unwrap_or_default();
        if live_target.is_none() {
            overlay_banners.extend(chat_overlay_banners(window, cx));
        }
        if let (Some(page_scroll), Some(live)) = (&page_scroll, &live_target) {
            if page_scroll.read(cx).needs_frame() {
                let metrics = timeline.read(cx).page_scroll_metrics(window);
                if let Some(delta) = page_scroll.update(cx, |session, _| session.frame(metrics)) {
                    timeline.update(cx, |timeline, cx| {
                        timeline.apply_page_scroll_delta(delta, cx);
                    });
                }
                window.request_animation_frame();
            } else if page_scroll.update(cx, |session, _| session.take_notify()) {
                window.request_animation_frame();
            }
            let _ = live;
        }
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
        let demo_card = live_target
            .is_none()
            .then(|| thread_details_card(window, cx, canvas_width, canvas_height))
            .flatten();
        // `reportDetailsCard`: the card's left edge and, once measured, where its content ends.
        let demo_lane_card = demo_card
            .as_ref()
            .map(|(layout, bottom, _)| ChatCanvasDetailsCard {
                left: layout.x,
                right: layout.x + layout.width,
                bottom: bottom.unwrap_or(layout.y + layout.height),
            });
        // The real card sits 12px from the top right, 280px wide (`THREAD_DETAILS_CARD_*`), when
        // the layout places it inline.
        let demo_lane_card = demo_lane_card.or_else(|| {
            let layout = resolve_thread_details_card_layout(ThreadDetailsCardLayoutInput {
                container: ThreadDetailsCardContainer {
                    width: canvas_width,
                    height: canvas_height,
                },
                lane: ThreadDetailsCardLane {
                    padding: 48.0,
                    min_chat_width: 640.0,
                },
                top_inset: 0.0,
            })?;
            self.thread_details_open.then_some(ChatCanvasDetailsCard {
                left: layout.x,
                right: layout.x + layout.width,
                bottom: layout.y + layout.height,
            })
        });
        let selection_toolbar = if live_target.is_some() {
            live_selection_toolbar(composer.clone(), canvas_width, canvas_height, window, cx)
        } else {
            selection_toolbar_demo()
        };
        let live_lineage_panel = live_target.as_ref().and_then(|live| {
            if !self.thread_details_open {
                return None;
            }
            let thread_id = live.thread.as_ref()?;
            let state = live.client.read(cx).state();
            let providers = provider_entries(&live.bus.providers().borrow());
            let driver = state.thread(thread_id).and_then(|entry| {
                let projection = entry.state.data.as_ref()?;
                let instance_id = projection.thread.model_selection.instance_id.as_str();
                providers
                    .iter()
                    .find(|entry| entry.instance_id == instance_id)
                    .map(|entry| entry.driver_kind.clone())
            });
            let snapshot = lineage_panel_snapshot(state, thread_id, driver)?;
            let actions =
                live_lineage_actions(live.client.clone(), live.bus.clone(), thread_id.clone());
            let panel = window.use_keyed_state(
                format!("live-thread-lineage:{}", thread_id.as_str()),
                cx,
                |_, _| ThreadRelationshipsPanel::new(snapshot.clone()).actions(actions.clone()),
            );
            panel.update(cx, |panel, cx| panel.reload(snapshot, cx));
            Some(panel)
        });
        let details_card = demo_card.map(|(_, _, card)| card).or_else(|| {
            if !self.thread_details_open {
                return None;
            }
            if live_target.is_some() {
                self.git_host
                    .read(cx)
                    .details_card_with_lineage(live_lineage_panel, cx)
            } else {
                self.git_host.read(cx).details_card(cx)
            }
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
                            .child(timeline.clone())
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
                                            .children(
                                                subagent_bar.is_none().then_some(drawer).flatten(),
                                            )
                                            // The provider runs a subagent thread, so there is
                                            // nothing to send: the bar stands in for the composer.
                                            .map(|this| match subagent_bar {
                                                Some(bar) => this.child(bar),
                                                None => this
                                                    .child(composer)
                                                    // The strip hangs under the composer, joined by the glass seam.
                                                    .children(composer_strip),
                                            }),
                                    )
                                    // Bottom spacer: `h-[calc(env(safe-area-inset-bottom)+1.25rem)]`.
                                    .child(div().h(rems(1.25))),
                            )
                            .into_any_element(),
                    ))
                    // `ThreadDetailsCard` (inline): top right of the canvas.
                    .children(details_card)
                    .child(chat_media_layer(timeline.clone(), window, cx))
                    .children(selection_toolbar),
            )
            .into_any_element()
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
) -> Option<(ThreadDetailsCardLayout, Option<f64>, gpui_kit::AnyElement)> {
    let panel = Rc::new(ThreadDetailsPanel::from_demo(window, cx)?);
    let layout = resolve_thread_details_card_layout(ThreadDetailsCardLayoutInput {
        container: ThreadDetailsCardContainer { width, height },
        lane: ThreadDetailsCardLane {
            padding: 48.0,
            min_chat_width: 640.0,
        },
        top_inset: 0.0,
    })?;
    let measurements = window.use_keyed_state("thread-details-card-measurements", cx, |_, _| {
        ThreadDetailsCardMeasurements::default()
    });
    let measurement_key = format!("demo:thread:{}", layout.width);
    let bottom = measurements.read(cx).card_bottom(&measurement_key, &layout);
    Some((
        layout,
        bottom,
        ThreadDetailsCard {
            layout,
            measurement_key,
            measurements,
            content: Rc::new(move |density, window, cx| panel.render_density(density, window, cx)),
        }
        .into_any_element(),
    ))
}

/// `TURIN_CHAT_DEMO=subagent-bar`: the bar over a running Codex subagent.
pub fn subagent_bar_demo() -> Option<SubagentBarData> {
    use crate::state::contracts::OrchestrationV2ExecutionNodeStatus;
    if !std::env::var("TURIN_CHAT_DEMO").is_ok_and(|demo| demo.contains("subagent-bar")) {
        return None;
    }
    let started = chrono::Utc::now() - chrono::Duration::seconds(42);
    Some(SubagentBarData {
        provider: Some(ProviderInstanceEntry::new("codex", "codex", "Codex")),
        model_label: "GPT-5.4".into(),
        effort_label: Some("High".into()),
        status: Some(ProviderSubagentStatus {
            status: OrchestrationV2ExecutionNodeStatus::Running,
            started_at: Some(started.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            completed_at: None,
        }),
        parent_thread_id: Some("thread-parent".into()),
    })
}

/// `TURIN_CHAT_DEMO=selection-toolbar`: the cite button over a made-up selection, until a GPUI
/// text selection in the assistant markdown reports captures.
fn live_selection_toolbar(
    composer: gpui_kit::Entity<crate::composer::views::chat_composer::ChatComposer>,
    canvas_width: f64,
    canvas_height: f64,
    _window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) -> Option<gpui_kit::AnyElement> {
    use super::chat::assistant_selection_state::{
        assistant_citation_from_capture, assistant_selection_capture, clear_assistant_selection,
        selection_toolbar_point,
    };
    use super::chat::assistant_selection_toolbar::AssistantSelectionToolbar;
    use crate::state::models::parse_scoped_thread_key;

    let capture = assistant_selection_capture(cx).lock().ok()?.clone();
    if capture.selected_text.is_empty() {
        return None;
    }
    let position = selection_toolbar_point(&capture, canvas_width, canvas_height)?;
    let (environment_id, thread_id) = parse_scoped_thread_key(composer.read(cx).thread_key())?;
    let citation =
        assistant_citation_from_capture(&capture, environment_id.as_str(), thread_id.as_str())?;
    let composer = composer.clone();
    let composer_focus = composer.read(cx).focus_handle(cx);
    Some(
        AssistantSelectionToolbar {
            citation_text: capture.selected_text.clone().into(),
            position,
            on_cite: std::rc::Rc::new(move |window, cx| {
                composer.update(cx, |composer, cx| {
                    composer.insert_assistant_citation_from_selection(citation.clone(), window, cx);
                });
                clear_assistant_selection(cx);
                window.focus(&composer_focus, cx);
                true
            }),
            on_dismiss: std::rc::Rc::new(|_, cx| clear_assistant_selection(cx)),
            on_clear_selection: std::rc::Rc::new(|_, cx| clear_assistant_selection(cx)),
        }
        .into_any_element(),
    )
}

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
