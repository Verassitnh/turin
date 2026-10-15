//! Port of upstream/apps/web/src/themePalette.ts — the theme resolution
//! helpers (preference → definition → appearance → colors).
//!
//! Ported: legacy id aliases, `canonicalThemePreference`,
//! `getStandardThemeColors`, `getThemeDefinition`, `singleAppearanceOf`,
//! `getThemeColorsForMode`, `getThemeModes`, `getThemePreferenceMode`,
//! `resolveThemeAppearance`, `isKnownThemePreference`,
//! `themeAllowsSidebarArtwork`. Deferred to the settings task: the custom /
//! environment theme library (storage, theme files, vivid palette editor),
//! light/dark halves, live previews and `applyThemePalette`'s CSS variables
//! (here `AppTheme::resolve` plays that role).

use super::theme_palettes::{
    BUILT_IN_THEMES, T3_CODE_DARK_THEME_COLORS, T3_CODE_LIGHT_THEME_COLORS, ThemeAppearance,
    ThemeColors, ThemeDefinition,
};

pub const T3_CHAT_THEME_ID: &str = "t3-chat";
const GROVE_THEME_ID: &str = "grove";
pub const OCEAN_THEME_ID: &str = "ocean";
const EMBER_THEME_ID: &str = "ember";
const IRIS_THEME_ID: &str = "iris";

const LEGACY_T3_CHAT_DARK_THEME_ID: &str = "t3-chat-dark";

/// A stored theme preference: `"system"`, `"light"`, `"dark"` (the standard
/// palette) or a theme id.
pub type ThemePreference = str;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePreferenceMode {
    Light,
    Dark,
    System,
}

// Earlier builds shipped every maintainer theme under a t3- prefix; only the
// genuinely T3-branded palette keeps it. Stored preferences and mixes with the
// old ids stay readable through this alias table.
const LEGACY_THEME_ID_ALIASES: [(&str, &str); 5] = [
    (LEGACY_T3_CHAT_DARK_THEME_ID, T3_CHAT_THEME_ID),
    ("t3-grove", GROVE_THEME_ID),
    ("t3-ocean", OCEAN_THEME_ID),
    ("t3-ember", EMBER_THEME_ID),
    ("t3-iris", IRIS_THEME_ID),
];

fn normalize_theme_id(theme_id: &str) -> &str {
    LEGACY_THEME_ID_ALIASES
        .iter()
        .find(|(legacy, _)| *legacy == theme_id)
        .map_or(theme_id, |(_, current)| current)
}

/// Map a stored preference onto the id the runtime applies, so selection state
/// matches the theme cards. The legacy dark-variant id stays as-is because it
/// still carries the appearance hint getThemePreferenceMode reads.
pub fn canonical_theme_preference(theme: &str) -> &str {
    if theme == LEGACY_T3_CHAT_DARK_THEME_ID {
        theme
    } else {
        normalize_theme_id(theme)
    }
}

fn theme_id_from_preference(theme: &ThemePreference) -> &str {
    normalize_theme_id(theme)
}

// Older builds stored the dark T3 Chat palette as a separate theme. Keep
// those preferences readable while mapping them to the dark variant.
fn legacy_theme_mode(theme: &ThemePreference) -> Option<ThemeAppearance> {
    (theme == LEGACY_T3_CHAT_DARK_THEME_ID).then_some(ThemeAppearance::Dark)
}

/// The standard T3 Code look as a theme palette.
pub fn get_standard_theme_colors(appearance: ThemeAppearance) -> ThemeColors {
    match appearance {
        ThemeAppearance::Dark => T3_CODE_DARK_THEME_COLORS,
        ThemeAppearance::Light => T3_CODE_LIGHT_THEME_COLORS,
    }
}

/// Built-ins only for now; custom and environment themes join with the
/// settings task.
pub fn get_theme_definition(theme: &ThemePreference) -> Option<&'static ThemeDefinition> {
    let theme_id = theme_id_from_preference(theme);
    BUILT_IN_THEMES
        .iter()
        .find(|definition| definition.id == theme_id)
}

/// Artwork palettes are reviewed alongside built-ins; user themes always use the pill fallback.
pub fn theme_allows_sidebar_artwork(theme: &ThemePreference) -> bool {
    let theme_id = theme_id_from_preference(theme);
    BUILT_IN_THEMES
        .iter()
        .find(|definition| definition.id == theme_id)
        .is_some_and(|definition| definition.sidebar_artwork)
}

