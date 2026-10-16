//! Port of upstream/apps/web/src/hooks/useTheme.ts — the appearance store.
//!
//! Upstream keeps module-level state over `window.localStorage`, the
//! `prefers-color-scheme` media query and `document.documentElement`, and
//! exposes it through `useSyncExternalStore`. Here the same state machine is
//! a [`ThemeStore`]: storage, the OS appearance and the desktop shell sit
//! behind small traits, the document root is a [`ThemeRoot`], and the GPUI
//! layer subscribes with [`ThemeStore::subscribe`] and repaints from
//! [`ThemeStore::root`]. Browser-chrome syncing (`<meta name="theme-color">`)
//! and the `no-transitions` class have no native counterpart and are omitted.

// Ported ahead of the views that consume it (theme UI, T22 wiring).
#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use thiserror::Error;

use super::theme_palette::{
    DesktopTheme, THEME_APPEARANCE_MODE_STORAGE_KEY, THEME_FOLLOW_SYSTEM_STORAGE_KEY,
    THEME_HALVES_STORAGE_KEY, ThemeHalves, ThemePreferenceMode, ThemeRoot, apply_theme_palette,
    canonical_theme_preference, get_theme_preference_mode, is_known_theme_preference,
    parse_theme_halves, resolve_desktop_theme, resolve_theme_appearance, resolve_theme_half,
};
use super::theme_palette_library::{CUSTOM_THEMES_STORAGE_KEY, invalidate_custom_themes};
use super::theme_palettes::ThemeAppearance;

pub const STORAGE_KEY: &str = "t3code:theme";
const DEFAULT_THEME: &str = "system";

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeSnapshot {
    pub theme: String,
    pub resolved_theme: ThemeAppearance,
    pub system_dark: bool,
    pub follow_system: bool,
    pub appearance_mode: ThemePreferenceMode,
    pub theme_halves: Option<ThemeHalves>,
}

impl Default for ThemeSnapshot {
    /// `DEFAULT_THEME_SNAPSHOT`.
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME.to_string(),
            resolved_theme: ThemeAppearance::Light,
            system_dark: false,
            follow_system: true,
            appearance_mode: ThemePreferenceMode::System,
            theme_halves: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeStorageOperation {
    Read,
    Write,
}

impl ThemeStorageOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// `ThemeStorageError`. `cause` keeps the exact storage failure for callers
/// that act on it; it is never part of [`Self::log_attributes`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Failed to {} theme preference for {storage_key}.", operation.as_str())]
pub struct ThemeStorageError {
    pub operation: ThemeStorageOperation,
    pub storage_key: String,
    pub theme: Option<String>,
    pub cause: String,
}

impl ThemeStorageError {
    /// `safeErrorLogAttributes`-style: tag plus context, no cause text.
    pub fn log_attributes(&self) -> Vec<(String, String)> {
        let mut attributes = vec![
            ("operation".to_string(), self.operation.as_str().to_string()),
            ("storageKey".to_string(), self.storage_key.clone()),
        ];
        if let Some(theme) = &self.theme {
            attributes.push(("theme".to_string(), theme.clone()));
        }
        attributes.push(("errorTag".to_string(), "ThemeStorageError".to_string()));
        attributes
    }
}

/// `DesktopThemeSyncError`.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Failed to sync the {theme} theme to the desktop shell.")]
pub struct DesktopThemeSyncError {
    pub theme: String,
    pub cause: String,
}

impl DesktopThemeSyncError {
    pub fn log_attributes(&self) -> Vec<(String, String)> {
        vec![
            ("theme".to_string(), self.theme.clone()),
            ("errorTag".to_string(), "DesktopThemeSyncError".to_string()),
        ]
    }
}

/// `window.localStorage`. Errors carry the failure's message as the cause.
pub trait ThemeStorage: Send + Sync {
    fn get_item(&self, key: &str) -> Result<Option<String>, String>;
    fn set_item(&self, key: &str, value: &str) -> Result<(), String>;
    fn remove_item(&self, key: &str) -> Result<(), String>;
}

/// In-memory storage, also the default when the host has none.
#[derive(Default)]
pub struct MemoryThemeStorage {
    items: Mutex<std::collections::BTreeMap<String, String>>,
}

impl ThemeStorage for MemoryThemeStorage {
    fn get_item(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.items.lock().get(key).cloned())
    }

