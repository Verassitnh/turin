//! Port of upstream/apps/web/src/components/chat/ThreadStatusLine.tsx
//!
//! One quiet line after the last message for thread state that does not change
//! what Enter does: settled, snoozed, or just woke.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{CHAT_CONTENT_MAX_WIDTH, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ThreadStatusLine {
    pub icon: AnyElement,
    pub label: SharedString,
    pub action_label: SharedString,
    pub action_disabled: bool,
    on_action: Option<ClickHandler>,
}

impl ThreadStatusLine {
    pub fn new(label: impl Into<SharedString>, action_label: impl Into<SharedString>) -> Self {
        Self {
            icon: div().into_any_element(),
            label: label.into(),
            action_label: action_label.into(),
            action_disabled: false,
            on_action: None,
        }
    }

    pub fn icon(mut self, icon: AnyElement) -> Self {
        self.icon = icon;
        self
    }

    pub fn action_disabled(mut self, disabled: bool) -> Self {
        self.action_disabled = disabled;
        self
    }

    pub fn on_action(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ThreadStatusLine {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut action = Button::new("thread-status-line-action")
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::Xs)
            .disabled(self.action_disabled)
            .label(self.action_label);
        if let Some(on_action) = self.on_action {
            action = action.on_click(move |event, window, cx| on_action(event, window, cx));
        }
        div()
            .id("chat-thread-status-line")
            .mx_auto()
            .flex()
            .w_full()
            .min_w_0()
            .max_w(CHAT_CONTENT_MAX_WIDTH)
            .items_center()
            .gap(rems(0.375))
            .px(rems(0.25))
            .pb(rems(0.5))
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(self.icon)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.label),
            )
            .child(div().child("·"))
            .child(action)
    }
}
