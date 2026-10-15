//! The palette's "Change theme" / "Change appearance" plumbing: the theme
//! cards (`STANDARD_THEME_CARDS` + `BUILT_IN_THEMES` through
//! `getThemeCardDefinition`) and the preference writes `useTheme`'s
//! `setTheme` / `setAppearanceMode` perform. Custom and environment themes
//! and light/dark halves belong to Settings (T20), as in `theme::theme_palette`.

use gpui_kit::App;

use super::command_palette_logic::{AppearanceModeChoice, ThemePreviewSpec};
use super::palette_source::ThemeCard;
use crate::theme::theme_palette::{
    ThemePreferenceMode, get_theme_colors_for_mode, get_theme_modes,
};
use crate::theme::theme_palettes::{BUILT_IN_THEMES, ThemeAppearance};
use crate::theme::theme_preview::standard_theme_preview_colors;
use crate::theme::{self, AppearancePreferences};

/// `STANDARD_THEME_CARDS` followed by the built-in themes.
pub fn theme_cards() -> Vec<ThemeCard> {
    let standard_previews = [ThemeAppearance::Light, ThemeAppearance::Dark]
        .into_iter()
        .map(|mode| {
            let colors = standard_theme_preview_colors(mode);
            ThemePreviewSpec {
                dark: mode.is_dark(),
                canvas: colors.canvas.to_owned(),
                accent: colors.accent.to_owned(),
                message_action: colors.message_action.to_owned(),
            }
        })
        .collect();
    let mut cards = vec![ThemeCard {
        id: None,
        label: "T3 Code".to_owned(),
        previews: standard_previews,
    }];
    for definition in BUILT_IN_THEMES.iter() {
        let previews = get_theme_modes(definition)
            .into_iter()
            .map(|mode| {
                let colors =
                    get_theme_colors_for_mode(definition, mode).unwrap_or(definition.colors);
                ThemePreviewSpec {
                    dark: mode.is_dark(),
                    canvas: colors.canvas.to_owned(),
                    accent: colors.accent.to_owned(),
                    message_action: colors.message_action.to_owned(),
                }
            })
            .collect();
        cards.push(ThemeCard {
            id: Some(definition.id.to_owned()),
            label: definition.label.to_owned(),
            previews,
        });
    }
    cards
}

fn mode_of(preferences: &AppearancePreferences) -> AppearanceModeChoice {
    match preferences.appearance_mode {
        Some(ThemePreferenceMode::Light) => AppearanceModeChoice::Light,
        Some(ThemePreferenceMode::Dark) => AppearanceModeChoice::Dark,
        Some(ThemePreferenceMode::System) => AppearanceModeChoice::System,
        None => match preferences.theme.as_str() {
            "light" => AppearanceModeChoice::Light,
            "dark" => AppearanceModeChoice::Dark,
            _ => AppearanceModeChoice::System,
        },
    }
}

/// The current appearance mode (`useTheme().appearanceMode`).
pub fn current_appearance_mode(cx: &App) -> AppearanceModeChoice {
    mode_of(&theme::preferences(cx))
}

/// The applied theme's card id (`None` for the standard look).
pub fn active_theme_id(cx: &App) -> Option<String> {
    let preferences = theme::preferences(cx);
    crate::theme::theme_palette::get_theme_definition(&preferences.theme)
        .map(|definition| definition.id.to_owned())
}

fn preference_mode(choice: AppearanceModeChoice) -> ThemePreferenceMode {
    match choice {
        AppearanceModeChoice::System => ThemePreferenceMode::System,
        AppearanceModeChoice::Light => ThemePreferenceMode::Light,
        AppearanceModeChoice::Dark => ThemePreferenceMode::Dark,
    }
}

fn choice_name(choice: AppearanceModeChoice) -> &'static str {
    match choice {
        AppearanceModeChoice::System => "system",
        AppearanceModeChoice::Light => "light",
        AppearanceModeChoice::Dark => "dark",
    }
}

/// `setAppearanceMode(mode)`.
pub fn apply_appearance_mode(choice: AppearanceModeChoice, cx: &mut App) {
    let mut preferences = theme::preferences(cx);
    preferences.appearance_mode = Some(preference_mode(choice));
    if matches!(preferences.theme.as_str(), "system" | "light" | "dark") {
        preferences.theme = choice_name(choice).to_owned();
    }
    theme::set_preferences(preferences, cx);
}

/// `setTheme(id ?? appearanceMode)`.
pub fn apply_theme(id: Option<String>, cx: &mut App) {
    let mut preferences = theme::preferences(cx);
    let mode = mode_of(&preferences);
    preferences.theme = id.unwrap_or_else(|| choice_name(mode).to_owned());
    theme::set_preferences(preferences, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_standard_look_then_the_built_in_themes() {
        let cards = theme_cards();
        assert_eq!(cards[0].id, None);
        assert_eq!(cards[0].label, "T3 Code");
        assert_eq!(cards[0].previews.len(), 2);
        let ids: Vec<Option<&str>> = cards.iter().map(|card| card.id.as_deref()).collect();
        assert_eq!(
            ids,
            vec![
                None,
                Some("t3-chat"),
                Some("grove"),
                Some("ocean"),
                Some("ember"),
                Some("iris")
            ]
        );
    }

    #[test]
    fn reads_the_appearance_mode_from_the_preferences() {
        let mut preferences = AppearancePreferences::default();
        assert_eq!(mode_of(&preferences), AppearanceModeChoice::System);
        preferences.theme = "dark".into();
        assert_eq!(mode_of(&preferences), AppearanceModeChoice::Dark);
        preferences.appearance_mode = Some(ThemePreferenceMode::Light);
        assert_eq!(mode_of(&preferences), AppearanceModeChoice::Light);
    }
}
