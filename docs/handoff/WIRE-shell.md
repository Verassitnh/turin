# WIRE-shell integration contract

Worktree: `turin-wt/WIRE-shell`, branch `task/WIRE-shell`.

Parent build owns `cargo check`; this worktree documents factory calls for
integration.

## Runtime (`../WIRE-runtime/docs/handoff/WIRE-runtime.md`)

Shell reads `CommandBus` from `CommandBusGlobal` and `ClientStore` from
`ClientStoreGlobal`. Live mode is on unless `TURIN_MOCK_UI=1`.

| Need | Call |
|------|------|
| Shared services | `bus.commands()?.runtime()` → `Arc<WriteRuntime>` |
| Settings read/write | `commands.server_settings()`, `update_server_settings`, `update_provider_instance` |
| Git status / refs / switch | `commands.git_status(cwd)`, `git_branches`, `git_switch_branch` |
| Usage page | `commands.usage_summary(input)` via `shell/usage_commands.rs` |
| Dock badge (approvals) | Prefer `commands.pending_approval_count()`; fallback counts shell rows with `has_pending_approvals` / `has_pending_user_input` |
| Provider list (onboarding, sidebar) | `bus.providers()` watch |

Live adapters use concrete `WriteRuntime` fields (merged `task/WIRE-runtime`).

## Settings (WIRE-settings)

Startup (live app, not `TURIN_MOCK_UI=1`):

```rust
use crate::settings::live_init::{boot_client_settings, hydrate_client_settings, initialize_live_settings};
use crate::shell::client_settings_persistence::DesktopFileClientSettingsPersistence;
use crate::shell::live_settings::start_live_client_settings;

start_live_client_settings(runtime.handle(), DesktopFileClientSettingsPersistence::new(path));
```

Open window: `shell/live_settings::{open_settings_named, open_setting_by_id}` →
`settings::live_window`.

`build_live_settings_services` maps `WriteRuntime` per `settings/live_bindings.rs`
(`source_control_discovery` for discovery).

## Chat navigation (WIRE-chat)

Each layout frame:

```rust
if let Some(thread_id) = components::chat::thread_navigation::take_thread_open_request(cx) {
    // scoped_thread_key(local_environment_id(), &ThreadId::new(thread_id)?)
    sidebar.navigate_to_thread(&key, cx);
}
```

`LiveSource::open_thread` enqueues via `request_open_thread`.

## Onboarding (WIRE-onboarding)

Live app (`main.rs`, not mock UI):

```rust
LiveOnboarding::start(client_store, command_bus, data_dir, cx);
FirstRunGate::new(backend.clone(), backend, true, layout);
```

See `../WIRE-onboarding/docs/handoff/WIRE-onboarding.md`.

## Pull requests (WIRE-pr-ui + WIRE-pr-backend)

`pull_request::live_navigation::mount_route` from `AppSidebarLayout` with
`open_thread` navigating the sidebar.

## Composer draft undo

- Global `shell/composer_draft_host::ComposerDraftHost` (draft store + `DiscardedDrafts` + `ThreadUndoNotices`).
- `ChatComposer` reads/writes the shared store when the global is registered.
- Sidebar `SidebarThreadUndoNotice` + `UndoLatestThread` (`mod-z` in shell context) call `undo_latest`.
- Live sidebar `discard_draft` → `discard_composer_draft`.

Thread archive/settle undo still lives in `ThreadActions` (not shell-wired).

## Shell UI overlays

| Piece | Location |
|-------|----------|
| Stacked toasts | `ui::toast::toasts` on layout root |
| Anchored toasts | `ui::toast::anchored_toasts` on layout root |
| Keybindings issues | `shell/keybindings_warning::start` when live bindings exist |
| Right panel inline | `PreviewPanelShell` inline column + resize handle |
| Right panel ≤980px | `PreviewPanelShell` sheet overlay (`right_panel_presentation_for_width`) |

## Shell-owned live modules

| Module | Replaces |
|--------|----------|
| `components/usage/live_source.rs` | Mock usage when `live` bindings present |
| `shell/composer_draft_host.rs` | Local composer draft store for undo |
| `shell/usage_commands.rs` | `Commands::usage_summary` |
| `shell/keybindings_warning.rs` | Keybindings config issue toast |

## Deviations / limits

- Git live backends may still call `GitVcsDriver` directly until shell switches to `Commands::git_*` everywhere.
- `UndoLatestThread` only runs draft restore via `ComposerDraftHost`; thread lifecycle undo needs a shared `ThreadActions` host.
- Usage price writes patch global `ServerSettings` (`usage_price_overrides` / aliases); per-environment pricing in the UI is not modeled separately on desktop yet.
- Question-attachment upload on respond: staged in chat; full transmit is WIRE-chat / worker follow-up (`WIRE-chat.md`).
- Keybindings warning uses `open` / `xdg-open` for “Open keybindings.json” (no preferred-editor registry yet).
