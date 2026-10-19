//! Port of upstream/apps/web/src/components/ui/select.tsx
//!
//! Desktop (`sm:`) trigger sizes: default `min-h-8` (32px) with 11px padding,
//! `sm` 28px / 9px, `lg` 36px / 11px, `xs` a fixed 24px, `compact` a fixed
//! 28px. The default face is `border-input bg-background shadow-xs/5` (dark
//! `bg-input/32`) plus the control lip. The popup is `dropdown-glass` with
//! the popover shadow, `p-1`, and rows at `sm:min-h-7`. Highlight is
//! `bg-accent`; the selected row is `bg-foreground/8`.
//!
//! Not ported: the focus ring, the invalid outline, typeahead, and the scroll
//! arrows. The list caps at 24rem in place of `--available-height`.

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, CursorStyle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use super::icon::{Icon, IconName};
use super::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_MD, RADIUS_SM, TEXT_SM, TEXT_XS};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectSize {
    Compact,
    #[default]
    Default,
    Sm,
    Lg,
    Xs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectVariant {
    #[default]
    Default,
    Ghost,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectMetrics {
    pub height: Pixels,
    pub fixed_height: bool,
    pub gap: Pixels,
    pub padding_x: Pixels,
    pub compact_radius: bool,
    pub text_xs: bool,
}

/// Desktop trigger metrics. `sm:` sizes win over the unprefixed ones.
pub fn select_metrics(size: SelectSize) -> SelectMetrics {
    match size {
        SelectSize::Compact => SelectMetrics {
            height: px(28.),
            fixed_height: true,
            gap: px(4.),
            padding_x: px(7.),
            compact_radius: true,
            text_xs: true,
        },
        SelectSize::Default => SelectMetrics {
            height: px(32.),
            fixed_height: false,
            gap: px(8.),
            padding_x: px(11.),
            compact_radius: false,
            text_xs: false,
        },
        SelectSize::Sm => SelectMetrics {
            height: px(28.),
            fixed_height: false,
            gap: px(6.),
            padding_x: px(9.),
            compact_radius: false,
            text_xs: false,
        },
        SelectSize::Lg => SelectMetrics {
            height: px(36.),
            fixed_height: false,
            gap: px(8.),
            padding_x: px(11.),
            compact_radius: false,
            text_xs: false,
        },
        SelectSize::Xs => SelectMetrics {
            height: px(24.),
            fixed_height: true,
            gap: px(4.),
            padding_x: px(7.),
            compact_radius: true,
            text_xs: true,
        },
    }
}

fn apply_metrics<E: Styled>(root: E, size: SelectSize) -> E {
    let metrics = select_metrics(size);
    let text = if metrics.text_xs { TEXT_XS } else { TEXT_SM };
    let radius = if metrics.compact_radius {
        RADIUS_MD
    } else {
        RADIUS_LG
    };
    let root = root
        .gap(metrics.gap)
        .px(metrics.padding_x)
        .rounded(radius)
        .text_size(text.size)
        .line_height(text.line_height);
    if metrics.fixed_height {
        root.h(metrics.height)
    } else {
        root.min_h(metrics.height)
    }
}

type Pick = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct SelectButton {
    size: SelectSize,
    label: SharedString,
    placeholder: bool,
    on_click: Option<Pick>,
}

impl SelectButton {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            size: SelectSize::Default,
            label: label.into(),
            placeholder: false,
            on_click: None,
        }
    }

    pub fn size(mut self, size: SelectSize) -> Self {
        self.size = size;
        self
    }

    pub fn placeholder(mut self, placeholder: bool) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SelectButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let label_color = if self.placeholder {
            theme.placeholder
        } else {
            theme.foreground
        };
        let mut button = div()
            .id("select-button")
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .w_full()
            .min_w(rems(9.))
            .items_center()
            .justify_between()
            .cursor(CursorStyle::PointingHand)
            .border_1()
            .border_color(theme.input)
            .bg(if theme.is_dark() {
                with_alpha(theme.input, 0.32).into()
            } else {
                theme.background
            })
            .text_color(theme.foreground)
            .shadow(tokens::shadow_xs(0.05));
        button = apply_metrics(button, self.size);
        if let Some(on_click) = self.on_click {
            button = button.on_click(move |_: &ClickEvent, window, cx| on_click(window, cx));
        }
        button
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(label_color)
                    .child(self.label),
            )
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(rems(0.75))
                    .color(with_alpha(theme.foreground, 0.5).into()),
            )
            .child(tokens::control_lip_overlay(
                tokens::control_lip(theme.is_dark()),
                px(8.),
            ))
    }
}

