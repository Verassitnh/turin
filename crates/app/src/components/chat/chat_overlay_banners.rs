//! Port of upstream/apps/web/src/components/chat/ProviderStatusBanner.tsx and
//! ThreadErrorBanner.tsx (floating alerts above the chat lane).

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, CursorStyle, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

use super::chat_gpt_sharing_control::ChatGptUsageButton;
use super::provider_status_banner::{
    get_provider_status_message_with_compatibility, has_provider_setup,
    provider_status_banner_is_warning, provider_status_banner_title,
};
use crate::assets::PROVIDER_OPENAI;
use crate::composer::catalog::ProviderInstanceEntry;
use crate::theme::ActiveAppTheme;
use crate::ui::alert::{
    Alert, AlertControlAlignment, AlertSurface, AlertVariant, alert_description,
    alert_description_color, alert_icon_color, alert_title,
};
use crate::ui::tooltip::Tooltip;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;
type OpenProviderSetup = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// `pointer-events-auto mx-auto w-fit max-w-[…] pt-3`: the alert centred over the lane.
fn floating(alert: impl IntoElement, max_width: gpui_kit::Rems) -> gpui_kit::Div {
    div()
        .flex()
        .justify_center()
        .w_full()
        .pt_3()
        .child(div().max_w(max_width).child(alert))
}

/// `<Tooltip><TooltipTrigger render={<div className="line-clamp-3" />}>`: the message clamps to
/// three lines and the popup shows all of it.
fn clamped_message(id: &'static str, message: SharedString) -> impl IntoElement {
    let full = message.clone();
    div()
        .id(id)
        .line_clamp(3)
        .child(message)
        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
}

#[derive(IntoElement)]
pub struct ProviderStatusBanner {
    pub entry: ProviderInstanceEntry,
    pub on_dismiss: Option<Dismiss>,
    pub on_open_provider_setup: Option<OpenProviderSetup>,
}

impl ProviderStatusBanner {
    pub fn new(entry: ProviderInstanceEntry) -> Self {
        Self {
            entry,
            on_dismiss: None,
            on_open_provider_setup: None,
        }
    }

    pub fn on_dismiss(mut self, on_dismiss: Dismiss) -> Self {
        self.on_dismiss = Some(on_dismiss);
        self
    }

    #[allow(dead_code)]
    pub fn on_open_provider_setup(mut self, handler: OpenProviderSetup) -> Self {
        self.on_open_provider_setup = Some(handler);
        self
    }
}

impl RenderOnce for ProviderStatusBanner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let variant = if provider_status_banner_is_warning(&self.entry) {
            AlertVariant::Warning
        } else {
            AlertVariant::Error
        };
        let title = provider_status_banner_title(&self.entry);
        let message = get_provider_status_message_with_compatibility(&self.entry);
        let provider_name = if self.entry.display_name.trim().is_empty() {
            self.entry.driver_kind.clone()
        } else {
            self.entry.display_name.trim().to_owned()
        };
        let setup = self
            .on_open_provider_setup
            .clone()
            .filter(|_| has_provider_setup(&self.entry));
        let instance_id = self.entry.instance_id.clone();
        let description = alert_description(alert_description_color(variant, &theme))
            .child(clamped_message("provider-status-message", message.into()))
            .when_some(setup, |this, open| {
                // `InlineButton`: `font-medium text-foreground`, underlined on hover.
                this.child(
                    div()
                        .id("provider-status-setup")
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .cursor(CursorStyle::PointingHand)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .hover(|style| style.underline())
                        .child("Open provider setup")
                        .on_click(move |_, window, cx| open(&instance_id, window, cx)),
                )
            });
        let mut alert = Alert::new()
            .variant(variant)
            .surface(AlertSurface::Glass)
            .control_alignment(AlertControlAlignment::FirstLine)
            .icon(
                Icon::new(IconName::Info)
                    .size(rems(1.))
                    .color(alert_icon_color(variant, &theme)),
            )
            .content(alert_title(title))
            .content(description);
        if let Some(on_dismiss) = self.on_dismiss {
            alert = alert.action(
                Button::new("provider-status-dismiss")
                    .variant(ButtonVariant::GhostMuted)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::X)
                    .tooltip(Tooltip::new(format!(
                        "Dismiss {provider_name} provider {}",
                        provider_status_label(&self.entry)
                    )))
                    .on_click(move |_, window, cx| on_dismiss(window, cx)),
            );
        }
        floating(alert, rems(80.))
    }
}

fn provider_status_label(entry: &ProviderInstanceEntry) -> &'static str {
    use crate::composer::catalog::ProviderStatus;
    match entry.status {
        ProviderStatus::Ready => "ready",
        ProviderStatus::Warning => "warning",
        ProviderStatus::Error => "error",
        ProviderStatus::Disabled => "disabled",
    }
}

#[derive(IntoElement)]
pub struct ThreadErrorBanner {
    pub error: String,
    pub usage_limit: bool,
    /// `chatGptUsageLimit`: a ChatGPT-plan stop gets its own wording and a usage button.
    pub chat_gpt_usage_limit: bool,
    pub on_dismiss: Option<Dismiss>,
}

impl ThreadErrorBanner {
    pub fn new(error: impl Into<String>, usage_limit: bool) -> Self {
        Self {
            error: error.into(),
            usage_limit,
            chat_gpt_usage_limit: false,
            on_dismiss: None,
        }
    }

    pub fn on_dismiss(mut self, on_dismiss: Dismiss) -> Self {
        self.on_dismiss = Some(on_dismiss);
        self
    }

    #[allow(dead_code)]
    pub fn chat_gpt_usage_limit(mut self, chat_gpt: bool) -> Self {
        self.chat_gpt_usage_limit = chat_gpt;
        self
    }
}

impl RenderOnce for ThreadErrorBanner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        // `if (!error) return null`.
        if self.error.is_empty() {
            return div().into_any_element();
        }
        let variant = if self.usage_limit {
            AlertVariant::Warning
        } else {
            AlertVariant::Error
        };
        let chat_gpt = self.chat_gpt_usage_limit;
        let icon = if chat_gpt {
            // `text-foreground!`.
            Icon::path(PROVIDER_OPENAI)
                .size(rems(1.))
                .color(theme.foreground)
        } else {
            Icon::new(IconName::CircleAlert)
                .size(rems(1.))
                .color(alert_icon_color(variant, &theme))
        };
        let description =
            alert_description(alert_description_color(variant, &theme)).child(if chat_gpt {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child("ChatGPT usage limit reached"),
                    )
                    .child("Review your usage settings in ChatGPT to continue.")
                    .into_any_element()
            } else {
                clamped_message("thread-error-message", self.error.into()).into_any_element()
            });
        let mut alert = Alert::new()
            .variant(variant)
            .surface(AlertSurface::Glass)
            .control_alignment(AlertControlAlignment::FirstLine)
            .icon(icon)
            .content(description);
        if chat_gpt {
            alert = alert.action(ChatGptUsageButton::new(ButtonSize::Sm));
        }
        if let Some(on_dismiss) = self.on_dismiss {
            alert = alert.action(
                Button::new("thread-error-dismiss")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::X)
                    .tooltip(Tooltip::new("Dismiss error"))
                    .on_click(move |_, window, cx| on_dismiss(window, cx)),
            );
        }
        // `max-w-[min(48rem,calc(100%-2rem))]`.
        floating(alert, rems(48.)).into_any_element()
    }
}
