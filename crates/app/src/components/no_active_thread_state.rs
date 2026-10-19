//! Port of upstream/apps/web/src/components/NoActiveThreadState.tsx
//! Also: Port of upstream/apps/web/src/components/WorkspacePageHeader.tsx (folded in:
//! `WORKSPACE_TOPBAR_HEIGHT` in `theme/tokens.rs`, the titlebar inset in
//! `components/workspace_titlebar.rs` and `components/drag_region.rs`).
//!
//! What the main column shows when no thread is open: the workspace top bar with a "No active
//! thread" label over a centred prompt to pick or create one.

use gpui_kit::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div,
};

use super::drag_region::DragRegion as _;
use super::workspace_titlebar::workspace_titlebar_content_left;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::empty::{Empty, empty_description, empty_header, empty_title};

pub const NO_ACTIVE_THREAD_LABEL: &str = "No active thread";
pub const NO_ACTIVE_THREAD_TITLE: &str = "Pick a thread to continue";
pub const NO_ACTIVE_THREAD_DESCRIPTION: &str =
    "Select an existing thread or create a new one to get started.";

#[derive(IntoElement)]
pub struct NoActiveThreadState {
    pub sidebar_open: bool,
}

impl RenderOnce for NoActiveThreadState {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        // The header's `pl-(--workspace-gutter-start)`: the collapsed sidebar's titlebar inset.
        let header_left = if self.sidebar_open {
            gpui_kit::px(20.)
        } else {
            workspace_titlebar_content_left(window)
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.background)
            .child(
                div()
                    .id("no-active-thread-header")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .h(WORKSPACE_TOPBAR_HEIGHT)
                    .pl(header_left)
                    .pr(gpui_kit::px(20.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(with_alpha(theme.muted_foreground, 0.5))
                            .child(NO_ACTIVE_THREAD_LABEL),
                    )
                    .drag_region(window, cx),
            )
            .child(
                Empty::new().child(
                    div()
                        .w_full()
                        .max_w(gpui_kit::rems(32.))
                        .px_8()
                        .py_12()
                        .child(
                            empty_header()
                                .max_w_full()
                                .child(empty_title(NO_ACTIVE_THREAD_TITLE, false))
                                .child(empty_description(NO_ACTIVE_THREAD_DESCRIPTION, cx)),
                        ),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_upstreams_copy() {
        assert_eq!(NO_ACTIVE_THREAD_LABEL, "No active thread");
        assert_eq!(NO_ACTIVE_THREAD_TITLE, "Pick a thread to continue");
        assert_eq!(
            NO_ACTIVE_THREAD_DESCRIPTION,
            "Select an existing thread or create a new one to get started."
        );
    }
}
