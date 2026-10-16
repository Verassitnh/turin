//! Port of upstream/apps/web/src/components/ui/alert.tsx (`surface="default"`)
//!
//! `relative rounded-xl border px-3.5 py-3 text-card-foreground text-sm` with
//! an optional leading icon column and a `gap-0.5` content column. Variants
//! tint the border (`/32`), surface, text and icon the way upstream's cva
//! table does. Not ported: the action slot and the `glass` surface.

use gpui_kit::{
    AnyElement, App, Div, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
    px, rems,
};

use super::icon::{Icon, IconName};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_XL, TEXT_SM};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertVariant {
    #[default]
    Default,
    Error,
    Info,
    Success,
    Warning,
}

#[derive(IntoElement)]
pub struct Alert {
    variant: AlertVariant,
    icon: Option<IconName>,
    /// `controlAlignment="first-line"`: the icon aligns with the first text line.
    first_line: bool,
    content: Vec<AnyElement>,
}

impl Alert {
    pub fn new(variant: AlertVariant) -> Self {
        Self {
            variant,
            icon: None,
            first_line: false,
            content: Vec::new(),
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn first_line(mut self) -> Self {
        self.first_line = true;
        self
    }

    /// An `AlertTitle`: `font-medium`.
    pub fn title(mut self, title: impl IntoElement) -> Self {
        self.content.push(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(title)
                .into_any_element(),
        );
        self
    }

    /// An `AlertDescription`: muted (or the variant's text at 80%).
    pub fn description(mut self, description: impl IntoElement, cx: &App) -> Self {
        let theme = cx.app_theme();
        let color = match self.variant {
            AlertVariant::Error => with_alpha(theme.error_foreground, 0.8).into(),
            AlertVariant::Warning => with_alpha(theme.warning_foreground, 0.8).into(),
            _ => theme.muted_foreground,
        };
        self.content.push(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.625))
                .text_color(color)
                .child(description)
                .into_any_element(),
        );
        self
    }
}

impl RenderOnce for Alert {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let dark = theme.is_dark();
        let (border, background, text, icon_color): (_, _, _, gpui_kit::Hsla) = match self.variant {
            AlertVariant::Default => (
                theme.border,
                if dark {
                    with_alpha(theme.input, 0.32).into()
                } else {
                    gpui_kit::transparent_black()
                },
                theme.card_foreground,
                theme.muted_foreground,
            ),
            AlertVariant::Error => (
                with_alpha(theme.error, 0.32).into(),
                theme.error_surface,
                theme.error_foreground,
                theme.error,
            ),
            AlertVariant::Info => (
                with_alpha(theme.info, 0.32).into(),
                with_alpha(theme.info, 0.04).into(),
                theme.card_foreground,
                theme.info,
            ),
            AlertVariant::Success => (
                with_alpha(theme.success, 0.32).into(),
                with_alpha(theme.success, 0.04).into(),
                theme.card_foreground,
                theme.success,
            ),
            AlertVariant::Warning => (
                with_alpha(theme.warning, 0.32).into(),
                theme.warning_surface,
                theme.warning_foreground,
                theme.warning,
            ),
        };
        let row = div().flex().gap_2();
        let row = if self.first_line {
            row.items_start()
        } else {
            row.items_center()
        };
        div()
            .relative()
            .rounded(RADIUS_XL)
            .border_1()
            .border_color(border)
            .bg(background)
            .text_color(text)
            .text_size(TEXT_SM.size)
            .px(px(14.))
            .py_3()
            .child(
                row.children(self.icon.map(|icon| -> Div {
                    let slot = div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .w(px(16.));
                    // `h-lh` for the first-line alignment: one text line tall.
                    let slot = if self.first_line {
                        slot.h(TEXT_SM.line_height)
                    } else {
                        slot.size(px(16.))
                    };
                    slot.child(Icon::new(icon).size(rems(1.)).color(icon_color))
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .children(self.content),
                ),
            )
    }
}
