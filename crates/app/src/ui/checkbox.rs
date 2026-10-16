//! Port of upstream/apps/web/src/components/ui/checkbox.tsx
//!
//! `relative inline-flex size-4 rounded-[.25rem] border border-input
//! bg-background shadow-xs/5` (dark unchecked `bg-input/32`); checked or
//! indeterminate fills with `bg-primary` and a 12px glyph. Not ported: focus
//! outline, invalid state.

use std::rc::Rc;

use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::icon::{Icon, IconName};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CheckState {
    #[default]
    Unchecked,
    Checked,
    Indeterminate,
}

type ChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    state: CheckState,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            state: CheckState::Unchecked,
            disabled: false,
            on_change: None,
        }
    }

    pub fn state(mut self, state: CheckState) -> Self {
        self.state = state;
        self
    }

    pub fn checked(self, checked: bool) -> Self {
        self.state(if checked {
            CheckState::Checked
        } else {
            CheckState::Unchecked
        })
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Fires with the next checked value (an indeterminate box becomes checked).
    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let filled = self.state != CheckState::Unchecked;
        let next = self.state != CheckState::Checked;
        let handler = self.on_change;
        let disabled = self.disabled;
        div()
            .id(self.id)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(16.))
            .rounded(px(4.))
            .border_1()
            .border_color(if filled { theme.primary } else { theme.input })
            .bg(if filled {
                theme.primary
            } else if theme.is_dark() {
                with_alpha(theme.input, 0.32).into()
            } else {
                theme.background
            })
            .when(!filled, |this| this.shadow(tokens::shadow_xs(0.05)))
            .when(disabled, |this| this.opacity(0.64))
            .when(!disabled, |this| this.cursor(CursorStyle::PointingHand))
            .when(filled, |this| {
                this.child(
                    Icon::new(if self.state == CheckState::Indeterminate {
                        IconName::Minus
                    } else {
                        IconName::Check
                    })
                    .size(gpui_kit::rems(0.75))
                    .color(theme.primary_foreground),
                )
            })
            .when_some(handler.filter(|_| !disabled), |this, handler| {
                this.on_click(move |_: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    handler(next, window, cx);
                })
            })
    }
}

use gpui_kit::prelude::FluentBuilder as _;
