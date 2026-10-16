//! Port of upstream/apps/web/src/components/settings/themeInspector.ts, pure
//! parts only.
//!
//! The upstream inspector walks the DOM: it reads computed styles, probes
//! each theme token with a sentinel color, and draws an SVG spotlight over the
//! matches. GPUI has no DOM or computed styles, so those steps are not ported;
//! the settings UI supplies element bounds and class lists from GPUI's own
//! tree. What remains is the decision logic those steps feed:
//!
//! - the paint kinds a probe can change, and their order;
//! - utility class → palette role (`bg-primary/90` → `message_action`),
//!   with stateful variants (`hover:`) excluded;
//! - the sentinel the probe writes, which alternates so a token already
//!   holding one value is always changed;
//! - the spotlight rectangle, its radius and placement, and the rounded key
//!   that dedupes overlapping matches;
//! - which role a candidate element reports, first paint kind wins.
//!
//! Role names are the snake_case fields of `ThemeColors`
//! (`theme_palettes::THEME_COLOR_ROLES`), not upstream's camelCase keys.

#![allow(dead_code)] // Consumed by the Settings → Appearance inspector (T20).

/// Upstream `ThemePaintKind`, in `THEME_PAINT_KIND_ORDER`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemePaintKind {
    Background,
    Border,
    Foreground,
}

/// `THEME_PAINT_KIND_ORDER`: a candidate reports background before border
/// before foreground.
pub const THEME_PAINT_KIND_ORDER: [ThemePaintKind; 3] = [
    ThemePaintKind::Background,
    ThemePaintKind::Border,
    ThemePaintKind::Foreground,
];

/// Upstream `ThemePaintSnapshot`: each kind's paints joined with `\n`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ThemePaintSnapshot {
    pub background: String,
    pub border: String,
    pub foreground: String,
}

impl ThemePaintSnapshot {
    fn get(&self, kind: ThemePaintKind) -> &str {
        match kind {
            ThemePaintKind::Background => &self.background,
            ThemePaintKind::Border => &self.border,
            ThemePaintKind::Foreground => &self.foreground,
        }
    }
}

/// Upstream `changedThemePaintKinds`: the kinds whose paint string differs.
pub fn changed_theme_paint_kinds(
    before: &ThemePaintSnapshot,
    after: &ThemePaintSnapshot,
) -> Vec<ThemePaintKind> {
    THEME_PAINT_KIND_ORDER
        .into_iter()
        .filter(|kind| before.get(*kind) != after.get(*kind))
        .collect()
}

/// Upstream `THEME_UTILITY_ROLES`: Tailwind semantic color name → palette role.
const THEME_UTILITY_ROLES: &[(&str, &str)] = &[
    ("background", "canvas"),
    ("foreground", "text"),
    ("card", "surface"),
    ("card-foreground", "text"),
    ("popover", "surface_overlay"),
    ("popover-foreground", "text"),
    ("surface-raised", "surface_raised"),
    ("primary", "message_action"),
    ("primary-foreground", "message_action_foreground"),
    ("secondary", "secondary"),
    ("secondary-foreground", "secondary_foreground"),
    ("muted", "muted"),
    ("muted-foreground", "muted_foreground"),
    ("placeholder", "placeholder"),
    ("secondary-label", "secondary_label"),
    ("icon-muted", "icon_muted"),
    ("accent", "accent_surface"),
    ("accent-foreground", "accent_surface_foreground"),
    ("destructive", "error"),
    ("destructive-foreground", "error_foreground"),
    ("error", "error"),
    ("error-foreground", "error_foreground"),
    ("error-surface", "error_surface"),
    ("warning", "warning"),
    ("warning-foreground", "warning_foreground"),
    ("warning-surface", "warning_surface"),
    ("update", "update"),
    ("update-foreground", "update_foreground"),
    ("update-surface", "update_surface"),
    ("message", "message_surface"),
    ("message-foreground", "message_foreground"),
    ("message-action", "message_action"),
    ("message-action-foreground", "message_action_foreground"),
    ("message-action-hover", "message_action_hover"),
    ("sidebar", "sidebar"),
    ("sidebar-foreground", "sidebar_foreground"),
    ("sidebar-muted-foreground", "sidebar_muted_foreground"),
    ("sidebar-control-surface", "sidebar_control_surface"),
    ("sidebar-row-hover", "sidebar_row_hover"),
    ("sidebar-row-active", "sidebar_row_active"),
    ("sidebar-row-selected", "sidebar_row_selected"),
    ("sidebar-border", "sidebar_border"),
    ("border", "border"),
    ("input", "input"),
    ("ring", "focus"),
];

