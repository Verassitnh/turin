//! Port of upstream/apps/web/src/components/settings/ThemeEditorHost.tsx
//!
//! Renders the theme editor for the open session of the theme runtime
//! (`themeEditorStore`). The editor paints its draft on the live app, so it has to
//! outlive the page that opened it: the point is to walk through threads, panels, and
//! pages while the colors are being tuned. Here the settings window mounts the host; the
//! draft is previewed through the app-wide theme runtime, so every window wears it.
//!
//! [`handle_saved`] is upstream's `handleSaved`: what a saved theme does to the active
//! theme, and the toast that says so. It runs against [`ThemeHostEffects`] so the rules
//! are tested without a window.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Window, div,
};

use super::theme_editor_panel::ThemeSavedContext;
use super::theme_editor_panel_view::ThemeEditorPanelView;
use crate::theme::theme_editor_store::ThemeEditorSession;
use crate::theme::theme_palette::get_theme_definition;
use crate::theme::theme_palettes::{ThemeAppearance, ThemeDefinition};
use crate::theme::theme_runtime::{self, ThemeRuntime};

/// What `handleSaved` reads and does.
pub trait ThemeHostEffects {
    /// `setTheme(id)`: false when storage refused.
    fn set_theme(&mut self, id: &str) -> bool;
    /// `refreshTheme()`: re-apply the stored palette.
    fn refresh_theme(&mut self);
    /// Whether `id` is the base theme or either half of the mix (`theme`, `themeHalves`).
    fn is_in_use(&self, id: &str) -> bool;
    fn toast(&mut self, toast: HostToast);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostToast {
    pub success: bool,
    pub title: String,
    pub description: String,
}

fn appearance_name(appearance: ThemeAppearance) -> &'static str {
    match appearance {
        ThemeAppearance::Light => "light",
        ThemeAppearance::Dark => "dark",
    }
}

fn storage_unavailable() -> HostToast {
    HostToast {
        success: false,
        title: "Could not save your theme".to_owned(),
        description: "Browser storage is unavailable, so the change was not kept.".to_owned(),
    }
}

/// `handleSaved`. Returns whether the theme could be made active; the editor rolls the
/// save back when it could not.
pub fn handle_saved(
    saved: &ThemeDefinition,
    context: ThemeSavedContext,
    effects: &mut dyn ThemeHostEffects,
) -> bool {
    // A merge completed an existing theme's light/dark pair; activating the whole theme
    // shows the new palette right away.
    if let Some(merged) = context.merged_appearance {
        if !effects.set_theme(saved.id.as_str()) {
            effects.toast(storage_unavailable());
            return false;
        }
        effects.toast(HostToast {
            success: true,
            title: format!("{} updated", saved.label),
            description: format!("Its {} palette was added.", appearance_name(merged)),
        });
        return true;
    }
    if !context.created {
        // The edited theme may be showing through the base preference or either half of
        // the mix; the preference itself is untouched (a setTheme here would clear the
        // mix), the palette just needs re-applying.
        let was_active = effects.is_in_use(saved.id.as_str());
        if was_active {
            effects.refresh_theme();
        }
        effects.toast(HostToast {
            success: true,
            title: format!("{} saved", saved.label),
            description: if was_active {
                "Your changes are now active."
            } else {
                "Your changes are saved."
            }
            .to_owned(),
        });
        return true;
    }
    if !effects.set_theme(saved.id.as_str()) {
        effects.toast(storage_unavailable());
        return false;
    }
    effects.toast(HostToast {
        success: true,
        title: format!("{} created", saved.label),
        description: "It\u{2019}s now active.".to_owned(),
    });
    true
}

/// The live effects: the theme runtime and the window's toasts.
struct LiveEffects<'a> {
    window: &'a mut Window,
    cx: &'a mut App,
}

