//! Port of upstream/apps/web/src/components/composerFooterLayout.ts
//!
//! Footer measurement rules: when the footer goes compact, when the composer
//! rests, how much the timeline reserves for the overlay, and which footer
//! controls fit (labels drop first, then trailing blocks move into the
//! overflow menu). `restingComposerControlsMeasurement.ts` reads widths from
//! the DOM; the GPUI footer supplies a [`RestingComposerControlsMeasurement`]
//! from shaped text instead.

pub const COMPOSER_FOOTER_COMPACT_BREAKPOINT_PX: f32 = 620.0;
pub const COMPOSER_FOOTER_WIDE_ACTIONS_COMPACT_BREAKPOINT_PX: f32 = 780.0;
const RESTING_COMPOSER_IMAGE_THUMBNAIL_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestingImagePreviewCounts {
    pub visible_count: usize,
    pub overflow_count: usize,
}

pub fn get_resting_composer_image_preview_counts(image_count: usize) -> RestingImagePreviewCounts {
    let visible_count = image_count.min(RESTING_COMPOSER_IMAGE_THUMBNAIL_LIMIT);
    RestingImagePreviewCounts {
        visible_count,
        overflow_count: image_count - visible_count,
    }
}

pub fn should_use_compact_composer_footer(width: Option<f32>, has_wide_actions: bool) -> bool {
    let breakpoint = if has_wide_actions {
        COMPOSER_FOOTER_WIDE_ACTIONS_COMPACT_BREAKPOINT_PX
    } else {
        COMPOSER_FOOTER_COMPACT_BREAKPOINT_PX
    };
    width.is_some_and(|width| width < breakpoint)
}

pub struct RestingLayoutInput {
    pub is_existing_thread: bool,
    pub is_mobile_viewport: bool,
    pub is_scroll_collapsed: bool,
    pub has_expanded_chrome: bool,
    pub has_multiline_prompt: bool,
    /// Whether the timeline has more content than fits above the composer.
    pub timeline_overflows: bool,
}

/// `shouldUseRestingComposerLayout`. Only a timeline scroll rests the
/// composer: the user asked for it with the gesture, and it lifts on the next
/// composer interaction. A thread that fits above the composer has nothing to
/// reclaim, so it stays expanded.
pub fn should_use_resting_composer_layout(input: &RestingLayoutInput) -> bool {
    input.is_existing_thread
        && !input.is_mobile_viewport
        && input.timeline_overflows
        && input.is_scroll_collapsed
        && !input.has_multiline_prompt
        && !input.has_expanded_chrome
}

/// How much taller the empty expanded composer is than its resting row on
/// desktop widths.
pub const COMPOSER_RESTING_EXPANSION_MIN_PX: f32 = 94.0;

pub struct TimelineInsetInput {
    pub current_inset: f32,
    pub overlay_height: f32,
    pub is_resting: bool,
    pub resting_only_height: f32,
}

/// `resolveComposerTimelineInset`: the space the timeline reserves at its end
/// for the composer overlay. While resting, keep the measured expanded height;
/// estimate the empty expansion only before that measurement exists.
pub fn resolve_composer_timeline_inset(input: &TimelineInsetInput) -> f32 {
    if input.is_resting {
        input.current_inset.max(
            input.overlay_height
                + if input.current_inset == 0.0 {
                    COMPOSER_RESTING_EXPANSION_MIN_PX - input.resting_only_height
                } else {
                    0.0
                },
        )
    } else {
        input.overlay_height
    }
}

/// `overlayComposerIsResting`: only a mounted composer can be resting.
pub fn overlay_composer_is_resting(
    composer_mounted: bool,
    composer_reported_resting: bool,
) -> bool {
    composer_mounted && composer_reported_resting
}

/// `shouldAnimateComposerRestingTransition`.
pub fn should_animate_composer_resting_transition(
    has_completed_initial_layout: bool,
    state_changed: bool,
    has_interrupted_animation: bool,
) -> bool {
    has_completed_initial_layout && (state_changed || has_interrupted_animation)
}

pub fn should_use_compact_composer_primary_actions(
    width: Option<f32>,
    has_wide_actions: bool,
) -> bool {
    has_wide_actions
        && width.is_some_and(|width| width < COMPOSER_FOOTER_WIDE_ACTIONS_COMPACT_BREAKPOINT_PX)
}

