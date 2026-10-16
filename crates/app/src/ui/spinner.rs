//! Port of upstream/apps/web/src/components/ui/spinner.tsx
//!
//! `LoaderCircleIcon` spinning once a second; `muted` tone `text-muted-foreground`.
//! Sizes: xs 12px, sm 14px, md 16px, lg 20px.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, App, ElementId, Hsla, IntoElement, RenderOnce, Styled as _,
    Transformation, Window, percentage, rems, svg,
};

use super::icon::IconName;
use crate::theme::ActiveAppTheme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpinnerSize {
    Xs,
    Sm,
    #[default]
    Md,
    Lg,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpinnerTone {
    #[default]
    Current,
    Muted,
}

#[derive(IntoElement)]
pub struct Spinner {
    id: ElementId,
    size: SpinnerSize,
    tone: SpinnerTone,
    color: Option<Hsla>,
}

impl Spinner {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            size: SpinnerSize::Md,
            tone: SpinnerTone::Current,
            color: None,
        }
    }

    pub fn size(mut self, size: SpinnerSize) -> Self {
        self.size = size;
        self
    }

    pub fn tone(mut self, tone: SpinnerTone) -> Self {
        self.tone = tone;
        self
    }

    /// The surrounding text color, when `Current` should not read the default.
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let size = match self.size {
            SpinnerSize::Xs => rems(0.75),
            SpinnerSize::Sm => rems(0.875),
            SpinnerSize::Md => rems(1.),
            SpinnerSize::Lg => rems(1.25),
        };
        let color = match self.tone {
            SpinnerTone::Muted => theme.muted_foreground,
            SpinnerTone::Current => self.color.unwrap_or_else(|| window.text_style().color),
        };
        svg()
            .path(IconName::LoaderCircle.path())
            .flex_shrink_0()
            .size(size)
            .text_color(color)
            .with_animation(
                self.id,
                Animation::new(Duration::from_millis(1000)).repeat(),
                |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
            )
    }
}