impl ThemeHostEffects for LiveEffects<'_> {
    fn set_theme(&mut self, id: &str) -> bool {
        theme_runtime::set_theme(id, self.cx)
    }

    fn refresh_theme(&mut self) {
        theme_runtime::refresh_theme(false, self.cx);
    }

    fn is_in_use(&self, id: &str) -> bool {
        let snapshot = theme_runtime::snapshot(self.cx);
        let halves = theme_runtime::theme_halves(self.cx);
        get_theme_definition(&snapshot.theme).is_some_and(|definition| definition.id.as_str() == id)
            || halves.is_some_and(|halves| {
                halves.get(ThemeAppearance::Light) == Some(id)
                    || halves.get(ThemeAppearance::Dark) == Some(id)
            })
    }

    fn toast(&mut self, toast: HostToast) {
        let notification = if toast.success {
            Notification::success(toast.description)
        } else {
            Notification::error(toast.description)
        };
        self.window
            .push_notification(notification.title(toast.title), self.cx);
    }
}

/// The themes a session names, read fresh from the library: a saved definition can change
/// without its id changing between sessions, and a removed theme resolves to none.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionThemes {
    pub editing_theme: Option<ThemeDefinition>,
    pub seed_theme: Option<ThemeDefinition>,
}

pub fn resolve_session_themes(session: &ThemeEditorSession) -> SessionThemes {
    let lookup = |id: &Option<String>| id.as_deref().and_then(get_theme_definition);
    SessionThemes {
        editing_theme: lookup(&session.editing_theme_id),
        seed_theme: lookup(&session.seed_theme_id),
    }
}

struct OpenEditor {
    session_id: u64,
    panel: Entity<ThemeEditorPanelView>,
}

pub struct ThemeEditorHost {
    open: Option<OpenEditor>,
    /// Unsubscribes from the library when the host goes away.
    _library: LibrarySubscription,
    _observe: Option<gpui_kit::Subscription>,
    _release: gpui_kit::Subscription,
}

struct LibrarySubscription(Option<Box<dyn FnOnce() + Send>>);

impl Drop for LibrarySubscription {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.0.take() {
            unsubscribe();
        }
    }
}

impl ThemeEditorHost {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The library changes under an open editor (an edit elsewhere, an import).
        let (changed, mut library_changes) = futures::channel::mpsc::unbounded::<()>();
        let unsubscribe =
            crate::theme::theme_palette_library::subscribe_to_custom_themes(move || {
                let _ = changed.unbounded_send(());
            });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while library_changes.next().await.is_some() {
                if this
                    .update(cx, |this, cx| this.refresh_editing_theme(cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        let observe = cx.has_global::<ThemeRuntime>().then(|| {
            cx.observe_global_in::<ThemeRuntime>(window, |this, window, cx| this.sync(window, cx))
        });
        // The window closing with the editor open must not leave the app wearing the draft.
        let release = cx.on_release(|this, cx| {
            if this.open.take().is_some() && cx.has_global::<ThemeRuntime>() {
                theme_runtime::end_preview(cx);
                theme_runtime::update_editor(cx, |store| store.close_theme_editor());
            }
        });
        let mut host = Self {
            open: None,
            _library: LibrarySubscription(Some(unsubscribe)),
            _observe: observe,
            _release: release,
        };
        host.sync(window, cx);
        host
    }

    /// Follows the runtime's editor session: a new session opens a fresh draft (even when it
    /// names the same themes as the last), none closes the editor.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<ThemeRuntime>() {
            return;
        }
        let session = theme_runtime::editor_session(cx);
        match (&self.open, session) {
            (Some(open), Some(session)) if open.session_id == session.id => {
                self.refresh_editing_theme(cx);
            }
            (_, None) => {
                if self.open.take().is_some() {
                    // The stored theme comes back.
                    theme_runtime::end_preview(cx);
                    cx.notify();
                }
            }
            (_, Some(session)) => {
                if self.open.take().is_some() {
                    theme_runtime::end_preview(cx);
                }
                self.open_session(&session, window, cx);
            }
        }
    }

    fn open_session(
        &mut self,
        session: &ThemeEditorSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let themes = resolve_session_themes(session);
        let on_saved: Rc<
            dyn Fn(&ThemeDefinition, ThemeSavedContext, &mut Window, &mut App) -> bool,
        > = Rc::new(|saved, context, window, cx| {
            handle_saved(saved, context, &mut LiveEffects { window, cx })
        });
        let on_close: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(|_, cx| {
            theme_runtime::update_editor(cx, |store| store.close_theme_editor());
        });
        let (seed_name, initial_appearance) =
            (session.seed_name.clone(), session.initial_appearance);
        let panel = cx.new(|cx| {
            ThemeEditorPanelView::new(
                themes.editing_theme,
                themes.seed_theme,
                seed_name,
                initial_appearance,
                on_saved,
                on_close,
                window,
                cx,
            )
        });
        self.open = Some(OpenEditor {
            session_id: session.id,
            panel,
        });
        cx.notify();
    }

