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

// Ported ahead of the views that consume it (theme UI, T22 wiring).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use super::theme_palette_library::{get_custom_themes, get_environment_themes, is_theme_color};
use super::theme_palettes::{
    BUILT_IN_THEMES, T3_CODE_DARK_THEME_COLORS, T3_CODE_LIGHT_THEME_COLORS, ThemeAppearance,
    ThemeColors, ThemeDefinition,
};

pub const T3_CHAT_THEME_ID: &str = "t3-chat";
const GROVE_THEME_ID: &str = "grove";
pub const OCEAN_THEME_ID: &str = "ocean";
const EMBER_THEME_ID: &str = "ember";
const IRIS_THEME_ID: &str = "iris";
pub const CURSOR_THEME_ID: &str = "cursor";

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

pub fn get_theme_definition(theme: &ThemePreference) -> Option<ThemeDefinition> {
    let theme_id = theme_id_from_preference(theme);
    for definition in BUILT_IN_THEMES {
        if definition.id.as_str() == theme_id {
            return Some(definition.clone());
        }
    }
    get_custom_themes()
        .into_iter()
        .find(|definition| definition.id.as_str() == theme_id)
        .or_else(|| {
            get_environment_themes()
                .into_iter()
                .find(|definition| definition.id.as_str() == theme_id)
        })
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

/// `resolveThemeAppearance`.
pub fn resolve_theme_appearance(
    theme: &ThemePreference,
    system_dark: bool,
    follow_system: Option<bool>,
    appearance_mode: Option<ThemePreferenceMode>,
    halves: Option<&ThemeHalves>,
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
    let resolve = |requested: ThemeAppearance| {
        // A configured half guarantees the appearance is renderable even when
        // the base theme lacks that mode.
        if halves.is_some_and(|halves| halves.get(requested).is_some()) {
            return requested;
        }
        match get_theme_definition(theme) {
            Some(definition) if get_theme_colors_for_mode(&definition, requested).is_none() => {
                definition.appearance
            }
            _ => requested,
        }
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

// ── Stored appearance: mixes ("halves"), desktop theme, live preview ──

pub const THEME_FOLLOW_SYSTEM_STORAGE_KEY: &str = "t3code:theme-follow-system";
pub const THEME_APPEARANCE_MODE_STORAGE_KEY: &str = "t3code:theme-appearance-mode";
pub const THEME_HALVES_STORAGE_KEY: &str = "t3code:theme-halves:v1";

/// Marks the root as wearing an unsaved draft rather than a stored theme.
pub const THEME_PREVIEW_ID: &str = "__preview";

impl ThemePreferenceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

fn appearance_key(appearance: ThemeAppearance) -> &'static str {
    match appearance {
        ThemeAppearance::Light => "light",
        ThemeAppearance::Dark => "dark",
    }
}

/// What the desktop shell is told (`resolveDesktopTheme`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopTheme {
    Light,
    Dark,
    System,
}

impl DesktopTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

impl From<ThemeAppearance> for DesktopTheme {
    fn from(appearance: ThemeAppearance) -> Self {
        match appearance {
            ThemeAppearance::Light => Self::Light,
            ThemeAppearance::Dark => Self::Dark,
        }
    }
}

/// An automatic-mode mix: a different theme per resolved appearance. Halves
/// only name real themes that can render their half; anything else is dropped
/// so a stale mix degrades to the base preference.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeHalves {
    pub light: Option<String>,
    pub dark: Option<String>,
}

impl ThemeHalves {
    pub fn get(&self, appearance: ThemeAppearance) -> Option<&str> {
        match appearance {
            ThemeAppearance::Light => self.light.as_deref(),
            ThemeAppearance::Dark => self.dark.as_deref(),
        }
    }

    pub fn set(&mut self, appearance: ThemeAppearance, theme_id: Option<String>) {
        match appearance {
            ThemeAppearance::Light => self.light = theme_id,
            ThemeAppearance::Dark => self.dark = theme_id,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.light.is_none() && self.dark.is_none()
    }
}

pub fn parse_theme_halves(raw: Option<&str>) -> Option<ThemeHalves> {
    let raw = raw.filter(|raw| !raw.is_empty())?;
    let value: Value = serde_json::from_str(raw).ok()?;
    let record = value.as_object()?;
    let mut halves = ThemeHalves::default();
    for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
        let Some(theme_id) = record
            .get(appearance_key(appearance))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(definition) = get_theme_definition(theme_id)
            && get_theme_colors_for_mode(&definition, appearance).is_some()
        {
            // Store the definition's id so legacy aliases resolve to the same
            // value the runtime applies to the document.
            halves.set(appearance, Some(definition.id.to_string()));
        }
    }
    (!halves.is_empty()).then_some(halves)
}

