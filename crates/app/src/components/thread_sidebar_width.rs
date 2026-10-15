//! Port of upstream/apps/web/src/components/threadSidebarWidth.ts

#[allow(dead_code)] // persistence lands with settings.
pub const THREAD_SIDEBAR_WIDTH_STORAGE_KEY: &str = "chat_thread_sidebar_width";
pub const THREAD_SIDEBAR_DEFAULT_WIDTH: f32 = 16. * 16.;
pub const THREAD_SIDEBAR_MIN_WIDTH: f32 = 13. * 16.;
pub const THREAD_MAIN_CONTENT_MIN_WIDTH: f32 = 40. * 16.;

/// The brand's measured width can raise the minimum so "T3 Code" never clips.
#[allow(dead_code)] // brand-width probe not wired yet (see PORTING Deviations).
pub fn resolve_thread_sidebar_minimum_width(brand_width: f32) -> f32 {
    THREAD_SIDEBAR_MIN_WIDTH.max(brand_width.ceil())
}

pub fn resolve_thread_sidebar_maximum_width(viewport_width: f32, minimum_width: f32) -> f32 {
    minimum_width.max(viewport_width.floor() - THREAD_MAIN_CONTENT_MIN_WIDTH)
}

pub fn clamp_thread_sidebar_width(width: f32, minimum_width: f32, maximum_width: f32) -> f32 {
    maximum_width.min(minimum_width.max(width))
}

pub fn resolve_initial_thread_sidebar_width(stored_width: Option<f32>, viewport_width: f32) -> f32 {
    let preferred_width = match stored_width {
        None => THREAD_SIDEBAR_DEFAULT_WIDTH,
        Some(stored) => THREAD_SIDEBAR_MIN_WIDTH.max(stored),
    };
    preferred_width.min(resolve_thread_sidebar_maximum_width(
        viewport_width,
        THREAD_SIDEBAR_MIN_WIDTH,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // describe("resolveThreadSidebarMinimumWidth")

    #[test]
    fn keeps_the_default_minimum_when_the_brand_fits() {
        assert_eq!(
            resolve_thread_sidebar_minimum_width(0.),
            THREAD_SIDEBAR_MIN_WIDTH
        );
        assert_eq!(
            resolve_thread_sidebar_minimum_width(194.),
            THREAD_SIDEBAR_MIN_WIDTH
        );
    }

    #[test]
    fn grows_to_a_brand_wider_than_the_default_rounding_up() {
        assert_eq!(resolve_thread_sidebar_minimum_width(237.2), 238.);
    }

    // describe("resolveThreadSidebarMaximumWidth")

    #[test]
    fn never_drops_below_a_raised_minimum_on_a_narrow_viewport() {
        assert_eq!(resolve_thread_sidebar_maximum_width(800., 238.), 238.);
        assert_eq!(resolve_thread_sidebar_maximum_width(1200., 238.), 560.);
    }

    // describe("clampThreadSidebarWidth")

    #[test]
    fn widens_a_stored_width_below_a_raised_minimum() {
        assert_eq!(clamp_thread_sidebar_width(208., 238., 560.), 238.);
    }

    #[test]
    fn keeps_widths_inside_the_range_and_caps_wide_ones() {
        assert_eq!(clamp_thread_sidebar_width(300., 238., 560.), 300.);
        assert_eq!(clamp_thread_sidebar_width(900., 238., 560.), 560.);
    }

    // Turin: resolveInitialThreadSidebarWidth has no upstream test.

    #[test]
    fn initial_width_defaults_and_respects_minimum() {
        assert_eq!(resolve_initial_thread_sidebar_width(None, 1400.), 256.);
        assert_eq!(
            resolve_initial_thread_sidebar_width(Some(100.), 1400.),
            208.
        );
        assert_eq!(resolve_initial_thread_sidebar_width(Some(400.), 900.), 260.);
    }
}
