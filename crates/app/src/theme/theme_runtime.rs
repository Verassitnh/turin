//! The theme runtime: the GPUI home of upstream's `useTheme` module state.
//!
//! [`ThemeStore`] (`use_theme.rs`) is the state machine; this module owns one as
//! an app global, mirrors its snapshot into [`AppearancePreferences`] after
//! every mutation so [`super::apply`] repaints, and carries the theme editor's
//! session and live preview. Device-local storage (upstream: `localStorage`)
//! is a JSON file next to the app's data ([`FileLocalStorage`]); the default
//! in-memory storage keeps demos and tests from touching it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, Global, WindowAppearance};
use parking_lot::Mutex;

use super::theme_editor_store::ThemeEditorStore;
use super::theme_palette::{
    ThemeHalves, ThemePreferenceMode, apply_theme_color_preview, canonical_theme_preference,
};
use super::theme_palette_library::{
    CUSTOM_THEMES_STORAGE_KEY, ThemeLibraryStorage, ThemeLibraryStorageError,
    ThemeLibraryStorageReason, set_theme_library_storage,
};
use super::theme_palettes::{ThemeAppearance, ThemeColors};
use super::use_theme::{
    MemoryThemeStorage, SystemAppearance, ThemeSnapshot, ThemeStorage, ThemeStore,
};
use super::{ThemePreviewDraft, preferences, set_preferences};

struct SharedSystemAppearance(Arc<AtomicBool>);

