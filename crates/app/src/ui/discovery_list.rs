//! Port of upstream/apps/web/src/components/ui/discovery-list.tsx
//!
//! `overflow-hidden rounded-xl border border-border/70 bg-background`, rows
//! divided by `border/60`. Each row is `gap-3 px-3 py-3`, title `text-sm
//! font-medium`, description `text-xs text-muted-foreground`, hover
//! `bg-accent/40`.

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_XL, TEXT_SM, TEXT_XS};

#[derive(IntoElement)]
pub struct DiscoveryList {
    children: Vec<AnyElement>,
}

impl DiscoveryList {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for DiscoveryList {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for DiscoveryList {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for DiscoveryList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(RADIUS_XL)
            .border_1()
            .border_color(with_alpha(theme.border, 0.7))
            .bg(theme.background)
            .children(self.children)
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct DiscoveryListRow {
    id: gpui_kit::ElementId,
    icon: AnyElement,
    title: SharedString,
    description: SharedString,
    action: Option<AnyElement>,
    divided: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl DiscoveryListRow {
    pub fn new(
        id: impl Into<gpui_kit::ElementId>,
        icon: impl IntoElement,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.into_any_element(),
            title: title.into(),
            description: description.into(),
            action: None,
            divided: false,
            disabled: false,
            on_click: None,
        }
    }

    /// `divide-y`: every row after the first.
    pub fn divided(mut self, divided: bool) -> Self {
        self.divided = divided;
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
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

impl RenderOnce for DiscoveryListRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut row = div()
            .id(self.id)
            .flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_3()
            .py_3()
            .when(self.divided, |this| {
                this.border_t_1()
                    .border_color(with_alpha(theme.border, 0.6))
            })
            .when(self.disabled, |this| this.opacity(0.64))
            .when(!self.disabled, |this| {
                this.hover(|style| style.bg(solid_over(theme.accent, 0.4, theme.background)))
            })
            .child(self.icon)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .line_height(TEXT_SM.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(self.title),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_XS.size)
                            .line_height(TEXT_XS.line_height)
                            .text_color(theme.muted_foreground)
                            .child(self.description),
                    ),
            )
            .children(self.action);
        if let Some(on_click) = self.on_click.filter(|_| !self.disabled) {
            row = row.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        row
    }
}
