//! Port of upstream/apps/web/src/components/ui/toggle-group.tsx
//!
//! Segmented (the default): `gap-0.5 rounded-lg bg-input/40 p-0.5`.
//! Default variant: `gap-0.5`. Outline joins edges (see [`super::group`]).
//! Children are [`super::toggle::Toggle`]s; pass the resolved variant and size
//! from [`resolved_toggle`] so they match the group context.

use gpui_kit::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div};

use super::toggle::{ToggleSize, ToggleVariant};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::RADIUS_LG;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToggleGroupOrientation {
    #[default]
    Horizontal,
    Vertical,
}

pub fn resolved_toggle(
    group_variant: ToggleVariant,
    group_size: Option<ToggleSize>,
    item_variant: Option<ToggleVariant>,
    item_size: Option<ToggleSize>,
) -> (ToggleVariant, ToggleSize) {
    let variant = item_variant.unwrap_or(group_variant);
    let size = item_size.unwrap_or(match (group_size, variant) {
        (Some(size), _) => size,
        (None, ToggleVariant::Segmented) => ToggleSize::Segmented,
        (None, _) => ToggleSize::Default,
    });
    (variant, size)
}

#[derive(IntoElement)]
pub struct ToggleGroup {
    variant: ToggleVariant,
    orientation: ToggleGroupOrientation,
    children: Vec<AnyElement>,
}

impl ToggleGroup {
    pub fn new() -> Self {
        Self {
            variant: ToggleVariant::Segmented,
            orientation: ToggleGroupOrientation::Horizontal,
            children: Vec::new(),
        }
    }

    pub fn variant(mut self, variant: ToggleVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn orientation(mut self, orientation: ToggleGroupOrientation) -> Self {
        self.orientation = orientation;
        self
    }
}

impl Default for ToggleGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for ToggleGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ToggleGroup {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let mut root = div().flex().w_auto();
        if self.orientation == ToggleGroupOrientation::Vertical {
            root = root.flex_col();
        }
        root = match self.variant {
            ToggleVariant::Segmented => root
                .gap(gpui_kit::px(2.))
                .rounded(RADIUS_LG)
                .p(gpui_kit::px(2.))
                .bg(with_alpha(theme.input, 0.4)),
            ToggleVariant::Default => root.gap(gpui_kit::px(2.)),
            _ => root,
        };
        root.children(self.children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmented_group_defaults_items_to_the_segmented_size() {
        assert_eq!(
            resolved_toggle(ToggleVariant::Segmented, None, None, None),
            (ToggleVariant::Segmented, ToggleSize::Segmented)
        );
        assert_eq!(
            resolved_toggle(
                ToggleVariant::Segmented,
                None,
                Some(ToggleVariant::Outline),
                Some(ToggleSize::Sm)
            ),
            (ToggleVariant::Outline, ToggleSize::Sm)
        );
    }
}