    fn set_item(&self, key: &str, value: &str) -> Result<(), String> {
        self.items.lock().insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn remove_item(&self, key: &str) -> Result<(), String> {
        self.items.lock().remove(key);
        Ok(())
    }
}

/// `window.matchMedia("(prefers-color-scheme: dark)").matches`.
pub trait SystemAppearance: Send + Sync {
    fn is_dark(&self) -> bool;
}

/// `DesktopBridge.setTheme`.
pub trait DesktopThemeBridge: Send + Sync {
    fn set_theme(&self, theme: DesktopTheme) -> BoxFut<Result<(), String>>;
}

/// Where `console.error(message, attributes)` goes.
pub type ThemeLogger = Arc<dyn Fn(&str, &[(String, String)]) + Send + Sync>;

/// Runs a desktop-sync future (upstream: a floating promise).
pub type ThemeSpawner = Arc<dyn Fn(BoxFut<()>) + Send + Sync>;

fn tracing_logger() -> ThemeLogger {
    Arc::new(|message, attributes| {
        tracing::error!(attributes = ?attributes, "{message}");
    })
}

fn tokio_spawner() -> ThemeSpawner {
    Arc::new(|future| {
        tokio::spawn(future);
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ApplyThemeOptions {
    pub suppress_transitions: bool,
    /// Keep the editor's draft visible until an explicit refresh restores the
    /// selection. Upstream defaults this to `true`.
    pub dont_preserve_preview: bool,
}

impl ApplyThemeOptions {
    fn preserve_preview(self) -> bool {
        !self.dont_preserve_preview
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LastAppliedTheme {
    theme: String,
    system_dark: bool,
    follow_system: bool,
    appearance_mode: ThemePreferenceMode,
    theme_halves: Option<ThemeHalves>,
}

fn theme_halves_signature(halves: Option<&ThemeHalves>) -> (String, String) {
    (
        halves.and_then(|h| h.light.clone()).unwrap_or_default(),
        halves.and_then(|h| h.dark.clone()).unwrap_or_default(),
    )
}

fn same_halves(left: Option<&ThemeHalves>, right: Option<&ThemeHalves>) -> bool {
    theme_halves_signature(left) == theme_halves_signature(right)
}

#[derive(Default)]
struct StoreState {
    listeners: Vec<(u64, Arc<dyn Fn() + Send + Sync>)>,
    next_listener_id: u64,
    last_snapshot: Option<ThemeSnapshot>,
    snapshot_stale: bool,
    last_desktop_theme: Option<DesktopTheme>,
    last_applied_theme: Option<LastAppliedTheme>,
    theme_storage_read_failure: Option<ThemeStorageError>,
    root: ThemeRoot,
}

pub struct ThemeStore {
    storage: Arc<dyn ThemeStorage>,
    system: Arc<dyn SystemAppearance>,
    desktop: Option<Arc<dyn DesktopThemeBridge>>,
    logger: ThemeLogger,
    spawner: ThemeSpawner,
    state: Arc<Mutex<StoreState>>,
}

impl ThemeStore {
    /// Applies the stored theme immediately, as upstream does on module load
    /// ("apply immediately to prevent flash").
    pub fn new(
        storage: Arc<dyn ThemeStorage>,
        system: Arc<dyn SystemAppearance>,
        desktop: Option<Arc<dyn DesktopThemeBridge>>,
    ) -> Arc<Self> {
        Self::with_runtime(storage, system, desktop, tracing_logger(), tokio_spawner())
    }

    pub fn with_runtime(
        storage: Arc<dyn ThemeStorage>,
        system: Arc<dyn SystemAppearance>,
        desktop: Option<Arc<dyn DesktopThemeBridge>>,
        logger: ThemeLogger,
        spawner: ThemeSpawner,
    ) -> Arc<Self> {
        let store = Arc::new(Self {
            storage,
            system,
            desktop,
            logger,
            spawner,
            state: Arc::new(Mutex::new(StoreState {
                snapshot_stale: true,
                ..StoreState::default()
            })),
        });
        let stored = store.get_stored();
        store.apply_theme(&stored, ApplyThemeOptions::default());
        store
    }

    /// A copy of the document root the GPUI layer repaints from.
    pub fn root(&self) -> ThemeRoot {
        self.state.lock().root.clone()
    }

    /// Mutates the document root (the theme editor's live preview paints
    /// through this).
    pub fn with_root<R>(&self, update: impl FnOnce(&mut ThemeRoot) -> R) -> R {
        update(&mut self.state.lock().root)
    }

    // ── storage ──────────────────────────────────────────────────────

    /// Live read of the stored appearance mix, for callers that must not rely
    /// on a render-time snapshot (for example rollback after an async dialog).
    pub fn read_theme_halves(&self) -> Option<ThemeHalves> {
        self.read_stored_theme_halves()
    }

    /// The stored mix as written, without resolvability pruning. Flows that
    /// rebuild the whole mix must capture this before a `setTheme` clears it: a
    /// published id resolves only once its set has streamed in, and treating
    /// "not resolvable yet" as "absent" silently rewrites that half.
    pub fn read_theme_halves_raw(&self) -> ThemeHalves {
        self.read_stored_theme_halves_raw()
    }

    fn read_stored_theme_halves(&self) -> Option<ThemeHalves> {
        let raw = self.storage.get_item(THEME_HALVES_STORAGE_KEY).ok()?;
        parse_theme_halves(raw.as_deref())
    }

    /// The stored mix as written, without resolvability pruning. An
    /// environment published id resolves only once its set has streamed in,
    /// so a write that merged over the pruned parse would silently erase that
    /// half whenever the other one changed before the set arrived.
    fn read_stored_theme_halves_raw(&self) -> ThemeHalves {
        let Ok(raw) = self.storage.get_item(THEME_HALVES_STORAGE_KEY) else {
            return ThemeHalves::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.as_deref().unwrap_or("null"))
        else {
            return ThemeHalves::default();
        };
        let Some(record) = value.as_object() else {
            return ThemeHalves::default();
        };
        let id = |key: &str| record.get(key).and_then(|v| v.as_str()).map(str::to_string);
        ThemeHalves {
            light: id("light"),
            dark: id("dark"),
        }
    }

    fn system_dark(&self) -> bool {
        self.system.is_dark()
    }

    fn read_stored_follow_system(&self, theme: &str) -> bool {
        // Fall back to the legacy theme value when the separate preference is unavailable.
        match self
            .storage
            .get_item(THEME_FOLLOW_SYSTEM_STORAGE_KEY)
            .ok()
            .flatten()
            .as_deref()
        {
            Some("true") => true,
            Some("false") => false,
            _ => theme == DEFAULT_THEME,
        }
    }

    pub fn read_appearance_mode_preference(&self, theme: &str) -> ThemePreferenceMode {
        // Fall back to the legacy preference below when storage is unavailable.
        if let Ok(Some(raw)) = self.storage.get_item(THEME_APPEARANCE_MODE_STORAGE_KEY)
            && let Some(mode) = ThemePreferenceMode::parse(&raw)
        {
            return mode;
        }
        if self.read_stored_follow_system(theme) {
            return ThemePreferenceMode::System;
        }
        get_theme_preference_mode(theme).map_or(ThemePreferenceMode::Light, |appearance| {
            match appearance {
                ThemeAppearance::Light => ThemePreferenceMode::Light,
                ThemeAppearance::Dark => ThemePreferenceMode::Dark,
            }
        })
    }

    fn write_appearance_mode_preference(
        &self,
        appearance_mode: ThemePreferenceMode,
    ) -> Result<(), ThemeStorageError> {
        // The legacy follow-system flag is read-only migration input now; the
        // mode key is the single source of truth.
        self.storage
            .set_item(THEME_APPEARANCE_MODE_STORAGE_KEY, appearance_mode.as_str())
            .map_err(|cause| ThemeStorageError {
                operation: ThemeStorageOperation::Write,
                storage_key: THEME_APPEARANCE_MODE_STORAGE_KEY.to_string(),
                theme: None,
                cause,
            })
    }

    pub fn read_theme_preference(&self) -> Result<String, ThemeStorageError> {
        let raw = self
            .storage
            .get_item(STORAGE_KEY)
            .map_err(|cause| ThemeStorageError {
                operation: ThemeStorageOperation::Read,
                storage_key: STORAGE_KEY.to_string(),
                theme: None,
                cause,
            })?;
        if let Some(raw) = raw
            && is_known_theme_preference(&raw)
        {
            return Ok(canonical_theme_preference(&raw).to_string());
        }
        Ok(DEFAULT_THEME.to_string())
    }

    pub fn write_theme_preference(&self, theme: &str) -> Result<(), ThemeStorageError> {
        self.storage
            .set_item(STORAGE_KEY, theme)
            .map_err(|cause| ThemeStorageError {
                operation: ThemeStorageOperation::Write,
                storage_key: STORAGE_KEY.to_string(),
                theme: Some(theme.to_string()),
                cause,
            })?;
        self.state.lock().theme_storage_read_failure = None;
        Ok(())
    }

    fn get_stored(&self) -> String {
        if self.state.lock().theme_storage_read_failure.is_some() {
            return DEFAULT_THEME.to_string();
        }
        match self.read_theme_preference() {
            Ok(theme) => theme,
            Err(error) => {
                self.log(&error.to_string(), &error.log_attributes());
                self.state.lock().theme_storage_read_failure = Some(error);
                DEFAULT_THEME.to_string()
            }
        }
    }

    fn log(&self, message: &str, attributes: &[(String, String)]) {
        (self.logger)(message, attributes);
    }

    // ── change notification ──────────────────────────────────────────

    fn emit_change(&self) {
        let listeners = {
            let mut state = self.state.lock();
            state.snapshot_stale = true;
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

    /// `subscribe`; returns the unsubscribe handle.
    pub fn subscribe(
        self: &Arc<Self>,
        listener: impl Fn() + Send + Sync + 'static,
    ) -> Box<dyn FnOnce() + Send> {
        let id = {
            let mut state = self.state.lock();
            let id = state.next_listener_id;
            state.next_listener_id += 1;
            state.listeners.push((id, Arc::new(listener)));
            id
        };
        let store = Arc::clone(self);
        Box::new(move || {
            store
                .state
                .lock()
                .listeners
                .retain(|(listener_id, _)| *listener_id != id);
        })
    }

    // ── applying ─────────────────────────────────────────────────────

    fn apply_theme(&self, theme: &str, options: ApplyThemeOptions) {
        // Keep the editor's draft visible until an explicit refresh restores the selection.
        if options.preserve_preview() && self.state.lock().root.is_previewing() {
            return;
        }
        let appearance_mode = self.read_appearance_mode_preference(theme);
        let follow_system = appearance_mode == ThemePreferenceMode::System;
        let system_dark = follow_system && self.system_dark();
        let theme_halves = self.read_stored_theme_halves();
        {
            let state = self.state.lock();
            if let Some(last) = &state.last_applied_theme
                && last.theme == theme
                && last.system_dark == system_dark
                && last.follow_system == follow_system
                && last.appearance_mode == appearance_mode
                && same_halves(last.theme_halves.as_ref(), theme_halves.as_ref())
            {
                drop(state);
                self.sync_desktop_theme(theme, Some(follow_system), Some(appearance_mode));
                return;
            }
        }

        let resolved_appearance = resolve_theme_appearance(
            theme,
            system_dark,
            Some(follow_system),
            Some(appearance_mode),
            theme_halves.as_ref(),
        );
        {
            let mut state = self.state.lock();
            let palette_theme =
                resolve_theme_half(theme, theme_halves.as_ref(), resolved_appearance);
            apply_theme_palette(&mut state.root, &palette_theme, Some(resolved_appearance));
            state.root.dark = resolved_appearance.is_dark();
            state.last_applied_theme = Some(LastAppliedTheme {
                theme: theme.to_string(),
                system_dark,
                follow_system,
                appearance_mode,
                theme_halves,
            });
        }
        self.sync_desktop_theme(theme, Some(follow_system), Some(appearance_mode));
    }

    /// `syncDesktopTheme`: tells the desktop shell once per resolved theme; a
    /// failed (cosmetic) sync is logged and retried on the next call.
    pub fn sync_desktop_theme(
        &self,
        theme: &str,
        follow_system: Option<bool>,
        appearance_mode: Option<ThemePreferenceMode>,
    ) {
        let halves = self.read_stored_theme_halves();
        let desktop_theme =
            resolve_desktop_theme(theme, follow_system, appearance_mode, halves.as_ref());
        let Some(bridge) = self.desktop.clone() else {
            return;
        };
        {
            let mut state = self.state.lock();
            if state.last_desktop_theme == Some(desktop_theme) {
                return;
            }
            state.last_desktop_theme = Some(desktop_theme);
        }
        let theme = theme.to_string();
        let logger = Arc::clone(&self.logger);
        let state = Arc::clone(&self.state);
        (self.spawner)(Box::pin(async move {
            let result = sync_desktop_theme_preference(
                bridge.as_ref(),
                &theme,
                follow_system,
                appearance_mode,
                halves.as_ref(),
            )
            .await;
            if let Err(error) = result {
                logger(&error.to_string(), &error.log_attributes());
                let mut state = state.lock();
                if state.last_desktop_theme == Some(desktop_theme) {
                    state.last_desktop_theme = None;
                }
            }
        }));
    }
}

/// `syncDesktopThemePreference`.
pub async fn sync_desktop_theme_preference(
    bridge: &dyn DesktopThemeBridge,
    theme: &str,
    follow_system: Option<bool>,
    appearance_mode: Option<ThemePreferenceMode>,
    halves: Option<&ThemeHalves>,
) -> Result<(), DesktopThemeSyncError> {
    bridge
        .set_theme(resolve_desktop_theme(
            theme,
            follow_system,
            appearance_mode,
            halves,
        ))
        .await
        .map_err(|cause| DesktopThemeSyncError {
            theme: theme.to_string(),
            cause,
        })
}

fn appearance_mode_of(appearance: ThemeAppearance) -> ThemePreferenceMode {
    match appearance {
        ThemeAppearance::Light => ThemePreferenceMode::Light,
        ThemeAppearance::Dark => ThemePreferenceMode::Dark,
    }
}

impl ThemeStore {
    /// `getSnapshot`. Reading the preference hits storage, so this only
    /// recomputes after a change was signalled; consumers call it on every
    /// render.
    pub fn snapshot(&self) -> ThemeSnapshot {
        {
            let state = self.state.lock();
            if !state.snapshot_stale
                && let Some(last) = &state.last_snapshot
            {
                return last.clone();
            }
        }
        self.state.lock().snapshot_stale = false;
        let theme = self.get_stored();
        let appearance_mode = self.read_appearance_mode_preference(&theme);
        let follow_system = appearance_mode == ThemePreferenceMode::System;
        let system_dark = follow_system && self.system_dark();
        let theme_halves = self.read_stored_theme_halves();
        let resolved_theme = resolve_theme_appearance(
            &theme,
            system_dark,
            Some(follow_system),
            Some(appearance_mode),
            theme_halves.as_ref(),
        );
        let next = ThemeSnapshot {
            theme,
            resolved_theme,
            system_dark,
            follow_system,
            appearance_mode,
            theme_halves,
        };
        let mut state = self.state.lock();
        if let Some(last) = &state.last_snapshot
            && *last == next
        {
            return last.clone();
        }
        state.last_snapshot = Some(next.clone());
        next
    }

    /// `useEffect(() => applyTheme(theme))`: keep the root in sync on mount.
    pub fn apply_current(&self) {
        let theme = self.snapshot().theme;
        self.apply_theme(&theme, ApplyThemeOptions::default());
    }

    fn suppressed() -> ApplyThemeOptions {
        ApplyThemeOptions {
            suppress_transitions: true,
            dont_preserve_preview: false,
        }
    }

    fn log_storage_error(&self, error: &ThemeStorageError) {
        self.log(&error.to_string(), &error.log_attributes());
    }

    fn write_error(storage_key: &str, theme: Option<&str>, cause: String) -> ThemeStorageError {
        ThemeStorageError {
            operation: ThemeStorageOperation::Write,
            storage_key: storage_key.to_string(),
            theme: theme.map(str::to_string),
            cause,
        }
    }

    /// `setTheme`. Choosing a whole theme replaces any automatic-mode mix.
    pub fn set_theme(&self, next: &str) -> bool {
        let result = (|| -> Result<(), ThemeStorageError> {
            // Preserve the current mode before replacing a legacy or inferred
            // theme preference. Otherwise a fresh System preference is
            // re-inferred from the new theme's base appearance, which can
            // switch a dark UI to light.
            let current = self.read_appearance_mode_preference(&self.get_stored());
            self.write_appearance_mode_preference(current)?;
            // The mix is captured first so a failed preference write can put
            // it back instead of erasing it or leaving it attached to the new
            // theme.
            let previous_halves_raw = self
                .storage
                .get_item(THEME_HALVES_STORAGE_KEY)
                .map_err(|cause| Self::write_error(STORAGE_KEY, Some(next), cause))?;
            self.storage
                .remove_item(THEME_HALVES_STORAGE_KEY)
                .map_err(|cause| Self::write_error(STORAGE_KEY, Some(next), cause))?;
            if let Err(error) = self.write_theme_preference(next) {
                if let Some(raw) = previous_halves_raw {
                    // Storage is failing wholesale; the caller reports it.
                    let _ = self.storage.set_item(THEME_HALVES_STORAGE_KEY, &raw);
                }
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.log_storage_error(&ThemeStorageError {
                theme: Some(next.to_string()),
                ..error
            });
            return false;
        }
        self.apply_theme(next, Self::suppressed());
        self.emit_change();
        true
    }

    pub fn set_appearance_mode(&self, next_appearance_mode: ThemePreferenceMode) -> bool {
        if let Err(error) = self.write_appearance_mode_preference(next_appearance_mode) {
            self.log_storage_error(&error);
            return false;
        }
        self.state.lock().theme_storage_read_failure = None;
        let stored = self.get_stored();
        self.apply_theme(&stored, Self::suppressed());
        self.emit_change();
        true
    }

    pub fn set_follow_system(&self, next_follow_system: bool) -> bool {
        let theme = self.snapshot().theme;
        let current_mode = self.read_appearance_mode_preference(&theme);
        let next_mode = if next_follow_system {
            ThemePreferenceMode::System
        } else if current_mode == ThemePreferenceMode::System {
            get_theme_preference_mode(&theme).map_or(ThemePreferenceMode::Light, appearance_mode_of)
        } else {
            current_mode
        };
        self.set_appearance_mode(next_mode)
    }

    fn write_theme_half(
        &self,
        appearance: ThemeAppearance,
        theme_id: Option<&str>,
    ) -> Result<(), String> {
        let mut next = self.read_stored_theme_halves_raw();
        next.set(appearance, theme_id.map(str::to_string));
        if next.is_empty() {
            return self.storage.remove_item(THEME_HALVES_STORAGE_KEY);
        }
        let mut record = serde_json::Map::new();
        if let Some(light) = &next.light {
            record.insert("light".into(), light.clone().into());
        }
        if let Some(dark) = &next.dark {
            record.insert("dark".into(), dark.clone().into());
        }
        self.storage.set_item(
            THEME_HALVES_STORAGE_KEY,
            &serde_json::Value::Object(record).to_string(),
        )
    }

    /// `setThemeHalf`: `None` removes that appearance's half.
    pub fn set_theme_half(&self, appearance: ThemeAppearance, theme_id: Option<&str>) -> bool {
        if let Err(cause) = self.write_theme_half(appearance, theme_id) {
            self.log_storage_error(&Self::write_error(THEME_HALVES_STORAGE_KEY, None, cause));
            return false;
        }
        let stored = self.get_stored();
        self.apply_theme(&stored, Self::suppressed());
        self.emit_change();
        true
    }

    pub fn clear_theme_halves(&self) -> bool {
        if let Err(cause) = self.storage.remove_item(THEME_HALVES_STORAGE_KEY) {
            self.log_storage_error(&Self::write_error(THEME_HALVES_STORAGE_KEY, None, cause));
            return false;
        }
        let stored = self.get_stored();
        self.apply_theme(&stored, Self::suppressed());
        self.emit_change();
        true
    }

    /// `refreshTheme`: re-applies the stored selection, restoring it over an
    /// editor draft unless `preserve_preview`.
    pub fn refresh_theme(&self, preserve_preview: bool) {
        self.state.lock().last_applied_theme = None;
        let stored = self.get_stored();
        self.apply_theme(
            &stored,
            ApplyThemeOptions {
                suppress_transitions: true,
                dont_preserve_preview: !preserve_preview,
            },
        );
        self.emit_change();
    }

    // ── external events ──────────────────────────────────────────────

    /// The OS light/dark switch (`matchMedia` change).
    pub fn handle_system_appearance_change(&self) {
        let stored = self.get_stored();
        if self.read_appearance_mode_preference(&stored) == ThemePreferenceMode::System {
            self.apply_theme(&stored, Self::suppressed());
        }
        self.emit_change();
    }

    /// A cross-process write to the appearance keys (`storage` event);
    /// `key: None` means the whole store was cleared.
    pub fn handle_storage_change(&self, key: Option<&str>) {
        match key {
            Some(STORAGE_KEY) => {
                self.state.lock().theme_storage_read_failure = None;
            }
            Some(
                THEME_FOLLOW_SYSTEM_STORAGE_KEY
                | THEME_APPEARANCE_MODE_STORAGE_KEY
                | THEME_HALVES_STORAGE_KEY,
            ) => {}
            Some(CUSTOM_THEMES_STORAGE_KEY) | None => {
                if key.is_none() {
                    self.state.lock().theme_storage_read_failure = None;
                }
                invalidate_custom_themes();
                self.state.lock().last_applied_theme = None;
            }
            Some(_) => return,
        }
        let stored = self.get_stored();
        self.apply_theme(&stored, Self::suppressed());
        self.emit_change();
    }
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/hooks/useTheme.test.ts, plus store
    // behaviour the hook test files leave to the DOM.
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::theme::theme_palette::{
        THEME_PREVIEW_ID, apply_theme_color_preview, get_standard_theme_colors,
    };
    use crate::theme::theme_palettes::GROVE_THEME;

    /// Process-wide theme-library and preview state is shared by every test
    /// that resolves themes; serialize them.
    use crate::theme::THEME_GLOBALS_TEST_LOCK as TEST_LOCK;

    type GetOverride = Arc<dyn Fn(&str) -> Option<Result<Option<String>, String>> + Send + Sync>;

    #[derive(Default)]
    struct TestStorage {
        inner: MemoryThemeStorage,
        get_override: Mutex<Option<GetOverride>>,
        fail_set: Mutex<Option<String>>,
        fail_set_keys: Mutex<Vec<String>>,
    }

    impl TestStorage {
        fn failing_reads(cause: &str) -> Arc<Self> {
            let storage = Arc::new(Self::default());
            let cause = cause.to_string();
            *storage.get_override.lock() = Some(Arc::new(move |_| Some(Err(cause.clone()))));
            storage
        }
    }

    impl ThemeStorage for TestStorage {
        fn get_item(&self, key: &str) -> Result<Option<String>, String> {
            if let Some(over) = self.get_override.lock().clone()
                && let Some(result) = over(key)
            {
                return result;
            }
            self.inner.get_item(key)
        }

        fn set_item(&self, key: &str, value: &str) -> Result<(), String> {
            if let Some(cause) = self.fail_set.lock().clone() {
                return Err(cause);
            }
            if self
                .fail_set_keys
                .lock()
                .iter()
                .any(|failing| failing == key)
            {
                return Err("blocked".to_string());
            }
            self.inner.set_item(key, value)
        }

        fn remove_item(&self, key: &str) -> Result<(), String> {
            self.inner.remove_item(key)
        }
    }

    struct FixedSystem(bool);

    impl SystemAppearance for FixedSystem {
        fn is_dark(&self) -> bool {
            self.0
        }
    }

    type LogEntry = (String, Vec<(String, String)>);

    #[derive(Default)]
    struct RecordedLogs(Mutex<Vec<LogEntry>>);

    impl RecordedLogs {
        fn logger(self: &Arc<Self>) -> ThemeLogger {
            let logs = Arc::clone(self);
            Arc::new(move |message, attributes| {
                logs.0
                    .lock()
                    .push((message.to_string(), attributes.to_vec()));
            })
        }

        fn count(&self) -> usize {
            self.0.lock().len()
        }
    }

    fn inline_spawner() -> ThemeSpawner {
        Arc::new(|future| {
            tokio::spawn(future);
        })
    }

    fn store_with(
        storage: Arc<dyn ThemeStorage>,
        system_dark: bool,
        desktop: Option<Arc<dyn DesktopThemeBridge>>,
        logs: &Arc<RecordedLogs>,
    ) -> Arc<ThemeStore> {
        ThemeStore::with_runtime(
            storage,
            Arc::new(FixedSystem(system_dark)),
            desktop,
            logs.logger(),
            inline_spawner(),
        )
    }

    fn plain_store(system_dark: bool) -> (Arc<ThemeStore>, Arc<TestStorage>) {
        let storage = Arc::new(TestStorage::default());
        let store = store_with(
            storage.clone(),
            system_dark,
            None,
            &Arc::new(RecordedLogs::default()),
        );
        (store, storage)
    }

    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    // describe("theme failure handling")

    #[test]
    fn preserves_exact_storage_causes_and_operation_context() {
        let _guard = TEST_LOCK.lock();
        let storage = Arc::new(TestStorage::default());
        *storage.get_override.lock() =
            Some(Arc::new(|_| Some(Err("storage read blocked".to_string()))));
        *storage.fail_set.lock() = Some("storage quota exceeded".to_string());
        let store = store_with(storage, false, None, &Arc::new(RecordedLogs::default()));

        assert_eq!(
            store.read_theme_preference(),
            Err(ThemeStorageError {
                operation: ThemeStorageOperation::Read,
                storage_key: "t3code:theme".to_string(),
                theme: None,
                cause: "storage read blocked".to_string(),
            })
        );
        assert_eq!(
            store.write_theme_preference("dark"),
            Err(ThemeStorageError {
                operation: ThemeStorageOperation::Write,
                storage_key: "t3code:theme".to_string(),
                theme: Some("dark".to_string()),
                cause: "storage quota exceeded".to_string(),
            })
        );
    }

    #[test]
    fn reads_the_persisted_t3_chat_theme_preference() {
        let _guard = TEST_LOCK.lock();
        let storage = Arc::new(TestStorage::default());
        *storage.get_override.lock() = Some(Arc::new(|_| Some(Ok(Some("t3-chat".to_string())))));
        let store = store_with(storage, false, None, &Arc::new(RecordedLogs::default()));
        assert_eq!(store.read_theme_preference().as_deref(), Ok("t3-chat"));
    }

    #[test]
    fn falls_back_during_initial_theme_application_and_logs_only_safe_attributes() {
        let _guard = TEST_LOCK.lock();
        let cause = "private browsing storage failure";
        let logs = Arc::new(RecordedLogs::default());
        let store = store_with(TestStorage::failing_reads(cause), false, None, &logs);

        assert_eq!(store.snapshot().theme, "system");
        let recorded = logs.0.lock();
        assert_eq!(
            recorded[0].0,
            "Failed to read theme preference for t3code:theme."
        );
        let attributes = &recorded[0].1;
        assert!(attributes.contains(&("operation".to_string(), "read".to_string())));
        assert!(attributes.contains(&("storageKey".to_string(), "t3code:theme".to_string())));
        assert!(attributes.contains(&("errorTag".to_string(), "ThemeStorageError".to_string())));
        assert!(attributes.iter().all(|(key, _)| key != "cause"));
        assert!(!format!("{attributes:?}").contains(cause));
    }

    #[test]
    fn retries_a_failed_storage_read_only_after_a_relevant_storage_event() {
        let _guard = TEST_LOCK.lock();
        let theme_reads = Arc::new(AtomicUsize::new(0));
        let storage = Arc::new(TestStorage::default());
        *storage.get_override.lock() = Some({
            let theme_reads = Arc::clone(&theme_reads);
            Arc::new(move |key| {
                (key == "t3code:theme").then(|| {
                    theme_reads.fetch_add(1, Ordering::SeqCst);
                    Err("persistent storage failure".to_string())
                })
            })
        });
        let logs = Arc::new(RecordedLogs::default());
        let store = store_with(storage, false, None, &logs);
        store.snapshot();
        store.snapshot();

        assert_eq!(theme_reads.load(Ordering::SeqCst), 1);
        assert_eq!(logs.count(), 1);

        let unsubscribe = store.subscribe(|| {});
        store.handle_storage_change(Some("t3code:theme"));
        store.snapshot();

        assert_eq!(theme_reads.load(Ordering::SeqCst), 2);
        assert_eq!(logs.count(), 2);
        unsubscribe();
    }

    struct FailingBridge {
        calls: Arc<AtomicUsize>,
    }

    impl DesktopThemeBridge for FailingBridge {
        fn set_theme(&self, _theme: DesktopTheme) -> BoxFut<Result<(), String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err("desktop IPC unavailable".to_string()) })
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn preserves_desktop_sync_causes_and_retries_after_a_failed_cosmetic_sync() {
        let _guard = TEST_LOCK.lock();
        let cause = "desktop IPC unavailable";
        let calls = Arc::new(AtomicUsize::new(0));
        let bridge = Arc::new(FailingBridge {
            calls: Arc::clone(&calls),
        });

        let error = sync_desktop_theme_preference(bridge.as_ref(), "dark", None, None, None)
            .await
            .expect_err("sync failure");
        assert_eq!(
            error,
            DesktopThemeSyncError {
                theme: "dark".to_string(),
                cause: cause.to_string(),
            }
        );

        calls.store(0, Ordering::SeqCst);
        let logs = Arc::new(RecordedLogs::default());
        let bridge: Arc<dyn DesktopThemeBridge> = bridge;
        let store = store_with(Arc::new(TestStorage::default()), false, Some(bridge), &logs);
        // Construction already attempted one sync of the default theme; the
        // upstream test starts counting from a clean mock, so do the same.
        settle().await;
        calls.store(0, Ordering::SeqCst);
        logs.0.lock().clear();

        store.sync_desktop_theme("dark", None, None);
        settle().await;
        store.sync_desktop_theme("dark", None, None);
        settle().await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let recorded = logs.0.lock();
        assert_eq!(
            recorded[0].0,
            "Failed to sync the dark theme to the desktop shell."
        );
        assert!(
            recorded[0]
                .1
                .contains(&("errorTag".to_string(), "DesktopThemeSyncError".to_string()))
        );
        assert!(
            recorded[0]
                .1
                .contains(&("theme".to_string(), "dark".to_string()))
        );
        for (_, attributes) in recorded.iter() {
            assert!(attributes.iter().all(|(key, _)| key != "cause"));
            assert!(!format!("{attributes:?}").contains(cause));
        }
    }

    // Store behaviour

    #[test]
    fn follows_the_system_appearance_and_repaints_on_change() {
        let _guard = TEST_LOCK.lock();
        let (dark_store, _) = plain_store(true);
        let snapshot = dark_store.snapshot();
        assert_eq!(snapshot.theme, "system");
        assert!(snapshot.follow_system);
        assert_eq!(snapshot.resolved_theme, ThemeAppearance::Dark);
        assert!(dark_store.root().dark);

        let (light_store, _) = plain_store(false);
        assert_eq!(
            light_store.snapshot().resolved_theme,
            ThemeAppearance::Light
        );
        assert!(!light_store.root().dark);
    }

    #[test]
    fn set_theme_applies_the_palette_and_notifies_subscribers() {
        let _guard = TEST_LOCK.lock();
        let (store, storage) = plain_store(false);
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let _unsubscribe = store.subscribe(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        assert!(store.set_theme(GROVE_THEME.id.as_str()));
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.root().theme_id.as_deref(),
            Some(GROVE_THEME.id.as_str())
        );
        assert_eq!(
            storage.get_item("t3code:theme"),
            Ok(Some("grove".to_string()))
        );
        // The mode was pinned before the preference changed.
        assert_eq!(
            storage.get_item("t3code:theme-appearance-mode"),
            Ok(Some("system".to_string()))
        );
        assert_eq!(store.snapshot().theme, "grove");
    }

    #[test]
    fn set_theme_clears_the_mix_and_restores_it_when_the_write_fails() {
        let _guard = TEST_LOCK.lock();
        let (store, storage) = plain_store(false);
        let mix = json!({ "dark": GROVE_THEME.id.as_str() }).to_string();
        storage
            .set_item("t3code:theme-halves:v1", &mix)
            .expect("seed");

        *storage.fail_set_keys.lock() = vec!["t3code:theme".to_string()];
        assert!(!store.set_theme("light"));
        assert_eq!(storage.get_item("t3code:theme-halves:v1"), Ok(Some(mix)));

        storage.fail_set_keys.lock().clear();
        assert!(store.set_theme("light"));
        assert_eq!(storage.get_item("t3code:theme-halves:v1"), Ok(None));
    }

    #[test]
    fn a_configured_half_themes_its_appearance() {
        let _guard = TEST_LOCK.lock();
        let (store, _) = plain_store(true);
        assert!(store.set_theme_half(ThemeAppearance::Dark, Some(GROVE_THEME.id.as_str())));
        assert_eq!(
            store.root().theme_id.as_deref(),
            Some(GROVE_THEME.id.as_str())
        );
        assert_eq!(
            store.snapshot().theme_halves,
            Some(ThemeHalves {
                light: None,
                dark: Some(GROVE_THEME.id.to_string()),
            })
        );

        assert!(store.set_theme_half(ThemeAppearance::Dark, None));
        assert_eq!(store.root().theme_id, None);
        assert_eq!(store.snapshot().theme_halves, None);
    }

    #[test]
    fn the_raw_mix_keeps_halves_that_do_not_resolve_yet() {
        let _guard = TEST_LOCK.lock();
        let (store, storage) = plain_store(false);
        storage
            .set_item(
                "t3code:theme-halves:v1",
                &json!({ "light": "published-later" }).to_string(),
            )
            .expect("seed");
        assert_eq!(store.read_theme_halves(), None);
        assert_eq!(
            store.read_theme_halves_raw().light.as_deref(),
            Some("published-later")
        );
        // Changing the other half must not erase the unresolved one.
        assert!(store.set_theme_half(ThemeAppearance::Dark, Some(GROVE_THEME.id.as_str())));
        assert_eq!(
            store.read_theme_halves_raw().light.as_deref(),
            Some("published-later")
        );
    }

    #[test]
    fn follow_system_off_pins_the_appearance_of_the_current_theme() {
        let _guard = TEST_LOCK.lock();
        let (store, _) = plain_store(true);
        assert!(store.set_follow_system(false));
        let snapshot = store.snapshot();
        assert!(!snapshot.follow_system);
        assert_eq!(snapshot.appearance_mode, ThemePreferenceMode::Light);
        assert_eq!(snapshot.resolved_theme, ThemeAppearance::Light);
        assert!(store.set_follow_system(true));
        assert_eq!(store.snapshot().resolved_theme, ThemeAppearance::Dark);
    }

    #[test]
    fn an_editor_preview_survives_ordinary_applies_until_refreshed() {
        let _guard = TEST_LOCK.lock();
        let (store, _) = plain_store(false);
        store.with_root(|root| {
            apply_theme_color_preview(
                root,
                &get_standard_theme_colors(ThemeAppearance::Dark),
                ThemeAppearance::Dark,
            );
        });
        assert_eq!(store.root().theme_id.as_deref(), Some(THEME_PREVIEW_ID));

        store.apply_current();
        store.handle_system_appearance_change();
        assert_eq!(store.root().theme_id.as_deref(), Some(THEME_PREVIEW_ID));

        // `refreshTheme({ preservePreview: true })` keeps the draft too.
        store.refresh_theme(true);
        assert_eq!(store.root().theme_id.as_deref(), Some(THEME_PREVIEW_ID));

        store.refresh_theme(false);
        assert_eq!(store.root().theme_id, None);
        assert!(!store.root().dark);
    }

    #[test]
    fn storage_events_for_unrelated_keys_are_ignored() {
        let _guard = TEST_LOCK.lock();
        let (store, storage) = plain_store(false);
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let _unsubscribe = store.subscribe(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        store.handle_storage_change(Some("something-else"));
        assert_eq!(notified.load(Ordering::SeqCst), 0);

        storage.set_item("t3code:theme", "dark").expect("write");
        store.handle_storage_change(Some("t3code:theme"));
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(store.snapshot().theme, "dark");
        assert!(store.root().dark);
    }
}
