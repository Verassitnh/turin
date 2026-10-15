//! Port of upstream/apps/web/src/components/WorkspaceBreadcrumb.tsx
//!
//! The titlebar breadcrumb shared by the chat header and the settings,
//! usage and pull-request pages: `ol.flex.items-center.gap-3.text-sm`, items
//! `font-medium`, current item `text-foreground`, others muted and
//! non-shrinking, `/` separators in `text-icon-muted`. Interactive crumbs
//! must use `drag_region::no_drag`, since the breadcrumb sits in a drag region.

use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, Refineable as _, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_SM;

#[derive(IntoElement, Default)]
pub struct WorkspaceBreadcrumb {
    children: Vec<AnyElement>,
}

impl WorkspaceBreadcrumb {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for WorkspaceBreadcrumb {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for WorkspaceBreadcrumb {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().flex().min_w_0().child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_3()
                .text_size(TEXT_SM.size)
                .children(self.children),
        )
    }
}

/// `WorkspaceBreadcrumbItem`: `flex min-w-0 items-center font-medium`;
/// current → `text-foreground`, otherwise `shrink-0 text-muted-foreground`.
/// Refinements (`.flex_shrink(1.)` for upstream's `className="shrink"`,
/// `.min_w(rems(2.5)).flex_1()` for `min-w-10 flex-1`) apply last, so they
/// override the base like tailwind-merge does.
#[derive(IntoElement)]
pub struct WorkspaceBreadcrumbItem {
    current: bool,
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl WorkspaceBreadcrumbItem {
    pub fn new(current: bool) -> Self {
        Self {
            current,
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for WorkspaceBreadcrumbItem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for WorkspaceBreadcrumbItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for WorkspaceBreadcrumbItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut item = div()
            .flex()
            .min_w_0()
            .items_center()
            .font_weight(FontWeight::MEDIUM)
            .children(self.children);
        if self.current {
            item = item.text_color(theme.foreground);
        } else {
            item = item.flex_shrink_0().text_color(theme.muted_foreground);
        }
        item.style().refine(&self.style);
        item
    }
}

/// `WorkspaceBreadcrumbText`: a truncating label (`py-1.5` stands in for
/// the cap-height text-box trim).
pub fn workspace_breadcrumb_text(text: impl Into<SharedString>) -> gpui_kit::Div {
    div()
        .min_w_0()
        .truncate()
        .py(rems(0.375))
        .child(text.into())
}

/// `WorkspaceBreadcrumbSeparator`: `/` in `text-icon-muted`.
#[derive(IntoElement)]
pub struct WorkspaceBreadcrumbSeparator;

impl RenderOnce for WorkspaceBreadcrumbSeparator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .text_color(cx.app_theme().icon_muted)
            .child("/")
    }
}
