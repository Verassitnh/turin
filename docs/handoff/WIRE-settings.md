# WIRE-settings handoff

Branch `task/WIRE-settings`. Live settings UI wiring (Step 1).

## Shell entry

```rust
use crate::settings::live_window;
use crate::settings::live_bindings::LiveSettingsServices;

live_window::open_named(services, client, "general", window_options, cx);
// or live_window::open(services, client, route, window_options, cx);
```

Build `LiveSettingsServices` once the runtime exposes the fields in
`docs/handoff/WIRE-runtime.md` (`ServerSettingsService`, `ProviderAuthService`,
`ProviderInstallation`, `SourceControlDiscovery`, `StorageCleanup`,
`ProviderRegistry`, `ProviderMaintenanceRunner`, plus `CommandBus` and
`EnvironmentInfo`).

Auth owner session: `live_bindings::SETTINGS_AUTH_OWNER_SESSION` (`turin-settings-ui`).

## Persistence

| Area | Path |
|------|------|
| Server / provider instances / secrets | `ServerSettingsService` → `settings.json` (use Commands facade when `update_server_settings` lands; until then service writes match upstream) |
| Client prefs | `settings_logic::use_settings` store (hydrate at app boot) |
| Keybindings | `Keybindings` → `keybindings.json` |
| Projects / threads | Orchestrator via `CommandBus` (`rename_project`, `thread.unarchive` / `thread.delete`; icon/delete via `live_projects`) |
| Providers | `ProviderRegistry` + `ProviderAuthService` + `ProviderInstallation` |
| Source control discovery | `SourceControlDiscovery::discover` |
| Storage cleanup | `StorageCleanup::run_now` |
| Licenses | Not ported (empty list; PORTING.md T05a) |

## Runtime still to wire

1. Construct `LiveSettingsServices` in `app_runtime` after `WriteRuntime::build` and pass into shell menu handlers.
2. Expose on `WriteRuntime`: fields listed in `WIRE-runtime.md` (currently contract-only in this branch).
3. Prefer `Commands::update_server_settings` / `update_provider_instance` for writes so provider reconciliation runs (replace direct `ServerSettingsService` calls in `live_source` when available).
4. Optional: `Commands::delete_project` / meta-update with icon — today `live_projects` commits via `WriteRuntime::stores`.

## Files

- `live_bindings.rs` — service bundle
- `live_source.rs` — `SettingsSource` impl + subscriptions
- `live_snapshot.rs` — `SettingsViewModel` builder
- `live_window.rs` — window open helper
- `live_projects.rs` — project delete / icon commits
