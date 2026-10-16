//! Port of upstream/apps/web/src/lib/selectionActions.ts (positioning and timing).
//!
//! The DOM observer `observeSelectionActions` (pointer, selectionchange and focus
//! gesture state) is not ported yet. It is event glue for the DOM selection model;
//! the GPUI text views will deliver their own selection events.

/// Window in which a double- or triple-click finishes before selection actions open.
pub const SELECTION_MULTI_CLICK_INTERVAL_MS: f64 = 500.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionActionPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionActionBounds {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionActionRect {
    pub right: f64,
    pub bottom: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionActionViewport {
    pub width: f64,
    pub height: f64,
}

/// Where the actions open: at the release point when there is one, otherwise under the
/// selection's end (or the surface's top-right when there is no selection rect). Clamped
/// to the surface and to an 8 px inset of the viewport.
pub fn resolve_selection_action_position(
    bounds: SelectionActionBounds,
    selection_rect: Option<SelectionActionRect>,
    pointer: Option<SelectionActionPoint>,
    viewport: SelectionActionViewport,
) -> SelectionActionPoint {
    let preferred = pointer.unwrap_or(SelectionActionPoint {
        x: selection_rect.map_or(bounds.left + bounds.width - 140.0, |rect| rect.right),
        y: selection_rect.map_or(bounds.top + 12.0, |rect| rect.bottom + 4.0),
    });
    SelectionActionPoint {
        x: 8.0_f64.max(
            (bounds.left + bounds.width)
                .min(preferred.x.max(bounds.left))
                .min(viewport.width - 8.0),
        ),
        y: 8.0_f64.max(
            (bounds.top + bounds.height)
                .min(preferred.y.max(bounds.top))
                .min(viewport.height - 8.0),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: SelectionActionBounds = SelectionActionBounds {
        left: 100.0,
        top: 50.0,
        width: 500.0,
        height: 220.0,
    };
    const VIEWPORT: SelectionActionViewport = SelectionActionViewport {
        width: 1024.0,
        height: 768.0,
    };
    const SELECTION: Option<SelectionActionRect> = Some(SelectionActionRect {
        right: 260.0,
        bottom: 140.0,
    });

    fn point(x: f64, y: f64) -> SelectionActionPoint {
        SelectionActionPoint { x, y }
    }

    #[test]
    fn prefers_the_cursor_release_over_the_selections_bounding_box() {
        assert_eq!(
            resolve_selection_action_position(
                BOUNDS,
                SELECTION,
                Some(point(520.0, 200.0)),
                VIEWPORT
            ),
            point(520.0, 200.0)
        );
    }

    #[test]
    fn anchors_keyboard_selections_just_below_the_text_end() {
        assert_eq!(
            resolve_selection_action_position(BOUNDS, SELECTION, None, VIEWPORT),
            point(260.0, 144.0)
        );
    }

    #[test]
    fn keeps_an_outside_release_within_the_surface() {
        assert_eq!(
            resolve_selection_action_position(
                BOUNDS,
                SELECTION,
                Some(point(720.0, 340.0)),
                VIEWPORT
            ),
            point(600.0, 270.0)
        );
        assert_eq!(
            resolve_selection_action_position(BOUNDS, SELECTION, Some(point(40.0, 20.0)), VIEWPORT),
            point(100.0, 50.0)
        );
    }

    #[test]
    fn keeps_the_anchor_inside_the_browser_even_if_the_surface_extends_beyond_it() {
        let bounds = SelectionActionBounds {
            left: -100.0,
            top: -100.0,
            width: 2000.0,
            height: 2000.0,
        };
        assert_eq!(
            resolve_selection_action_position(
                bounds,
                SELECTION,
                Some(point(1800.0, -50.0)),
                VIEWPORT
            ),
            point(1016.0, 8.0)
        );
    }

    #[test]
    fn falls_back_to_the_surface_corner_without_a_selection_rect() {
        assert_eq!(
            resolve_selection_action_position(BOUNDS, None, None, VIEWPORT),
            point(460.0, 62.0)
        );
    }
}