/// The theme that should render the given appearance under a mix, if any.
pub fn resolve_theme_half(
    theme: &ThemePreference,
    halves: Option<&ThemeHalves>,
    appearance: ThemeAppearance,
) -> String {
    halves
        .and_then(|halves| halves.get(appearance))
        .unwrap_or(theme)
        .to_string()
}

pub fn resolve_desktop_theme(
    theme: &ThemePreference,
    follow_system: Option<bool>,
    appearance_mode: Option<ThemePreferenceMode>,
    halves: Option<&ThemeHalves>,
) -> DesktopTheme {
    let mode = appearance_mode.or_else(|| {
        follow_system
            .unwrap_or(theme == "system")
            .then_some(ThemePreferenceMode::System)
    });
    match mode {
        Some(ThemePreferenceMode::System) => {
            let definition = get_theme_definition(theme);
            // A configured half fills in an appearance the base theme cannot render.
            let has_mode = |appearance: ThemeAppearance| {
                halves.is_some_and(|halves| halves.get(appearance).is_some())
                    || definition.as_ref().is_some_and(|definition| {
                        get_theme_colors_for_mode(definition, appearance).is_some()
                    })
            };
            match definition {
                Some(definition)
                    if !has_mode(ThemeAppearance::Light) || !has_mode(ThemeAppearance::Dark) =>
                {
                    definition.appearance.into()
                }
                _ => DesktopTheme::System,
            }
        }
        Some(mode @ (ThemePreferenceMode::Light | ThemePreferenceMode::Dark)) => {
            let appearance = if mode == ThemePreferenceMode::Dark {
                ThemeAppearance::Dark
            } else {
                ThemeAppearance::Light
            };
            if halves.is_some_and(|halves| halves.get(appearance).is_some()) {
                return appearance.into();
            }
            match get_theme_definition(theme) {
                Some(definition)
                    if get_theme_colors_for_mode(&definition, appearance).is_none() =>
                {
                    definition.appearance.into()
                }
                _ => appearance.into(),
            }
        }
        None => get_theme_preference_mode(theme).map_or(DesktopTheme::System, Into::into),
    }
}

// Live custom-theme preview: the editor paints a draft onto the running app.

struct ThemePreviewState {
    sidebar_artwork: Option<bool>,
    next_listener_id: u64,
    listeners: Vec<(u64, Arc<dyn Fn() + Send + Sync>)>,
}

static THEME_PREVIEW_STATE: Mutex<ThemePreviewState> = Mutex::new(ThemePreviewState {
    sidebar_artwork: None,
    next_listener_id: 0,
    listeners: Vec::new(),
});

pub fn get_theme_preview_sidebar_artwork() -> Option<bool> {
    THEME_PREVIEW_STATE.lock().sidebar_artwork
}

/// Returns the unsubscribe handle.
pub fn subscribe_to_theme_preview(
    listener: impl Fn() + Send + Sync + 'static,
) -> Box<dyn FnOnce() + Send> {
    let id = {
        let mut state = THEME_PREVIEW_STATE.lock();
        let id = state.next_listener_id;
        state.next_listener_id += 1;
        state.listeners.push((id, Arc::new(listener)));
        id
    };
    Box::new(move || {
        THEME_PREVIEW_STATE
            .lock()
            .listeners
            .retain(|(listener_id, _)| *listener_id != id);
    })
}

fn set_theme_preview_sidebar_artwork(next: Option<bool>) {
    let listeners = {
        let mut state = THEME_PREVIEW_STATE.lock();
        if state.sidebar_artwork == next {
            return;
        }
        state.sidebar_artwork = next;
        state
            .listeners
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect::<Vec<_>>()
    };
    for listener in listeners {
        listener();
    }
}

/// Stand-in for `document.documentElement`: the theme id (`data-theme-id`),
/// the `dark` class, and the role colors painted as CSS variables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeRoot {
    pub theme_id: Option<String>,
    pub dark: bool,
    pub colors: BTreeMap<&'static str, String>,
}

impl ThemeRoot {
    pub fn is_previewing(&self) -> bool {
        self.theme_id.as_deref() == Some(THEME_PREVIEW_ID)
    }
}

/// Paint a draft palette onto the live app without installing it, so the editor
/// can be judged against the real interface instead of a miniature. Callers
/// restore the stored theme (refreshTheme) when the draft goes away.
pub fn apply_theme_color_preview(
    root: &mut ThemeRoot,
    colors: &ThemeColors,
    appearance: ThemeAppearance,
) {
    // Drafts become user-controlled themes when saved, so their preview keeps
    // the fixed stage artwork hidden even when it was seeded from a built-in.
    set_theme_preview_sidebar_artwork(Some(false));
    root.theme_id = Some(THEME_PREVIEW_ID.to_string());
    root.dark = appearance.is_dark();
    for (role, value) in colors.entries() {
        // A half-typed hex keeps the last good value instead of blanking the role.
        if is_theme_color(value) {
            root.colors.insert(role, value.to_string());
        }
    }
}

