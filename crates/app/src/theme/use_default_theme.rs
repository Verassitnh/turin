//! Port of upstream/apps/web/src/hooks/useDefaultTheme.ts — adopting the
//! environment's `defaultTheme` (`t3 theme set <id>`) once per set.

// Ported ahead of the views that consume it (theme UI, T22 wiring).
#![allow(dead_code)]

use super::theme_palette::{ThemePreferenceMode, get_theme_definition, single_appearance_of};
use super::theme_palettes::ThemeAppearance;
use super::use_theme::{ThemeStorage, ThemeStore};

/// Scoped per environment: each machine's `t3 theme set` is its own act, so
/// hopping between primary environments neither replays one environment's
/// theme over the user's pick nor swallows another's.
const APPLIED_DEFAULT_THEME_STORAGE_PREFIX: &str = "t3code:default-theme-applied:v2:";

/// One generation per set: keyed on when the theme was set, not just its
/// value, so re-asserting the same theme still acts. Environments provisioned
/// by builds without the timestamp degrade to applying once per value.
pub fn default_theme_generation(theme: &str, set_at: &str) -> String {
    if set_at.is_empty() {
        theme.to_string()
    } else {
        format!("{theme}@{set_at}")
    }
}

pub struct DefaultThemeToApplyInput<'a> {
    pub environment_id: Option<&'a str>,
    pub default_theme: &'a str,
    pub default_theme_set_at: &'a str,
    pub applied_generation: Option<&'a str>,
    pub resolves: bool,
}

/// The generation to apply, or `None` to leave this client alone. Pure so the
/// rule -- apply once per set, never replay one already applied, wait for a
/// theme that has not arrived yet -- can be tested without a renderer.
pub fn default_theme_to_apply(input: &DefaultThemeToApplyInput<'_>) -> Option<String> {
    if input.environment_id.is_none() || input.default_theme.is_empty() {
        return None;
    }
    let generation = default_theme_generation(input.default_theme, input.default_theme_set_at);
    if input.applied_generation == Some(generation.as_str()) {
        return None;
    }
    // The setting and the palette it names arrive independently, so an id that
    // does not resolve yet is not a failure -- the effect runs again when the
    // published set changes.
    if !input.resolves {
        return None;
    }
    Some(generation)
}

fn read_applied_generation(storage: &dyn ThemeStorage, storage_key: &str) -> Option<String> {
    storage.get_item(storage_key).ok().flatten()
}

fn write_applied_generation(storage: &dyn ThemeStorage, storage_key: &str, generation: &str) {
    // Unrecordable means the next config event applies again; harmless.
    let _ = storage.set_item(storage_key, generation);
}

