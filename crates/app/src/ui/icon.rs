//! Lucide icons, the way upstream renders `lucide-react` glyphs: a `size-*`
//! box tinted with the text color. Icons must be listed in
//! [`crate::assets::AppIcons`] to be embedded.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Hsla, InteractiveElement as _, IntoElement, Rems, RenderOnce, SharedString, Styled,
    Transformation, Window, percentage, rems, svg,
};

pub use gpui_kit::assets::IconName;

/// What an [`Icon`] draws: a catalog icon or an app asset path.
#[derive(Clone, Debug)]
enum Source {
    Named(IconName),
    Path(SharedString),
}

#[derive(IntoElement)]
pub struct Icon {
    source: Source,
    size: Rems,
    color: Option<Hsla>,
    group_hover: Option<(SharedString, Hsla)>,
    rotated_half: bool,
}

impl Icon {
    /// Defaults to `size-4`, the size upstream gives most control icons.
    pub fn new(name: IconName) -> Self {
        Self {
            source: Source::Named(name),
            size: rems(1.),
            color: None,
            group_hover: None,
            rotated_half: false,
        }
    }

    /// A monochrome SVG from [`crate::assets`] (e.g. a brand glyph).
    pub fn path(path: impl Into<SharedString>) -> Self {
        Self {
            source: Source::Path(path.into()),
            ..Self::new(IconName::Search)
        }
    }

    /// Tailwind `size-N` in rems (`size-3.5` → `rems(0.875)`).
    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// `rotate-180`.
    pub fn rotate_180(mut self, rotated: bool) -> Self {
        self.rotated_half = rotated;
        self
    }

    /// `group-hover:text-*`: recolor while the named group is hovered.
    pub fn group_hover_color(mut self, group: impl Into<SharedString>, color: Hsla) -> Self {
        self.group_hover = Some((group.into(), color));
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let path = match self.source {
            Source::Named(name) => name.path(),
            Source::Path(path) => path,
        };
        svg()
            .path(path)
            .flex_shrink_0()
            .size(self.size)
            .text_color(color)
            .when(self.rotated_half, |this| {
                this.with_transformation(Transformation::rotate(percentage(0.5)))
            })
            .when_some(self.group_hover, |this, (group, hover)| {
                this.group_hover(group, move |style| style.text_color(hover))
            })
    }
}
