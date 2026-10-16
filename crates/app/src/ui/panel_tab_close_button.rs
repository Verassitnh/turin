//! Port of upstream/apps/web/src/components/ui/panel-tab-close-button.tsx
//!
//! A `size-4` hit target. The tab identity shows until the ancestor group
//! named `tab` is hovered, then a `size-3` X replaces it. `hover:bg-muted`.
//! The parent row must call `.group("tab")`.
//!
//! Not ported: `group-focus-visible/close` (no focus-visible group).

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, rems,
};

use super::icon::{Icon, IconName};
use super::tooltip::Tooltip;
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::RADIUS_SM;

/// The group name the parent tab row must use.
pub const TAB_GROUP: &str = "tab";

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct PanelTabCloseButton {
    label: SharedString,
    tooltip: Option<SharedString>,
    identity: AnyElement,
    on_click: Option<ClickHandler>,
}

impl PanelTabCloseButton {
    pub fn new(label: impl Into<SharedString>, identity: impl IntoElement) -> Self {
        Self {
            label: label.into(),
            tooltip: None,
            identity: identity.into_any_element(),
            on_click: None,
        }
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
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

impl RenderOnce for PanelTabCloseButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let id: SharedString = format!("panel-tab-close-{}", self.label).into();
        let mut button = div()
            .id(id.clone())
            .on_mouse_down(MouseButton::Left, no_drag)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(rems(1.))
            .rounded(RADIUS_SM)
            .cursor(CursorStyle::PointingHand)
            .hover(|style| style.bg(theme.muted))
            .child(
                div()
                    .absolute()
                    .size(rems(0.75))
                    .flex()
                    .items_center()
                    .justify_center()
                    .group_hover(TAB_GROUP, |style| style.opacity(0.))
                    .child(self.identity),
            )
            .child(
                div()
                    .absolute()
                    .size(rems(0.75))
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.)
                    .group_hover(TAB_GROUP, |style| style.opacity(1.))
                    .child(Icon::new(IconName::X).size(rems(0.75))),
            );
        if let Some(on_click) = self.on_click {
            button = button.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        if let Some(tooltip) = self.tooltip {
            button =
                button.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
        }
        button
    }
}
