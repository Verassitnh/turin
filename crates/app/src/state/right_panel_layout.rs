//! Port of upstream/apps/web/src/rightPanelLayout.ts
//!
//! Upstream exports a CSS media query string, which `ChatView` feeds to
//! `useMediaQuery` to pick the sheet (popover) presentation over the inline
//! column. GPUI has no media queries, so the query becomes a width threshold
//! tested against the window's viewport width.

/// `RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY` (`"(max-width: 980px)"`): the
/// largest viewport width, in px, at which the right panel uses the sheet
/// layout. `max-width` is inclusive.
pub const RIGHT_PANEL_INLINE_LAYOUT_MAX_WIDTH: f32 = 980.;

/// `ThreadPanelPresentation`: the inline column or the popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadPanelPresentation {
    Inline,
    Popover,
}

/// `useMediaQuery(RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY)` (`shouldUsePlanSidebarSheet`
/// in ChatView.tsx) evaluated for a viewport width.
pub fn matches_right_panel_inline_layout_query(viewport_width: f32) -> bool {
    viewport_width <= RIGHT_PANEL_INLINE_LAYOUT_MAX_WIDTH
}

/// The presentation the right panel takes at a viewport width: the popover
/// where the media query matches, the inline column otherwise.
pub fn right_panel_presentation_for_width(viewport_width: f32) -> ThreadPanelPresentation {
    if matches_right_panel_inline_layout_query(viewport_width) {
        ThreadPanelPresentation::Popover
    } else {
        ThreadPanelPresentation::Inline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_threshold_is_980_inclusive() {
        assert!(matches_right_panel_inline_layout_query(980.));
        assert!(matches_right_panel_inline_layout_query(600.));
        assert!(!matches_right_panel_inline_layout_query(981.));
    }

    #[test]
    fn narrow_viewports_use_the_popover() {
        assert_eq!(
            right_panel_presentation_for_width(980.),
            ThreadPanelPresentation::Popover
        );
        assert_eq!(
            right_panel_presentation_for_width(1280.),
            ThreadPanelPresentation::Inline
        );
    }
}