/// Upstream `THEME_UTILITY_PREFIXES`.
fn theme_utility_prefixes(kind: ThemePaintKind) -> &'static [&'static str] {
    match kind {
        ThemePaintKind::Background => &["bg-"],
        ThemePaintKind::Border => &["border-", "outline-", "ring-"],
        ThemePaintKind::Foreground => &["text-", "caret-", "fill-", "stroke-"],
    }
}

/// Upstream `themeRoleFromUtilityClass`: the palette role a utility class
/// paints with, or `None`. Variant classes (`hover:bg-accent`) may not be
/// painting now, so they are never inferred.
pub fn theme_role_from_utility_class(
    class_name: &str,
    kind: ThemePaintKind,
) -> Option<&'static str> {
    if class_name.contains(':') {
        return None;
    }
    let prefix = theme_utility_prefixes(kind)
        .iter()
        .find(|prefix| class_name.starts_with(**prefix))?;
    // `split("/", 1)[0]` drops an opacity suffix such as `/90`.
    let color_name = class_name[prefix.len()..].split('/').next().unwrap_or("");
    THEME_UTILITY_ROLES
        .iter()
        .find(|(name, _)| *name == color_name)
        .map(|(_, role)| *role)
}

/// Upstream `themeRoleFromUtilities`: the first kind, in paint order, that
/// has a role among the element's classes.
pub fn theme_role_from_utilities<'a>(
    classes: impl IntoIterator<Item = &'a str> + Clone,
) -> Option<&'static str> {
    THEME_PAINT_KIND_ORDER.into_iter().find_map(|kind| {
        classes
            .clone()
            .into_iter()
            .find_map(|class_name| theme_role_from_utility_class(class_name, kind))
    })
}

/// Upstream `inspectThemeRoleFromUtilitiesAtElement` over an ancestor chain.
/// `candidates` is nearest-first; each entry carries its class list. Returns
/// the index of the first candidate with a utility role, and that role.
pub fn inspect_theme_role_from_utilities<'a, I, C>(candidates: I) -> Option<(usize, &'static str)>
where
    I: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'a str> + Clone,
{
    candidates
        .into_iter()
        .enumerate()
        .find_map(|(index, classes)| theme_role_from_utilities(classes).map(|role| (index, role)))
}

/// Per-candidate roles found by the computed-style probe, one slot per paint
/// kind. Upstream fills these with `??=`, so the first role to change a kind
/// wins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbedPaintRoles {
    pub background: Option<&'static str>,
    pub border: Option<&'static str>,
    pub foreground: Option<&'static str>,
}

impl ProbedPaintRoles {
    fn slot(&mut self, kind: ThemePaintKind) -> &mut Option<&'static str> {
        match kind {
            ThemePaintKind::Background => &mut self.background,
            ThemePaintKind::Border => &mut self.border,
            ThemePaintKind::Foreground => &mut self.foreground,
        }
    }

    /// Records `role` for every kind that changed, keeping earlier roles.
    pub fn record(&mut self, role: &'static str, changed: &[ThemePaintKind]) {
        for kind in changed {
            self.slot(*kind).get_or_insert(role);
        }
    }

    /// Upstream's final loop in `inspectThemeRoleAtElement`: first kind in
    /// paint order with a role.
    pub fn first_role(&self) -> Option<&'static str> {
        THEME_PAINT_KIND_ORDER
            .into_iter()
            .find_map(|kind| match kind {
                ThemePaintKind::Background => self.background,
                ThemePaintKind::Border => self.border,
                ThemePaintKind::Foreground => self.foreground,
            })
    }
}

/// Sentinel colors the probe writes into a token. Upstream alternates them so
/// a token already holding one value still changes.
pub const THEME_TOKEN_PROBE_COLOR: &str = "#01fea7";
pub const THEME_TOKEN_ALTERNATE_PROBE_COLOR: &str = "#fe01a7";

/// The sentinel to write over a token whose current value is `resolved`.
pub fn theme_token_probe_color(resolved: &str) -> &'static str {
    if resolved.trim().to_ascii_lowercase() == THEME_TOKEN_PROBE_COLOR {
        THEME_TOKEN_ALTERNATE_PROBE_COLOR
    } else {
        THEME_TOKEN_PROBE_COLOR
    }
}

/// Upstream spotlight padding around a matched element, in CSS px.
pub const SPOTLIGHT_PADDING: f32 = 5.0;

