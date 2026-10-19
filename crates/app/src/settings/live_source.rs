//! Live [`SettingsSource`]: server settings, keybindings, providers, projects,
//! and storage backed by the write runtime (see `docs/handoff/WIRE-settings.md`).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::{App, AppContext as _, Entity};
use indexmap::IndexMap;
use serde_json::Value;
use turin_contracts::keybindings::KeybindingRule;
use turin_contracts::provider_instance::{
    ProviderInstanceId, ProviderInstanceMutation as ContractMutation,
};
use turin_contracts::provider_setup::{
    ProviderAuthCallbackMode, ProviderAuthRespondInput, ProviderAuthStartInput,
    ProviderInstallCancelInput, ProviderSetupInput,
};
use turin_contracts::settings::{ClientSettingsPatch, ServerSettingsPatch};
use turin_contracts::{ProjectId, ThreadId};
use turin_providers::provider_maintenance_runner::ProviderUpdateTarget;
use turin_runtime::keybindings::UpsertKeybindingInput;

use crate::state::store::ClientStore;

use super::live_bindings::{LiveSettingsServices, SETTINGS_AUTH_OWNER_SESSION};
use super::live_commands::{
    self, update_provider_instance as persist_provider_instance,
    update_server_settings as persist_server_settings,
};
use super::live_projects::set_project_icons;
use super::live_snapshot::{
    LiveSnapshotInput, build_snapshot, cleanup_report, read_client_settings,
};
use super::mock_source::{
    apply_appearance, runtime_appearance, set_runtime_appearance_mode, set_runtime_theme,
};
use super::view_model::{
    ProviderInstanceMutation, RemoveKeybinding, SettingsSource, SettingsStore, SettingsViewModel,
    SetupResult, UpsertKeybinding,
};
use crate::settings_logic::use_settings::{
    persist_client_settings_patch, prepare_model_preferences_patch,
};

struct LiveState {
    services: LiveSettingsServices,
    appearance: super::view_model::AppearanceState,
    providers: std::sync::Arc<Vec<turin_provider_core::server_provider::ServerProvider>>,
    providers_refreshing: bool,
    provider_auth: IndexMap<String, turin_contracts::provider_setup::ProviderAuthState>,
    provider_install: IndexMap<String, turin_contracts::provider_setup::ProviderInstallState>,
    source_control: turin_contracts::source_control::SourceControlDiscoveryResult,
    storage_cleanup: Option<super::view_model::CleanupReport>,
    chat_gpt_welcome_acknowledged: Vec<String>,
    pending_provider_auth_delivery: Option<super::view_model::ProviderAuthDelivery>,
    server: turin_contracts::settings::ServerSettings,
    keybindings: turin_runtime::keybindings::KeybindingsConfigState,
}

pub struct LiveSettingsSource {
    inner: Arc<Mutex<LiveState>>,
    client: Entity<ClientStore>,
    store: Rc<RefCell<Option<Entity<SettingsStore>>>>,
    /// Wakes the UI thread from runtime tasks (see [`UiEvents`]).
    ui_events: UiEvents,
}

/// Runtime tasks cannot touch GPUI. The state they change lives behind a
/// mutex, so a write alone never repaints: each sends `()` here and the UI
/// thread rebuilds and publishes the view model.
type UiEvents = tokio::sync::mpsc::UnboundedSender<()>;

/// The pieces needed to publish the view model or report a failure from the
/// UI thread. Cheap to clone into GPUI tasks.
#[derive(Clone)]
struct LiveUi {
    inner: Arc<Mutex<LiveState>>,
    client: Entity<ClientStore>,
    store: Rc<RefCell<Option<Entity<SettingsStore>>>>,
}

impl LiveUi {
    fn publish(&self, cx: &mut App) {
        let path = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .keybindings
            .config_path()
            .display()
            .to_string();
        let vm = snapshot_from(&self.inner, &self.client, cx, &path);
        apply_appearance(&vm, cx);
        if let Some(store) = self.store.borrow().clone() {
            store.update(cx, |store, cx| store.publish(vm, cx));
        }
    }

    fn report(&self, title: &str, description: &str, cx: &mut App) {
        if let Some(store) = self.store.borrow().clone() {
            store.update(cx, |store, cx| {
                store.report_failure(title, description, cx);
            });
        }
    }
}

