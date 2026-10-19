//! Port of upstream/apps/web/src/components/sidebar/SidebarChrome.tsx
//!
//! `SidebarChromeHeader` (the 52px titlebar row with the brand) and
//! `SidebarChromeFooter` (the utility icon row, on `ui::sidebar` primitives). The brand width probe, the
//! environment badge, stage artwork, update pill and undo notice are not
//! ported yet.

use gpui_kit::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, WeakEntity, Window, div, rems, svg,
};

use super::SidebarEvent;
use super::sidebar_provider_update_pill::SidebarProviderUpdatePill;
use crate::assets::WORDMARK;
use crate::branding::APP_BASE_NAME;
use crate::components::drag_region::DragRegion as _;
use crate::components::provider_update_launch_notification_logic::ProviderUpdateSidebarPillView;
use crate::components::workspace_titlebar::workspace_titlebar_content_left;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{CONTROL_RADIUS, TEXT_SM, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::sidebar::{SidebarFooter, SidebarMenuButton, SidebarMenuButtonSize};
use crate::ui::{IconName, Tooltip};

#[derive(IntoElement)]
pub struct SidebarChromeHeader;

impl RenderOnce for SidebarChromeHeader {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
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
                            // SidebarBrandMark slot styling (`text-sm font-medium
                            // tracking-tight`): the T mark (`h-[1cap]` on 14px
                            // text is ~10px, so `rems(0.625)` at the 16px
                            // default; width from the 48.8:56 viewBox) then
                            // Turin's name in place of "Code". Rems scale with
                            // the interface font.
                            .gap_1()
                            .child(
                                svg()
                                    .path(WORDMARK)
                                    .flex_shrink_0()
                                    .w(rems(0.625 * 48.8 / 56.))
                                    .h(rems(0.625))
                                    .text_color(theme.foreground),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(APP_BASE_NAME),
                            ),
                    ),
            )
            .drag_region(window, cx)
    }
}

/// `SidebarChromeFooter`: the utility row (settings, pull requests, usage)
/// as `SidebarMenuButton size="icon"` inside `SidebarFooter`.
#[derive(IntoElement)]
pub struct SidebarChromeFooter {
    sidebar: WeakEntity<super::Sidebar>,
    provider_update_pill: Option<ProviderUpdateSidebarPillView>,
}

impl SidebarChromeFooter {
    pub fn new(sidebar: WeakEntity<super::Sidebar>) -> Self {
        Self {
            sidebar,
            provider_update_pill: None,
        }
    }

    /// `SidebarProviderUpdatePill`, above the utility row.
    pub fn provider_update_pill(mut self, pill: Option<ProviderUpdateSidebarPillView>) -> Self {
        self.provider_update_pill = pill;
        self
    }
}

impl RenderOnce for SidebarChromeFooter {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let button =
            |id: &'static str, icon: IconName, label: &'static str, event: SidebarEvent| {
                let sidebar = self.sidebar.clone();
                SidebarMenuButton::new(id)
                    .size(SidebarMenuButtonSize::Icon)
                    .icon(icon)
                    .tooltip(Tooltip::new(label))
                    .on_click(move |_, _, cx| {
                        let event = event.clone();
                        sidebar.update(cx, |_, cx| cx.emit(event)).ok();
                    })
            };
        let pill = self.provider_update_pill.clone().map(|view| {
            let (open, dismiss) = (self.sidebar.clone(), self.sidebar.clone());
            let key = view.key.clone();
            SidebarProviderUpdatePill {
                view,
                exiting: false,
                on_open_settings: Box::new(move |_, _, cx| {
                    open.update(cx, |_, cx| cx.emit(SidebarEvent::OpenProviderSettings))
                        .ok();
                }),
                on_dismiss: Box::new(move |_, _, cx| {
                    let key = key.clone();
                    dismiss
                        .update(cx, |_, cx| {
                            cx.emit(SidebarEvent::DismissProviderUpdatePill(key))
                        })
                        .ok();
                }),
            }
        });
        SidebarFooter::new().children(pill).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(button(
                    "sidebar-settings",
                    IconName::Settings,
                    "Settings",
                    SidebarEvent::OpenSettings,
                ))
                .child(button(
                    "sidebar-pull-requests",
                    IconName::GitPullRequestArrow,
                    "Pull Requests",
                    SidebarEvent::OpenPullRequestsPage,
                ))
                .child(button(
                    "sidebar-usage",
                    IconName::ChartNoAxesColumn,
                    "Usage",
                    SidebarEvent::OpenUsage,
                )),
        )
    }
}
