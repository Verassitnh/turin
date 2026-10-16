//! Port of upstream/apps/web/src/components/ui/radio-group.tsx
//!
//! Desktop radio is `size-4` (16px): `rounded-full border border-input
//! bg-background` (`dark:bg-input/32` while unchecked), `shadow-xs/5`, and the
//! 1px lip. Checked fills `bg-primary` with a `size-1.5` `primary-foreground`
//! dot. The group is `flex flex-col gap-3`.
//!
//! Not ported: the focus ring and the invalid outline. gpui-component's
//! `Radio` draws its own check glyph, so this is a primitive.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, InteractiveElement, IntoElement,
    MouseButton, ParentElement, RenderOnce, StatefulInteractiveElement, Styled, Window, div, px,
    rems,
};

use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct RadioGroup {
    children: Vec<AnyElement>,
}

impl RadioGroup {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for RadioGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for RadioGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for RadioGroup {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().flex().flex_col().gap_3().children(self.children)
    }
}

#[derive(IntoElement)]
pub struct Radio {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Radio {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            disabled: false,
            on_click: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Radio {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let interactive = !self.disabled;
        let unchecked_bg = if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        };
        let mut root = div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(rems(1.))
            .rounded_full()
            .border_1()
            .border_color(theme.input)
            .when(!self.checked, |this| {
                this.bg(unchecked_bg).shadow(tokens::shadow_xs(0.05))
            })
            .when(self.checked, |this| this.bg(theme.primary))
            .when(!interactive, |this| this.opacity(0.64))
            .when(interactive, |this| this.cursor(CursorStyle::PointingHand))
            .when(self.checked, |this| {
                this.child(
                    div()
                        .size(rems(0.375))
                        .rounded_full()
                        .bg(theme.primary_foreground),
                )
            });
        if interactive && !self.checked {
            root = root.child(tokens::control_lip_overlay(
                tokens::control_lip(theme.is_dark()),
                px(8.),
            ));
        }
        if let Some(on_click) = self.on_click.filter(|_| interactive) {
            root = root.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        root
    }
}
