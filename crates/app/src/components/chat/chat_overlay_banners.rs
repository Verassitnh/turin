//! Port of upstream/apps/web/src/components/chat/ProviderStatusBanner.tsx and
//! ThreadErrorBanner.tsx (floating alerts above the chat lane).

use gpui_kit::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div, px, rems,
};

use super::composer_pending_panels::{BannerVariant, banner_surface};
use super::provider_status_banner::{
    get_provider_status_message_with_compatibility, provider_status_banner_is_warning,
    provider_status_banner_title,
};
use crate::composer::catalog::ProviderInstanceEntry;
use crate::theme::ActiveAppTheme;
use crate::ui::{Icon, IconName};

#[derive(IntoElement)]
pub struct ProviderStatusBanner {
    pub entry: ProviderInstanceEntry,
}

impl RenderOnce for ProviderStatusBanner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let variant = if provider_status_banner_is_warning(&self.entry) {
            BannerVariant::Warning
        } else {
            BannerVariant::Error
        };
        let title = provider_status_banner_title(&self.entry);
        let message = get_provider_status_message_with_compatibility(&self.entry);
        banner_surface(
            variant,
            false,
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(rems(0.875))
                        .child(title),
                )
                .child(div().text_size(rems(0.875)).child(message))
                .into_any_element(),
            window,
            &theme,
        )
    }
}

#[derive(IntoElement)]
pub struct ThreadErrorBanner {
    pub error: String,
    pub usage_limit: bool,
}

impl RenderOnce for ThreadErrorBanner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let variant = if self.usage_limit {
            BannerVariant::Warning
        } else {
            BannerVariant::Error
        };
        let icon = Icon::new(IconName::CircleAlert)
            .size(rems(1.))
            .color(if self.usage_limit {
                theme.warning
            } else {
                theme.error
            });
        div()
            .flex()
            .justify_center()
            .w_full()
            .pt_3()
            .child(banner_surface(
                variant,
                false,
                div()
                    .id("thread-error-banner")
                    .flex()
                    .gap_2()
                    .items_start()
                    .max_w(rems(48.))
                    .child(icon)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(rems(0.875))
                            .child(self.error),
                    )
                    .into_any_element(),
                window,
                &theme,
            ))
    }
}
