//! Port of upstream/apps/web/src/components/chat/ChatHeader.tsx and
//! WorkspaceBreadcrumb.tsx (layout only).
//!
//! Breadcrumb: project crumb (favicon + name, `max-w-40`) → "/" → thread
//! title. `text-sm font-medium`, non-current crumbs muted. The title menu,
//! rename and parent-thread crumb arrive with the thread view task.

use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div, rems,
};

use crate::components::project_monogram::ProjectMonogram;
use crate::theme::ActiveAppTheme;
use crate::theme::color::hex;
use crate::theme::tokens::TEXT_SM;

#[derive(IntoElement)]
pub struct ChatHeader {
    pub project: SharedString,
    pub title: SharedString,
    /// `pr-10` when the right panel owns the titlebar, else `pr-24` to clear
    /// the fixed layout controls.
    pub right_panel_open: bool,
}

impl RenderOnce for ChatHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
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
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_3()
                    .text_size(TEXT_SM.size)
                    .font_weight(FontWeight::MEDIUM)
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(rems(0.375))
                            .text_color(theme.muted_foreground)
                            .child(
                                ProjectMonogram::new("T", hex(0x155dfc).into()).size(rems(0.875)),
                            )
                            .child(div().max_w(rems(10.)).truncate().child(self.project)),
                    )
                    .child(div().text_color(theme.icon_muted).child("/"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(self.title),
                    ),
            )
    }
}
