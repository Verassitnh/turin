//! Port of upstream/apps/web/src/hooks/usePreviewPanelInlineSize.ts
//!
//! Only the sizing rules; the owning view re-measures on every layout, which
//! replaces the resize observer.

pub const PREVIEW_PANEL_WIDTH_STORAGE_KEY: &str = "t3code:preview-panel-width";
pub const PREVIEW_PANEL_MIN_WIDTH: f32 = 360.;
/// Upper bound as a fraction of the viewport; only binds on wide screens.
/// On narrow windows the container clamp below is what preserves the
/// sibling column's usable width.
pub const PREVIEW_PANEL_MAX_WIDTH_FRACTION: f32 = 0.7;
/// Floor for the column beside the panel: below this the chat column's
/// composer overflows, so the panel yields instead.
pub const SIBLING_COLUMN_MIN_WIDTH: f32 = 360.;
pub const PREVIEW_PANEL_DEFAULT_WIDTH: f32 = 540.;

pub fn get_preview_panel_max_width(viewport_width: f32, container_width: Option<f32>) -> f32 {
    let fraction_cap = (viewport_width * PREVIEW_PANEL_MAX_WIDTH_FRACTION).floor();
    let container_cap = match container_width {
        None => f32::INFINITY,
        Some(width) => width.floor() - SIBLING_COLUMN_MIN_WIDTH,
    };
    // Never below the panel's own minimum: when the row cannot fit both
    // columns' minimums the sibling yields, and the width clamp must not see
    // max < min.
    PREVIEW_PANEL_MIN_WIDTH.max(fraction_cap.min(container_cap))
}

/// `useResizableWidth`'s clamp.
pub fn clamp_preview_panel_width(width: f32, max_width: f32) -> f32 {
    width.clamp(
        PREVIEW_PANEL_MIN_WIDTH,
        max_width.max(PREVIEW_PANEL_MIN_WIDTH),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_width_takes_the_tighter_cap() {
        assert_eq!(get_preview_panel_max_width(2000., None), 1400.);
        assert_eq!(get_preview_panel_max_width(2000., Some(1000.)), 640.);
    }

    #[test]
    fn max_width_never_drops_below_minimum() {
        assert_eq!(get_preview_panel_max_width(400., Some(500.)), 360.);
    }

    #[test]
    fn clamp_keeps_width_in_range() {
        assert_eq!(clamp_preview_panel_width(100., 800.), 360.);
        assert_eq!(clamp_preview_panel_width(900., 800.), 800.);
    }
}
