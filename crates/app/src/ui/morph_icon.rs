//! Port of upstream/apps/web/src/components/MorphIcon.tsx
//!
//! Upstream morphs between lucide shapes with `morphicons`. GPUI has no path
//! morph, so a change of icon swaps the glyph. `reducedMotion` is therefore
//! always on.

use gpui_kit::{App, Hsla, IntoElement, Rems, RenderOnce, Window, rems};

use super::icon::{Icon, IconName};

#[derive(IntoElement)]
pub struct MorphIcon {
    icon: IconName,
    size: Rems,
    color: Option<Hsla>,
}

impl MorphIcon {
    pub fn new(icon: IconName) -> Self {
        Self {
            icon,
            size: rems(1.),
            color: None,
        }
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for MorphIcon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut icon = Icon::new(self.icon).size(self.size);
        if let Some(color) = self.color {
            icon = icon.color(color);
        }
        icon
    }
}
