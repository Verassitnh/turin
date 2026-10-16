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

#[allow(dead_code)] // Light/Dark come from the settings task's appearance picker.
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
#[allow(dead_code)] // settings task (T20) selection state.
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
        ThemeAppearance::Dark => T3_CODE_DARK_THEME_COLORS.clone(),
        ThemeAppearance::Light => T3_CODE_LIGHT_THEME_COLORS.clone(),
    }
}

/// Built-ins only for now; custom and environment themes join with the
/// settings task.
pub fn get_theme_definition(theme: &ThemePreference) -> Option<ThemeDefinition> {
    let theme_id = theme_id_from_preference(theme);
    for definition in BUILT_IN_THEMES {
        if definition.id.as_str() == theme_id {
            return Some(definition.clone());
        }
    }
    None
}

/// Artwork palettes are reviewed alongside built-ins; user themes always use the pill fallback.
#[allow(dead_code)] // sidebar artwork lands with the sidebar task.
pub fn theme_allows_sidebar_artwork(theme: &ThemePreference) -> bool {
    let theme_id = theme_id_from_preference(theme);
    for definition in BUILT_IN_THEMES.iter() {
        if definition.id.as_str() == theme_id {
            return definition.sidebar_artwork;
        }
    }
    false
}

/// Which half a theme can claim, or `None` when it renders both appearances.
/// Selecting a single-appearance theme as the base preference would clear the
/// light/dark mix and leave the appearance tiles disagreeing with what is on
/// screen, so every path that selects a theme has to make the same call.
#[allow(dead_code)] // theme picker (T20).
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
        return Some(theme.colors.clone());
    }
    match mode {
        ThemeAppearance::Light => theme.variants.light.clone(),
        ThemeAppearance::Dark => theme.variants.dark.clone(),
    }
}