#[derive(Debug, Clone, Default)]
pub struct RestingComposerControlsMeasurement {
    pub gap: f32,
    pub natural_fixed_width: f32,
    pub minimum_fixed_width: f32,
    pub block_widths: Vec<f32>,
    pub overflow_width: f32,
    pub icon_only_block_widths: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestingControlsLayout {
    pub hidden_count: usize,
    pub icon_only_count: Option<usize>,
    pub visible: bool,
}

fn resting_controls_width(
    input: &RestingComposerControlsMeasurement,
    hidden_count: usize,
    fixed_width: f32,
    icon_only_count: usize,
) -> f32 {
    let block_count = input.block_widths.len();
    let visible_count = block_count - hidden_count;
    let blocks: f32 = input.block_widths[..visible_count]
        .iter()
        .enumerate()
        .map(|(index, width)| {
            if index >= block_count - icon_only_count {
                input
                    .icon_only_block_widths
                    .as_ref()
                    .and_then(|widths| widths.get(index))
                    .copied()
                    .unwrap_or(*width)
            } else {
                *width
            }
        })
        .sum();
    fixed_width
        + blocks
        + if hidden_count > 0 {
            input.overflow_width
        } else {
            0.0
        }
        + input.gap * (visible_count + usize::from(hidden_count > 0)) as f32
}

/// `resolveRestingComposerControlsNaturalWidth`: the width the controls take
/// with nothing moved into overflow.
pub fn resolve_resting_composer_controls_natural_width(
    input: &RestingComposerControlsMeasurement,
) -> f32 {
    resting_controls_width(input, 0, input.natural_fixed_width, 0)
}

const RESTING_CONTROLS_SLACK_PX: f32 = 1.0;

/// `resolveRestingComposerControlsLayout`: fit footer controls using natural
/// widths: remove trailing labels first, then move trailing blocks into
/// overflow. Promotions need a pixel of slack so fractional jitter cannot flip
/// the layout forever; demotions are immediate.
pub fn resolve_resting_composer_controls_layout(
    input: &RestingComposerControlsMeasurement,
    host_width: f32,
    previous: Option<RestingControlsLayout>,
) -> RestingControlsLayout {
    let block_count = input.block_widths.len();
    let icon_steps = if input.icon_only_block_widths.is_some() {
        block_count
    } else {
        0
    };
    let previous_step = previous.map_or(0, |previous| {
        if previous.hidden_count > 0 {
            icon_steps + previous.hidden_count.min(block_count)
        } else {
            previous.icon_only_count.unwrap_or(0).min(icon_steps)
        }
    });
    let width_at_step = |candidate: usize, fixed_width: f32| {
        resting_controls_width(
            input,
            candidate.saturating_sub(icon_steps),
            fixed_width,
            candidate.min(icon_steps),
        )
    };
    let mut step = 0;
    while step < icon_steps + block_count
        && width_at_step(step, input.natural_fixed_width)
            > host_width
                - if step < previous_step {
                    RESTING_CONTROLS_SLACK_PX
                } else {
                    0.0
                }
    {
        step += 1;
    }
    let hidden_count = step.saturating_sub(icon_steps);
    let icon_only_count = step.min(icon_steps);
    let minimum_width = width_at_step(step, input.minimum_fixed_width);
    let visible = if previous.is_some_and(|previous| !previous.visible) {
        minimum_width <= host_width - RESTING_CONTROLS_SLACK_PX
    } else {
        minimum_width <= host_width
    };
    RestingControlsLayout {
        hidden_count,
        icon_only_count: input
            .icon_only_block_widths
            .is_some()
            .then_some(icon_only_count),
        visible,
    }
}

pub struct ClearanceRect {
    pub top: f32,
    pub left: f32,
    pub right: f32,
}

/// `resolveScrollToEndClearance`.
pub fn resolve_scroll_to_end_clearance(
    overlay_height: f32,
    main_surface_top: f32,
    button: (f32, f32),
    attachments: &[ClearanceRect],
) -> f32 {
    let mut content_top = main_surface_top;
    let mut top = content_top;
    for attachment in attachments {
        content_top = content_top.min(attachment.top);
        if attachment.left < button.1 && attachment.right > button.0 {
            top = top.min(attachment.top);
        }
    }
    (overlay_height - (top - content_top)).ceil()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_at_most_three_thumbnails_and_counts_the_remainder() {
        assert_eq!(
            get_resting_composer_image_preview_counts(5),
            RestingImagePreviewCounts {
                visible_count: 3,
                overflow_count: 2
            }
        );
        assert_eq!(
            get_resting_composer_image_preview_counts(2),
            RestingImagePreviewCounts {
                visible_count: 2,
                overflow_count: 0
            }
        );
    }

    #[test]
    fn compact_footer_breakpoints() {
        assert!(!should_use_compact_composer_footer(None, false));
        assert!(should_use_compact_composer_footer(Some(619.0), false));
        assert!(!should_use_compact_composer_footer(Some(620.0), false));
        assert!(!should_use_compact_composer_footer(Some(700.0), false));
        assert!(should_use_compact_composer_footer(Some(700.0), true));
        assert!(!should_use_compact_composer_footer(Some(780.0), true));
        assert!(should_use_compact_composer_primary_actions(
            Some(779.0),
            true
        ));
        assert!(!should_use_compact_composer_primary_actions(
            Some(300.0),
            false
        ));
    }

    #[test]
    fn a_status_bar_drops_a_resting_reservation() {
        let resting = overlay_composer_is_resting(false, true);
        assert!(!resting);
        assert_eq!(
            resolve_composer_timeline_inset(&TimelineInsetInput {
                current_inset: 0.0,
                overlay_height: 56.0,
                is_resting: resting,
                resting_only_height: 0.0
            }),
            56.0
        );
        assert!(overlay_composer_is_resting(true, true));
    }

    fn inset(current: f32, overlay: f32, resting: bool, only: f32) -> f32 {
        resolve_composer_timeline_inset(&TimelineInsetInput {
            current_inset: current,
            overlay_height: overlay,
            is_resting: resting,
            resting_only_height: only,
        })
    }

    #[test]
    fn timeline_inset_rules() {
        assert_eq!(inset(160.0, 140.0, false, 0.0), 140.0);
        assert_eq!(inset(200.0, 60.0, true, 0.0), 200.0);
        assert_eq!(inset(172.0, 110.0, true, 0.0), 172.0);
        let expanded = inset(0.0, 172.0, false, 0.0);
        let collapsed = inset(expanded, 110.0, true, 32.0);
        assert_eq!(collapsed, expanded);
        assert_eq!(inset(collapsed, 172.0, false, 0.0), expanded);
        assert_eq!(
            inset(0.0, 60.0, true, 0.0),
            60.0 + COMPOSER_RESTING_EXPANSION_MIN_PX
        );
    }

    #[test]
    fn resting_layout_conditions() {
        let base = || RestingLayoutInput {
            is_existing_thread: true,
            is_mobile_viewport: false,
            is_scroll_collapsed: true,
            has_expanded_chrome: false,
            has_multiline_prompt: false,
            timeline_overflows: true,
        };
        assert!(should_use_resting_composer_layout(&base()));
        assert!(!should_use_resting_composer_layout(&RestingLayoutInput {
            is_scroll_collapsed: false,
            ..base()
        }));
        assert!(!should_use_resting_composer_layout(&RestingLayoutInput {
            timeline_overflows: false,
            ..base()
        }));
        assert!(!should_use_resting_composer_layout(&RestingLayoutInput {
            is_existing_thread: false,
            ..base()
        }));
        assert!(!should_use_resting_composer_layout(&RestingLayoutInput {
            is_mobile_viewport: true,
            ..base()
        }));
        assert!(!should_use_resting_composer_layout(&RestingLayoutInput {
            has_expanded_chrome: true,
            ..base()
        }));
        for collapsed in [false, true] {
            assert!(!should_use_resting_composer_layout(&RestingLayoutInput {
                has_multiline_prompt: true,
                is_scroll_collapsed: collapsed,
                ..base()
            }));
        }
    }

    #[test]
    fn resting_transition_animates_only_after_initial_layout() {
        assert!(!should_animate_composer_resting_transition(
            false, true, false
        ));
        assert!(should_animate_composer_resting_transition(
            true, true, false
        ));
        assert!(should_animate_composer_resting_transition(
            true, false, true
        ));
    }

    // Picker 140 natural / 96 minimum, plus a 9px separator. Traits 60,
    // mode 140, overflow 24, gap 4.
    fn base() -> RestingComposerControlsMeasurement {
        RestingComposerControlsMeasurement {
            gap: 4.0,
            natural_fixed_width: 149.0,
            minimum_fixed_width: 105.0,
            block_widths: vec![60.0, 140.0],
            overflow_width: 24.0,
            icon_only_block_widths: None,
        }
    }

    fn layout(host: f32) -> (usize, bool) {
        let layout = resolve_resting_composer_controls_layout(&base(), host, None);
        (layout.hidden_count, layout.visible)
    }

    fn prev(hidden: usize, visible: bool) -> Option<RestingControlsLayout> {
        Some(RestingControlsLayout {
            hidden_count: hidden,
            icon_only_count: None,
            visible,
        })
    }

    #[test]
    fn controls_move_into_overflow_until_the_rest_fits() {
        assert_eq!(layout(357.0), (0, true));
        assert_eq!(layout(356.0), (1, true));
        assert_eq!(layout(241.0), (1, true));
        assert_eq!(layout(240.0), (2, true));
        assert_eq!(layout(176.0), (2, true));
        assert_eq!(layout(133.0), (2, true));
        assert_eq!(layout(132.0), (2, false));
        assert_eq!(layout(0.0), (2, false));
    }

    #[test]
    fn a_single_leading_control_without_overflow_blocks() {
        let single = RestingComposerControlsMeasurement {
            natural_fixed_width: 140.0,
            minimum_fixed_width: 140.0,
            block_widths: vec![],
            overflow_width: 0.0,
            ..base()
        };
        let layout = resolve_resting_composer_controls_layout(&single, 139.0, None);
        assert_eq!((layout.hidden_count, layout.visible), (0, false));
    }

    #[test]
    fn hysteresis_requires_slack_before_restoring() {
        let resolve = |host, previous| {
            let layout = resolve_resting_composer_controls_layout(&base(), host, previous);
            (layout.hidden_count, layout.visible)
        };
        assert_eq!(resolve(357.0, prev(1, true)), (1, true));
        assert_eq!(resolve(358.0, prev(1, true)), (0, true));
        assert_eq!(resolve(357.0, prev(2, true)), (1, true));
        assert_eq!(resolve(358.0, prev(1, true)), (0, true));
        assert_eq!(resolve(241.0, prev(2, true)), (2, true));
        assert_eq!(resolve(242.0, prev(2, true)), (1, true));
        assert_eq!(resolve(133.0, prev(2, false)), (2, false));
        assert_eq!(resolve(134.0, prev(2, false)), (2, true));
    }

    #[test]
    fn settles_when_the_measured_picker_width_jitters_below_a_pixel() {
        let mut current = RestingControlsLayout {
            hidden_count: 0,
            icon_only_count: None,
            visible: true,
        };
        let mut settled = std::collections::HashSet::new();
        for index in 0..10 {
            let measurement = RestingComposerControlsMeasurement {
                natural_fixed_width: if index % 2 == 0 { 149.0 } else { 149.5 },
                ..base()
            };
            current = resolve_resting_composer_controls_layout(&measurement, 357.0, Some(current));
            if index >= 2 {
                settled.insert((current.hidden_count, current.visible));
            }
        }
        assert_eq!(settled.len(), 1);
    }

    fn progressive() -> RestingComposerControlsMeasurement {
        RestingComposerControlsMeasurement {
            gap: 4.0,
            natural_fixed_width: 140.0,
            minimum_fixed_width: 80.0,
            block_widths: vec![80.0, 140.0],
            icon_only_block_widths: Some(vec![40.0, 60.0]),
            overflow_width: 24.0,
        }
    }

    #[test]
    fn labels_drop_before_controls() {
        for (host, icon_only, hidden) in [
            (368.0, 0, 0),
            (367.0, 1, 0),
            (288.0, 1, 0),
            (287.0, 2, 0),
            (248.0, 2, 0),
            (247.0, 2, 1),
            (211.0, 2, 2),
        ] {
            let layout = resolve_resting_composer_controls_layout(&progressive(), host, None);
            assert_eq!(
                (layout.hidden_count, layout.icon_only_count, layout.visible),
                (hidden, Some(icon_only), true),
                "host {host}"
            );
        }
    }

    #[test]
    fn labels_and_controls_need_slack_to_come_back() {
        for (host, previous, promoted) in [
            (368.0, (0, 1), (0, 0)),
            (288.0, (0, 2), (0, 1)),
            (248.0, (1, 2), (0, 2)),
        ] {
            let previous = RestingControlsLayout {
                hidden_count: previous.0,
                icon_only_count: Some(previous.1),
                visible: true,
            };
            assert_eq!(
                resolve_resting_composer_controls_layout(&progressive(), host, Some(previous)),
                previous
            );
            let next = resolve_resting_composer_controls_layout(
                &progressive(),
                host + 1.0,
                Some(previous),
            );
            assert_eq!((next.hidden_count, next.icon_only_count.unwrap()), promoted);
        }
    }

    #[test]
    fn natural_width_reserves_everything_visible() {
        assert_eq!(
            resolve_resting_composer_controls_natural_width(&base()),
            357.0
        );
    }

    #[test]
    fn scroll_to_end_clearance_ignores_the_side_tab_gap() {
        for overlay in [120.0, 214.0] {
            let at = |attachments: &[ClearanceRect], button: (f32, f32)| {
                resolve_scroll_to_end_clearance(overlay, 534.0, button, attachments)
            };
            let side = [ClearanceRect {
                top: 500.0,
                left: 600.0,
                right: 700.0,
            }];
            assert_eq!(at(&side, (340.0, 460.0)), overlay - 34.0);
            assert_eq!(at(&[], (340.0, 460.0)), overlay);
            let both = [
                ClearanceRect {
                    top: 500.0,
                    left: 600.0,
                    right: 700.0,
                },
                ClearanceRect {
                    top: 500.0,
                    left: 100.0,
                    right: 700.0,
                },
            ];
            assert_eq!(at(&both, (340.0, 460.0)), overlay);
            assert_eq!(at(&side, (590.0, 710.0)), overlay);
        }
    }
}
