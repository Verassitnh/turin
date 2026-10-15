//! Port of upstream/apps/web/src/components/ui/kbd.tsx
//!
//! `inline-flex h-5 min-w-5 items-center justify-center gap-1 rounded bg-muted
//! px-1 font-medium font-sans text-muted-foreground text-xs`.

use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, Keystroke, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{RADIUS_DEFAULT, TEXT_XS};

#[derive(IntoElement)]
pub struct Kbd {
    label: SharedString,
}

impl Kbd {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
        }
    }

    /// Formats a keystroke the platform way (`⌘⇧B` on macOS).
    pub fn keystroke(keystroke: &Keystroke) -> Self {
        Self::new(gpui_kit::component::kbd::Kbd::format(keystroke))
    }
}

impl RenderOnce for Kbd {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap_1()
            .h(rems(1.25))
            .min_w(rems(1.25))
            .px_1()
            .rounded(RADIUS_DEFAULT)
            .bg(theme.muted)
            .font_family(theme.fonts.sans.clone())
            .font_weight(FontWeight::MEDIUM)
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .text_color(theme.muted_foreground)
            .child(self.label)
    }
}

/// `KbdGroup`: `inline-flex items-center gap-1`.
#[derive(IntoElement, Default)]
pub struct KbdGroup {
    children: Vec<AnyElement>,
}

impl KbdGroup {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for KbdGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for KbdGroup {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().flex().items_center().gap_1().children(self.children)
    }
}