#[allow(dead_code)] // theme picker (T20); used by single_appearance_of.
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
        Some(definition) if get_theme_colors_for_mode(&definition, requested).is_none() => {
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

#[allow(dead_code)] // settings import validation (T20).
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

    fn contrast_ratio(first: impl AsRef<str>, second: impl AsRef<str>) -> f32 {
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
        let (a, b) = (luminance(first.as_ref()), luminance(second.as_ref()));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn oklab_distance(first: impl AsRef<str>, second: impl AsRef<str>) -> f32 {
        let a = rgba_to_oklab(parse_color(first.as_ref()).unwrap());
        let b = rgba_to_oklab(parse_color(second.as_ref()).unwrap());
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
            palettes.push(theme.colors.clone());
            palettes.extend(theme.variants.light.clone());
            palettes.extend(theme.variants.dark.clone());
        }
        for colors in palettes {
            assert!(
                contrast_ratio(
                    &colors.search_match_foreground,
                    &colors.search_match_background
                ) >= 4.5
            );
            assert!(
                contrast_ratio(
                    &colors.search_match_active_foreground,
                    &colors.search_match_active_background
                ) >= 4.5
            );
            assert!(
                oklab_distance(
                    &colors.search_match_background,
                    &colors.search_match_active_background
                ) > 0.14
            );
            // Check inline-code tints, code blocks, and user-message bubbles.
            for background in [
                &colors.muted,
                &colors.message_surface,
                &colors.code_background,
            ] {
                assert!(oklab_distance(&colors.search_match_background, background) > 0.08);
                assert!(oklab_distance(&colors.search_match_active_background, background) > 0.08);
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
    fn keeps_the_t3_chat_palette_faithful_and_readable() {
        expect_theme_colors(
            &T3_CHAT_THEME.colors,
            &[
                ("canvas", "#fdf7fd"),
                ("chrome", "#fdf7fd"),
                ("toolbar_border", "#efbdeb"),
                ("toolbar_control", "#f3e6f5"),
                ("toolbar_control_hover", "#eccfe3"),
                ("surface_raised", "#fdfafd"),
                ("input", "#e7c1dc"),
                ("focus", "#db2777"),
                ("message_surface", "#f7def2"),
                ("code_background", "#f5ecf9"),
                ("code_foreground", "#673c8b"),
                ("accent_surface", "#f3e6f5"),
                ("sidebar", "#f2e1f4"),
            ],
        );
        expect_theme_colors(
            &T3_CHAT_THEME.variants.dark.expect("dark variant"),
            &[
                ("canvas", "#1f1a24"),
                ("chrome", "#1f1a24"),
                ("surface", "#29232d"),
                ("surface_raised", "#2c2631"),
                ("input", "#302029"),
                ("focus", "#db2777"),
                ("message_surface", "#2b2431"),
                ("code_background", "#1f1a24"),
                ("sidebar", "#171018"),
                ("sidebar_border", "#322028"),
            ],
        );

        for mode in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            let colors = get_theme_colors_for_mode(&T3_CHAT_THEME, mode).expect("both halves");
            assert!(contrast_ratio(&colors.text, &colors.canvas) >= 7.);
            assert!(contrast_ratio(&colors.text_muted, &colors.canvas) >= 4.5);
            assert!(contrast_ratio(&colors.message_foreground, &colors.message_surface) >= 4.5);
            assert!(contrast_ratio(&colors.secondary_foreground, &colors.secondary) >= 4.5);
            assert!(contrast_ratio(&colors.sidebar_foreground, &colors.sidebar) >= 4.5);
            assert!(contrast_ratio(&colors.accent_foreground, &colors.accent) >= 4.5);
        }
    }

    #[test]
    fn includes_the_dual_mode_maintainer_themes() {
        for theme in [
            &T3_CHAT_THEME,
            &GROVE_THEME,
            &OCEAN_THEME,
            &EMBER_THEME,
            &IRIS_THEME,
        ] {
            assert_eq!(get_theme_definition(theme.id.as_str()), Some(theme.clone()));
            assert_eq!(
                get_theme_modes(theme),
                vec![ThemeAppearance::Light, ThemeAppearance::Dark]
            );
            assert!(theme.sidebar_artwork);
            assert!(theme_allows_sidebar_artwork(theme.id.as_str()));
            assert!(theme.colors.accent.starts_with("oklch("));
            assert!(
                theme
                    .variants
                    .dark
                    .as_ref()
                    .is_some_and(|dark| dark.accent.starts_with("oklch("))
            );

            for mode in [ThemeAppearance::Light, ThemeAppearance::Dark] {
                let colors = get_theme_colors_for_mode(theme, mode).expect("both halves");
                assert!(contrast_ratio(&colors.text, &colors.canvas) >= 4.5);
                assert!(contrast_ratio(&colors.text_muted, &colors.canvas) >= 4.5);
                if theme.id.as_str() != T3_CHAT_THEME.id.as_str() {
                    let muted = contrast_ratio(&colors.text_muted, &colors.canvas);
                    assert!(muted < 5.5);
                    let expected = if mode == ThemeAppearance::Dark {
                        5.082
                    } else {
                        4.705
                    };
                    // `toBeCloseTo(expected, 1)`: within 0.05.
                    assert!(
                        (muted - expected).abs() < 0.05,
                        "{}: {muted}",
                        theme.id.as_str()
                    );
                }
                assert!(contrast_ratio(&colors.accent_foreground, &colors.accent) >= 4.5);
                assert!(
                    contrast_ratio(&colors.toolbar_control_foreground, &colors.toolbar_control)
                        >= 4.5
                );
                assert!(contrast_ratio(&colors.message_foreground, &colors.message_surface) >= 4.5);
                assert!(
                    contrast_ratio(&colors.message_action_foreground, &colors.message_action)
                        >= 4.5
                );
                assert!(
                    contrast_ratio(
                        &colors.message_action_foreground,
                        &colors.message_action_hover
                    ) >= 4.5
                );
                assert!(contrast_ratio(&colors.muted_foreground, &colors.muted) >= 4.5);
                assert!(contrast_ratio(&colors.placeholder, &colors.surface_raised) >= 4.5);
            }
        }
        assert!(!theme_allows_sidebar_artwork("my-custom-theme"));
    }

    #[test]
    fn keeps_a_single_mode_theme_on_its_only_palette() {
        let theme = ThemeDefinition {
            id: gpui_kit::SharedString::new_static("midnight-slate"),
            label: gpui_kit::SharedString::new_static("Midnight Slate"),
            appearance: ThemeAppearance::Dark,
            colors: T3_CODE_DARK_THEME_COLORS.clone(),
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
        assert_eq!(
            get_theme_definition("t3-chat-dark"),
            Some(T3_CHAT_THEME.clone())
        );
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
            assert_eq!(get_theme_definition(legacy), Some(theme.clone()));
            assert!(is_known_theme_preference(legacy));
            assert_eq!(canonical_theme_preference(legacy), theme.id.as_str());
        }
        // The dark-variant alias keeps its raw form: it still carries a mode hint.
        assert_eq!(canonical_theme_preference("t3-chat-dark"), "t3-chat-dark");
    }

    #[test]
    fn recognizes_only_preferences_the_runtime_can_render() {
        for preference in [
            "light",
            "dark",
            "system",
            T3_CHAT_THEME.id.as_str(),
            GROVE_THEME.id.as_str(),
        ] {
            assert!(is_known_theme_preference(preference));
        }
        assert!(!is_known_theme_preference("grove:dark"));
        assert!(!is_known_theme_preference("missing-theme"));
    }

    // describe("singleAppearanceOf")

    #[test]
    fn reports_the_only_half_a_theme_can_claim_and_null_for_a_pair() {
        let single = ThemeDefinition {
            id: gpui_kit::SharedString::new_static("x"),
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
