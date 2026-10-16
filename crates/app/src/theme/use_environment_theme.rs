//! Port of upstream/apps/web/src/hooks/useEnvironmentTheme.ts — the machine's
//! published themes as theme-library entries, and the sync that keeps them
//! there. Published themes arrive as [`EnvironmentTheme`]s (see
//! `environment_theme.rs`); the React hooks become [`EnvironmentThemeSync`] and
//! [`use_environment_theme_definitions`].

use serde_json::{Map, Value};

use super::environment_theme::{EnvironmentTheme, EnvironmentThemeColors};
use super::theme_palette_library::{
    create_vivid_theme_colors, get_default_theme_colors, get_environment_themes,
    is_reserved_theme_id, lenient_theme_color_overrides, merge_overrides, set_environment_themes,
};
use super::theme_palettes::{ThemeAppearance, ThemeColors, ThemeDefinition, ThemeVariants};
use super::use_theme::ThemeStore;

fn colors_to_map(colors: &EnvironmentThemeColors) -> Map<String, Value> {
    colors
        .iter()
        .map(|(role, color)| (role.clone(), Value::String(color.clone())))
        .collect()
}

fn published_theme_colors(
    theme: &EnvironmentTheme,
    appearance: ThemeAppearance,
    colors: Option<&EnvironmentThemeColors>,
) -> ThemeColors {
    // Seeds generate the base with the guided theme editor's generator, so a
    // desktop theme arrives as a coherent T3 Code palette rather than a
    // foreign one -- but only for the appearance they describe. A variant
    // builds on that appearance's stock defaults: the generator follows the
    // seed canvas's luminance, so dark seeds would give a light variant
    // unreadable colors.
    let base = match (&theme.canvas, &theme.accent) {
        (Some(canvas), Some(accent)) if appearance == theme.appearance => {
            create_vivid_theme_colors(appearance, canvas, accent)
        }
        _ => get_default_theme_colors(appearance),
    };
    let overrides = colors
        .map(|colors| lenient_theme_color_overrides(&colors_to_map(colors)))
        .unwrap_or_default();
    merge_overrides(base, overrides)
}

/// A published theme as the theme library renders it. Both published forms are
/// accepted: the seeded short form a desktop generates, and the standard
/// exported theme file the Download button produces -- so any theme someone
/// shared can be dropped into the machine's themes directory as-is.
pub fn environment_theme_definition(theme: &EnvironmentTheme) -> ThemeDefinition {
    let mut variants = ThemeVariants {
        light: None,
        dark: None,
    };
    if let Some(published) = &theme.variants {
        for variant_appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            if variant_appearance == theme.appearance {
                continue;
            }
            let Some(variant_colors) = published.get(variant_appearance) else {
                continue;
            };
            let colors = Some(published_theme_colors(
                theme,
                variant_appearance,
                Some(variant_colors),
            ));
            match variant_appearance {
                ThemeAppearance::Light => variants.light = colors,
                ThemeAppearance::Dark => variants.dark = colors,
            }
        }
    }

    ThemeDefinition {
        id: theme.id.clone().into(),
        label: theme.name.clone().into(),
        appearance: theme.appearance,
        colors: published_theme_colors(theme, theme.appearance, theme.colors.as_ref()),
        variants,
        collection: None,
        // Only the pure seeded form is guided-generator output. An exported
        // file, or seeds carrying explicit role overrides, must open Duplicate
        // in the advanced editor -- the guided one regenerates from canvas and
        // accent and would discard whatever the machine hand-tuned.
        managed: theme.canvas.is_some()
            && theme.accent.is_some()
            && theme.colors.is_none()
            && theme.variants.is_none(),
        sidebar_artwork: false,
    }
}

/// Published palettes this client can render. Reserved ids are dropped here
/// rather than rendered: a published `t3-iris.json` would show this palette on
/// its card while "Use" resolved the built-in, and a published `dark.json`
/// would capture everyone whose stored preference is the stock `"dark"`.
pub fn published_theme_definitions(themes: &[EnvironmentTheme]) -> Vec<ThemeDefinition> {
    themes
        .iter()
        .filter(|theme| {
            if is_reserved_theme_id(&theme.id) {
                return false;
            }
            if theme.canvas.is_some() && theme.accent.is_some() {
                return true;
            }
            let other_appearance = match theme.appearance {
                ThemeAppearance::Dark => ThemeAppearance::Light,
                ThemeAppearance::Light => ThemeAppearance::Dark,
            };
            [
                theme.colors.as_ref(),
                theme
                    .variants
                    .as_ref()
                    .and_then(|variants| variants.get(other_appearance)),
            ]
            .into_iter()
            .flatten()
            .any(|colors| !lenient_theme_color_overrides(&colors_to_map(colors)).is_empty())
        })
        .map(environment_theme_definition)
        .collect()
}

