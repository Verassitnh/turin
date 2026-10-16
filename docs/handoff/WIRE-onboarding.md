# WIRE-onboarding API contract

Worktree: `turin-wt/WIRE-onboarding`, branch `task/WIRE-onboarding`.

## Factory

```rust
use std::path::PathBuf;

use gpui_kit::{App, Entity};
use crate::components::onboarding::live_source::LiveOnboarding;
use crate::state::store::ClientStore;
use crate::app_runtime::write::bus::CommandBus;

let backend = LiveOnboarding::start(
    client_store: Entity<ClientStore>,
    bus: CommandBus,
    data_dir: PathBuf, // `app_runtime::data_dir::resolve_data_dir()` in the shell
    cx: &mut App,
);
// Pass `backend.clone()` to `FirstRunGate::new(first, backend, enabled, layout)`.
```

Shell Composer (WIRE-shell) replaces `MockOnboardingBackend` with this type; do not change `main.rs` or `app_sidebar_layout.rs` in this slice.

## Traits

`LiveOnboardingBackend` implements `FirstRunBackend` + `OnboardingBackend` (`components/onboarding/backend.rs`).

| Seam | Live source |
|------|-------------|
| First-run evidence | Hydrated client settings (`onboarding_completed_at`), `ClientStore` shell snapshot, command-bus readiness |
| Complete onboarding | `persist_client_settings_patch` with ISO `onboardingCompletedAt` |
| Providers | `CommandBus::providers()` watch + `ProviderRegistry::refresh` on reprobe |
| Create project | `Commands::create_project_with_id` (stable id for wizard retries) |
| Scan / import agent sessions | `AgentSessionDiscovery` + `AgentSessionImporter` over `Commands::runtime()` |
| T3 adopt | `T3Import::import_into` only when `~/.t3/userdata/statev2.sqlite` exists and Turin has no DB yet; UI button only |

## Runtime dependency

Agent session and provider refresh call `Commands::runtime()` (see `../WIRE-runtime/docs/handoff/WIRE-runtime.md`). Client settings persistence must be installed before the gate reads real hydration (shell `start_live_client_settings`).

## Limitations

- No native folder picker on the onboarding backend; project paths come from scan candidates or future shell wiring (`cx.prompt_for_paths` lives on the command palette today).
- `reload` is a no-op; recovery uses `retry_settings` (re-hydrate client settings).
- T3 import is offered only when Turin’s database file is absent; existing installs never see the card.
- Provider reprobe is single-flighted; concurrent refreshes coalesce to one in-flight registry refresh.
- Agent import uses a minimal `ProjectServiceApi` (read-by-id only); full project service wiring stays in N2/orchestration slices.
