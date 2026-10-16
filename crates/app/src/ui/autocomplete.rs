//! Port of upstream/apps/web/src/components/ui/autocomplete.tsx
//!
//! Same field chrome as the combobox, with the trigger hidden unless asked.
//! The popup is `rounded-lg border bg-popover shadow-lg/5` with the control
//! lip, not dropdown glass, and caps at 23rem. Rows are `sm:min-h-7`.
//! Selected is `bg-accent/50`; highlighted (including a selected highlight)
//! is `bg-accent`. Group labels are `px-2 py-1.5 text-xs font-medium`.
//!
//! Not ported: the virtualized list and locale filtering.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
    rems,
};

use super::combobox::{ComboboxSize, combobox_padding};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_SM, TEXT_SM, TEXT_XS};

#[derive(IntoElement)]
pub struct AutocompleteItem {
    label: SharedString,
    selected: bool,
    highlighted: bool,
    disabled: bool,
}

impl AutocompleteItem {
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

impl RenderOnce for AutocompleteItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let highlighted = self.highlighted;
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
        if highlighted {
            row = row.bg(theme.accent).text_color(theme.accent_foreground);
        } else if self.selected {
            row = row
                .bg(with_alpha(theme.accent, 0.5))
                .text_color(theme.foreground);
        } else {
            row = row.text_color(theme.foreground);
        }
        row.child(self.label)
    }
}

pub fn autocomplete_group_label(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .px_2()
        .py(rems(0.375))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_size(TEXT_XS.size)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

pub fn autocomplete_empty(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .p_2()
        .text_center()
        .text_size(TEXT_SM.size)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

/// End padding the input keeps clear of a trigger or clear button.
pub fn autocomplete_end_pad(size: ComboboxSize, show_affordance: bool) -> gpui_kit::Pixels {
    if show_affordance {
        combobox_padding(size).end
    } else {
        px(11.)
    }
}

#[derive(IntoElement)]
pub struct AutocompletePopup {
    open: bool,
    children: Vec<AnyElement>,
}

impl AutocompletePopup {
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

impl Default for AutocompletePopup {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for AutocompletePopup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for AutocompletePopup {
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
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .text_color(theme.foreground)
                    .shadow(tokens::shadow_lg(0.05))
                    .child(tokens::control_lip_overlay(
                        tokens::control_lip(theme.is_dark()),
                        RADIUS_LG,
                    ))
                    .children(self.children),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_trigger_does_not_reserve_end_padding() {
        assert_eq!(autocomplete_end_pad(ComboboxSize::Default, false), px(11.));
        assert_eq!(autocomplete_end_pad(ComboboxSize::Default, true), px(28.));
        assert_eq!(autocomplete_end_pad(ComboboxSize::Sm, true), px(26.));
    }
}
