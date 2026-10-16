//! Port of upstream/apps/web/src/components/sidebar/SidebarProviderUpdatePill.tsx
#![allow(dead_code)]

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, CursorStyle, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, StatefulInteractiveElement, Styled as _, Window, div, rems,
};

use super::super::provider_update_launch_notification_logic::{
    ProviderUpdateSidebarPillTone, ProviderUpdateSidebarPillView,
};
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_2XS;
use crate::ui::{Icon, IconName, Tooltip};

type PillClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct SidebarProviderUpdatePill {
    pub view: ProviderUpdateSidebarPillView,
    pub exiting: bool,
    pub on_open_settings: PillClickHandler,
    pub on_dismiss: PillClickHandler,
}

impl RenderOnce for SidebarProviderUpdatePill {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let scope = cx.app_theme().sidebar_scope();
        let theme = cx.app_theme();
        let (bg, text, hover_bg) = match self.view.tone {
            ProviderUpdateSidebarPillTone::Warning => (
                with_alpha(theme.warning, 0.12),
                theme.warning,
                with_alpha(theme.warning, 0.18),
            ),
            ProviderUpdateSidebarPillTone::Error => (
                with_alpha(theme.destructive, 0.12),
                theme.destructive,
                with_alpha(theme.destructive, 0.18),
            ),
            ProviderUpdateSidebarPillTone::Loading | ProviderUpdateSidebarPillTone::Success => (
                with_alpha(scope.sidebar_control_surface, 1.0),
                scope.sidebar_foreground,
                with_alpha(scope.sidebar_row_hover, 1.0),
            ),
        };
        div()
            .flex()
            .w_full()
            .min_h(rems(1.75))
            .items_center()
            .rounded(rems(0.5))
            .bg(bg)
            .text_color(text)
            .text_size(TEXT_2XS.size)
            .font_weight(FontWeight::MEDIUM)
            .when(self.exiting, |this| this.opacity(0.0))
            .hover(|style| style.bg(hover_bg))
            .child(
                div()
                    .id("provider-update-main")
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(rems(0.375))
                    .cursor(CursorStyle::PointingHand)
                    .on_mouse_down(MouseButton::Left, no_drag)
                    .on_click({
                        let handler = self.on_open_settings;
                        move |event, window, cx| handler(event, window, cx)
                    })
                    .tooltip({
                        let description = self.view.description.clone();
                        move |window, cx| Tooltip::new(description.clone()).build(window, cx)
                    })
                    .child(match self.view.tone {
                        ProviderUpdateSidebarPillTone::Loading => Icon::new(IconName::LoaderCircle)
                            .size(rems(0.875))
                            .color(text),
                        ProviderUpdateSidebarPillTone::Success => Icon::new(IconName::CircleCheck)
                            .size(rems(0.875))
                            .color(text),
                        ProviderUpdateSidebarPillTone::Error => Icon::new(IconName::TriangleAlert)
                            .size(rems(0.875))
                            .color(text),
                        ProviderUpdateSidebarPillTone::Warning => {
                            Icon::new(IconName::Download).size(rems(0.875)).color(text)
                        }
                    })
                    .child(
                        div()
                            .min_w_0()
                            .text_color(text)
                            .text_size(TEXT_2XS.size)
                            .child(self.view.title.clone()),
                    ),
            )
            .when(self.view.dismissible, |this| {
                this.child(
                    div()
                        .id("provider-update-dismiss")
                        .mr_1()
                        .flex()
                        .size(rems(1.25))
                        .items_center()
                        .justify_center()
                        .rounded(rems(0.375))
                        .cursor(CursorStyle::PointingHand)
                        .on_mouse_down(MouseButton::Left, no_drag)
                        .on_click({
                            let handler = self.on_dismiss;
                            move |event, window, cx| handler(event, window, cx)
                        })
                        .tooltip(|window, cx| {
                            Tooltip::new("Dismiss until provider status changes").build(window, cx)
                        })
                        .child(Icon::new(IconName::X).size(rems(0.875)).color(text)),
                )
            })
    }
}
