//! Port of upstream/apps/web/src/components/sidebar/SidebarThreadHeader.tsx
//!
//! The search field and the project-scope / add-project / new-thread icon
//! buttons above the thread list. Static for now: the field is a styled
//! placeholder, not an input.

use gpui_kit::{
    App, CursorStyle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, Styled as _, Window, div, rems,
};

use super::sidebar_chrome::sidebar_icon_button;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM};
use crate::ui::{Icon, IconName};

#[derive(IntoElement)]
pub struct SidebarThreadHeader {
    pub has_projects: bool,
}

impl RenderOnce for SidebarThreadHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut actions = div().flex().flex_shrink_0().items_center();
        if self.has_projects {
            actions = actions
                .child(sidebar_icon_button(
                    "sidebar-project-scope".into(),
                    IconName::ListFilter,
                    rems(1.75),
                    "Filter by project".into(),
                    cx,
                ))
                .child(sidebar_icon_button(
                    "sidebar-add-project".into(),
                    IconName::FolderPlus,
                    rems(1.75),
                    "Add project".into(),
                    cx,
                ));
        }
        actions = actions.child(sidebar_icon_button(
            "sidebar-new-thread".into(),
            IconName::SquarePen,
            rems(1.75),
            "New thread".into(),
            cx,
        ));

        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .id("sidebar-search")
                    .flex()
                    .h_8()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .px_2()
                    .py(rems(0.375))
                    .text_size(TEXT_SM.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.sidebar_muted_foreground)
                    .cursor(CursorStyle::IBeam)
                    .hover(|style| {
                        style
                            .bg(theme.sidebar_row_hover)
                            .text_color(theme.sidebar_foreground)
                    })
                    .child(Icon::new(IconName::Search).color(theme.sidebar_icon_color))
                    .child("Search"),
            )
            .child(actions)
    }
}
