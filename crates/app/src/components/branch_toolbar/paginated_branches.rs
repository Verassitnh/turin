//! Port of upstream/apps/web/src/state/paginatedBranches.ts
//! (`shouldLoadNextBranchPageAfterScroll`). `isPaginatedBranchesNextPagePending`
//! reads Effect `AsyncResult`s; the backend reports `is_fetching_next_page`
//! directly instead.

const DEFAULT_NEXT_PAGE_DISTANCE_PX: f32 = 96.;

pub fn should_load_next_branch_page_after_scroll(
    previous_scroll_top: Option<f32>,
    scroll_top: f32,
    scroll_height: f32,
    client_height: f32,
) -> bool {
    let Some(previous) = previous_scroll_top else {
        return false;
    };
    if scroll_top <= previous {
        return false;
    }
    scroll_height - scroll_top - client_height <= DEFAULT_NEXT_PAGE_DISTANCE_PX
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of paginatedBranches.test.ts "paginated branch scroll trigger".
    #[test]
    fn does_not_load_from_mount_layout_or_an_unchanged_scroll_position() {
        assert!(!should_load_next_branch_page_after_scroll(
            None, 0., 224., 224.
        ));
        assert!(!should_load_next_branch_page_after_scroll(
            Some(120.),
            120.,
            344.,
            224.
        ));
    }

    #[test]
    fn does_not_load_when_scrolling_upward_near_the_bottom() {
        assert!(!should_load_next_branch_page_after_scroll(
            Some(160.),
            150.,
            450.,
            224.
        ));
    }

    #[test]
    fn loads_only_after_a_downward_scroll_reaches_the_end_threshold() {
        assert!(!should_load_next_branch_page_after_scroll(
            Some(0.),
            100.,
            800.,
            224.
        ));
        assert!(should_load_next_branch_page_after_scroll(
            Some(100.),
            500.,
            800.,
            224.
        ));
        assert!(should_load_next_branch_page_after_scroll(
            Some(499.5),
            500.,
            800.,
            224.
        ));
    }
}