    /// `useThemeDefinition`: an open editor follows the library for the theme it edits.
    fn refresh_editing_theme(&mut self, cx: &mut Context<Self>) {
        let (Some(open), true) = (&self.open, cx.has_global::<ThemeRuntime>()) else {
            return;
        };
        let Some(session) = theme_runtime::editor_session(cx) else {
            return;
        };
        let editing = resolve_session_themes(&session).editing_theme;
        open.panel
            .update(cx, |panel, cx| panel.set_editing_theme(editing, cx));
    }
}

impl Render for ThemeEditorHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A plain wrapper with no handlers takes no part in hit testing, so the page under
        // it stays clickable; only the panel itself occludes.
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .children(self.open.as_ref().map(|open| open.panel.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::theme::THEME_GLOBALS_TEST_LOCK;
    use crate::theme::theme_palette_library::{
        InMemoryThemeLibraryStorage, install_custom_theme, invalidate_custom_themes,
        parse_theme_file, remove_custom_theme, set_theme_library_storage, theme_color_to_hex,
        update_custom_theme,
    };

    fn library() -> parking_lot::MutexGuard<'static, ()> {
        let guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        guard
    }

    fn install(id: &str, accent: &str) -> ThemeDefinition {
        install_custom_theme(
            parse_theme_file(json!({
                "version": 1,
                "id": id,
                "name": id,
                "appearance": "dark",
                "colors": { "accent": accent },
            }))
            .expect("theme"),
        )
        .expect("install")
    }

    fn session(editing: Option<&str>, seed: Option<&str>, id: u64) -> ThemeEditorSession {
        ThemeEditorSession {
            input: crate::theme::theme_editor_store::ThemeEditorSessionInput {
                editing_theme_id: editing.map(str::to_owned),
                seed_theme_id: seed.map(str::to_owned),
                seed_name: None,
                initial_appearance: ThemeAppearance::Dark,
            },
            id,
        }
    }

    fn accent(theme: &Option<ThemeDefinition>) -> String {
        theme_color_to_hex(
            theme
                .as_ref()
                .expect("theme resolves")
                .colors
                .accent
                .as_str(),
        )
        .expect("accent is a colour")
    }

    #[test]
    fn reopens_the_same_theme_with_its_saved_colors() {
        let _library = library();
        let theme = install("saved-colors", "#1f6e4a");
        for field in ["editing", "seed"] {
            let named = |id: u64| match field {
                "editing" => session(Some("saved-colors"), None, id),
                _ => session(None, Some("saved-colors"), id),
            };
            let pick = |themes: SessionThemes| match field {
                "editing" => themes.editing_theme,
                _ => themes.seed_theme,
            };
            assert_eq!(accent(&pick(resolve_session_themes(&named(1)))), "#1f6e4a");

            let mut updated = theme.clone();
            updated.colors.accent = "#7241b8".into();
            update_custom_theme(updated).expect("update");

            assert_eq!(accent(&pick(resolve_session_themes(&named(2)))), "#7241b8");

            // Restore for the next field.
            let mut restored = theme.clone();
            restored.colors.accent = "#1f6e4a".into();
            update_custom_theme(restored).expect("restore");
        }
    }

    #[test]
    fn refreshes_an_open_theme_when_the_library_changes() {
        let _library = library();
        let theme = install("updated-theme", "#1f6e4a");
        let open = session(Some("updated-theme"), None, 1);
        assert_eq!(
            accent(&resolve_session_themes(&open).editing_theme),
            "#1f6e4a"
        );

        let mut updated = theme;
        updated.colors.accent = "#7241b8".into();
        update_custom_theme(updated).expect("update");

        // The same session, resolved again, sees the saved colours.
        assert_eq!(
            accent(&resolve_session_themes(&open).editing_theme),
            "#7241b8"
        );
    }

    #[test]
    fn does_not_keep_editing_a_theme_removed_from_the_library() {
        let _library = library();
        install("removed-theme", "#1f6e4a");
        let open = session(Some("removed-theme"), None, 1);
        assert!(resolve_session_themes(&open).editing_theme.is_some());

        remove_custom_theme("removed-theme").expect("remove");

        assert_eq!(resolve_session_themes(&open).editing_theme, None);
    }

    #[test]
    fn built_in_themes_resolve_as_seeds() {
        let _library = library();
        let themes = resolve_session_themes(&session(None, Some("nope"), 1));
        assert_eq!(themes.seed_theme, None);
    }

    /// Records the calls `handle_saved` makes.
    #[derive(Default)]
    struct Fake {
        set_theme_ok: bool,
        in_use: bool,
        calls: Vec<String>,
        toasts: Vec<HostToast>,
    }

    impl ThemeHostEffects for Fake {
        fn set_theme(&mut self, id: &str) -> bool {
            self.calls.push(format!("set_theme {id}"));
            self.set_theme_ok
        }
        fn refresh_theme(&mut self) {
            self.calls.push("refresh_theme".to_owned());
        }
        fn is_in_use(&self, _: &str) -> bool {
            self.in_use
        }
        fn toast(&mut self, toast: HostToast) {
            self.toasts.push(toast);
        }
    }

    fn definition(label: &str) -> ThemeDefinition {
        let _guard = library();
        install(&label.to_lowercase().replace(' ', "-"), "#336699")
    }

    fn context(created: bool, merged: Option<ThemeAppearance>) -> ThemeSavedContext {
        ThemeSavedContext {
            created,
            merged_appearance: merged,
        }
    }

    #[test]
    fn a_created_theme_becomes_active() {
        let theme = definition("Aurora");
        let mut fake = Fake {
            set_theme_ok: true,
            ..Default::default()
        };

        assert!(handle_saved(&theme, context(true, None), &mut fake));

        assert_eq!(fake.calls, vec!["set_theme aurora"]);
        assert_eq!(
            fake.toasts,
            vec![HostToast {
                success: true,
                title: "aurora created".to_owned(),
                description: "It\u{2019}s now active.".to_owned(),
            }]
        );
    }

    #[test]
    fn a_created_theme_that_cannot_be_activated_reports_the_storage_failure() {
        let theme = definition("Aurora");
        let mut fake = Fake::default();

        assert!(!handle_saved(&theme, context(true, None), &mut fake));

        assert_eq!(fake.toasts.len(), 1);
        assert!(!fake.toasts[0].success);
        assert_eq!(fake.toasts[0].title, "Could not save your theme");
    }

    #[test]
    fn an_edit_of_the_theme_in_use_refreshes_it_without_touching_the_preference() {
        let theme = definition("Aurora");
        let mut fake = Fake {
            in_use: true,
            ..Default::default()
        };

        assert!(handle_saved(&theme, context(false, None), &mut fake));

        // A setTheme here would clear the light/dark mix.
        assert_eq!(fake.calls, vec!["refresh_theme"]);
        assert_eq!(fake.toasts[0].title, "aurora saved");
        assert_eq!(fake.toasts[0].description, "Your changes are now active.");
    }

    #[test]
    fn an_edit_of_another_theme_only_saves_it() {
        let theme = definition("Aurora");
        let mut fake = Fake::default();

        assert!(handle_saved(&theme, context(false, None), &mut fake));

        assert!(fake.calls.is_empty());
        assert_eq!(fake.toasts[0].description, "Your changes are saved.");
    }

    #[test]
    fn a_merge_activates_the_whole_theme_and_names_the_added_palette() {
        let theme = definition("Aurora");
        let mut fake = Fake {
            set_theme_ok: true,
            ..Default::default()
        };

        assert!(handle_saved(
            &theme,
            context(false, Some(ThemeAppearance::Light)),
            &mut fake
        ));

        assert_eq!(fake.calls, vec!["set_theme aurora"]);
        assert_eq!(fake.toasts[0].title, "aurora updated");
        assert_eq!(fake.toasts[0].description, "Its light palette was added.");
    }

    #[test]
    fn a_merge_that_cannot_be_activated_fails() {
        let theme = definition("Aurora");
        let mut fake = Fake::default();

        assert!(!handle_saved(
            &theme,
            context(false, Some(ThemeAppearance::Dark)),
            &mut fake
        ));
        assert!(!fake.toasts[0].success);
    }
}
