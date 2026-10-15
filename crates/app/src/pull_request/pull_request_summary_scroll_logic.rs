//! Port of upstream/apps/web/src/components/pullRequest/pullRequestSummaryScroll.logic.ts.
const STICKY_EDGE_TOLERANCE_PX: f64 = 1.0;
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SectionCollapseAnchorInput {
    pub scroll_top: f64,
    pub viewport_top: f64,
    pub section_top: f64,
    pub heading_top: f64,
}
/// Keep the natural section top at the pinned heading when its body collapses.
pub fn section_collapse_anchor_scroll_top(input: SectionCollapseAnchorInput) -> Option<f64> {
    if input.section_top >= input.viewport_top
        || input.heading_top > input.viewport_top + STICKY_EDGE_TOLERANCE_PX
    {
        return None;
    }
    Some((input.scroll_top + input.section_top - input.viewport_top).max(0.0))
}
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn run(scroll_top: f64, viewport_top: f64, section_top: f64, heading_top: f64) -> Option<f64> {
        section_collapse_anchor_scroll_top(SectionCollapseAnchorInput {
            scroll_top,
            viewport_top,
            section_top,
            heading_top,
        })
    }
    #[test]
    fn natural_offset_when_pinned() {
        assert_eq!(run(600.0, 264.0, -200.0, 264.0), Some(136.0));
    }
    #[test]
    fn fractional_edge() {
        assert_eq!(run(500.0, 264.0, 100.0, 264.5), Some(336.0));
    }
    #[test]
    fn before_sticky() {
        assert_eq!(run(120.0, 264.0, 320.0, 320.0), None);
    }
    #[test]
    fn never_negative() {
        assert_eq!(run(20.0, 264.0, 200.0, 264.0), Some(0.0));
    }
}