#[derive(IntoElement)]
pub struct SelectItem {
    id: SharedString,
    label: SharedString,
    selected: bool,
    highlighted: bool,
    disabled: bool,
    on_click: Option<Pick>,
}

impl SelectItem {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected: false,
            highlighted: false,
            disabled: false,
            on_click: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SelectItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut row = div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .min_h(px(28.))
            .items_center()
            .px_2()
            .py_1()
            .rounded(RADIUS_SM)
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .text_color(if self.highlighted {
                theme.accent_foreground
            } else {
                theme.foreground
            });
        if self.selected {
            row = row.bg(solid_over(theme.foreground, 0.08, theme.background));
        }
        if self.highlighted {
            row = row.bg(theme.accent);
        }
        if self.disabled {
            row = row.opacity(0.64);
        } else {
            row = row.cursor(CursorStyle::PointingHand);
            if let Some(on_click) = self.on_click {
                row = row.on_click(move |_: &ClickEvent, window, cx| on_click(window, cx));
            }
        }
        row.child(div().flex_1().min_w_0().truncate().child(self.label))
    }
}

pub fn select_group_label(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .px_2()
        .py(rems(0.375))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_size(TEXT_XS.size)
        .line_height(TEXT_XS.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

/// `SelectPopup`: glass surface, popover shadow, `p-1` list.
#[derive(IntoElement, Default)]
pub struct SelectPopup {
    open: bool,
    children: Vec<gpui_kit::AnyElement>,
}

impl SelectPopup {
    pub fn new() -> Self {
        Self {
            open: false,
            children: Vec::new(),
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
}

impl ParentElement for SelectPopup {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui_kit::AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SelectPopup {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div().when(self.open, |this| {
            this.child(
                div()
                    .absolute()
                    .top_full()
                    .left_0()
                    .mt(px(4.))
                    .min_w_full()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(dropdown_glass_border(theme))
                    .bg(dropdown_glass_background(theme))
                    .shadow(tokens::shadow_popover(theme.is_dark()))
                    .child(
                        div()
                            .id("select-list")
                            .max_h(rems(24.))
                            .overflow_y_scroll()
                            .p_1()
                            .children(self.children),
                    ),
            )
        })
    }
}

/// Ghost trigger face: transparent border, `text-secondary-label`, hover accent.
pub fn select_ghost_trigger(label: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    let theme = cx.app_theme();
    let mut row = div()
        .flex()
        .items_center()
        .justify_between()
        .border_1()
        .border_color(with_alpha(theme.foreground, 0.))
        .text_color(theme.secondary_label)
        .hover(|style| style.bg(theme.accent).text_color(theme.foreground));
    row = apply_metrics(row, SelectSize::Default);
    row.child(label.into()).child(
        Icon::new(IconName::ChevronDown)
            .size(rems(0.75))
            .color(with_alpha(theme.foreground, 0.5).into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_sizes_follow_the_sm_breakpoint() {
        let default = select_metrics(SelectSize::Default);
        assert_eq!(default.height, px(32.));
        assert!(!default.fixed_height);
        assert_eq!(default.padding_x, px(11.));
        assert_eq!(select_metrics(SelectSize::Sm).height, px(28.));
        assert_eq!(select_metrics(SelectSize::Sm).padding_x, px(9.));
        assert_eq!(select_metrics(SelectSize::Lg).height, px(36.));
        let xs = select_metrics(SelectSize::Xs);
        assert_eq!(xs.height, px(24.));
        assert!(xs.fixed_height);
        assert!(xs.text_xs);
        let compact = select_metrics(SelectSize::Compact);
        assert_eq!(compact.height, px(28.));
        assert!(compact.fixed_height);
        assert_eq!(compact.padding_x, px(7.));
    }
}