/// The published themes as library entries; empty while none are published.
pub fn use_environment_theme_definitions() -> Vec<ThemeDefinition> {
    get_environment_themes()
}

/// `useEnvironmentThemeSync`: keeps the machine's published themes in the theme
/// library for as long as the primary environment publishes them. A client
/// with a published theme selected retints the moment the machine rewrites it;
/// everyone else just gains cards in the theme library.
#[derive(Default)]
pub struct EnvironmentThemeSync {
    last_published: Option<Vec<EnvironmentTheme>>,
}

impl EnvironmentThemeSync {
    /// Call whenever the primary environment's published set is delivered.
    pub fn sync(&mut self, published: &[EnvironmentTheme], store: &ThemeStore) {
        // Every reconnect snapshot delivers a fresh but usually identical
        // array; regenerating palettes and repainting the document for it is
        // exactly the wasted-frame class this codebase audits for.
        if self.last_published.as_deref() == Some(published) {
            return;
        }
        self.last_published = Some(published.to_vec());

        // The palette is painted from a snapshot taken when the theme last
        // changed, so new colors only land if the active theme is re-applied.
        if set_environment_themes(published_theme_definitions(published)) {
            store.refresh_theme(true);
        }
    }
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/hooks/useEnvironmentTheme.test.ts.
    use std::sync::Arc;

    use super::super::environment_theme::EnvironmentThemeFile;
    use super::super::theme_palette::get_theme_definition;
    use super::super::theme_palette_library::{
        InMemoryThemeLibraryStorage, install_custom_theme, invalidate_custom_themes,
        set_theme_library_storage,
    };
    use super::super::theme_palettes::THEME_COLOR_ROLES;
    use super::*;
    use crate::theme::THEME_GLOBALS_TEST_LOCK;

    fn colors(entries: &[(&str, &str)]) -> Option<EnvironmentThemeColors> {
        Some(
            entries
                .iter()
                .map(|(role, color)| (role.to_string(), color.to_string()))
                .collect(),
        )
    }

    fn theme(id: &str, name: &str, appearance: ThemeAppearance) -> EnvironmentTheme {
        EnvironmentTheme {
            id: id.to_string(),
            file: EnvironmentThemeFile {
                version: None,
                name: name.to_string(),
                appearance,
                canvas: None,
                accent: None,
                colors: None,
                variants: None,
            },
        }
    }

    fn nightfall() -> EnvironmentTheme {
        let mut nightfall = theme("nightfall", "Nightfall", ThemeAppearance::Dark);
        nightfall.file.canvas = Some("#1a1b26".to_string());
        nightfall.file.accent = Some("#7aa2f7".to_string());
        nightfall
    }