pub fn apply_theme_palette(
    root: &mut ThemeRoot,
    theme: &ThemePreference,
    appearance: Option<ThemeAppearance>,
) {
    set_theme_preview_sidebar_artwork(None);
    match get_theme_definition(theme) {
        Some(palette) => {
            root.theme_id = Some(palette.id.to_string());
            let mode = appearance
                .or_else(|| legacy_theme_mode(theme))
                .unwrap_or(palette.appearance);
            let colors =
                get_theme_colors_for_mode(&palette, mode).unwrap_or_else(|| palette.colors.clone());
            for (role, value) in colors.entries() {
                root.colors.insert(role, value.to_string());
            }
        }
        None => {
            root.theme_id = None;
            root.colors.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::{parse_color, rgba_to_oklab, to_hex};
    use crate::theme::theme_palettes::{
        EMBER_THEME, GROVE_THEME, IRIS_THEME, OCEAN_THEME, T3_CHAT_THEME, ThemeVariants,
        get_theme_colors_for_appearance,
    };

    fn as_hex(value: &str) -> String {
        to_hex(parse_color(value).expect("theme color"))
    }

    #[test]
    fn cursor_theme_resolves_and_is_darker_than_cursors_editor_background() {
        let theme = get_theme_definition(CURSOR_THEME_ID).expect("cursor theme");
        assert_eq!(theme.label.as_str(), "Cursor");
        assert_eq!(
            get_theme_modes(&theme),
            vec![ThemeAppearance::Light, ThemeAppearance::Dark]
        );
        // Cursor Light / Cursor Dark `editor.background`.
        for (appearance, original) in [
            (ThemeAppearance::Light, "#FCFCFC"),
            (ThemeAppearance::Dark, "#181818"),
        ] {
            let colors = get_theme_colors_for_appearance(&theme, appearance).expect("colors");
            let lightness = |value: &str| rgba_to_oklab(parse_color(value).expect("color")).l;
            let drop = lightness(original) - lightness(&colors.canvas);
            assert!((0.02..=0.05).contains(&drop), "{appearance:?} drop {drop}");
            for (role, value) in colors.entries() {
                let alpha = parse_color(value).expect("color").a;
                assert!((alpha - 1.0).abs() < f32::EPSILON, "{role} is translucent");
            }
        }
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
            collection: None,
            managed: false,
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
            resolve_theme_appearance("t3-chat-dark", true, Some(false), None, None),
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
            resolve_theme_appearance("system", true, None, None, None),
            ThemeAppearance::Dark
        );
        assert_eq!(
            resolve_theme_appearance("system", false, None, None, None),
            ThemeAppearance::Light
        );
        assert_eq!(
            resolve_theme_appearance("dark", false, None, None, None),
            ThemeAppearance::Dark
        );
    }

    // Stored mixes, desktop theme, live preview.

    use crate::theme::THEME_GLOBALS_TEST_LOCK;
    use crate::theme::theme_palette_library::{
        InMemoryThemeLibraryStorage, THEME_FILE_VERSION, install_custom_theme,
        invalidate_custom_themes, parse_theme_file, remove_custom_theme, set_theme_library_storage,
    };
    use crate::theme::theme_palettes::THEME_COLOR_ROLES;

    #[test]
    fn lets_a_configured_half_unlock_an_appearance_the_base_theme_lacks() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        // Light-only base: without halves, dark requests fall back to light.
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        install_custom_theme(
            parse_theme_file(serde_json::json!({
                "version": THEME_FILE_VERSION,
                "id": "paper",
                "name": "Paper",
                "appearance": "light",
                "colors": { "canvas": "#f8fbff" },
            }))
            .expect("paper"),
        )
        .expect("install");

        assert_eq!(
            resolve_theme_appearance("paper", true, Some(true), None, None),
            ThemeAppearance::Light
        );
        let halves = ThemeHalves {
            light: None,
            dark: Some(GROVE_THEME.id.to_string()),
        };
        assert_eq!(
            resolve_theme_appearance("paper", true, Some(true), None, Some(&halves)),
            ThemeAppearance::Dark
        );
        assert_eq!(
            resolve_theme_appearance(
                "paper",
                false,
                Some(false),
                Some(ThemePreferenceMode::Dark),
                Some(&halves)
            ),
            ThemeAppearance::Dark
        );
        assert_eq!(
            resolve_desktop_theme("paper", Some(true), None, Some(&halves)),
            DesktopTheme::System
        );
        // Without the half the desktop is pinned to what the base can render.
        assert_eq!(
            resolve_desktop_theme("paper", Some(true), None, None),
            DesktopTheme::Light
        );

        remove_custom_theme("paper").expect("remove");
        invalidate_custom_themes();
    }

    #[test]
    fn resolves_the_desktop_theme_for_dual_mode_and_legacy_preferences() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        assert_eq!(
            resolve_desktop_theme(T3_CHAT_THEME.id.as_str(), Some(true), None, None),
            DesktopTheme::System
        );
        assert_eq!(
            resolve_desktop_theme(
                T3_CHAT_THEME.id.as_str(),
                Some(false),
                Some(ThemePreferenceMode::Dark),
                None
            ),
            DesktopTheme::Dark
        );
        assert_eq!(
            resolve_desktop_theme("t3-chat-dark", Some(false), None, None),
            DesktopTheme::Dark
        );
        assert_eq!(
            resolve_desktop_theme("system", None, None, None),
            DesktopTheme::System
        );
        assert_eq!(
            resolve_desktop_theme("dark", Some(false), None, None),
            DesktopTheme::Dark
        );
    }

    #[test]
    fn parses_stored_mixes_onto_canonical_ids_and_drops_the_unrenderable() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        // A stored mix that predates the rename resolves to the new ids.
        assert_eq!(
            parse_theme_halves(Some(r#"{"light":"t3-ocean","dark":"t3-grove"}"#)),
            Some(ThemeHalves {
                light: Some(OCEAN_THEME.id.to_string()),
                dark: Some(GROVE_THEME.id.to_string()),
            })
        );
        assert_eq!(
            parse_theme_halves(Some(r#"{"light":"missing","dark":7}"#)),
            None
        );
        assert_eq!(
            parse_theme_halves(Some(r#"{"light":"missing","dark":"grove"}"#)),
            Some(ThemeHalves {
                light: None,
                dark: Some("grove".to_string()),
            })
        );
        assert_eq!(parse_theme_halves(Some("not json")), None);
        assert_eq!(parse_theme_halves(Some("[]")), None);
        assert_eq!(parse_theme_halves(Some("")), None);
        assert_eq!(parse_theme_halves(None), None);
        let halves = ThemeHalves {
            light: None,
            dark: Some("grove".to_string()),
        };
        assert_eq!(
            resolve_theme_half("ocean", Some(&halves), ThemeAppearance::Dark),
            "grove"
        );
        assert_eq!(
            resolve_theme_half("ocean", Some(&halves), ThemeAppearance::Light),
            "ocean"
        );
        assert_eq!(
            resolve_theme_half("ocean", None, ThemeAppearance::Dark),
            "ocean"
        );
    }

    #[test]
    fn suppresses_sidebar_artwork_during_a_live_custom_theme_preview() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let unsubscribe = subscribe_to_theme_preview(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        // Start from a known state: nothing previewed.
        let mut root = ThemeRoot::default();
        apply_theme_palette(&mut root, "system", None);
        calls.store(0, Ordering::SeqCst);

        apply_theme_color_preview(&mut root, &T3_CHAT_THEME.colors, ThemeAppearance::Light);
        assert_eq!(get_theme_preview_sidebar_artwork(), Some(false));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(root.is_previewing());
        assert!(!root.dark);

        apply_theme_palette(&mut root, "system", None);
        assert_eq!(get_theme_preview_sidebar_artwork(), None);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!root.is_previewing());

        unsubscribe();
    }

    #[test]
    fn a_preview_keeps_the_last_good_value_for_half_typed_colors() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let mut root = ThemeRoot::default();
        apply_theme_color_preview(&mut root, &T3_CHAT_THEME.colors, ThemeAppearance::Dark);
        let good = root.colors.get("accent").cloned().expect("accent");
        let mut draft = T3_CHAT_THEME.colors.clone();
        draft.accent = "#12".into();
        apply_theme_color_preview(&mut root, &draft, ThemeAppearance::Dark);
        assert_eq!(root.colors.get("accent"), Some(&good));
        assert!(root.dark);
    }

    #[test]
    fn applying_a_palette_paints_its_roles_and_the_standard_look_clears_them() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let mut root = ThemeRoot::default();
        apply_theme_palette(&mut root, "grove", Some(ThemeAppearance::Dark));
        assert_eq!(root.theme_id.as_deref(), Some("grove"));
        assert_eq!(root.colors.len(), THEME_COLOR_ROLES.len());
        apply_theme_palette(&mut root, "dark", Some(ThemeAppearance::Dark));
        assert_eq!(root.theme_id, None);
        assert!(root.colors.is_empty());
    }
}