/// Which half a theme can claim, or `None` when it renders both appearances.
/// Selecting a single-appearance theme as the base preference would clear the
/// light/dark mix and leave the appearance tiles disagreeing with what is on
/// screen, so every path that selects a theme has to make the same call.
pub fn single_appearance_of(theme: &ThemeDefinition) -> Option<ThemeAppearance> {
    match get_theme_modes(theme).as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

pub fn get_theme_colors_for_mode(
    theme: &ThemeDefinition,
    mode: ThemeAppearance,
) -> Option<ThemeColors> {
    if mode == theme.appearance {
        return Some(theme.colors);
    }
    match mode {
        ThemeAppearance::Light => theme.variants.light,
        ThemeAppearance::Dark => theme.variants.dark,
    }
}

pub fn get_theme_modes(theme: &ThemeDefinition) -> Vec<ThemeAppearance> {
    [ThemeAppearance::Light, ThemeAppearance::Dark]
        .into_iter()
        .filter(|mode| get_theme_colors_for_mode(theme, *mode).is_some())
        .collect()
}

pub fn get_theme_preference_mode(theme: &ThemePreference) -> Option<ThemeAppearance> {
    match theme {
        "system" => None,
        "light" => Some(ThemeAppearance::Light),
        "dark" => Some(ThemeAppearance::Dark),
        _ => legacy_theme_mode(theme)
            .or_else(|| get_theme_definition(theme).map(|definition| definition.appearance)),
    }
}

/// `resolveThemeAppearance` without the `halves` argument (light/dark mixes
/// are deferred with the rest of the theme library).
pub fn resolve_theme_appearance(
    theme: &ThemePreference,
    system_dark: bool,
    follow_system: Option<bool>,
    appearance_mode: Option<ThemePreferenceMode>,
) -> ThemeAppearance {
    let system_appearance = if system_dark {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    };
    let mode = appearance_mode.or_else(|| {
        follow_system
            .unwrap_or(theme == "system")
            .then_some(ThemePreferenceMode::System)
    });
    let resolve = |requested: ThemeAppearance| match get_theme_definition(theme) {
        Some(definition) if get_theme_colors_for_mode(definition, requested).is_none() => {
            definition.appearance
        }
        _ => requested,
    };
    match mode {
        Some(ThemePreferenceMode::System) => resolve(system_appearance),
        Some(ThemePreferenceMode::Light) => resolve(ThemeAppearance::Light),
        Some(ThemePreferenceMode::Dark) => resolve(ThemeAppearance::Dark),
        None => get_theme_preference_mode(theme).unwrap_or(ThemeAppearance::Light),
    }
}

pub fn is_known_theme_preference(theme: &str) -> bool {
    matches!(theme, "light" | "dark" | "system") || get_theme_definition(theme).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::{parse_color, rgba_to_oklab, to_hex};
    use crate::theme::theme_palettes::{
        EMBER_THEME, GROVE_THEME, IRIS_THEME, OCEAN_THEME, T3_CHAT_THEME, ThemeVariants,
    };

    fn as_hex(value: &str) -> String {
        to_hex(parse_color(value).expect("theme color"))
    }

    fn expect_theme_colors(colors: &ThemeColors, expected: &[(&str, &str)]) {
        let entries = colors.entries();
        for (role, value) in expected {
            let actual = entries.iter().find(|(r, _)| r == role).unwrap().1;
            assert_eq!(as_hex(actual), *value, "{role}");
        }
    }

    fn contrast_ratio(first: &str, second: &str) -> f32 {
        let luminance = |value: &str| {
            let c = parse_color(value).unwrap();
            [c.r, c.g, c.b]
                .into_iter()
                .map(|channel| {
                    // Upstream measures the 8-bit hex.
                    let channel = (channel * 255.).round() / 255.;
                    if channel <= 0.03928 {
                        channel / 12.92
                    } else {
                        ((channel + 0.055) / 1.055).powf(2.4)
                    }
                })
                .zip([0.2126, 0.7152, 0.0722])
                .map(|(channel, weight)| channel * weight)
                .sum::<f32>()
        };
        let (a, b) = (luminance(first), luminance(second));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn oklab_distance(first: &str, second: &str) -> f32 {
        let a = rgba_to_oklab(parse_color(first).unwrap());
        let b = rgba_to_oklab(parse_color(second).unwrap());
        ((a.l - b.l).powi(2) + (a.a - b.a).powi(2) + (a.b - b.b).powi(2)).sqrt()
    }

    // describe("theme files")

    #[test]
    fn keeps_built_in_and_standard_search_pairs_readable_and_distinct_from_inline_code() {
        let mut palettes = vec![
            get_standard_theme_colors(ThemeAppearance::Light),
            get_standard_theme_colors(ThemeAppearance::Dark),
        ];
        for theme in BUILT_IN_THEMES {
            palettes.push(theme.colors);
            palettes.extend(theme.variants.light);
            palettes.extend(theme.variants.dark);
        }
        for colors in palettes {
            assert!(
                contrast_ratio(
                    colors.search_match_foreground,
                    colors.search_match_background
                ) >= 4.5
            );
            assert!(
                contrast_ratio(
                    colors.search_match_active_foreground,
                    colors.search_match_active_background
                ) >= 4.5
            );
            assert!(
                oklab_distance(
                    colors.search_match_background,
                    colors.search_match_active_background
                ) > 0.14
            );
            // Check inline-code tints, code blocks, and user-message bubbles.
            for background in [colors.muted, colors.message_surface, colors.code_background] {
                assert!(oklab_distance(colors.search_match_background, background) > 0.08);
                assert!(oklab_distance(colors.search_match_active_background, background) > 0.08);
            }
        }
    }

    #[test]
    fn keeps_stock_dark_controls_in_the_neutral_black_surface_hierarchy() {
        expect_theme_colors(
            &get_standard_theme_colors(ThemeAppearance::Dark),
            &[
                ("canvas", "#0a0a0a"),
                ("surface", "#111111"),
                ("surface_raised", "#111111"),
                ("surface_overlay", "#111111"),
                ("toolbar_control", "#111111"),
                ("secondary", "#111111"),
                ("muted", "#111111"),
                ("accent_surface", "#141414"),
            ],
        );
    }

    #[test]
    fn keeps_the_stock_sidebar_and_chat_on_distinct_surfaces_in_both_appearances() {
        expect_theme_colors(
            &get_standard_theme_colors(ThemeAppearance::Light),
            &[
                ("canvas", "#fcfcfc"),
                ("sidebar", "#fafafa"),
                ("sidebar_row_active", "#ffffff"),
                ("message_surface", "#f4f4f5"),
            ],
        );
        expect_theme_colors(
            &get_standard_theme_colors(ThemeAppearance::Dark),
            &[
                ("canvas", "#0a0a0a"),
                ("sidebar", "#000000"),
                ("sidebar_row_active", "#1a1b1b"),
                ("message_surface", "#141414"),
            ],
        );
    }

    #[test]
    fn keeps_a_single_mode_theme_on_its_only_palette() {
        let theme = ThemeDefinition {
            id: "midnight-slate",
            label: "Midnight Slate",
            appearance: ThemeAppearance::Dark,
            colors: T3_CODE_DARK_THEME_COLORS,
            variants: ThemeVariants {
                light: None,
                dark: None,
            },
            sidebar_artwork: false,
        };
        assert_eq!(get_theme_modes(&theme), vec![ThemeAppearance::Dark]);
        assert!(get_theme_colors_for_mode(&theme, ThemeAppearance::Dark).is_some());
        assert!(get_theme_colors_for_mode(&theme, ThemeAppearance::Light).is_none());
    }

    // describe("stored theme preferences")

    #[test]
    fn resolves_the_legacy_t3_chat_dark_preference_to_dark_t3_chat() {
        assert_eq!(get_theme_definition("t3-chat-dark"), Some(&T3_CHAT_THEME));
        assert_eq!(
            get_theme_preference_mode("t3-chat-dark"),
            Some(ThemeAppearance::Dark)
        );
        assert_eq!(
            resolve_theme_appearance("t3-chat-dark", true, Some(false), None),
            ThemeAppearance::Dark
        );
        assert!(is_known_theme_preference("t3-chat-dark"));
    }

    #[test]
    fn resolves_legacy_t3_prefixed_ids_onto_the_renamed_themes() {
        for (legacy, theme) in [
            ("t3-grove", &GROVE_THEME),
            ("t3-ocean", &OCEAN_THEME),
            ("t3-ember", &EMBER_THEME),
            ("t3-iris", &IRIS_THEME),
        ] {
            assert_eq!(get_theme_definition(legacy), Some(theme));
            assert!(is_known_theme_preference(legacy));
            assert_eq!(canonical_theme_preference(legacy), theme.id);
        }
        // The dark-variant alias keeps its raw form: it still carries a mode hint.
        assert_eq!(canonical_theme_preference("t3-chat-dark"), "t3-chat-dark");
    }

    #[test]
    fn recognizes_only_preferences_the_runtime_can_render() {
        for preference in ["light", "dark", "system", T3_CHAT_THEME.id, GROVE_THEME.id] {
            assert!(is_known_theme_preference(preference));
        }
        assert!(!is_known_theme_preference("grove:dark"));
        assert!(!is_known_theme_preference("missing-theme"));
    }

    // describe("singleAppearanceOf")

    #[test]
    fn reports_the_only_half_a_theme_can_claim_and_null_for_a_pair() {
        let single = ThemeDefinition {
            id: "x",
            appearance: ThemeAppearance::Dark,
            variants: ThemeVariants {
                light: None,
                dark: None,
            },
            ..T3_CHAT_THEME
        };
        assert_eq!(single_appearance_of(&single), Some(ThemeAppearance::Dark));
        assert_eq!(single_appearance_of(&T3_CHAT_THEME), None);
    }

    #[test]
    fn system_preference_follows_the_os() {
        assert_eq!(
            resolve_theme_appearance("system", true, None, None),
            ThemeAppearance::Dark
        );
        assert_eq!(
            resolve_theme_appearance("system", false, None, None),
            ThemeAppearance::Light
        );
        assert_eq!(
            resolve_theme_appearance("dark", false, None, None),
            ThemeAppearance::Dark
        );
    }
}
