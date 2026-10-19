//! Port of upstream/apps/web/src/components/chat/ChatHeader.tsx (layout only).
//!
//! Breadcrumb: project crumb (favicon `size-3.5` + name, `max-w-40`) → "/"
//! → thread title, via [`WorkspaceBreadcrumb`]. The title menu, rename and
//! parent-thread crumb arrive with the thread view task.

use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div, rems,
};

use std::sync::Arc;

use crate::components::project_favicon::ProjectFavicon;
use crate::components::project_monogram::ProjectMonogram;
use crate::components::workspace_breadcrumb::{
    WorkspaceBreadcrumb, WorkspaceBreadcrumbItem, WorkspaceBreadcrumbSeparator,
    workspace_breadcrumb_text,
};
use crate::project_icon_colors::project_icon_color;
use crate::project_identity::derive_project_identity;
use crate::state::models::EnvironmentProject;
use crate::theme::ActiveAppTheme as _;

#[derive(IntoElement)]
pub struct ChatHeader {
    pub project: SharedString,
    /// The thread's project, for its configured icon or favicon. Without it
    /// (fixture demos) the crumb shows the monogram generated from the name.
    pub project_record: Option<Arc<EnvironmentProject>>,
    pub title: SharedString,
    /// `pr-10` when the right panel owns the titlebar, else `pr-24` to clear
    /// the fixed layout controls.
    pub right_panel_open: bool,
    /// The env / branch strip, right after the breadcrumb. It shrinks (its
    /// branch name truncates) before the title does.
    pub trailing: Option<AnyElement>,
    /// Without a thread (a draft) only the trailing strip shows.
    pub show_breadcrumb: bool,
}

impl RenderOnce for ChatHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let icon = match &self.project_record {
            Some(project) => ProjectFavicon::new(project)
                .size(rems(0.875))
                .into_any_element(),
            None => {
                let identity = derive_project_identity(&self.project);
                ProjectMonogram::new(
                    identity.monogram,
                    project_icon_color(identity.color, cx.app_theme().is_dark()),
                )
                .size(rems(0.875))
                .into_any_element()
            }
        };
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
            .children(self.show_breadcrumb.then(|| {
                WorkspaceBreadcrumb::new()
                    .child(
                        // `className="shrink"`: the project crumb gives up width
                        // before the title does.
                        WorkspaceBreadcrumbItem::new(false).flex_shrink(1.).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .child(icon)
                                .child(workspace_breadcrumb_text(self.project).max_w(rems(10.))),
                        ),
                    )
                    .child(WorkspaceBreadcrumbSeparator)
                    .child(
                        // `min-w-10 flex-1`
                        WorkspaceBreadcrumbItem::new(true)
                            .min_w(rems(2.5))
                            .flex_1()
                            .child(workspace_breadcrumb_text(self.title)),
                    )
            }))
            .children(self.trailing.map(|trailing| {
                div()
                    .flex()
                    .min_w_0()
                    .flex_shrink(1.)
                    .justify_end()
                    .overflow_hidden()
                    .child(trailing)
            }))
    }
}