impl SystemAppearance for SharedSystemAppearance {
    fn is_dark(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct ThemeRuntime {
    store: Arc<ThemeStore>,
    editor: ThemeEditorStore,
    system_dark: Arc<AtomicBool>,
}

impl Global for ThemeRuntime {}

/// Installs the runtime on in-memory storage. Call after [`super::init`].
pub fn init(cx: &mut App) {
    init_with_storage(cx, Arc::new(MemoryThemeStorage::default()));
}

/// Installs the runtime over `storage` (the appearance keys of `useTheme`).
pub fn init_with_storage(cx: &mut App, storage: Arc<dyn ThemeStorage>) {
    let system_dark = Arc::new(AtomicBool::new(system_is_dark(cx.window_appearance())));
    // The store applies the stored theme on construction ("apply immediately to
    // prevent flash"); it spawns desktop syncs on tokio, so it has none here.
    let store = ThemeStore::with_runtime(
        storage,
        Arc::new(SharedSystemAppearance(Arc::clone(&system_dark))),
        None,
        Arc::new(|message, attributes| {
            tracing::error!(attributes = ?attributes, "{message}");
        }),
        Arc::new(|_| {}),
    );
    cx.set_global(ThemeRuntime {
        store,
        editor: ThemeEditorStore::default(),
        system_dark,
    });
    sync(cx);
}

/// Persists the appearance keys and the custom-theme library to
/// `<dir>/local-storage.json` and adopts what is stored there.
pub fn persist_to(dir: PathBuf, cx: &mut App) {
    let storage = Arc::new(FileLocalStorage::open(dir.join("local-storage.json")));
    set_theme_library_storage(storage.clone());
    super::theme_palette_library::invalidate_custom_themes();
    init_with_storage(cx, storage);
}

fn system_is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

fn runtime(cx: &App) -> &ThemeRuntime {
    cx.global::<ThemeRuntime>()
}

pub fn store(cx: &App) -> Arc<ThemeStore> {
    Arc::clone(&runtime(cx).store)
}

/// `useTheme()`'s snapshot.
pub fn snapshot(cx: &App) -> ThemeSnapshot {
    runtime(cx).store.snapshot()
}

pub fn theme_halves(cx: &App) -> Option<ThemeHalves> {
    snapshot(cx).theme_halves
}

/// `readThemeHalvesRaw`.
pub fn theme_halves_raw(cx: &App) -> ThemeHalves {
    runtime(cx).store.read_theme_halves_raw()
}

/// Mirrors the store into the appearance preferences and repaints.
pub fn sync(cx: &mut App) {
    let snapshot = runtime(cx).store.snapshot();
    let mut next = preferences(cx);
    next.theme = canonical_theme_preference(&snapshot.theme).to_string();
    next.appearance_mode = Some(snapshot.appearance_mode);
    next.theme_halves = snapshot.theme_halves;
    set_preferences(next, cx);
}

pub fn set_theme(theme: &str, cx: &mut App) -> bool {
    let ok = runtime(cx).store.set_theme(theme);
    sync(cx);
    ok
}

pub fn set_appearance_mode(mode: ThemePreferenceMode, cx: &mut App) -> bool {
    let ok = runtime(cx).store.set_appearance_mode(mode);
    sync(cx);
    ok
}

pub fn set_theme_half(appearance: ThemeAppearance, theme_id: Option<&str>, cx: &mut App) -> bool {
    let ok = runtime(cx).store.set_theme_half(appearance, theme_id);
    sync(cx);
    ok
}

pub fn clear_theme_halves(cx: &mut App) -> bool {
    let ok = runtime(cx).store.clear_theme_halves();
    sync(cx);
    ok
}

/// `refreshTheme`: re-apply the stored selection (after the library changed).
pub fn refresh_theme(preserve_preview: bool, cx: &mut App) {
    runtime(cx).store.refresh_theme(preserve_preview);
    sync(cx);
}

/// The OS light/dark switch.
pub fn follow_system_appearance(appearance: WindowAppearance, cx: &mut App) {
    if !cx.has_global::<ThemeRuntime>() {
        return;
    }
    let runtime = runtime(cx);
    runtime
        .system_dark
        .store(system_is_dark(appearance), Ordering::SeqCst);
    runtime.store.handle_system_appearance_change();
}

/// `applyThemeColorPreview`: wear the editor's draft until [`end_preview`].
pub fn preview_draft(colors: &ThemeColors, appearance: ThemeAppearance, cx: &mut App) {
    runtime(cx)
        .store
        .with_root(|root| apply_theme_color_preview(root, colors, appearance));
    let mut next = preferences(cx);
    let draft = ThemePreviewDraft {
        colors: colors.clone(),
        appearance,
    };
    if next.preview.as_ref() == Some(&draft) {
        return;
    }
    next.preview = Some(draft);
    set_preferences(next, cx);
}

/// The draft stops being previewed: the stored theme comes back.
pub fn end_preview(cx: &mut App) {
    let mut next = preferences(cx);
    if next.preview.take().is_none() {
        return;
    }
    set_preferences(next, cx);
    refresh_theme(false, cx);
}

pub fn is_previewing(cx: &App) -> bool {
    preferences(cx).preview.is_some()
}

pub fn editor_session(cx: &App) -> Option<super::theme_editor_store::ThemeEditorSession> {
    runtime(cx).editor.session.clone()
}

/// Mutates the editor store (`useThemeEditorStore.setState`).
pub fn update_editor<R>(cx: &mut App, update: impl FnOnce(&mut ThemeEditorStore) -> R) -> R {
    update(&mut cx.global_mut::<ThemeRuntime>().editor)
}

// ── Device-local storage ─────────────────────────────────────────────

/// `window.localStorage` backed by one JSON file. Writes go through a temp file
/// and a rename, so a crash never leaves a half-written library.
pub struct FileLocalStorage {
    path: PathBuf,
    items: Mutex<BTreeMap<String, String>>,
}

impl FileLocalStorage {
    /// Missing or unreadable files start empty (like a fresh profile).
    pub fn open(path: PathBuf) -> Self {
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, String>>(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            items: Mutex::new(items),
        }
    }

    fn flush(&self, items: &BTreeMap<String, String>) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let raw = serde_json::to_string_pretty(items).map_err(|error| error.to_string())?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, raw).map_err(|error| error.to_string())?;
        std::fs::rename(&temp, &self.path).map_err(|error| error.to_string())
    }
}

