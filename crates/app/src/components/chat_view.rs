//! Port of upstream/apps/web/src/components/ChatView.tsx (layout only).
//!
//! The chat column: the 52px drag-region header, the messages timeline
//! (empty state for now) and the composer docked at the bottom of the
//! `--chat-content-max-width` lane. Timeline, banners, find and the thread
//! details panel arrive with the thread view task; the terminal drawer is
//! cut.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div, px, rems,
};

use super::chat::chat_header::ChatHeader;
use super::chat::composer_surface::ComposerSurface;
use super::drag_region::DragRegion as _;
use super::sidebar::sidebar_chrome::WORKSPACE_TITLEBAR_CONTENT_LEFT;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{CHAT_CONTENT_MAX_WIDTH, TEXT_SM, WORKSPACE_TOPBAR_HEIGHT};

#[derive(IntoElement)]
pub struct ChatView {
    pub sidebar_open: bool,
    pub right_panel_open: bool,
}

impl RenderOnce for ChatView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        // `px-3 sm:px-5`; with the sidebar collapsed the title clears the
        // traffic lights and the fixed sidebar toggle.
        let header_left = if self.sidebar_open {
            px(20.)
        } else {
            px(WORKSPACE_TITLEBAR_CONTENT_LEFT)
        };
        // `.chat-composer-lane` / timeline `padding-inline: 3rem` at ≥640px.
        let lane_inset = rems(3.);

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
                    .child(ChatHeader {
                        project: "turin".into(),
                        title: "Port design system and app shell".into(),
                        right_panel_open: self.right_panel_open,
                    })
                    .drag_region(window, cx),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        // Empty server thread (MessagesTimeline.tsx).
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .px(lane_inset)
                            .text_size(TEXT_SM.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.3))
                            .child("Send a message to start the conversation."),
                    )
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
                                    .child(ComposerSurface),
                            )
                            // Bottom spacer: `h-[calc(env(safe-area-inset-bottom)+1.25rem)]`.
                            .child(div().h(rems(1.25))),
                    ),
            )
    }
}
