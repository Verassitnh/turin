//! Port of upstream/apps/web/src/components/chat/ChatGptSharingControl.tsx
//! and upstream/apps/web/src/components/settings/ChatGptUsageButton.tsx
//!
//! The "Using ChatGPT plan" row a ChatGPT-shared provider adds to its picker, with the button
//! that opens the usage page in the system browser.

use std::rc::Rc;

use gpui_kit::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, rems,
};
use turin_shared::usage_limits::{CHATGPT_USAGE_URL, UsageProviderInfo, uses_chatgpt_sharing};

use crate::assets::PROVIDER_OPENAI;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

type OpenUrl = Rc<dyn Fn(&str, &mut App)>;

/// `ChatGptUsageButton`: `ghost-muted`, "Manage usage" with an external-link glyph.
#[derive(IntoElement)]
pub struct ChatGptUsageButton {
    size: ButtonSize,
    open_external: OpenUrl,
}

impl ChatGptUsageButton {
    pub fn new(size: ButtonSize) -> Self {
        Self {
            size,
            open_external: Rc::new(|url, cx| cx.open_url(url)),
        }
    }

    /// How the usage page opens (`shell.openExternal`); the system browser by default.
    #[allow(dead_code)]
    pub fn open_external(mut self, open: OpenUrl) -> Self {
        self.open_external = open;
        self
    }
}

impl RenderOnce for ChatGptUsageButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let open = self.open_external;
        Button::new("chatgpt-usage")
            .variant(ButtonVariant::GhostMuted)
            .size(self.size)
            .child("Manage usage")
            .child(Icon::new(IconName::ExternalLink).size(rems(0.875)))
            .on_click(move |_, _, cx| open(CHATGPT_USAGE_URL, cx))
    }
}

/// `ChatGptSharingControl`: `None` unless the provider shares a ChatGPT plan.
pub fn chat_gpt_sharing_control<P: UsageProviderInfo>(
    provider: Option<&P>,
) -> Option<ChatGptSharingControl> {
    uses_chatgpt_sharing(provider).then_some(ChatGptSharingControl)
}

#[derive(IntoElement)]
pub struct ChatGptSharingControl;

impl RenderOnce for ChatGptSharingControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .px_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(
                        Icon::path(PROVIDER_OPENAI)
                            .size(rems(0.875))
                            .color(theme.muted_foreground),
                    )
                    .child(SharedString::from("Using ChatGPT plan")),
            )
            .child(ChatGptUsageButton::new(ButtonSize::Xs))
    }
}