/// `useDefaultThemeAdoption`'s effect body. Applies the environment's theme
/// (`t3 theme set <id>`). Each set switches this client once -- live when
/// connected, on the next connect otherwise -- and then steps aside: a theme
/// the user picks in Settings afterwards wins until the environment's theme is
/// set again. The environment's own published themes are valid targets, which
/// is why this waits for an id that does not resolve yet. Call it whenever the
/// environment id, `defaultTheme`, `defaultThemeSetAt` or the published
/// themes change. Returns whether a generation was adopted.
pub fn adopt_default_theme(
    store: &ThemeStore,
    storage: &dyn ThemeStorage,
    environment_id: Option<&str>,
    default_theme: &str,
    default_theme_set_at: &str,
) -> bool {
    let Some(environment_id) = environment_id else {
        return false;
    };
    let storage_key = format!("{APPLIED_DEFAULT_THEME_STORAGE_PREFIX}{environment_id}");
    let definition = get_theme_definition(default_theme);
    let generation = default_theme_to_apply(&DefaultThemeToApplyInput {
        environment_id: Some(environment_id),
        default_theme,
        default_theme_set_at,
        applied_generation: read_applied_generation(storage, &storage_key).as_deref(),
        resolves: definition.is_some(),
    });
    let (Some(generation), Some(definition)) = (generation, definition) else {
        return false;
    };

    // Deliberately not the card's rule. Clicking a card is a user choosing one
    // half of their own mix; a set theme is the environment saying what this
    // client opens on, so it takes the base preference and, for a
    // single-appearance theme, the matching mode -- otherwise a dark-only
    // theme on a light client is recorded as applied while nothing changes.
    if !store.set_theme(default_theme) {
        return false;
    }
    let half = single_appearance_of(&definition);
    // Recorded only once both land: marking the generation applied while the
    // appearance switch failed would leave a dark-only theme rendering its
    // light half with no retry.
    if let Some(half) = half {
        let mode = match half {
            ThemeAppearance::Light => ThemePreferenceMode::Light,
            ThemeAppearance::Dark => ThemePreferenceMode::Dark,
        };
        if !store.set_appearance_mode(mode) {
            return false;
        }
    }
    write_applied_generation(storage, &storage_key, &generation);
    true
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/hooks/useDefaultTheme.test.ts, plus the
    // adoption effect.
    use std::sync::Arc;

    use super::super::theme_palette_library::{
        InMemoryThemeLibraryStorage, install_custom_theme, invalidate_custom_themes,
        parse_theme_file, remove_custom_theme, set_theme_library_storage,
    };
    use super::super::use_theme::{MemoryThemeStorage, SystemAppearance};
    use super::*;
    use crate::theme::THEME_GLOBALS_TEST_LOCK;

    const SET_AT: &str = "2026-08-28T00:00:00.000Z";

    fn base() -> DefaultThemeToApplyInput<'static> {
        DefaultThemeToApplyInput {
            environment_id: Some("env-1"),
            default_theme: "nightfall",
            default_theme_set_at: SET_AT,
            applied_generation: None,
            resolves: true,
        }
    }

    #[test]
    fn applies_a_set_this_client_has_not_seen() {
        assert_eq!(
            default_theme_to_apply(&base()),
            Some(default_theme_generation("nightfall", SET_AT))
        );
    }

    #[test]
    fn does_not_replay_a_generation_it_already_applied() {
        let generation = default_theme_generation("nightfall", SET_AT);
        assert_eq!(
            default_theme_to_apply(&DefaultThemeToApplyInput {
                applied_generation: Some(&generation),
                ..base()
            }),
            None
        );
    }

    // `t3 theme set` of a theme this client already wears must still act, which
    // is why the generation carries the set time and not just the value.
    #[test]
    fn applies_the_same_theme_again_when_the_environment_re_sets_it() {
        let applied = default_theme_generation("nightfall", "2026-08-28T00:00:00.000Z");
        let next = default_theme_to_apply(&DefaultThemeToApplyInput {
            default_theme_set_at: "2026-08-29T00:00:00.000Z",
            applied_generation: Some(&applied),
            ..base()
        });
        assert!(next.is_some());
        assert_ne!(next.as_deref(), Some(applied.as_str()));
    }

    // Environments provisioned before the timestamp existed.
    #[test]
    fn falls_back_to_once_per_value_without_a_set_time() {
        let generation = default_theme_generation("nightfall", "");
        assert_eq!(
            default_theme_to_apply(&DefaultThemeToApplyInput {
                default_theme_set_at: "",
                applied_generation: None,
                ..base()
            }),
            Some(generation.clone())
        );
        assert_eq!(
            default_theme_to_apply(&DefaultThemeToApplyInput {
                default_theme_set_at: "",
                applied_generation: Some(&generation),
                ..base()
            }),
            None
        );
    }

    // The setting and the palette it names arrive independently.
    #[test]
    fn waits_for_a_theme_that_has_not_arrived_yet() {
        assert_eq!(
            default_theme_to_apply(&DefaultThemeToApplyInput {
                resolves: false,
                ..base()
            }),
            None
        );
    }

    #[test]
    fn leaves_a_client_alone_with_no_environment_or_no_theme_set() {
        assert_eq!(
            default_theme_to_apply(&DefaultThemeToApplyInput {
                environment_id: None,
                ..base()
            }),
            None
        );
        assert_eq!(
            default_theme_to_apply(&DefaultThemeToApplyInput {
                default_theme: "",
                ..base()
            }),
            None
        );
    }

    struct Light;

    impl SystemAppearance for Light {
        fn is_dark(&self) -> bool {
            false
        }
    }

    fn client() -> (Arc<ThemeStore>, Arc<MemoryThemeStorage>) {
        let storage = Arc::new(MemoryThemeStorage::default());
        let store = ThemeStore::new(storage.clone(), Arc::new(Light), None);
        (store, storage)
    }

    #[test]
    fn adoption_switches_the_client_once_per_set_and_then_steps_aside() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let (store, storage) = client();

        assert!(adopt_default_theme(
            &store,
            storage.as_ref(),
            Some("env-1"),
            "grove",
            SET_AT
        ));
        assert_eq!(store.snapshot().theme, "grove");

        // The user picks something else afterwards; the same generation does
        // not replay over it.
        assert!(store.set_theme("light"));
        assert!(!adopt_default_theme(
            &store,
            storage.as_ref(),
            Some("env-1"),
            "grove",
            SET_AT
        ));
        assert_eq!(store.snapshot().theme, "light");

        // A re-set acts again, and another environment is tracked separately.
        assert!(adopt_default_theme(
            &store,
            storage.as_ref(),
            Some("env-1"),
            "grove",
            "2026-08-29T00:00:00.000Z"
        ));
        assert!(adopt_default_theme(
            &store,
            storage.as_ref(),
            Some("env-2"),
            "grove",
            "2026-08-29T00:00:00.000Z"
        ));
    }

    #[test]
    fn adoption_waits_for_an_unresolved_theme() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        let (store, storage) = client();
        assert!(!adopt_default_theme(
            &store,
            storage.as_ref(),
            Some("env-1"),
            "not-published-yet",
            SET_AT
        ));
        assert_eq!(store.snapshot().theme, "system");
        assert!(!adopt_default_theme(
            &store,
            storage.as_ref(),
            None,
            "grove",
            SET_AT
        ));
    }

    // A dark-only theme on a light client is recorded as applied only once the
    // appearance switch lands too.
    #[test]
    fn adoption_of_a_single_appearance_theme_also_switches_the_mode() {
        let _guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        let midnight = install_custom_theme(
            parse_theme_file(serde_json::json!({
                "version": 1,
                "id": "midnight-only",
                "name": "Midnight",
                "appearance": "dark",
                "colors": { "canvas": "#0b0f1a", "accent": "#7aa2f7" },
            }))
            .expect("theme"),
        )
        .expect("install");
        let (store, storage) = client();

        assert!(adopt_default_theme(
            &store,
            storage.as_ref(),
            Some("env-1"),
            midnight.id.as_str(),
            SET_AT
        ));
        let snapshot = store.snapshot();
        assert_eq!(snapshot.appearance_mode, ThemePreferenceMode::Dark);
        assert_eq!(snapshot.resolved_theme, ThemeAppearance::Dark);

        remove_custom_theme("midnight-only").expect("remove");
        invalidate_custom_themes();
    }
}
