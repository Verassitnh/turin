//! Port of upstream/apps/web/src/components/WorkspacePageContainer.tsx and
//! WorkspacePageHeader.tsx.
//!
//! Container: `mx-auto flex w-full flex-col gap-6 px-6 pt-6 pb-12` at the
//! desktop gutter, capped at `max-w-4xl` / `5xl` / `6xl`.
//! Header: exactly `--workspace-topbar-height` (52px), `gap-3`, padded by
//! `--workspace-gutter`. A collapsed sidebar pads the start to
//! `--workspace-titlebar-content-left`. `electron` makes the bar a drag region.

use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, RenderOnce, Styled, Window,
    div, rems,
};

use crate::components::drag_region::DragRegion as _;
use crate::components::workspace_titlebar::workspace_titlebar_content_left;
use crate::theme::tokens::{WORKSPACE_GUTTER, WORKSPACE_TOPBAR_HEIGHT};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspacePageWidth {
    #[default]
    Readable,
    Wide,
    Expanded,
}

fn max_width(width: WorkspacePageWidth) -> f32 {
    match width {
        WorkspacePageWidth::Readable => 56.,
        WorkspacePageWidth::Wide => 64.,
        WorkspacePageWidth::Expanded => 72.,
    }
}

#[derive(IntoElement)]
pub struct WorkspacePageContainer {
    width: WorkspacePageWidth,
    children: Vec<AnyElement>,
}

impl WorkspacePageContainer {
    pub fn new() -> Self {
        Self {
            width: WorkspacePageWidth::Readable,
            children: Vec::new(),
        }
    }

    pub fn width(mut self, width: WorkspacePageWidth) -> Self {
        self.width = width;
        self
    }
}

impl Default for WorkspacePageContainer {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for WorkspacePageContainer {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for WorkspacePageContainer {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .mx_auto()
            .flex()
            .flex_col()
            .w_full()
            .gap_6()
            .px(rems(1.5))
            .pt(rems(1.5))
            .pb(rems(3.))
            .max_w(rems(max_width(self.width)))
            .children(self.children)
    }
}

#[derive(IntoElement)]
pub struct WorkspacePageHeader {
    drag: bool,
    sidebar_collapsed: bool,
    children: Vec<AnyElement>,
}

impl WorkspacePageHeader {
    pub fn new() -> Self {
        Self {
            drag: false,
            sidebar_collapsed: false,
            children: Vec::new(),
        }
    }

    /// Upstream `electron`: the bar is a window drag region.
    pub fn drag(mut self, drag: bool) -> Self {
        self.drag = drag;
        self
    }

    /// `[[data-sidebar-state=collapsed]_&]:pl-[var(--workspace-titlebar-content-left)]`.
    pub fn sidebar_collapsed(mut self, collapsed: bool) -> Self {
        self.sidebar_collapsed = collapsed;
        self
    }
}

impl Default for WorkspacePageHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for WorkspacePageHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for WorkspacePageHeader {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let start = if self.sidebar_collapsed {
            workspace_titlebar_content_left(window)
        } else {
            WORKSPACE_GUTTER.to_pixels(window.rem_size())
        };
        let mut header = div()
            .id("workspace-page-header")
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .h(WORKSPACE_TOPBAR_HEIGHT)
            .min_h(WORKSPACE_TOPBAR_HEIGHT)
            .max_h(WORKSPACE_TOPBAR_HEIGHT)
            .pl(start)
            .pr(WORKSPACE_GUTTER.to_pixels(window.rem_size()))
            .children(self.children);
        if self.drag {
            header = header.drag_region(window, cx);
        }
        header
    }
}
