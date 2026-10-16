# WIRE-settings handoff

Branch `task/WIRE-settings`. Live settings UI wiring (Step 1).

## Shell startup

```rust
use crate::settings::live_init::{boot_client_settings, hydrate_client_settings, initialize_live_settings};
use crate::settings_logic::use_settings::ClientSettingsPersistence;

// Once persistence exists (runtime):
tokio::spawn(async {
    initialize_live_settings(persistence).await?;
    Ok::<_, ClientSettingsHydrationError>(())
});

// Or split: boot_client_settings(persistence) before window; hydrate in background.
```

## Shell entry (settings window)

```rust
use crate::settings::live_window;
use crate::settings::live_bindings::LiveSettingsServices;

live_window::open_named(services, client, "general", window_options, cx);
```

## WIRE-runtime Sol: `LiveSettingsServices` sources

See the table in `crates/app/src/settings/live_bindings.rs`. Summary:

- `WriteRuntime`: `server_settings`, `provider_auth`, `provider_installation`, `source_control`, `source_control_host`, `providers.registry`, plus `StorageCleanup` + `Keybindings` started beside it.
- `CommandBus` with `Commands` installed (required for persisted server patches).
- `initialize_live_settings` before first settings open.

Server **writes** from the UI use `Commands::update_server_settings` /
`update_provider_instance` via `settings::live_commands` (not direct service patches).

Auth owner: `SETTINGS_AUTH_OWNER_SESSION` = `turin-settings-ui`.

## Persistence

| Area | Path |
|------|------|
| Server / provider instances / secrets | `Commands` facade → `settings.json` + reconciliation |
| Client prefs | `live_init` + `use_settings` store |
| Keybindings | `Keybindings` → `keybindings.json` |
| Projects / threads | `CommandBus` + `live_projects` |
| Providers | registry + auth + installation services |
| Source control | `SourceControlDiscovery` |
| Storage | `StorageCleanup` |
| Licenses | empty (T05a cut) |

## Modules

- `live_init.rs` — client settings boot/hydrate
- `live_commands.rs` — server settings Commands facade
- `live_source.rs` — `SettingsSource`
- `live_window.rs` — open helper
- `live_snapshot.rs`, `live_projects.rs`, `live_bindings.rs`
