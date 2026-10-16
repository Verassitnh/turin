//! Port of upstream/apps/web/src/components/ui/preview-card.tsx
//!
//! The popup is `rounded-lg border bg-popover text-popover-foreground
//! shadow-lg`, offset 6px from the trigger. GPUI has no hover-card portal, so
//! the popup is a positioned child of the trigger (`side` top by default).
//! Enter/exit scale is not ported (no element transforms).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{self, RADIUS_LG};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewSide {
    #[default]
    Top,
    Bottom,
}

#[derive(IntoElement)]
pub struct PreviewCard {
    open: bool,
    side: PreviewSide,
    trigger: AnyElement,
    popup: Option<AnyElement>,
}

impl PreviewCard {
    pub fn new(trigger: impl IntoElement) -> Self {
        Self {
            open: false,
            side: PreviewSide::Top,
            trigger: trigger.into_any_element(),
            popup: None,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn side(mut self, side: PreviewSide) -> Self {
        self.side = side;
        self
    }

    pub fn popup(mut self, popup: impl IntoElement) -> Self {
        self.popup = Some(popup.into_any_element());
        self
    }
}

impl RenderOnce for PreviewCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let side = self.side;
        div().relative().child(self.trigger).when_some(
            self.popup.filter(|_| self.open),
            |this, popup| {
                this.child(
                    div()
                        .absolute()
                        .when(side == PreviewSide::Top, |this| {
                            this.bottom_full().mb(px(6.))
                        })
                        .when(side == PreviewSide::Bottom, |this| {
                            this.top_full().mt(px(6.))
                        })
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .shadow(tokens::shadow_lg(0.1))
                        .child(popup),
                )
            },
        )
    }
}
