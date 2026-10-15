//! Port of upstream/apps/web/src/components/ui/separator.tsx
//!
//! `shrink-0 bg-border`, `h-px w-full` horizontally, `w-px self-stretch`
//! vertically.

use gpui_kit::Refineable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Hsla, IntoElement, RenderOnce, StyleRefinement, Styled, Window, div, px};

use crate::theme::ActiveAppTheme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(IntoElement)]
pub struct Separator {
    orientation: Orientation,
    color: Option<Hsla>,
    style: StyleRefinement,
}

impl Separator {
    pub fn horizontal() -> Self {
        Self {
            orientation: Orientation::Horizontal,
            color: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn vertical() -> Self {
        Self {
            orientation: Orientation::Vertical,
            ..Self::horizontal()
        }
    }

    /// Override `bg-border` (e.g. `bg-sidebar-border/60`).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Styled for Separator {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Separator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.app_theme().border);
        let mut el = div()
            .flex_shrink_0()
            .bg(color)
            .map(|this| match self.orientation {
                Orientation::Horizontal => this.h(px(1.)).w_full(),
                Orientation::Vertical => this.w(px(1.)).self_stretch(),
            });
        el.style().refine(&self.style);
        el
    }
}