/// Border box of an element in viewport coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ElementBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl ElementBounds {
    fn width(&self) -> f32 {
        self.right - self.left
    }

    fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// The hole the spotlight cuts around an element.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotlightRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

/// Upstream `spotlightRect`. `None` when the element is empty or fully outside
/// the viewport. `element_radius` is the element's top-left corner radius
/// (`parseFloat(...) || 0`, so callers pass 0 for an unparseable value).
pub fn spotlight_rect(
    bounds: ElementBounds,
    element_radius: f32,
    viewport: (f32, f32),
) -> Option<SpotlightRect> {
    let (viewport_width, viewport_height) = viewport;
    if bounds.width() <= 0.0
        || bounds.height() <= 0.0
        || bounds.right < 0.0
        || bounds.bottom < 0.0
        || bounds.left > viewport_width
        || bounds.top > viewport_height
    {
        return None;
    }
    // Partially offscreen elements keep their true bounds: clamping would draw
    // a fake glow edge along the crop.
    Some(SpotlightRect {
        x: bounds.left - SPOTLIGHT_PADDING,
        y: bounds.top - SPOTLIGHT_PADDING,
        width: bounds.width() + SPOTLIGHT_PADDING * 2.0,
        height: bounds.height() + SPOTLIGHT_PADDING * 2.0,
        radius: (element_radius + SPOTLIGHT_PADDING).clamp(7.0, 18.0),
    })
}

/// Upstream `hover.dataset.placement`: the hover label sits below when the
/// element is within 32px of the top of the viewport.
pub fn spotlight_placement(rect: &SpotlightRect) -> &'static str {
    if rect.y < 32.0 { "below" } else { "above" }
}

/// JavaScript `Math.round`: halves round toward +∞, so `-0.5` becomes `-0`.
fn js_round(value: f32) -> i64 {
    (value + 0.5).floor() as i64
}

