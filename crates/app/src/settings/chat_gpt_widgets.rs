//! Port of upstream/apps/web/src/components/settings/ChatGptConnectionButton.tsx
//! and ChatGptUsageButton.tsx.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, img, rems,
};

use super::view::SettingsView;
use crate::assets::PROVIDER_OPENAI;
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName};

const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/usage";

impl SettingsView {
    pub(super) fn chat_gpt_connection_button(
        &self,
        id: &str,
        label: &str,
        disabled: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut gpui_kit::Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(img(PROVIDER_OPENAI).size(rems(1.)).flex_shrink_0())
            .child(
                Button::new(SharedString::from(id))
                    .variant(ButtonVariant::Default)
                    .size(ButtonSize::Sm)
                    .label(label)
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        on_click(this, window, cx);
                    })),
            )
            .into_any_element()
    }

    pub(super) fn chat_gpt_usage_button(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let url = CHATGPT_USAGE_URL.to_owned();
        Button::new(SharedString::from(id))
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::Sm)
            .label("Manage usage")
            .icon(IconName::ExternalLink)
            .icon_size(rems(0.875))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.store.read(cx).source().open_external_url(&url, cx);
            }))
            .into_any_element()
    }
}
