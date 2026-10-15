//! Port of upstream/apps/web/src/components/sidebar/SidebarChrome.tsx
//!
//! `SidebarChromeHeader` (the 52px titlebar row with the brand) and
//! `SidebarChromeFooter` (the utility icon row). The brand width probe, the
//! environment badge, stage artwork, update pill and undo notice are not
//! ported yet.

use gpui_kit::{
    App, CursorStyle, ElementId, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems, svg,
};

use crate::assets::T3_WORDMARK;
use crate::components::drag_region::{DragRegion as _, no_drag};
use crate::components::workspace_titlebar::workspace_titlebar_content_left;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{
    CONTROL_RADIUS, SIDEBAR_CONTENT_INSET, TEXT_SM, WORKSPACE_TOPBAR_HEIGHT,
};
use crate::ui::{Icon, IconName, Tooltip};

#[derive(IntoElement)]
pub struct SidebarChromeHeader;

impl RenderOnce for SidebarChromeHeader {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        // `h-[1cap]` of the 14px brand text.
        let cap = 10.;
        let wordmark_width = cap * 94.3941 / 56.96;
        div()
            .id("sidebar-chrome-header")
            .relative()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .h(WORKSPACE_TOPBAR_HEIGHT)
            .pr_3()
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_8()
                    .items_center()
                    .gap_x_2()
                    .overflow_hidden()
                    .child(
                        div()
                            .ml(workspace_titlebar_content_left(window))
                            .flex()
                            .h_7()
                            .items_center()
                            .rounded(CONTROL_RADIUS)
                            .text_color(theme.foreground)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(
                                        svg()
                                            .path(T3_WORDMARK)
                                            .w(px(wordmark_width))
                                            .h(px(cap))
                                            .text_color(theme.foreground),
                                    )
                                    .child(div().text_color(theme.muted_foreground).child("Code")),
                            ),
                    ),
            )
            .drag_region(window, cx)
    }
}

/// `SidebarMenuButton size="icon"` in the footer: 32×32, `rounded-[var(--control-radius)]`,
/// icon `size-4` in `--sidebar-icon-color`, hover `bg-sidebar-row-hover`.
#[derive(IntoElement)]
pub struct SidebarUtilityButton {
    id: ElementId,
    icon: IconName,
    label: SharedString,
}

impl SidebarUtilityButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
        }
    }
}

impl RenderOnce for SidebarUtilityButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        sidebar_icon_button(self.id, self.icon, rems(2.), self.label, cx)
    }
}

/// Shared look of the sidebar's icon buttons (header and footer).
pub fn sidebar_icon_button(
    id: ElementId,
    icon: IconName,
    size: gpui_kit::Rems,
    label: SharedString,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.app_theme();
    let group: SharedString = format!("sidebar-icon-{id:?}").into();
    let tooltip = Tooltip::new(label);
    div()
        .id(id)
        .on_mouse_down(MouseButton::Left, no_drag)
        .group(group.clone())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(CONTROL_RADIUS)
        .cursor(CursorStyle::PointingHand)
        .hover(|style| style.bg(theme.sidebar_row_hover))
        .active(|style| style.bg(theme.sidebar_row_active))
        .child(
            Icon::new(icon)
                .color(theme.sidebar_icon_color)
                .group_hover_color(group, theme.sidebar_foreground),
        )
        .tooltip(move |window, cx| tooltip.clone().build(window, cx))
}

#[derive(IntoElement)]
pub struct SidebarChromeFooter;

impl RenderOnce for SidebarChromeFooter {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px(SIDEBAR_CONTENT_INSET)
            .py_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(SidebarUtilityButton::new(
                        "sidebar-settings",
                        IconName::Settings,
                        "Settings",
                    ))
                    .child(SidebarUtilityButton::new(
                        "sidebar-pull-requests",
                        IconName::GitPullRequestArrow,
                        "Pull Requests",
                    ))
                    .child(SidebarUtilityButton::new(
                        "sidebar-usage",
                        IconName::ChartNoAxesColumn,
                        "Usage",
                    )),
            )
    }
}