impl ThemeStorage for FileLocalStorage {
    fn get_item(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.items.lock().get(key).cloned())
    }

    fn set_item(&self, key: &str, value: &str) -> Result<(), String> {
        let mut items = self.items.lock();
        let previous = items.insert(key.to_string(), value.to_string());
        self.flush(&items).inspect_err(|_| {
            match previous {
                Some(previous) => items.insert(key.to_string(), previous),
                None => items.remove(key),
            };
        })
    }

    fn remove_item(&self, key: &str) -> Result<(), String> {
        let mut items = self.items.lock();
        let Some(previous) = items.remove(key) else {
            return Ok(());
        };
        self.flush(&items).inspect_err(|_| {
            items.insert(key.to_string(), previous);
        })
    }
}

/// The custom-theme library lives under [`CUSTOM_THEMES_STORAGE_KEY`].
impl ThemeLibraryStorage for FileLocalStorage {
    fn read(&self) -> Result<Option<String>, ThemeLibraryStorageError> {
        Ok(self.items.lock().get(CUSTOM_THEMES_STORAGE_KEY).cloned())
    }

    fn write(&self, raw: &str) -> Result<(), ThemeLibraryStorageError> {
        self.set_item(CUSTOM_THEMES_STORAGE_KEY, raw)
            .map_err(|_| ThemeLibraryStorageError {
                storage_key: CUSTOM_THEMES_STORAGE_KEY.to_string(),
                operation: super::theme_palette_library::ThemeLibraryStorageOperation::Write,
                reason: ThemeLibraryStorageReason::StorageUnavailable,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_survive_a_reopen_and_removal_is_durable() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("nested").join("local-storage.json");
        let storage = FileLocalStorage::open(path.clone());
        assert_eq!(storage.get_item("t3code:theme"), Ok(None));
        storage.set_item("t3code:theme", "grove").expect("set");
        storage
            .set_item("t3code:theme-appearance-mode", "dark")
            .expect("set");

        let reopened = FileLocalStorage::open(path.clone());
        assert_eq!(reopened.get_item("t3code:theme"), Ok(Some("grove".into())));
        reopened.remove_item("t3code:theme").expect("remove");
        reopened.remove_item("never-set").expect("noop remove");

        let again = FileLocalStorage::open(path);
        assert_eq!(again.get_item("t3code:theme"), Ok(None));
        assert_eq!(
            again.get_item("t3code:theme-appearance-mode"),
            Ok(Some("dark".into()))
        );
    }

    #[test]
    fn the_theme_library_shares_the_file_under_its_own_key() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("local-storage.json");
        let storage = FileLocalStorage::open(path.clone());
        assert_eq!(ThemeLibraryStorage::read(&storage), Ok(None));
        ThemeLibraryStorage::write(&storage, "[]").expect("write");
        storage.set_item("t3code:theme", "dark").expect("set");

        let reopened = FileLocalStorage::open(path);
        assert_eq!(
            ThemeLibraryStorage::read(&reopened),
            Ok(Some("[]".to_string()))
        );
        assert_eq!(reopened.get_item("t3code:theme"), Ok(Some("dark".into())));
    }

    #[test]
    fn a_corrupt_file_starts_empty_instead_of_failing() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("local-storage.json");
        std::fs::write(&path, "{ not json").expect("seed");
        let storage = FileLocalStorage::open(path);
        assert_eq!(storage.get_item("t3code:theme"), Ok(None));
        storage.set_item("t3code:theme", "light").expect("set");
        assert_eq!(storage.get_item("t3code:theme"), Ok(Some("light".into())));
    }

    #[test]
    fn a_failed_write_rolls_the_in_memory_value_back() {
        let dir = tempfile::tempdir().expect("dir");
        // The parent "directory" is a file, so creating it fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").expect("blocker");
        let storage = FileLocalStorage::open(blocker.join("local-storage.json"));
        assert!(storage.set_item("t3code:theme", "grove").is_err());
        assert_eq!(storage.get_item("t3code:theme"), Ok(None));
    }
}
