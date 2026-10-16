//! Port of upstream/apps/web/src/components/settings/themeEditorStore.ts.
//!
//! The theme editor lives above the router so a draft survives navigation: the
//! palette is painted on the live app, and judging it means browsing the app
//! while the editor stays open. Themes are referenced by id because the stored
//! definitions change underneath an open session (an edit, an import).
//!
//! Upstream's zustand store becomes a plain struct; the GPUI layer owns one in
//! a global and notifies on change.

use super::theme_palette::{ThemeHalves, ThemePreference, get_theme_definition};
use super::theme_palettes::ThemeAppearance;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeEditorSessionInput {
    /// Set when editing an installed theme; `None` creates a new one.
    pub editing_theme_id: Option<String>,
    /// Theme a new theme starts from: the active one, or a duplicate target.
    pub seed_theme_id: Option<String>,
    /// Prefilled name, used by duplicate.
    pub seed_name: Option<String>,
    pub initial_appearance: ThemeAppearance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeEditorSession {
    pub input: ThemeEditorSessionInput,
    /// Distinguishes two sessions that name the same themes, so asking for a
    /// fresh draft while the panel is already open still reseeds it.
    pub id: u64,
}

impl std::ops::Deref for ThemeEditorSession {
    type Target = ThemeEditorSessionInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

#[derive(Debug, Default)]
pub struct ThemeEditorStore {
    pub session: Option<ThemeEditorSession>,
    next_session_id: u64,
}

impl ThemeEditorStore {
    pub fn open_theme_editor(&mut self, session: ThemeEditorSessionInput) {
        self.next_session_id += 1;
        self.session = Some(ThemeEditorSession {
            input: session,
            id: self.next_session_id,
        });
    }

    pub fn close_theme_editor(&mut self) {
        self.session = None;
    }

    /// `toggleThemeEditorForTheme`: closes an open editor, otherwise opens a
    /// new draft seeded from the theme in use for `initial_appearance`.
    pub fn toggle_theme_editor_for_theme(
        &mut self,
        theme: &ThemePreference,
        theme_halves: Option<&ThemeHalves>,
        initial_appearance: ThemeAppearance,
    ) {
        if self.session.is_some() {
            self.close_theme_editor();
            return;
        }

        let base_theme_id = get_theme_definition(theme).map(|definition| definition.id.to_string());
        let active_theme_id = theme_halves
            .and_then(|halves| halves.get(initial_appearance))
            .map(str::to_string)
            .or(base_theme_id);
        let seed_theme_id = active_theme_id
            .and_then(|id| get_theme_definition(&id))
            .map(|definition| definition.id.to_string());
        self.open_theme_editor(ThemeEditorSessionInput {
            editing_theme_id: None,
            seed_theme_id,
            seed_name: None,
            initial_appearance,
        });
    }
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/components/settings/themeEditorStore.test.ts
    // (the first two tests), plus session and seed rules.
    use super::*;
    use crate::theme::theme_palette::{OCEAN_THEME_ID, T3_CHAT_THEME_ID};

    #[test]
    fn opens_a_new_editor_seeded_from_the_theme_active_for_the_current_appearance() {
        let mut store = ThemeEditorStore::default();
        let halves = ThemeHalves {
            light: None,
            dark: Some(OCEAN_THEME_ID.to_string()),
        };
        store.toggle_theme_editor_for_theme(T3_CHAT_THEME_ID, Some(&halves), ThemeAppearance::Dark);

        let session = store.session.expect("session");
        assert_eq!(session.editing_theme_id, None);
        assert_eq!(session.seed_theme_id.as_deref(), Some(OCEAN_THEME_ID));
        assert_eq!(session.seed_name, None);
        assert_eq!(session.initial_appearance, ThemeAppearance::Dark);
    }

    #[test]
    fn closes_an_open_editor() {
        let mut store = ThemeEditorStore::default();
        store.toggle_theme_editor_for_theme(T3_CHAT_THEME_ID, None, ThemeAppearance::Light);
        store.toggle_theme_editor_for_theme(T3_CHAT_THEME_ID, None, ThemeAppearance::Light);
        assert_eq!(store.session, None);
    }

    fn input(seed: Option<&str>) -> ThemeEditorSessionInput {
        ThemeEditorSessionInput {
            editing_theme_id: None,
            seed_theme_id: seed.map(str::to_string),
            seed_name: None,
            initial_appearance: ThemeAppearance::Light,
        }
    }

    #[test]
    fn every_opened_session_gets_a_fresh_id_so_an_open_panel_reseeds() {
        let mut store = ThemeEditorStore::default();
        store.open_theme_editor(input(Some("grove")));
        let first = store.session.clone().expect("session");
        store.open_theme_editor(input(Some("grove")));
        let second = store.session.clone().expect("session");
        assert_eq!(first.input, second.input);
        assert_ne!(first.id, second.id);

        store.close_theme_editor();
        assert_eq!(store.session, None);
    }

    #[test]
    fn toggling_closes_an_open_editor_and_opens_a_closed_one() {
        let mut store = ThemeEditorStore::default();
        store.toggle_theme_editor_for_theme("grove", None, ThemeAppearance::Light);
        let session = store.session.clone().expect("opened");
        assert_eq!(session.seed_theme_id.as_deref(), Some("grove"));
        assert_eq!(session.editing_theme_id, None);
        assert_eq!(session.initial_appearance, ThemeAppearance::Light);

        store.toggle_theme_editor_for_theme("grove", None, ThemeAppearance::Light);
        assert_eq!(store.session, None);
    }

    #[test]
    fn the_seed_is_the_half_in_use_for_the_appearance_else_the_base_theme() {
        let halves = ThemeHalves {
            light: None,
            dark: Some("ocean".to_string()),
        };
        let mut store = ThemeEditorStore::default();
        store.toggle_theme_editor_for_theme("grove", Some(&halves), ThemeAppearance::Dark);
        assert_eq!(
            store
                .session
                .as_ref()
                .and_then(|s| s.seed_theme_id.as_deref()),
            Some("ocean")
        );
        store.close_theme_editor();

        store.toggle_theme_editor_for_theme("grove", Some(&halves), ThemeAppearance::Light);
        assert_eq!(
            store
                .session
                .as_ref()
                .and_then(|s| s.seed_theme_id.as_deref()),
            Some("grove")
        );
        store.close_theme_editor();

        // The standard look has no definition to seed from.
        store.toggle_theme_editor_for_theme("system", None, ThemeAppearance::Light);
        assert_eq!(
            store.session.as_ref().map(|s| s.seed_theme_id.clone()),
            Some(None)
        );
    }

    #[test]
    fn legacy_aliases_seed_from_the_canonical_id() {
        let mut store = ThemeEditorStore::default();
        store.toggle_theme_editor_for_theme("t3-grove", None, ThemeAppearance::Light);
        assert_eq!(
            store
                .session
                .as_ref()
                .and_then(|s| s.seed_theme_id.as_deref()),
            Some("grove")
        );
    }
}
