//! Port of upstream/apps/web/src/components/ui/alert.tsx
//!
//! `Alert` is `relative rounded-xl border px-3.5 py-3 text-sm` around an icon column, a content
//! column (`gap-0.5`, titles and descriptions) and an action column. `surface="glass"` floats it
//! over content: the background is the app background at the glass opacity with a 4% tint of the
//! variant's color (GPUI has no backdrop blur, so only the translucent fill is drawn).

use crate::theme::color::solid_over;
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, px, rems,
};

use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{RADIUS_XL, TEXT_SM};
use crate::theme::{ActiveAppTheme, AppTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertVariant {
    #[default]
    Default,
    Error,
    Info,
    Success,
    Warning,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertSurface {
    #[default]
    Default,
    Glass,
}

/// `controlAlignment`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertControlAlignment {
    #[default]
    Center,
    FirstLine,
}

/// Border, fill, text and icon colors of a variant (`alertVariants`).
struct VariantColors {
    border: Option<Hsla>,
    fill: Option<Hsla>,
    text: Hsla,
    description: Hsla,
    icon: Hsla,
    tint: Option<Hsla>,
}

fn variant_colors(variant: AlertVariant, theme: &AppTheme) -> VariantColors {
    let muted = theme.muted_foreground;
    match variant {
        AlertVariant::Default => VariantColors {
            border: None,
            fill: theme
                .is_dark()
                .then(|| with_alpha(theme.input, 0.32).into()),
            text: theme.card_foreground,
            description: muted,
            icon: muted,
            tint: None,
        },
        AlertVariant::Error => VariantColors {
            border: Some(with_alpha(theme.error, 0.32).into()),
            fill: Some(theme.error_surface),
            text: theme.error_foreground,
            description: with_alpha(theme.error_foreground, 0.8).into(),
            icon: theme.error,
            tint: Some(theme.destructive),
        },
        AlertVariant::Info => VariantColors {
            border: Some(with_alpha(theme.info, 0.32).into()),
            fill: Some(solid_over(theme.info, 0.04, theme.background)),
            text: theme.card_foreground,
            description: muted,
            icon: theme.info,
            tint: Some(theme.info),
        },
        AlertVariant::Success => VariantColors {
            border: Some(with_alpha(theme.success, 0.32).into()),
            fill: Some(solid_over(theme.success, 0.04, theme.background)),
            text: theme.card_foreground,
            description: muted,
            icon: theme.success,
            tint: Some(theme.success),
        },
        AlertVariant::Warning => VariantColors {
            border: Some(with_alpha(theme.warning, 0.32).into()),
            fill: Some(theme.warning_surface),
            text: theme.warning_foreground,
            description: with_alpha(theme.warning_foreground, 0.8).into(),
            icon: theme.warning,
            tint: Some(theme.warning),
        },
    }
}

/// The icon color a variant gives its glyph (`[&_svg]:text-*`).
pub fn alert_icon_color(variant: AlertVariant, theme: &AppTheme) -> Hsla {
    variant_colors(variant, theme).icon
}

/// `alert-glass`: the background at `--glass-opacity` (80%), over which the variant's color is
/// mixed at 4%.
fn glass_fill(tint: Option<Hsla>, theme: &AppTheme) -> Hsla {
    let base = theme.background;
    match tint {
        Some(tint) => color_mix(MixSpace::Srgb, tint, 0.04, base).into(),
        None => base,
    }
}

#[derive(IntoElement)]
pub struct Alert {
    variant: AlertVariant,
    surface: AlertSurface,
    alignment: AlertControlAlignment,
    icon: Option<AnyElement>,
    content: Vec<AnyElement>,
    action: Vec<AnyElement>,
}

impl Alert {
    pub fn new() -> Self {
        Self {
            variant: AlertVariant::Default,
            surface: AlertSurface::Default,
            alignment: AlertControlAlignment::Center,
            icon: None,
            content: Vec::new(),
            action: Vec::new(),
        }
    }

    pub fn variant(mut self, variant: AlertVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn surface(mut self, surface: AlertSurface) -> Self {
        self.surface = surface;
        self
    }

    pub fn control_alignment(mut self, alignment: AlertControlAlignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    /// An `AlertTitle`, `AlertDescription` or any other content child.
    pub fn content(mut self, child: impl IntoElement) -> Self {
        self.content.push(child.into_any_element());
        self
    }

    /// An `AlertAction` child.
    pub fn action(mut self, child: impl IntoElement) -> Self {
        self.action.push(child.into_any_element());
        self
    }
}

impl Default for Alert {
    fn default() -> Self {
        Self::new()
    }
}

/// `AlertTitle`: `font-medium`.
pub fn alert_title(text: impl Into<SharedString>) -> gpui_kit::Div {
    div().font_weight(FontWeight::MEDIUM).child(text.into())
}

/// `AlertDescription`: `flex flex-col gap-2.5 text-muted-foreground`; the variant recolors it.
pub fn alert_description(color: Hsla) -> gpui_kit::Div {
    div().flex().flex_col().gap(rems(0.625)).text_color(color)
}

/// The description color of a variant, for [`alert_description`].
pub fn alert_description_color(variant: AlertVariant, theme: &AppTheme) -> Hsla {
    variant_colors(variant, theme).description
}

impl RenderOnce for Alert {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let colors = variant_colors(self.variant, &theme);
        let first_line = self.alignment == AlertControlAlignment::FirstLine;
        let has_action = !self.action.is_empty();
        let fill = match self.surface {
            AlertSurface::Glass => Some(glass_fill(colors.tint, &theme)),
            AlertSurface::Default => colors.fill,
        };
        let mut root = div()
            .relative()
            .rounded(RADIUS_XL)
            .border_1()
            .border_color(colors.border.unwrap_or(theme.border))
            .px(px(14.))
            .py(px(12.))
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .text_color(colors.text);
        if let Some(fill) = fill {
            root = root.bg(fill);
        }
        let mut row = div().flex().gap_2();
        row = if first_line {
            row.items_start()
        } else {
            row.items_center()
        };
        if first_line && has_action {
            // `min-h-7 pt-1 sm:min-h-6 sm:pt-0.5`.
            row = row.min_h(rems(1.5)).pt(px(2.));
        }
        if let Some(icon) = self.icon {
            // `h-lh w-4` (first line) or `size-4`.
            let slot = div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .w(rems(1.));
            row = row.child(if first_line {
                slot.h(TEXT_SM.line_height).child(icon)
            } else {
                slot.h(rems(1.)).child(icon)
            });
        }
        if !self.content.is_empty() {
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.125))
                    .min_w_0()
                    .flex_1()
                    .children(self.content),
            );
        }
        if has_action {
            let column = div().flex().flex_shrink_0().items_center().gap_1();
            row = row.child(
                if first_line {
                    column.h(TEXT_SM.line_height).self_start()
                } else {
                    column.self_center()
                }
                .children(self.action),
            );
        }
        root.child(row)
    }
}