/// Upstream dedupe key in `renderThemeInspectorSpotlight`: rounded
/// `x:y:width:height`. Matches that share it draw one hole.
pub fn spotlight_key(rect: &SpotlightRect) -> (i64, i64, i64, i64) {
    (
        js_round(rect.x),
        js_round(rect.y),
        js_round(rect.width),
        js_round(rect.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of upstream/apps/web/src/components/settings/themeInspector.test.ts

    fn white_paint() -> ThemePaintSnapshot {
        ThemePaintSnapshot {
            background: "rgb(255, 255, 255)\nnone".to_string(),
            border: "rgb(255, 255, 255)\nnone".to_string(),
            foreground: "rgb(255, 255, 255)\nnone".to_string(),
        }
    }

    #[test]
    fn does_not_match_an_identical_hardcoded_color() {
        assert_eq!(
            changed_theme_paint_kinds(&white_paint(), &white_paint()),
            Vec::<ThemePaintKind>::new()
        );
    }

    #[test]
    fn reports_only_paint_that_changed_with_the_probed_token() {
        let after = ThemePaintSnapshot {
            background: "rgb(1, 254, 167)\nnone".to_string(),
            foreground: "rgb(1, 254, 167)\nnone".to_string(),
            ..white_paint()
        };
        assert_eq!(
            changed_theme_paint_kinds(&white_paint(), &after),
            vec![ThemePaintKind::Background, ThemePaintKind::Foreground]
        );
    }

    #[test]
    fn maps_semantic_utilities_to_their_actual_palette_role() {
        assert_eq!(
            theme_role_from_utility_class("text-foreground", ThemePaintKind::Foreground),
            Some("text")
        );
        assert_eq!(
            theme_role_from_utility_class("text-muted-foreground", ThemePaintKind::Foreground),
            Some("muted_foreground")
        );
        assert_eq!(
            theme_role_from_utility_class("bg-primary/90", ThemePaintKind::Background),
            Some("message_action")
        );
        assert_eq!(
            theme_role_from_utility_class("ring-ring", ThemePaintKind::Border),
            Some("focus")
        );
    }

    #[test]
    fn does_not_infer_hardcoded_or_potentially_inactive_colors() {
        assert_eq!(
            theme_role_from_utility_class("bg-white", ThemePaintKind::Background),
            None
        );
        assert_eq!(
            theme_role_from_utility_class("hover:bg-accent", ThemePaintKind::Background),
            None
        );
    }

    // Turin-added: the pure helpers that replace the DOM steps. No upstream
    // test covers them.

    #[test]
    fn utility_role_lookup_follows_the_prefix_for_each_paint_kind() {
        // `ring-` is a border prefix, so it is not a foreground utility.
        assert_eq!(
            theme_role_from_utility_class("ring-ring", ThemePaintKind::Foreground),
            None
        );
        assert_eq!(
            theme_role_from_utility_class("caret-input", ThemePaintKind::Foreground),
            Some("input")
        );
        assert_eq!(
            theme_role_from_utility_class("outline-border", ThemePaintKind::Border),
            Some("border")
        );
    }

    #[test]
    fn utilities_resolve_background_before_border_before_foreground() {
        let classes = ["text-foreground", "border-sidebar-border", "bg-sidebar"];
        assert_eq!(theme_role_from_utilities(classes), Some("sidebar"));
        let text_only = ["text-foreground", "p-2"];
        assert_eq!(theme_role_from_utilities(text_only), Some("text"));
    }

    #[test]
    fn nearest_candidate_with_a_utility_role_wins() {
        let plain: Vec<&str> = vec!["flex"];
        let painted: Vec<&str> = vec!["bg-card"];
        let outer: Vec<&str> = vec!["bg-sidebar"];
        let candidates = [plain, painted, outer];
        assert_eq!(
            inspect_theme_role_from_utilities(candidates),
            Some((1, "surface"))
        );
    }

    #[test]
    fn probe_alternates_so_a_token_always_changes() {
        assert_eq!(
            theme_token_probe_color("#01FEA7 "),
            THEME_TOKEN_ALTERNATE_PROBE_COLOR
        );
        assert_eq!(theme_token_probe_color("#fe01a7"), THEME_TOKEN_PROBE_COLOR);
        assert_eq!(
            theme_token_probe_color("oklch(0.5 0 0)"),
            THEME_TOKEN_PROBE_COLOR
        );
    }

    #[test]
    fn probed_roles_keep_the_first_role_per_kind_and_report_in_paint_order() {
        let mut roles = ProbedPaintRoles::default();
        roles.record("canvas", &[ThemePaintKind::Foreground]);
        roles.record(
            "text",
            &[ThemePaintKind::Foreground, ThemePaintKind::Border],
        );
        assert_eq!(roles.foreground, Some("canvas"));
        assert_eq!(roles.border, Some("text"));
        assert_eq!(roles.first_role(), Some("text"));
    }

    #[test]
    fn spotlight_pads_the_bounds_and_clamps_the_radius() {
        let bounds = ElementBounds {
            left: 10.0,
            top: 20.0,
            right: 110.0,
            bottom: 60.0,
        };
        assert_eq!(
            spotlight_rect(bounds, 0.0, (800.0, 600.0)),
            Some(SpotlightRect {
                x: 5.0,
                y: 15.0,
                width: 110.0,
                height: 50.0,
                radius: 7.0
            })
        );
        // A 100px radius caps at 18.
        assert_eq!(
            spotlight_rect(bounds, 100.0, (800.0, 600.0))
                .unwrap()
                .radius,
            18.0
        );
    }

    #[test]
    fn spotlight_skips_empty_or_offscreen_elements_and_keeps_partial_ones() {
        let empty = ElementBounds {
            left: 10.0,
            top: 20.0,
            right: 10.0,
            bottom: 60.0,
        };
        assert_eq!(spotlight_rect(empty, 0.0, (800.0, 600.0)), None);
        let left_of_screen = ElementBounds {
            left: -90.0,
            top: 0.0,
            right: -10.0,
            bottom: 10.0,
        };
        assert_eq!(spotlight_rect(left_of_screen, 0.0, (800.0, 600.0)), None);
        let partial = ElementBounds {
            left: -20.0,
            top: 0.0,
            right: 40.0,
            bottom: 10.0,
        };
        let rect = spotlight_rect(partial, 0.0, (800.0, 600.0)).unwrap();
        assert_eq!(rect.x, -25.0);
        assert_eq!(rect.width, 70.0);
    }

    #[test]
    fn hover_label_sits_below_only_near_the_top_edge() {
        let top = SpotlightRect {
            x: 0.0,
            y: 10.0,
            width: 1.0,
            height: 1.0,
            radius: 7.0,
        };
        let lower = SpotlightRect { y: 40.0, ..top };
        assert_eq!(spotlight_placement(&top), "below");
        assert_eq!(spotlight_placement(&lower), "above");
    }

    #[test]
    fn spotlight_key_rounds_halves_toward_positive_infinity() {
        let rect = SpotlightRect {
            x: -0.5,
            y: 0.5,
            width: 10.4,
            height: 9.6,
            radius: 7.0,
        };
        assert_eq!(spotlight_key(&rect), (0, 1, 10, 10));
    }
}
