//! Port of upstream/apps/web/src/components/ui/standalone-page.tsx
//!
//! Full-window centered card (`max-w-xl rounded-2xl border border-border/80
//! shadow-2xl`) over a wash. Upstream's radial glows have no GPUI equivalent
//! (backgrounds are solid or linear), so each tone is a top-down linear fade
//! of the same mix plus the 145° background wash.

use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, linear_color_stop, linear_gradient, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::{self, MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, LEADING_RELAXED, RADIUS_2XL, TEXT_SM};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandaloneTone {
    Pairing,
    Error,
    Brand,
}

fn tone_color(tone: StandaloneTone) -> gpui_kit::Rgba {
    match tone {
        StandaloneTone::Pairing => tokens::tw::EMERALD_500,
        // Tailwind v4 `red-500`.
        StandaloneTone::Error => color::hex(0xfb2c36),
        StandaloneTone::Brand => tokens::tw::BLUE_500,
    }
}

fn tone_alpha(tone: StandaloneTone) -> f32 {
    match tone {
        StandaloneTone::Pairing => 0.14,
        StandaloneTone::Error => 0.16,
        StandaloneTone::Brand => 0.12,
    }
}

#[derive(IntoElement)]
pub struct StandalonePage {
    tone: StandaloneTone,
    masthead: Option<AnyElement>,
    children: Vec<AnyElement>,
}

impl StandalonePage {
    pub fn new(tone: StandaloneTone) -> Self {
        Self {
            tone,
            masthead: None,
            children: Vec::new(),
        }
    }

    pub fn masthead(mut self, masthead: impl IntoElement) -> Self {
        self.masthead = Some(masthead.into_any_element());
        self
    }
}

impl ParentElement for StandalonePage {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for StandalonePage {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let wash_weight = if self.tone == StandaloneTone::Brand {
            0.94
        } else {
            0.90
        };
        let wash_stop = if self.tone == StandaloneTone::Brand {
            0.62
        } else {
            0.55
        };
        let wash = color_mix(
            MixSpace::Srgb,
            theme.background,
            wash_weight,
            tokens::tw::BLACK,
        );
        let glow = with_alpha(tone_color(self.tone), tone_alpha(self.tone));
        let card_alpha = if self.tone == StandaloneTone::Brand {
            0.94
        } else {
            0.90
        };
        div()
            .relative()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .px(rems(1.5))
            .py(rems(2.5))
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(rems(if self.tone == StandaloneTone::Brand {
                        18.
                    } else {
                        11.
                    }))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(glow, 0.),
                        linear_color_stop(with_alpha(glow, 0.), 1.),
                    )),
            )
            .child(div().absolute().inset_0().bg(linear_gradient(
                145.,
                linear_color_stop(wash, 0.),
                linear_color_stop(theme.background, wash_stop),
            )))
            .child(
                div()
                    .relative()
                    .w_full()
                    .max_w(rems(36.))
                    .rounded(RADIUS_2XL)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.8))
                    .bg(with_alpha(theme.card, card_alpha))
                    .shadow(tokens::shadow_2xl(0.2))
                    .overflow_hidden()
                    .children(self.masthead)
                    .child(div().p(rems(2.)).children(self.children)),
            )
    }
}

pub struct StandalonePageHeader {
    pub eyebrow: SharedString,
    pub title: SharedString,
    pub description: SharedString,
}

impl StandalonePageHeader {
    pub fn render(self, cx: &App) -> gpui_kit::Div {
        let theme = cx.app_theme();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(tokens::TEXT_2XS.size)
                    .line_height(tokens::TEXT_2XS.line_height)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(self.eyebrow.to_string().to_uppercase())),
            )
            .child(
                div()
                    .mt(rems(0.75))
                    .text_size(rems(1.875))
                    .line_height(rems(2.25))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(TEXT_SM.size)
                    .line_height(rems(TEXT_SM.size.0 * LEADING_RELAXED))
                    .text_color(theme.muted_foreground)
                    .child(self.description),
            )
    }
}
