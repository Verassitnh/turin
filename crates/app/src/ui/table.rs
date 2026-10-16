//! Port of upstream/apps/web/src/components/ui/table.tsx
//!
//! `text-xs` rows with `border-b`, a header `h-10 px-2 font-medium`, cells
//! `p-2`, hover `bg-muted/50`, selected `bg-muted`. The last body row drops
//! its border (`last`).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;

#[derive(IntoElement)]
pub struct Table {
    children: Vec<AnyElement>,
}

impl Table {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Table {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Table {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id("ui-table")
            .relative()
            .w_full()
            .overflow_x_scroll()
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .children(self.children)
    }
}

#[derive(IntoElement)]
pub struct TableRow {
    header: bool,
    selected: bool,
    last: bool,
    children: Vec<AnyElement>,
}

impl TableRow {
    pub fn body() -> Self {
        Self {
            header: false,
            selected: false,
            last: false,
            children: Vec::new(),
        }
    }

    pub fn header() -> Self {
        Self {
            header: true,
            ..Self::body()
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// `[&_tr:last-child]:border-0`.
    pub fn last(mut self, last: bool) -> Self {
        self.last = last;
        self
    }
}

impl ParentElement for TableRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TableRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let bg: Option<Hsla> = if self.selected {
            Some(theme.muted)
        } else {
            None
        };
        div()
            .flex()
            .w_full()
            .items_center()
            .when(!self.last, |this| {
                this.border_b_1().border_color(theme.border)
            })
            .when_some(bg, |this, bg| this.bg(bg))
            .hover(|style| style.bg(with_alpha(theme.muted, 0.5)))
            .children(self.children)
    }
}

pub fn table_head(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .h(rems(2.5))
        .px_2()
        .flex()
        .items_center()
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .text_color(cx.app_theme().foreground)
        .child(text.into())
}

pub fn table_cell(text: impl Into<SharedString>) -> gpui_kit::Div {
    div().p_2().whitespace_nowrap().child(text.into())
}
