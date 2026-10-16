//! Live settings window entry: build [`LiveSettingsServices`] from [`WriteRuntime`]
//! (see `settings/live_bindings.rs`) and open via [`settings::live_window`].

use std::sync::Arc;

use gpui_kit::{App, Window, WindowOptions};

use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::Commands;
use crate::app_runtime::write::runtime::WriteRuntime;
use crate::settings::live_bindings::LiveSettingsServices;
use crate::settings::live_window;
use crate::settings::search::{SETTINGS_SEARCH_ITEMS, SettingsSearchItem};
use crate::settings::view::SettingsRoute;
use crate::settings::view_model::EnvironmentInfo;
use crate::state::store::ClientStore;
use turin_shared::keybindings::is_mac_platform;

/// Maps `WriteRuntime` per `settings/live_bindings.rs`.
pub fn build_live_settings_services(
    bus: CommandBus,
    commands: &Commands,
    cx: &App,
) -> Option<LiveSettingsServices> {
    let rt = commands.runtime();
    Some(LiveSettingsServices {
        bus,
        server_settings: Arc::clone(&rt.server_settings),
        keybindings: Arc::clone(&rt.keybindings),
        provider_auth: Arc::clone(&rt.provider_auth),
        provider_installation: Arc::clone(&rt.provider_installation),
        source_control: Arc::clone(&rt.source_control_discovery),
        storage_cleanup: Arc::clone(&rt.storage_cleanup),
        provider_registry: Arc::clone(&rt.providers.registry),
        provider_maintenance: Arc::clone(&rt.provider_maintenance),
        environment: environment_info(&rt),
        platform: settings_platform(cx),
    })
}

fn settings_platform(_cx: &App) -> String {
    if is_mac_platform() {
        "MacIntel".into()
    } else {
        "Linux x86_64".into()
    }
}

fn environment_info(rt: &WriteRuntime) -> EnvironmentInfo {
    let paths = &rt.providers.host.paths;
    let worktrees_dir = turin_runtime::config::derive_server_paths(
        std::path::Path::new(&paths.base_dir),
        None,
        turin_runtime::config::DeriveServerPathsOptions::default(),
    )
    .worktrees_dir
    .to_string_lossy()
    .into_owned();
    EnvironmentInfo {
        label: "This device".into(),
        platform_os: if cfg!(target_os = "macos") {
            "darwin".into()
        } else {
            "linux".into()
        },
        app_version: env!("CARGO_PKG_VERSION").into(),
        home_dir: std::env::var("HOME").unwrap_or_default(),
        worktrees_dir,
    }
}

/// Opens the settings window when live services and the command bus are ready.
pub fn open_settings_route(
    live: &LiveShellHandle,
    route: SettingsRoute,
    options: WindowOptions,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(services) = live.try_services(cx) else {
        window.push_notification(
            gpui_kit::component::notification::Notification::info(
                "Settings are not available yet.",
            ),
            cx,
        );
        return false;
    };
    live_window::open(
        services,
        live.client.clone(),
        route,
        options,
        cx,
    );
    true
}

pub fn open_settings_named(
    live: &LiveShellHandle,
    page: &str,
    options: WindowOptions,
    window: &mut Window,
    cx: &mut App,
) {
    let route = SettingsRoute::from_name(page).unwrap_or_else(|| {
        tracing::warn!(%page, "unknown settings page, showing General");
        SettingsRoute::from_name("general").expect("general route")
    });
    let _ = open_settings_route(live, route, options, window, cx);
}

pub fn open_setting_by_id(
    live: &LiveShellHandle,
    setting_id: &str,
    options: WindowOptions,
    window: &mut Window,
    cx: &mut App,
) {
    let route = SETTINGS_SEARCH_ITEMS
        .iter()
        .find(|item: &&SettingsSearchItem| item.id == setting_id)
        .map(|item| SettingsRoute::Section(item.to))
        .unwrap_or_else(|| {
            tracing::warn!(%setting_id, "unknown setting id, showing General");
            SettingsRoute::from_name("general").expect("general route")
        });
    let _ = open_settings_route(live, route, options, window, cx);
}

/// Handle for palette/sidebar routing (client store + bus + cached services).
#[derive(Clone)]
pub struct LiveShellHandle {
    pub client: gpui_kit::Entity<ClientStore>,
    pub bus: CommandBus,
    services: std::cell::OnceCell<LiveSettingsServices>,
}

impl LiveShellHandle {
    pub fn new(client: gpui_kit::Entity<ClientStore>, bus: CommandBus) -> Self {
        Self {
            client,
            bus,
            services: std::cell::OnceCell::new(),
        }
    }

    pub fn try_services(&self, cx: &App) -> Option<LiveSettingsServices> {
        if let Some(services) = self.services.get() {
            return Some(services.clone());
        }
        let commands = self.bus.commands().ok()?;
        let services = build_live_settings_services(self.bus.clone(), &commands, cx)?;
        let _ = self.services.set(services.clone());
        Some(services)
    }
}

/// Boot client-settings persistence (sync) and hydrate on the runtime executor.
pub fn start_live_client_settings(
    runtime: &tokio::runtime::Handle,
    persistence: Arc<dyn crate::settings_logic::use_settings::ClientSettingsPersistence>,
) {
    crate::settings::live_init::boot_client_settings(persistence);
    runtime.spawn(async {
        if let Err(error) = crate::settings::live_init::hydrate_client_settings().await {
            tracing::warn!(%error, "client settings hydration failed");
        }
    });
}
