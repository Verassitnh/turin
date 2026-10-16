//! Port of upstream/apps/web/src/components/sidebar/SidebarProviderUpdatePill.tsx
#![allow(dead_code)]

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div, rems,
};

use super::super::provider_update_launch_notification_logic::{
    ProviderUpdateSidebarPillTone, ProviderUpdateSidebarPillView,
};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_2XS;
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

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
        let bg = scope.sidebar_control_surface;
        let text = match self.view.tone {
            ProviderUpdateSidebarPillTone::Warning => theme.warning,
            ProviderUpdateSidebarPillTone::Error => theme.destructive,
            ProviderUpdateSidebarPillTone::Loading | ProviderUpdateSidebarPillTone::Success => {
                scope.sidebar_foreground
            }
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
            .child(
                Button::new("provider-update-main")
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .label(self.view.title.clone())
                    .tooltip(Tooltip::new(self.view.description.clone()))
                    .icon(match self.view.tone {
                        ProviderUpdateSidebarPillTone::Loading => IconName::LoaderCircle,
                        ProviderUpdateSidebarPillTone::Success => IconName::CircleCheck,
                        ProviderUpdateSidebarPillTone::Error => IconName::TriangleAlert,
                        _ => IconName::RefreshCw,
                    })
                    .on_click(self.on_open_settings),
            )
            .when(self.view.dismissible, |this| {
                this.child(
                    Button::new("provider-update-dismiss")
                        .size(ButtonSize::IconXs)
                        .variant(ButtonVariant::Ghost)
                        .icon(IconName::X)
                        .tooltip(Tooltip::new("Dismiss until provider status changes"))
                        .on_click(self.on_dismiss),
                )
            })
    }
}
