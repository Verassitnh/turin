//! Port of upstream/apps/web/src/components/ui/badge.tsx (desktop `sm:` values).

use gpui_kit::Refineable as _;
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement, ParentElement, Pixels, Rems, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, div, px, relative, rems,
};

use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{RADIUS_MD, RADIUS_SM, TEXT_SM, TEXT_XS, tw};
use crate::theme::{ActiveAppTheme, AppTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum BadgeVariant {
    #[default]
    Default,
    Destructive,
    Error,
    Info,
    Outline,
    Secondary,
    Success,
    Warning,
    /// A code-host label tinted from the label color.
    Label(Hsla),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeSize {
    Control,
    #[default]
    Default,
    Lg,
    Sm,
}

struct Spec {
    height: Rems,
    padding_x: Pixels,
    text: Pixels,
    radius: Pixels,
}

impl BadgeSize {
    fn spec(self) -> Spec {
        let rem = 16.;
        match self {
            // sm:h-6 sm:min-w-6 rounded-[var(--control-radius)] px-[calc(--spacing(2)-1px)] sm:text-xs
            Self::Control => Spec {
                height: rems(1.5),
                padding_x: px(7.),
                text: px(TEXT_XS.size.0 * rem),
                radius: RADIUS_MD,
            },
            // sm:h-4.5 px-[calc(--spacing(1)-1px)] sm:text-xs, rounded-sm
            Self::Default => Spec {
                height: rems(1.125),
                padding_x: px(3.),
                text: px(TEXT_XS.size.0 * rem),
                radius: RADIUS_SM,
            },
            // sm:h-5.5 px-[calc(--spacing(1.5)-1px)] sm:text-sm
            Self::Lg => Spec {
                height: rems(1.375),
                padding_x: px(5.),
                text: px(TEXT_SM.size.0 * rem),
                radius: RADIUS_SM,
            },
            // sm:h-4 rounded-[.25rem] sm:text-[.625rem] leading-none
            Self::Sm => Spec {
                height: rems(1.),
                padding_x: px(3.),
                text: px(10.),
                radius: px(4.),
            },
        }
    }
}

fn colors(variant: BadgeVariant, theme: &AppTheme) -> (Hsla, Hsla, Hsla) {
    let dark = theme.is_dark();
    let tint = |color: Hsla| -> Hsla { with_alpha(color, if dark { 0.16 } else { 0.08 }).into() };
    let none: Hsla = with_alpha(tw::BLACK, 0.).into();
    // (background, foreground, border)
    match variant {
        BadgeVariant::Default => (theme.primary, theme.primary_foreground, none),
        BadgeVariant::Destructive => (theme.destructive, tw::WHITE.into(), none),
        BadgeVariant::Error => (tint(theme.destructive), theme.destructive_foreground, none),
        BadgeVariant::Info => (tint(theme.info), theme.info_foreground, none),
        BadgeVariant::Outline => (
            if dark {
                with_alpha(theme.input, 0.32).into()
            } else {
                theme.background
            },
            theme.foreground,
            theme.input,
        ),
        BadgeVariant::Secondary => (theme.secondary, theme.secondary_foreground, none),
        BadgeVariant::Success => (tint(theme.success), theme.success_foreground, none),
        BadgeVariant::Warning => (tint(theme.warning), theme.warning_foreground, none),
        BadgeVariant::Label(label) => {
            let bg = with_alpha(label, if dark { 0.12 } else { 0.08 });
            let fg = color_mix(
                MixSpace::Srgb,
                label,
                if dark { 0.45 } else { 0.30 },
                theme.foreground,
            );
            (bg.into(), fg.into(), none)
        }
    }
}

#[derive(IntoElement)]
pub struct Badge {
    variant: BadgeVariant,
    size: BadgeSize,
    children: Vec<AnyElement>,
    style: StyleRefinement,
}

impl Badge {
    pub fn new() -> Self {
        Self {
            variant: BadgeVariant::Default,
            size: BadgeSize::Default,
            children: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    pub fn label(label: impl Into<SharedString>) -> Self {
        Self::new().child(label.into())
    }

    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: BadgeSize) -> Self {
        self.size = size;
        self
    }
}

impl Default for Badge {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Badge {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Badge {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let spec = self.size.spec();
        let (bg, fg, border) = colors(self.variant, theme);
        let mut el = div()
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap_1()
            .whitespace_nowrap()
            .h(spec.height)
            .min_w(spec.height)
            .px(spec.padding_x)
            .rounded(spec.radius)
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .font_weight(FontWeight::MEDIUM)
            .text_size(spec.text)
            .line_height(relative(1.))
            .children(self.children);
        el.style().refine(&self.style);
        el
    }
}
