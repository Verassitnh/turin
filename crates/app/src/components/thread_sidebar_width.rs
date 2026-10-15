//! Port of upstream/apps/web/src/components/threadSidebarWidth.ts

pub const THREAD_SIDEBAR_WIDTH_STORAGE_KEY: &str = "chat_thread_sidebar_width";
pub const THREAD_SIDEBAR_DEFAULT_WIDTH: f32 = 16. * 16.;
pub const THREAD_SIDEBAR_MIN_WIDTH: f32 = 13. * 16.;
pub const THREAD_MAIN_CONTENT_MIN_WIDTH: f32 = 40. * 16.;

/// The brand's measured width can raise the minimum so "T3 Code" never clips.
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

    #[test]
    fn initial_width_defaults_and_respects_minimum() {
        assert_eq!(resolve_initial_thread_sidebar_width(None, 1400.), 256.);
        assert_eq!(resolve_initial_thread_sidebar_width(Some(100.), 1400.), 208.);
        assert_eq!(resolve_initial_thread_sidebar_width(Some(320.), 1400.), 320.);
    }

    #[test]
    fn initial_width_leaves_room_for_main_content() {
        // 900 - 640 = 260 max.
        assert_eq!(resolve_initial_thread_sidebar_width(Some(400.), 900.), 260.);
        // A narrow viewport still keeps the minimum.
        assert_eq!(resolve_initial_thread_sidebar_width(None, 700.), 208.);
    }

    #[test]
    fn brand_width_raises_minimum() {
        assert_eq!(resolve_thread_sidebar_minimum_width(150.), 208.);
        assert_eq!(resolve_thread_sidebar_minimum_width(220.2), 221.);
    }

    #[test]
    fn clamps_between_bounds() {
        assert_eq!(clamp_thread_sidebar_width(100., 208., 500.), 208.);
        assert_eq!(clamp_thread_sidebar_width(900., 208., 500.), 500.);
        assert_eq!(clamp_thread_sidebar_width(300., 208., 500.), 300.);
    }
}
