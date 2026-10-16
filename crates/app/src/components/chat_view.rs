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

use super::chat::chat_git_host::ChatGitHost;
use super::chat::chat_header::ChatHeader;
use super::chat::chat_media::chat_media_layer;
use super::chat::composer_pending_panels::{
    ComposerPendingApprovalBanner, ComposerPendingUserInputBanner,
};
use super::chat::live_chat::LiveChat;
use super::chat::messages_timeline::MessagesTimeline;
use super::chat::timeline::session_logic::{derive_pending_approvals, derive_pending_user_inputs};
use super::chat::timeline::source::MockTimelineSource;
use super::drag_region::DragRegion as _;
use super::workspace_titlebar::workspace_titlebar_content_left;
use crate::app_runtime::write::bus::CommandBus;
use crate::composer::mock_source::MockComposerSource;
use crate::composer::views::chat_composer::ChatComposer;
use crate::state::store::ClientStore;
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
        let (timeline, composer) = match &self.live {
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
                let timeline = window.use_keyed_state(
                    SharedString::from(format!("messages-timeline-{thread_id}")),
                    cx,
                    |window, cx| MessagesTimeline::new(timeline_store, window, cx),
                );
                let composer = window.use_keyed_state(
                    SharedString::from(format!("chat-composer-{thread_id}")),
                    cx,
                    |window, cx| ChatComposer::new(composer_store, window, cx),
                );
                (timeline, composer)
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
                (timeline, composer)
            }
        };
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

        let composer_strip = self.git_host.read(cx).composer_strip();
        let details_card = self
            .thread_details_open
            .then(|| self.git_host.read(cx).details_card(cx))
            .flatten();
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
                    // `MessagesTimeline`, owned by the chat column for now; the
                    // thread route will own it once threads are selectable.
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
                                    .children(drawer)
                                    .child(composer)
                                    // The strip hangs under the composer, joined by the glass seam.
                                    .children(composer_strip),
                            )
                            // Bottom spacer: `h-[calc(env(safe-area-inset-bottom)+1.25rem)]`.
                            .child(div().h(rems(1.25))),
                    )
                    // `ThreadDetailsCard` (inline): top right of the canvas.
                    .children(details_card)
                    .child(chat_media_layer(timeline, window, cx)),
            )
            .into_any_element()
    }
}