    fn role<'a>(colors: &'a ThemeColors, name: &str) -> &'a str {
        colors
            .entries()
            .into_iter()
            .find(|(role, _)| *role == name)
            .unwrap_or_else(|| panic!("unknown role {name}"))
            .1
    }

    fn assert_every_role_present(colors: &ThemeColors) {
        for name in THEME_COLOR_ROLES {
            assert!(!role(colors, name).is_empty(), "missing {name}");
        }
    }

    #[test]
    fn generates_every_role_from_the_two_published_seeds() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let definition = environment_theme_definition(&nightfall());
        assert_eq!(definition.id.as_str(), "nightfall");
        assert_eq!(definition.label.as_str(), "Nightfall");
        assert_eq!(definition.appearance, ThemeAppearance::Dark);
        assert_every_role_present(&definition.colors);
    }

    #[test]
    fn layers_published_roles_over_the_generated_palette() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let generated = environment_theme_definition(&nightfall());
        let mut overridden = nightfall();
        overridden.file.colors = colors(&[("terminalSelection", "#292e42"), ("error", "#f7768e")]);
        let overridden = environment_theme_definition(&overridden);

        assert_ne!(
            role(&overridden.colors, "terminal_selection"),
            role(&generated.colors, "terminal_selection")
        );
        assert_ne!(
            role(&overridden.colors, "error"),
            role(&generated.colors, "error")
        );
        // Roles the machine did not publish keep the generated value.
        assert_eq!(
            role(&overridden.colors, "sidebar"),
            role(&generated.colors, "sidebar")
        );
    }

    // The standard exported theme file -- the Download button's output -- is a
    // valid published theme, so any shared theme can be dropped into the
    // machine's themes directory as-is.
    #[test]
    fn renders_an_exported_theme_file_on_the_stock_defaults() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let mut shared = theme("shared-light", "Shared Light", ThemeAppearance::Light);
        shared.file.version = Some(1);
        shared.file.colors = colors(&[("canvas", "oklch(0.95 0.01 250)"), ("accent", "#1e66f5")]);
        shared.file.variants = Some(super::super::environment_theme::EnvironmentThemeVariants {
            light: None,
            dark: colors(&[("canvas", "#1a1b26")]),
        });
        let definition = environment_theme_definition(&shared);

        assert_eq!(definition.label.as_str(), "Shared Light");
        assert!(!role(&definition.colors, "accent").is_empty());
        let dark = definition.variants.dark.expect("dark variant");
        assert!(!role(&dark, "canvas").is_empty());
        assert_every_role_present(&definition.colors);
        assert_every_role_present(&dark);
    }

    // The generator follows the seed canvas's luminance, so a dark theme's
    // seeds must never produce its light variant: the variant builds on that
    // appearance's stock defaults instead.
    #[test]
    fn builds_a_seeded_themes_variant_on_the_variant_appearances_defaults() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let mut seeded = nightfall();
        seeded.file.variants = Some(super::super::environment_theme::EnvironmentThemeVariants {
            light: colors(&[("canvas", "#eff1f5")]),
            dark: None,
        });
        let definition = environment_theme_definition(&seeded);

        let light_defaults = get_default_theme_colors(ThemeAppearance::Light);
        let light = definition.variants.light.expect("light variant");
        assert_eq!(role(&light, "text"), role(&light_defaults, "text"));
        assert_ne!(role(&light, "canvas"), role(&definition.colors, "canvas"));
    }

    // A published `t3-iris.json` would show its palette on a card that applies
    // the built-in, and a published `dark.json` would capture everyone whose
    // stored preference is the stock "dark". Reserved ids never become cards.
    #[test]
    fn drops_published_themes_with_reserved_ids() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let impostor = |id: &str, name: &str| EnvironmentTheme {
            id: id.to_string(),
            file: EnvironmentThemeFile {
                name: name.to_string(),
                ..nightfall().file
            },
        };
        let definitions = published_theme_definitions(&[
            nightfall(),
            impostor("t3-iris", "Impostor Iris"),
            impostor("ocean", "Impostor Ocean"),
            impostor("dark", "Impostor Dark"),
        ]);
        let ids: Vec<&str> = definitions.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["nightfall"]);
    }

    #[test]
    fn drops_published_palettes_with_no_usable_colors() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let invalid = |id: &str| theme(id, "Invalid", ThemeAppearance::Dark);
        let mut bad_canvas = invalid("invalid");
        bad_canvas.file.colors = colors(&[("canvas", "not-a-color")]);
        let mut unknown_roles = invalid("unknown-roles");
        unknown_roles.file.colors = colors(&[("futureRole", "#ffffff")]);
        let mut ignored_variant = invalid("ignored-variant");
        ignored_variant.file.colors = colors(&[("canvas", "not-a-color")]);
        ignored_variant.file.variants =
            Some(super::super::environment_theme::EnvironmentThemeVariants {
                light: None,
                dark: colors(&[("canvas", "#112233")]),
            });
        assert!(
            published_theme_definitions(&[bad_canvas, unknown_roles, ignored_variant]).is_empty()
        );
    }

    #[test]
    fn keeps_seeds_partial_palettes_and_usable_alternate_appearances() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let shared = |id: &str| theme(id, "Shared", ThemeAppearance::Dark);
        let mut partial = shared("partial");
        partial.file.colors = colors(&[
            ("canvas", "not-a-color"),
            ("text", "#ffffff"),
            ("futureRole", "#ffffff"),
        ]);
        let mut light_variant = shared("light-variant");
        light_variant.file.colors = colors(&[("canvas", "not-a-color")]);
        light_variant.file.variants =
            Some(super::super::environment_theme::EnvironmentThemeVariants {
                light: colors(&[("canvas", "#eff1f5")]),
                dark: None,
            });
        let definitions = published_theme_definitions(&[nightfall(), partial, light_variant]);
        let ids: Vec<&str> = definitions.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["nightfall", "partial", "light-variant"]);
    }

    // A machine may publish roles a newer client added; an older one has to
    // ignore them rather than render a broken palette.
    #[test]
    fn ignores_published_roles_this_build_does_not_render() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let mut future = nightfall();
        future.file.colors = colors(&[("notARole", "#ff0000"), ("text", "#ffffff")]);
        let definition = environment_theme_definition(&future);
        assert!(
            definition
                .colors
                .entries()
                .iter()
                .all(|(role, _)| *role != "not_a_role" && *role != "notARole")
        );
        assert_every_role_present(&definition.colors);
    }

    // ThemeEditorPanel opens Duplicate in the guided editor for managed
    // themes, and the guided editor regenerates from canvas and accent -- which
    // would discard any role the machine tuned by hand.
    #[test]
    fn marks_only_the_pure_seeded_form_as_managed() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        assert!(environment_theme_definition(&nightfall()).managed);
        let mut tuned = nightfall();
        tuned.file.colors = colors(&[("error", "#f7768e")]);
        assert!(!environment_theme_definition(&tuned).managed);
        let mut exported = theme("shared-light", "Shared Light", ThemeAppearance::Light);
        exported.file.version = Some(1);
        exported.file.colors = colors(&[("canvas", "#eff1f5"), ("accent", "#1e66f5")]);
        assert!(!environment_theme_definition(&exported).managed);
    }

    #[test]
    fn resolves_published_ids_only_while_the_machine_publishes_them() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_environment_themes(vec![]);
        assert!(get_theme_definition("nightfall").is_none());

        set_environment_themes(vec![environment_theme_definition(&nightfall())]);
        assert_eq!(
            get_theme_definition("nightfall")
                .expect("published")
                .label
                .as_str(),
            "Nightfall"
        );

        // The palettes are never saved, so they have to disappear with the
        // machine that published them rather than linger as stale entries.
        set_environment_themes(vec![]);
        assert!(get_theme_definition("nightfall").is_none());
    }

    // Published ids share one namespace with the user's saved themes, and the
    // user was here first: their theme keeps working even if the machine later
    // publishes under the same id.
    #[test]
    fn lets_a_theme_the_user_saved_win_an_id_collision() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        let environment = environment_theme_definition(&nightfall());
        set_environment_themes(vec![environment.clone()]);
        install_custom_theme(ThemeDefinition {
            label: "My Nightfall".into(),
            ..environment
        })
        .expect("install");

        assert_eq!(
            get_theme_definition("nightfall")
                .expect("saved")
                .label
                .as_str(),
            "My Nightfall"
        );
        // Dropping the saved theme restores the published one.
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        assert_eq!(
            get_theme_definition("nightfall")
                .expect("published")
                .label
                .as_str(),
            "Nightfall"
        );
        set_environment_themes(vec![]);
    }

    #[test]
    fn sync_republishes_only_when_the_set_changes() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_environment_themes(vec![]);
        let store = ThemeStore::new(
            Arc::new(super::super::use_theme::MemoryThemeStorage::default()),
            Arc::new(AlwaysLight),
            None,
        );
        let mut sync = EnvironmentThemeSync::default();
        sync.sync(&[nightfall()], &store);
        assert_eq!(get_environment_themes().len(), 1);

        // An identical reconnect snapshot is dropped, even after the library
        // moved on underneath.
        set_environment_themes(vec![]);
        sync.sync(&[nightfall()], &store);
        assert!(get_environment_themes().is_empty());

        sync.sync(&[], &store);
        assert!(get_environment_themes().is_empty());
        sync.sync(&[nightfall()], &store);
        assert_eq!(get_environment_themes().len(), 1);
        set_environment_themes(vec![]);
    }

    struct AlwaysLight;

    impl super::super::use_theme::SystemAppearance for AlwaysLight {
        fn is_dark(&self) -> bool {
            false
        }
    }
}
