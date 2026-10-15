//! Port of upstream/apps/web/src/components/chat/ChatHeader.tsx (layout only).
//!
//! Breadcrumb: project crumb (favicon `size-3.5` + name, `max-w-40`) → "/"
//! → thread title, via [`WorkspaceBreadcrumb`]. The title menu, rename and
//! parent-thread crumb arrive with the thread view task.

use gpui_kit::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, rems,
};

use crate::components::project_monogram::ProjectMonogram;
use crate::components::workspace_breadcrumb::{
    WorkspaceBreadcrumb, WorkspaceBreadcrumbItem, WorkspaceBreadcrumbSeparator,
    workspace_breadcrumb_text,
};
use crate::theme::color::hex;

#[derive(IntoElement)]
pub struct ChatHeader {
    pub project: SharedString,
    pub title: SharedString,
    /// `pr-10` when the right panel owns the titlebar, else `pr-24` to clear
    /// the fixed layout controls.
    pub right_panel_open: bool,
}

impl RenderOnce for ChatHeader {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .flex()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap_3()
            .pr(if self.right_panel_open {
                rems(2.5)
            } else {
                rems(6.)
            })
            .child(
                WorkspaceBreadcrumb::new()
                    .child(
                        WorkspaceBreadcrumbItem::new(false).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .child(
                                    ProjectMonogram::new("T", hex(0x155dfc).into())
                                        .size(rems(0.875)),
                                )
                                .child(workspace_breadcrumb_text(self.project).max_w(rems(10.))),
                        ),
                    )
                    .child(WorkspaceBreadcrumbSeparator)
                    .child(
                        WorkspaceBreadcrumbItem::new(true)
                            .child(workspace_breadcrumb_text(self.title)),
                    ),
            )
    }
}
