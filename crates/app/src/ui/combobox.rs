//! Port of upstream/apps/web/src/components/ui/combobox.tsx
//!
//! The field is full width. A start addon pads the input `sm:` `ps` of
//! `calc(--spacing(8) - 1px)` (31px), or 27px at `sm` size. A trigger or
//! clear control reserves `pe-7` (28px), or `pe-6.5` (26px) at `sm`, and sits
//! in a 28px hit target. The popup is `dropdown-glass`, at least as wide as
//! the anchor, and at most 23rem tall. Rows are `sm:min-h-7`; the selected
//! row is `bg-foreground/8` and the highlight is `bg-accent`.
//!
//! [`combobox_contains`] is the default filter (`useFilter().contains`) for
//! ASCII: an empty query matches, otherwise a case-insensitive substring.
//! Not ported: locale collation, chips, and the virtualized list.

use crate::theme::color::solid_over;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, Pixels, RenderOnce, SharedString, Styled, Window,
    div, px, rems,
};

use super::icon::{Icon, IconName};
use super::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_SM, TEXT_SM, TEXT_XS};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComboboxSize {
    Sm,
    #[default]
    Default,
    Lg,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComboboxPadding {
    pub start: Pixels,
    pub end: Pixels,
    pub trigger: Pixels,
}

/// Desktop padding when a start addon and a trigger (or clear) are showing.
pub fn combobox_padding(size: ComboboxSize) -> ComboboxPadding {
    match size {
        ComboboxSize::Sm => ComboboxPadding {
            start: px(27.),
            end: px(26.),
            trigger: px(28.),
        },
        ComboboxSize::Default | ComboboxSize::Lg => ComboboxPadding {
            start: px(31.),
            end: px(28.),
            trigger: px(28.),
        },
    }
}

/// `useFilter().contains` for ASCII text.
pub fn combobox_contains(item: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    item.to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase())
}

pub fn combobox_matches<'a>(items: &'a [&'a str], query: &str) -> Vec<&'a str> {
    items
        .iter()
        .copied()
        .filter(|item| combobox_contains(item, query))
        .collect()
}

#[derive(IntoElement)]
pub struct ComboboxItem {
    label: SharedString,
    selected: bool,
    highlighted: bool,
    disabled: bool,
}

impl ComboboxItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            selected: false,
            highlighted: false,
            disabled: false,
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
}

impl RenderOnce for ComboboxItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut row = div()
            .flex()
            .min_h(px(28.))
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(RADIUS_SM)
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height);
        if self.disabled {
            row = row.opacity(0.64);
        }
        if self.highlighted {
            row = row.bg(theme.accent).text_color(theme.accent_foreground);
        } else if self.selected {
            row = row
                .bg(solid_over(theme.foreground, 0.08, theme.background))
                .text_color(theme.foreground);
        } else {
            row = row.text_color(theme.foreground);
        }
        row.child(div().flex_1().min_w_0().truncate().child(self.label))
    }
}

pub fn combobox_empty(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .p_2()
        .text_center()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

pub fn combobox_status(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .px_3()
        .py_2()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_size(TEXT_XS.size)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

/// Search field at the top of a popup: `px-3 pt-2.5`, a bottom rule, a
/// `size-4` search icon, and an unstyled `sm` input.
pub fn combobox_search_input(query: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    let theme = cx.app_theme();
    div().px_3().pt(px(10.)).child(
        div()
            .relative()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(with_alpha(theme.border, 0.7))
            .pb(px(6.))
            .pl(px(20.))
            .h(px(26.))
            .text_size(TEXT_SM.size)
            .text_color(theme.foreground)
            .child(
                div().absolute().left_0().top(px(6.)).child(
                    Icon::new(IconName::Search)
                        .size(rems(1.))
                        .color(with_alpha(theme.muted_foreground, 0.55).into()),
                ),
            )
            .child(query.into()),
    )
}

#[derive(IntoElement, Default)]
pub struct ComboboxPopup {
    open: bool,
    children: Vec<AnyElement>,
}

impl ComboboxPopup {
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

impl ParentElement for ComboboxPopup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ComboboxPopup {
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
                    .max_h(rems(23.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(dropdown_glass_border(theme))
                    .bg(dropdown_glass_background(theme))
                    .text_color(theme.foreground)
                    .shadow(tokens::shadow_popover(theme.is_dark()))
                    .children(self.children),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_padding_reserves_the_addon_and_the_trigger() {
        let default = combobox_padding(ComboboxSize::Default);
        assert_eq!(default.start, px(31.));
        assert_eq!(default.end, px(28.));
        assert_eq!(default.trigger, px(28.));
        let sm = combobox_padding(ComboboxSize::Sm);
        assert_eq!(sm.start, px(27.));
        assert_eq!(sm.end, px(26.));
    }

    #[test]
    fn contains_is_a_case_insensitive_substring() {
        assert!(combobox_contains("Sonnet", ""));
        assert!(combobox_contains("Sonnet", "son"));
        assert!(combobox_contains("Sonnet", "NET"));
        assert!(!combobox_contains("Sonnet", "opus"));
        assert_eq!(
            combobox_matches(&["Opus", "Sonnet", "Haiku"], "s"),
            vec!["Opus", "Sonnet"]
        );
    }
}