fn runtime_handle(inner: &Arc<Mutex<LiveState>>) -> tokio::runtime::Handle {
    inner
        .lock()
        .expect("live settings state")
        .services
        .bus
        .handle()
        .clone()
}

impl LiveSettingsSource {
    /// Builds the store, attaches live feeds, and returns the settings entity.
    pub fn start(
        services: LiveSettingsServices,
        client: Entity<ClientStore>,
        cx: &mut App,
    ) -> Entity<SettingsStore> {
        let handle = services.bus.handle().clone();
        let keybindings_path = services.keybindings.config_path().display().to_string();
        let initial_server = handle
            .block_on(live_commands::server_settings(&services.bus))
            .unwrap_or_default();
        let initial_keybindings = handle.block_on(services.keybindings.snapshot()).unwrap_or(
            turin_runtime::keybindings::KeybindingsConfigState {
                keybindings: Vec::new(),
                issues: Vec::new(),
            },
        );
        let initial_providers = services.bus.providers().borrow().clone();
        let source_control = handle.block_on(services.source_control.discover());
        let storage_cleanup = services
            .storage_cleanup
            .latest_report()
            .map(|report| cleanup_report(&report));
        let inner = Arc::new(Mutex::new(LiveState {
            services,
            appearance: super::view_model::AppearanceState::default(),
            providers: initial_providers,
            providers_refreshing: false,
            provider_auth: IndexMap::new(),
            provider_install: IndexMap::new(),
            source_control,
            storage_cleanup,
            chat_gpt_welcome_acknowledged: Vec::new(),
            pending_provider_auth_delivery: None,
            server: initial_server,
            keybindings: initial_keybindings,
        }));
        let vm = snapshot_from(&inner, &client, cx, &keybindings_path);
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let source = Rc::new(Self {
            inner: inner.clone(),
            client: client.clone(),
            store: Rc::new(RefCell::new(None)),
            ui_events: events_tx,
        });
        let store = cx.new(|_| SettingsStore::new(vm, source.clone()));
        *source.store.borrow_mut() = Some(store.clone());
        source.spawn_feeds(events_rx, cx);
        store
    }

    fn publish(&self, cx: &mut App) {
        self.ui().publish(cx);
    }

    fn ui(&self) -> LiveUi {
        LiveUi {
            inner: self.inner.clone(),
            client: self.client.clone(),
            store: self.store.clone(),
        }
    }

