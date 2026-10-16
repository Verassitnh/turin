//! Port of upstream/apps/web/src/components/NoActiveThreadState.tsx
//! Also ports upstream/apps/web/src/components/WorkspacePageHeader.tsx (folded in:
//! `WORKSPACE_TOPBAR_HEIGHT` in `theme/tokens.rs`, the titlebar inset in
//! `components/workspace_titlebar.rs` and `components/drag_region.rs`).
//!
//! The empty state shown when no thread is selected: a `WorkspacePageHeader`
//! reading "No active thread" over the `Empty` card "Pick a thread to
//! continue". Upstream's `isElectron` branch (the drag-region header with
//! `text-xs text-muted-foreground/50`) is cut with the Electron shell; the
//! native window keeps the web branch, and the header is a window drag region
//! as in the other pages (see PORTING.md Deviations).

#![allow(dead_code)] // wired by the app shell (T22).

use gpui_kit::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, Styled,
    Window, div, rems,
};

use crate::components::drag_region::DragRegion as _;
use crate::components::workspace_titlebar::workspace_titlebar_content_left;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_SM, WORKSPACE_GUTTER, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::empty::{Empty, empty_description, empty_header, empty_title};
use crate::ui::sidebar::SidebarInset;

/// The `md` breakpoint (768px) that switches the label from `text-foreground`
/// to `text-muted-foreground/60`.
const MD_BREAKPOINT: f32 = 768.;

/// Copy shown under the header.
pub const NO_ACTIVE_THREAD_LABEL: &str = "No active thread";
pub const NO_ACTIVE_THREAD_TITLE: &str = "Pick a thread to continue";
pub const NO_ACTIVE_THREAD_DESCRIPTION: &str =
    "Select an existing thread or create a new one to get started.";

#[derive(IntoElement, Default)]
pub struct NoActiveThreadState {
    sidebar_collapsed: bool,
}

impl NoActiveThreadState {
    pub fn new() -> Self {
        Self::default()
    }

    /// `[[data-sidebar-state=collapsed]_&]:pl-[var(--workspace-titlebar-content-left)]`.
    pub fn sidebar_collapsed(mut self, collapsed: bool) -> Self {
        self.sidebar_collapsed = collapsed;
        self
    }
}

impl RenderOnce for NoActiveThreadState {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let gutter = WORKSPACE_GUTTER.to_pixels(window.rem_size());
        let start = if self.sidebar_collapsed {
            workspace_titlebar_content_left(window)
        } else {
            gutter
        };
        let label_color = if f32::from(window.viewport_size().width) >= MD_BREAKPOINT {
            with_alpha(theme.muted_foreground, 0.6).into()
        } else {
            theme.foreground
        };

        // `WorkspacePageHeader className="border-b border-border"`.
        let header = div()
            .id("no-active-thread-header")
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .h(WORKSPACE_TOPBAR_HEIGHT)
            .min_h(WORKSPACE_TOPBAR_HEIGHT)
            .max_h(WORKSPACE_TOPBAR_HEIGHT)
            .pl(start)
            .pr(gutter)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().flex().items_center().gap_2().child(
                    div()
                        .text_size(TEXT_SM.size)
                        .line_height(TEXT_SM.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(label_color)
                        .child(NO_ACTIVE_THREAD_LABEL),
                ),
            )
            .drag_region(window, cx);

        // `<Empty className="flex-1">` around `w-full max-w-lg px-8 py-12`.
        let empty = Empty::new().child(
            div().w_full().max_w(rems(32.)).px_8().py(rems(3.)).child(
                empty_header()
                    .max_w_full()
                    .child(empty_title(NO_ACTIVE_THREAD_TITLE, false))
                    .child(empty_description(NO_ACTIVE_THREAD_DESCRIPTION, cx)),
            ),
        );

        // `SidebarInset className="h-dvh min-h-0 overflow-hidden overscroll-y-none"`.
        SidebarInset::new()
            .with(|this| this.h_full().min_h_0().overflow_hidden())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .bg(theme.background)
                    .child(header)
                    .child(empty),
            )
    }
}
