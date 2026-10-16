//! Port of upstream/apps/web/src/components/T3Wordmark.tsx
//!
//! The wordmark path, `fill="currentColor"`, viewBox `15.5309 37 94.3941 56.96`.

use gpui_kit::{App, Hsla, IntoElement, Rems, RenderOnce, Styled, Window, rems, svg};

use crate::assets::T3_WORDMARK;

/// viewBox width / height.
const ASPECT: f32 = 94.3941 / 56.96;

#[derive(IntoElement)]
pub struct T3Wordmark {
    height: Rems,
    color: Option<Hsla>,
}

impl T3Wordmark {
    pub fn new() -> Self {
        Self {
            height: rems(1.5),
            color: None,
        }
    }

    pub fn height(mut self, height: Rems) -> Self {
        self.height = height;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Default for T3Wordmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for T3Wordmark {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let height = self.height.to_pixels(window.rem_size());
        let width = height * ASPECT;
        let mut mark = svg().path(T3_WORDMARK).flex_shrink_0().h(height).w(width);
        if let Some(color) = self.color {
            mark = mark.text_color(color);
        }
        mark
    }
}
