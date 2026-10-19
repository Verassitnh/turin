//! Port of upstream/apps/web/src/components/ui/tooltip.tsx (`TooltipPopup`).
//!
//! GPUI positions tooltips itself (`.tooltip(..)` on a stateful element), so
//! only the popup surface is ported: `rounded-md border bg-popover text-xs
//! shadow-md/5 px-2 py-1 max-w-80 leading-snug`, with the `glass` and `code`
//! variants. Upstream's `side`/`align` and the scroll-dismiss area have no
//! GPUI equivalent yet.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, px, rems,
};

use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, LEADING_RELAXED, LEADING_SNUG, RADIUS_MD, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TooltipVariant {
    #[default]
    Default,
    /// `dropdown-glass` surface.
    Glass,
    /// Monospace content that breaks anywhere, for paths and commands.
    Code,
}

#[derive(Clone, Debug)]
pub struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    variant: TooltipVariant,
}

impl Tooltip {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            shortcut: None,
            variant: TooltipVariant::Default,
        }
    }

    /// A keyboard shortcut label, appended as plain text the way upstream
    /// builds its labels: `Toggle main sidebar (⌘B)`.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn variant(mut self, variant: TooltipVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn build(self, _: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| TooltipView(self)).into()
    }
}

impl From<&'static str> for Tooltip {
    fn from(text: &'static str) -> Self {
        Self::new(text)
    }
}

impl From<SharedString> for Tooltip {
    fn from(text: SharedString) -> Self {
        Self::new(text)
    }
}

impl From<String> for Tooltip {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// Dropdown surface: solid popover fill with a solid border (no glass).
pub fn dropdown_glass_background(theme: &AppTheme) -> gpui_kit::Hsla {
    theme.popover
}

pub fn dropdown_glass_border(theme: &AppTheme) -> gpui_kit::Hsla {
    theme.border
}

struct TooltipView(Tooltip);

impl Render for TooltipView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        let tooltip = &self.0;
        let code = tooltip.variant == TooltipVariant::Code;
        let glass = tooltip.variant == TooltipVariant::Glass;
        let label: SharedString = match &tooltip.shortcut {
            Some(shortcut) => format!("{} ({shortcut})", tooltip.text).into(),
            None => tooltip.text.clone(),
        };
        // Offset from the pointer, as Base UI's `sideOffset` does from the trigger.
        div().pl(px(4.)).pt(px(4.)).child(
            div()
                .relative()
                .flex()
                .items_center()
                .gap_2()
                .rounded(RADIUS_MD)
                .border_1()
                .px_2()
                .py_1()
                .text_size(TEXT_XS.size)
                .text_color(theme.popover_foreground)
                .font_family(theme.fonts.sans.clone())
                .map(|this| {
                    if glass {
                        this.bg(dropdown_glass_background(theme))
                            .border_color(dropdown_glass_border(theme))
                            .shadow(tokens::shadow_xl(0.25))
                    } else {
                        this.bg(theme.popover)
                            .border_color(theme.border)
                            .shadow(tokens::shadow_md(0.05))
                    }
                })
                .map(|this| {
                    if code {
                        this.max_w(rems(30.))
                            .font_family(theme.fonts.mono.clone())
                            .text_size(px(11.))
                            .line_height(rems(11. / 16. * LEADING_RELAXED))
                    } else {
                        this.max_w(rems(20.))
                            .line_height(rems(TEXT_XS.size.0 * LEADING_SNUG))
                    }
                })
                .child(label)
                // `before:shadow-[0_1px_black/4%]` / dark `0_-1px_white/6%`;
                // the glass variant hides it (`before:hidden`).
                .when(!glass, |this| {
                    this.child(tokens::control_lip_overlay(
                        tokens::control_lip(theme.is_dark()),
                        RADIUS_MD,
                    ))
                }),
        )
    }
}