    /// Runs `work` on the runtime, then `finish` on the UI thread with its
    /// output, and publishes the view model afterwards (the state `work`
    /// changed lives behind a mutex, so nothing repaints on its own).
    fn run_then<T: Send + 'static>(
        &self,
        work: impl std::future::Future<Output = T> + Send + 'static,
        finish: impl FnOnce(T, &LiveUi, &mut App) + 'static,
        cx: &mut App,
    ) {
        let (done, outcome) = futures::channel::oneshot::channel();
        runtime_handle(&self.inner).spawn(async move {
            let _ = done.send(work.await);
        });
        let ui = self.ui();
        cx.spawn(async move |cx| {
            let Ok(output) = outcome.await else {
                return;
            };
            cx.update(|cx| {
                finish(output, &ui, cx);
                ui.publish(cx);
            });
        })
        .detach();
    }

    fn spawn_feeds(&self, mut events: tokio::sync::mpsc::UnboundedReceiver<()>, cx: &mut App) {
        let ui = self.ui();
        cx.observe(&self.client, {
            let ui = ui.clone();
            move |_, cx| ui.publish(cx)
        })
        .detach();

        // A theme picked elsewhere (the command palette) repaints Settings too.
        // `publish` stores what it shows, so its own repaint does not loop.
        cx.observe_global::<crate::theme::AppTheme>({
            let ui = ui.clone();
            move |cx| {
                let changed = runtime_appearance(cx).is_some_and(|appearance| {
                    ui.inner.lock().expect("live settings state").appearance != appearance
                });
                if changed {
                    ui.publish(cx);
                }
            }
        })
        .detach();

        // One publish per burst of events.
        cx.spawn(async move |cx| {
            while events.recv().await.is_some() {
                while events.try_recv().is_ok() {}
                cx.update(|cx| ui.publish(cx));
            }
        })
        .detach();

        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let tx = self.ui_events.clone();
        handle.spawn(async move {
            let services = inner.lock().expect("live settings state").services.clone();
            let mut changes = services.server_settings.subscribe();
            while let Some(settings) = changes.recv().await {
                inner.lock().expect("live settings state").server = settings;
                let _ = tx.send(());
            }
        });
        let inner = self.inner.clone();
        let tx = self.ui_events.clone();
        handle.spawn(async move {
            let services = inner.lock().expect("live settings state").services.clone();
            let mut rx = services.keybindings.subscribe();
            while let Some(config) = rx.recv().await {
                inner.lock().expect("live settings state").keybindings = config;
                let _ = tx.send(());
            }
        });
        let inner = self.inner.clone();
        let tx = self.ui_events.clone();
        handle.spawn(async move {
            let services = inner.lock().expect("live settings state").services.clone();
            let mut rx = services.storage_cleanup.subscribe();
            while rx.changed().await.is_ok() {
                inner.lock().expect("live settings state").storage_cleanup =
                    rx.borrow().clone().map(|report| cleanup_report(&report));
                let _ = tx.send(());
            }
        });
        let inner = self.inner.clone();
        let tx = self.ui_events.clone();
        let bus = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        handle.spawn(async move {
            let mut providers = bus.providers();
            let watched = Arc::new(Mutex::new(SetupWatchers::default()));
            loop {
                let snapshot = providers.borrow_and_update().clone();
                inner.lock().expect("live settings state").providers = snapshot.clone();
                watch_setup_states(&inner, &tx, &watched, &snapshot);
                let _ = tx.send(());
                if providers.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    fn report(&self, title: &str, description: &str, cx: &mut App) {
        self.ui().report(title, description, cx);
    }

    /// Awaits a command on the UI thread and toasts `title` with the error
    /// message when it fails (upstream's `toastManager.add({ type: "error" })`
    /// on a rejected dispatch); either way the view republishes afterwards.
    fn report_failure_of<T: 'static>(
        &self,
        task: gpui_kit::Task<Result<T, crate::app_runtime::write::commands::CommandError>>,
        title: &'static str,
        cx: &mut App,
    ) {
        let ui = self.ui();
        cx.spawn(async move |cx| {
            let outcome = task.await;
            cx.update(|cx| {
                if let Err(error) = outcome {
                    ui.report(title, &error.message, cx);
                }
                ui.publish(cx);
            });
        })
        .detach();
    }
}

/// Instances whose sign-in / install streams already have a watcher task.
#[derive(Default)]
struct SetupWatchers {
    auth: std::collections::HashSet<String>,
    install: std::collections::HashSet<String>,
}

/// Subscribes (once per instance) to the sign-in and install state streams so
/// flow progress (authorization URL, prompts, download progress) reaches the
/// view. A stream that ends (instance removed, no controller) frees its slot
/// so the next provider update retries.
fn watch_setup_states(
    inner: &Arc<Mutex<LiveState>>,
    tx: &UiEvents,
    watched: &Arc<Mutex<SetupWatchers>>,
    providers: &[turin_provider_core::server_provider::ServerProvider],
) {
    use futures::StreamExt as _;
    let (services, handle) = {
        let state = inner.lock().expect("live settings state");
        (state.services.clone(), state.services.bus.handle().clone())
    };
    for provider in providers {
        let id = provider.instance_id.clone();
        let key = id.as_str().to_owned();
        let new_auth = watched
            .lock()
            .expect("setup watchers")
            .auth
            .insert(key.clone());
        if new_auth {
            let (inner, tx, watched, services, id, key) = (
                inner.clone(),
                tx.clone(),
                watched.clone(),
                services.clone(),
                id.clone(),
                key.clone(),
            );
            handle.spawn(async move {
                let mut stream = services
                    .provider_auth
                    .subscribe(&id, SETTINGS_AUTH_OWNER_SESSION);
                while let Some(Ok(state)) = stream.next().await {
                    inner
                        .lock()
                        .expect("live settings state")
                        .provider_auth
                        .insert(key.clone(), state);
                    let _ = tx.send(());
                }
                watched.lock().expect("setup watchers").auth.remove(&key);
            });
        }
        let new_install = watched
            .lock()
            .expect("setup watchers")
            .install
            .insert(key.clone());
        if new_install {
            let (inner, tx, watched, services, id, key) = (
                inner.clone(),
                tx.clone(),
                watched.clone(),
                services.clone(),
                id.clone(),
                key.clone(),
            );
            handle.spawn(async move {
                let input = ProviderSetupInput { instance_id: id };
                if let Ok(mut stream) = services.provider_installation.subscribe(&input).await {
                    while let Some(state) = stream.next().await {
                        inner
                            .lock()
                            .expect("live settings state")
                            .provider_install
                            .insert(key.clone(), state);
                        let _ = tx.send(());
                    }
                }
                watched.lock().expect("setup watchers").install.remove(&key);
            });
        }
    }
}

fn snapshot_from(
    inner: &Arc<Mutex<LiveState>>,
    client: &Entity<ClientStore>,
    cx: &mut App,
    keybindings_path: &str,
) -> SettingsViewModel {
    let mut state = inner.lock().expect("live settings state");
    // Theme and appearance mode are the theme runtime's (device-local).
    if let Some(appearance) = runtime_appearance(cx) {
        state.appearance = appearance;
    }
    build_snapshot(LiveSnapshotInput {
        appearance: state.appearance.clone(),
        client: read_client_settings(),
        server: state.server.clone(),
        platform: &state.services.platform,
        environment: state.services.environment.clone(),
        keybindings: state.keybindings.clone(),
        keybindings_config_path: keybindings_path.to_owned(),
        providers: state.providers.clone(),
        providers_refreshing: state.providers_refreshing,
        provider_auth: state.provider_auth.clone(),
        provider_install: state.provider_install.clone(),
        chat_gpt_welcome_acknowledged: state.chat_gpt_welcome_acknowledged.clone(),
        pending_provider_auth_delivery: state.pending_provider_auth_delivery.clone(),
        source_control: state.source_control.clone(),
        storage_cleanup: state.storage_cleanup.clone(),
        client_store: client.read(cx).state(),
    })
}

fn parse_instance_id(id: &str) -> Result<ProviderInstanceId, String> {
    ProviderInstanceId::new(id).map_err(|error| error.to_string())
}

fn contract_mutation(mutation: ProviderInstanceMutation) -> Result<ContractMutation, String> {
    match mutation {
        ProviderInstanceMutation::Upsert {
            instance_id,
            instance,
        } => {
            let instance_id = parse_instance_id(&instance_id)?;
            let instance = serde_json::from_value(instance)
                .map_err(|error| format!("Invalid provider instance: {error}"))?;
            Ok(ContractMutation::Upsert {
                instance_id,
                instance,
            })
        }
        ProviderInstanceMutation::Remove { instance_id } => Ok(ContractMutation::Remove {
            instance_id: parse_instance_id(&instance_id)?,
        }),
    }
}

impl SettingsSource for LiveSettingsSource {
    fn update_client(&self, patch: Value, cx: &mut App) {
        let patch: ClientSettingsPatch = match serde_json::from_value(patch) {
            Ok(patch) => patch,
            Err(error) => {
                self.report("Could not update settings", &error.to_string(), cx);
                return;
            }
        };
        // Publish now (optimistic client snapshot), and again once persisted.
        self.publish(cx);
        self.run_then(persist_client_settings_patch(patch), |(), _, _| {}, cx);
    }

    fn update_server(&self, patch: Value, cx: &mut App) {
        let patch: ServerSettingsPatch = match serde_json::from_value(patch) {
            Ok(patch) => patch,
            Err(error) => {
                self.report("Could not update settings", &error.to_string(), cx);
                return;
            }
        };
        let inner = Arc::clone(&self.inner);
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        self.run_then(
            async move {
                let current = live_commands::server_settings(&bus)
                    .await
                    .map_err(|error| error.message)?;
                let patch = prepare_model_preferences_patch(&current, patch)
                    .await
                    .map_err(|error| error.to_string())?;
                let settings = persist_server_settings(&bus, patch)
                    .await
                    .map_err(|error| error.message)?;
                inner.lock().expect("live settings state").server = settings;
                Ok::<(), String>(())
            },
            |result, ui, cx| {
                if let Err(message) = result {
                    ui.report("Could not update settings", &message, cx);
                }
            },
            cx,
        );
    }

    fn mutate_provider_instance(
        &self,
        mutation: ProviderInstanceMutation,
        patch: Option<Value>,
        cx: &mut App,
    ) {
        self.mutate_provider_instance_then(mutation, patch, Box::new(|_, _| {}), cx);
    }

    fn mutate_provider_instance_then(
        &self,
        mutation: ProviderInstanceMutation,
        patch: Option<Value>,
        done: super::view_model::MutationDone,
        cx: &mut App,
    ) {
        let mutation = match contract_mutation(mutation) {
            Ok(mutation) => mutation,
            Err(error) => {
                self.report("Could not update provider instance", &error, cx);
                done(Err(error), cx);
                return;
            }
        };
        let server_patch = match patch.map(serde_json::from_value::<ServerSettingsPatch>) {
            Some(Ok(patch)) => Some(patch),
            Some(Err(error)) => {
                let error = error.to_string();
                self.report("Could not update provider instance", &error, cx);
                done(Err(error), cx);
                return;
            }
            None => None,
        };
        let inner = Arc::clone(&self.inner);
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        self.run_then(
            async move {
                if let Some(patch) = server_patch {
                    persist_server_settings(&bus, patch)
                        .await
                        .map_err(|error| error.message)?;
                }
                let settings = persist_provider_instance(&bus, mutation)
                    .await
                    .map_err(|error| error.message)?;
                inner.lock().expect("live settings state").server = settings;
                Ok::<(), String>(())
            },
            move |result, ui, cx| {
                if let Err(message) = &result {
                    ui.report("Could not update provider instance", message, cx);
                }
                done(result, cx);
            },
            cx,
        );
    }

    fn upsert_keybinding(&self, input: UpsertKeybinding, cx: &mut App) {
        let rule = match serde_json::from_value::<KeybindingRule>(serde_json::json!({
            "key": input.key,
            "command": input.command,
            "when": input.when.filter(|when| !when.is_empty()),
        })) {
            Ok(rule) => rule,
            Err(error) => {
                self.report("Unable to save keybinding", &error.to_string(), cx);
                return;
            }
        };
        let replace = input.replaces.and_then(|replaces| {
            serde_json::from_value::<KeybindingRule>(serde_json::json!({
                "key": replaces.key,
                "command": replaces.command,
                "when": replaces.when,
            }))
            .ok()
        });
        let keybindings = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .keybindings
            .clone();
        self.run_then(
            async move {
                keybindings
                    .upsert_keybinding_rule(UpsertKeybindingInput { rule, replace })
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
            |result, ui, cx| {
                if let Err(message) = result {
                    ui.report("Unable to save keybinding", &message, cx);
                }
            },
            cx,
        );
    }

    fn remove_keybinding(&self, input: RemoveKeybinding, cx: &mut App) {
        let rule = match serde_json::from_value::<KeybindingRule>(serde_json::json!({
            "key": input.key,
            "command": input.command,
            "when": input.when,
        })) {
            Ok(rule) => rule,
            Err(error) => {
                self.report("Unable to save keybinding", &error.to_string(), cx);
                return;
            }
        };
        let keybindings = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .keybindings
            .clone();
        self.run_then(
            async move {
                keybindings
                    .remove_keybinding_rule(&rule)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
            |result, ui, cx| {
                if let Err(message) = result {
                    ui.report("Unable to save keybinding", &message, cx);
                }
            },
            cx,
        );
    }

    fn refresh_providers(&self, cx: &mut App) {
        self.inner
            .lock()
            .expect("live settings state")
            .providers_refreshing = true;
        self.publish(cx);
        let inner = self.inner.clone();
        let services = inner.lock().expect("live settings state").services.clone();
        self.run_then(
            async move {
                let providers = services.provider_registry.refresh(None).await;
                let mut state = inner.lock().expect("live settings state");
                state.providers_refreshing = false;
                state.providers = Arc::new(providers);
            },
            |(), _, _| {},
            cx,
        );
    }

    fn rescan_source_control(&self, cx: &mut App) {
        let inner = self.inner.clone();
        let source_control = inner
            .lock()
            .expect("live settings state")
            .services
            .source_control
            .clone();
        self.run_then(
            async move {
                let discovery = source_control.discover().await;
                inner.lock().expect("live settings state").source_control = discovery;
            },
            |(), _, _| {},
            cx,
        );
    }

    fn update_provider(&self, instance_id: &str, target_version: Option<String>, cx: &mut App) {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            self.report("Could not update provider", "Invalid instance id.", cx);
            return;
        };
        let (maintenance, providers) = {
            let state = self.inner.lock().expect("live settings state");
            (
                state.services.provider_maintenance.clone(),
                state.providers.clone(),
            )
        };
        self.run_then(
            async move {
                let provider = providers
                    .iter()
                    .find(|entry| entry.instance_id.as_str() == instance_id.as_str())
                    .map(|entry| entry.driver.clone())
                    .ok_or_else(|| "That provider instance no longer exists.".to_owned())?;
                maintenance
                    .update_provider(ProviderUpdateTarget {
                        provider,
                        instance_id: Some(instance_id),
                        target_version,
                    })
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
            |result, ui, cx| {
                if let Err(message) = result {
                    ui.report("Could not update provider", &message, cx);
                }
            },
            cx,
        );
    }

    fn start_provider_auth(
        &self,
        instance_id: &str,
        method_id: Option<&str>,
        cx: &mut App,
    ) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let input = ProviderAuthStartInput {
            instance_id,
            method_id: method_id
                .and_then(|id| turin_contracts::provider_setup::SetupOperationId::new(id).ok()),
            return_url: None,
            callback_mode: Some(ProviderAuthCallbackMode::Client),
        };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let auth = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_auth
            .clone();
        let result = handle.block_on(async move {
            auth.start(&input, SETTINGS_AUTH_OWNER_SESSION)
                .await
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(state) => {
                self.inner
                    .lock()
                    .expect("live settings state")
                    .provider_auth
                    .insert(state.instance_id.as_str().to_owned(), state);
                self.publish(cx);
                Ok(())
            }
            Err(message) => Err(message),
        }
    }

    fn cancel_provider_auth(&self, instance_id: &str, flow_id: &str, cx: &mut App) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let auth = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_auth
            .clone();
        let result = handle.block_on(async move {
            auth.cancel(&instance_id, flow_id, SETTINGS_AUTH_OWNER_SESSION)
                .await
                .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_auth
                .insert(state.instance_id.as_str().to_owned(), state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn respond_provider_auth(&self, input: ProviderAuthRespondInput, cx: &mut App) -> SetupResult {
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let auth = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_auth
            .clone();
        let result = handle.block_on(async move {
            auth.respond(input, SETTINGS_AUTH_OWNER_SESSION)
                .await
                .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_auth
                .insert(state.instance_id.as_str().to_owned(), state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn complete_provider_auth(
        &self,
        instance_id: &str,
        flow_id: &str,
        callback_url: &str,
        cx: &mut App,
    ) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let auth = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_auth
            .clone();
        let result = handle.block_on(async move {
            auth.complete(
                &instance_id,
                flow_id,
                callback_url,
                SETTINGS_AUTH_OWNER_SESSION,
            )
            .await
            .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_auth
                .insert(state.instance_id.as_str().to_owned(), state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn open_external_url(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }

    fn logout_provider(&self, instance_id: &str, cx: &mut App) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let auth = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_auth
            .clone();
        let result = handle.block_on(async move {
            auth.logout(&instance_id)
                .await
                .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_auth
                .insert(state.instance_id.as_str().to_owned(), state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn start_provider_install(&self, instance_id: &str, cx: &mut App) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let key = instance_id.as_str().to_owned();
        let input = ProviderSetupInput { instance_id };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let installation = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_installation
            .clone();
        let result = handle.block_on(async move {
            installation
                .start(&input)
                .await
                .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_install
                .insert(key, state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn cancel_provider_install(
        &self,
        instance_id: &str,
        operation_id: &str,
        cx: &mut App,
    ) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let key = instance_id.as_str().to_owned();
        let operation_id = turin_contracts::provider_setup::SetupOperationId::new(operation_id)
            .map_err(|error| error.to_string())?;
        let input = ProviderInstallCancelInput {
            instance_id,
            operation_id,
        };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let installation = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_installation
            .clone();
        let result = handle.block_on(async move {
            installation
                .cancel(&input)
                .await
                .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_install
                .insert(key, state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn remove_provider_installation(&self, instance_id: &str, cx: &mut App) -> SetupResult {
        let Ok(instance_id) = parse_instance_id(instance_id) else {
            return Err("Invalid provider instance.".into());
        };
        let key = instance_id.as_str().to_owned();
        let input = ProviderSetupInput { instance_id };
        let inner = self.inner.clone();
        let handle = inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .handle()
            .clone();
        let installation = inner
            .lock()
            .expect("live settings state")
            .services
            .provider_installation
            .clone();
        let result = handle.block_on(async move {
            installation
                .remove(&input)
                .await
                .map_err(|error| error.to_string())
        });
        if let Ok(ref state) = result {
            self.inner
                .lock()
                .expect("live settings state")
                .provider_install
                .insert(key, state.clone());
            self.publish(cx);
        }
        result.map(|_| ())
    }

    fn acknowledge_chat_gpt_welcome(&self, key: &str, cx: &mut App) {
        let state = &mut self.inner.lock().expect("live settings state");
        if !state
            .chat_gpt_welcome_acknowledged
            .iter()
            .any(|entry| entry == key)
        {
            state.chat_gpt_welcome_acknowledged.push(key.to_owned());
        }
        self.publish(cx);
    }

    fn clear_provider_auth_delivery(&self, cx: &mut App) {
        self.inner
            .lock()
            .expect("live settings state")
            .pending_provider_auth_delivery = None;
        self.publish(cx);
    }

    fn set_theme(&self, theme: String, cx: &mut App) {
        self.inner
            .lock()
            .expect("live settings state")
            .appearance
            .theme = theme.clone();
        set_runtime_theme(&theme, cx);
        self.publish(cx);
    }

    fn set_appearance_mode(&self, mode: String, cx: &mut App) {
        {
            let state = &mut self.inner.lock().expect("live settings state");
            if matches!(state.appearance.theme.as_str(), "system" | "light" | "dark") {
                state.appearance.theme = mode.clone();
            }
            state.appearance.mode = mode.clone();
        }
        set_runtime_appearance_mode(&mode, cx);
        self.publish(cx);
    }

    fn open_keybindings_file(&self, cx: &mut App) {
        let path = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .keybindings
            .config_path()
            .display()
            .to_string();
        cx.open_url(&format!("file://{path}"));
    }

    fn unarchive_thread(&self, thread_id: &str, cx: &mut App) {
        let Ok(thread_id) = ThreadId::new(thread_id) else {
            self.report("Could not unarchive thread", "Invalid thread id.", cx);
            return;
        };
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        let task = bus.task(cx, move |commands| async move {
            commands
                .thread_command("thread.unarchive", &thread_id, serde_json::json!({}))
                .await
        });
        self.report_failure_of(task, "Failed to unarchive thread", cx);
        self.publish(cx);
    }

    fn rename_project(&self, project_id: &str, title: &str, cx: &mut App) {
        let Ok(project_id) = ProjectId::new(project_id) else {
            self.report("Could not rename project", "Invalid project id.", cx);
            return;
        };
        let title = title.to_owned();
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        let task = bus.task(cx, move |commands| async move {
            commands.rename_project(&project_id, &title).await
        });
        self.report_failure_of(task, "Could not rename project", cx);
        self.publish(cx);
    }

    fn remove_project(&self, project_id: &str, cx: &mut App) {
        let Ok(project_id) = ProjectId::new(project_id) else {
            self.report("Could not remove project", "Invalid project id.", cx);
            return;
        };
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        // `deleteProject` with `force: true`: the project's threads go first.
        let task = bus.task(cx, move |commands| async move {
            commands.delete_project(&project_id).await
        });
        let ui = self.ui();
        cx.spawn(async move |cx| {
            let outcome = task.await;
            cx.update(|cx| {
                if let Err(error) = outcome {
                    ui.report("Could not remove project", &error.message, cx);
                }
                ui.publish(cx);
            });
        })
        .detach();
        self.publish(cx);
    }

    fn set_project_icon(
        &self,
        project_ids: &[String],
        favicon_path: Option<String>,
        project_icon: Option<crate::state::models::ProjectIcon>,
        cx: &mut App,
    ) -> SetupResult {
        let ids: Result<Vec<ProjectId>, _> = project_ids
            .iter()
            .map(|id| ProjectId::new(id.as_str()))
            .collect();
        let ids = ids.map_err(|_| "Invalid project id.".to_string())?;
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        bus.run_blocking(move |commands| async move {
            set_project_icons(
                commands.runtime().as_ref(),
                &ids,
                favicon_path,
                project_icon,
            )
            .await
        })
        .map_err(|error| error.message)?;
        self.publish(cx);
        Ok(())
    }

    fn search_project_files(
        &self,
        cwd: &str,
        query: &str,
        _cx: &mut App,
    ) -> Result<Vec<super::project_file_picker_logic::ProjectEntry>, String> {
        use super::project_file_picker_logic::{ProjectEntry, ProjectEntryKind};
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        let cwd = cwd.to_owned();
        let query = query.to_owned();
        let paths = bus
            .run_blocking(move |commands| async move {
                commands.search_image_files(&cwd, &query, 40).await
            })
            .map_err(|error| error.message)?;
        Ok(paths
            .into_iter()
            .map(|(path, is_dir)| ProjectEntry {
                kind: if is_dir {
                    ProjectEntryKind::Directory
                } else {
                    ProjectEntryKind::File
                },
                path,
            })
            .collect())
    }

    fn pick_project_favicon(
        &self,
        _cwd: &str,
        cx: &mut App,
    ) -> gpui_kit::Task<Result<Option<String>, String>> {
        let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |_| match picked.await {
            Ok(Ok(paths)) => {
                classify_picked_favicon(paths.and_then(|paths| paths.into_iter().next()))
            }
            Ok(Err(error)) => Err(error.to_string()),
            // The platform dropped the dialog without answering: same as cancel.
            Err(_) => Ok(None),
        })
    }

    fn run_storage_cleanup(&self, cx: &mut App) {
        let inner = self.inner.clone();
        let cleanup = inner
            .lock()
            .expect("live settings state")
            .services
            .storage_cleanup
            .clone();
        self.run_then(
            async move {
                let report = cleanup.run_now().await;
                inner.lock().expect("live settings state").storage_cleanup =
                    Some(cleanup_report(&report));
            },
            |(), _, _| {},
            cx,
        );
    }

    fn delete_thread(&self, thread_id: &str, cx: &mut App) {
        let Ok(thread_id) = ThreadId::new(thread_id) else {
            self.report("Could not delete thread", "Invalid thread id.", cx);
            return;
        };
        let bus = self
            .inner
            .lock()
            .expect("live settings state")
            .services
            .bus
            .clone();
        let task = bus.task(cx, move |commands| async move {
            commands
                .thread_command("thread.delete", &thread_id, serde_json::json!({}))
                .await
        });
        self.report_failure_of(task, "Failed to delete thread", cx);
        self.publish(cx);
    }
}

/// The native picker has no extension filter here (`PathPromptOptions`), so the
/// `PROJECT_FAVICON_EXTENSIONS` check upstream's dialog filter does runs on the
/// answer instead.
fn classify_picked_favicon(path: Option<std::path::PathBuf>) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let path = path.to_string_lossy().into_owned();
    if turin_shared::project_favicon::is_project_favicon_path(&path) {
        Ok(Some(path))
    } else {
        Err("Choose an image file (png, jpg, svg, ico, icns, gif, webp or avif).".to_owned())
    }
}

#[cfg(test)]
mod favicon_pick_tests {
    use super::classify_picked_favicon;
    use std::path::PathBuf;

    #[test]
    fn cancel_is_not_an_error() {
        assert_eq!(classify_picked_favicon(None), Ok(None));
    }

    #[test]
    fn image_paths_are_kept_verbatim() {
        assert_eq!(
            classify_picked_favicon(Some(PathBuf::from("/Users/me/logo.PNG"))),
            Ok(Some("/Users/me/logo.PNG".to_owned()))
        );
        assert!(classify_picked_favicon(Some(PathBuf::from("/a/app.icns"))).is_ok());
    }

    #[test]
    fn non_images_report_an_error() {
        assert!(classify_picked_favicon(Some(PathBuf::from("/a/notes.txt"))).is_err());
    }
}
