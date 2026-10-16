//! Port of upstream/apps/web/src/components/ui/group.tsx
//!
//! `flex w-fit` (`flex-col` when vertical). Joined corners and the shared
//! border come from [`group_child_edges`]: a non-last item drops its end
//! radius and end border, a non-first item drops its start radius and start
//! border. Separators are skipped the way `data-slot=separator` is.
//!
//! Not ported: the focus z-index and the dark separator `before` recolor
//! (no focus-within on a static group).

use gpui_kit::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div};

use super::separator::{Orientation, Separator};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupOrientation {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupChildEdges {
    pub round_start: bool,
    pub round_end: bool,
    pub border_start: bool,
    pub border_end: bool,
}

/// Edges for a non-separator child. `index` counts only non-separator siblings.
pub fn group_child_edges(index: usize, count: usize) -> GroupChildEdges {
    GroupChildEdges {
        round_start: index == 0,
        round_end: count == 0 || index + 1 == count,
        border_start: index == 0,
        border_end: count == 0 || index + 1 == count,
    }
}

#[derive(IntoElement)]
pub struct Group {
    orientation: GroupOrientation,
    children: Vec<AnyElement>,
}

impl Group {
    pub fn horizontal() -> Self {
        Self {
            orientation: GroupOrientation::Horizontal,
            children: Vec::new(),
        }
    }

    pub fn vertical() -> Self {
        Self {
            orientation: GroupOrientation::Vertical,
            ..Self::horizontal()
        }
    }
}

impl ParentElement for Group {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Group {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut root = div().flex().w_auto();
        if self.orientation == GroupOrientation::Vertical {
            root = root.flex_col();
        }
        root.children(self.children)
    }
}

/// `GroupSeparator`: a non-interactive separator. Default orientation is vertical.
pub fn group_separator(orientation: Orientation) -> Separator {
    match orientation {
        Orientation::Horizontal => Separator::horizontal(),
        Orientation::Vertical => Separator::vertical(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_every_edge_except_the_outer_ones() {
        let first = group_child_edges(0, 3);
        assert!(first.round_start && first.border_start);
        assert!(!first.round_end && !first.border_end);
        let mid = group_child_edges(1, 3);
        assert!(!mid.round_start && !mid.round_end);
        assert!(!mid.border_start && !mid.border_end);
        let last = group_child_edges(2, 3);
        assert!(!last.round_start && !last.border_start);
        assert!(last.round_end && last.border_end);
    }
}
