# WIRE-runtime API contract

Worktree: `turin-wt/WIRE-runtime`, branch `task/WIRE-runtime`.

Available immediately: `Commands::runtime(&self) -> Arc<WriteRuntime>` (owned clone).
All backend calls run on Tokio; never block GPUI renders.

Public `WriteRuntime` services:
- `provider_auth: Arc<ProviderAuthService>`; `start/respond/complete/cancel/logout/subscribe`
  use existing `turin_providers::provider_auth_service` signatures. Use one stable
  owner session ID for each settings UI's auth flow. Native composition now enforces
  **Server callback mode** via `ProviderAuthService::with_callback_mode(Server)`;
  this overrides the settings adapter's old Client request. Codex owns a real
  `127.0.0.1:0` listener bound before its browser interaction is published, validates
  state/PKCE, exchanges and stores tokens, delivers the real completion page, and
  drops the listener on completion/cancel. No app-wide server or T3Connect is used.
- `provider_installation: Arc<ProviderInstallation>`; existing provider setup/install
  APIs. Shared Codex managed installation used by actual driver, not a second installer.
- `server_settings: Arc<ServerSettingsService>`; use Commands facade for patches so
  persisted changes feed live provider reconciliation. Never persist via desktop handle.
- `source_control: Arc<SourceControlProviderRegistry>`, real GitHub driver.
- `pull_requests: Arc<PullRequestService>` and `review_guides: Arc<PullRequestReviewGuideService>` (top-level fields, confirmed directly to PR Sol).
- `source_control_discovery: Arc<SourceControlDiscovery>`, `keybindings: Arc<Keybindings>`,
  `provider_maintenance: Arc<ProviderMaintenanceRunner>`. Keybindings watcher retained.
- `source_control_host: Arc<ServerSourceControlHost>`.
- `git: Arc<GitVcsDriverCore>`; branch/status/switch methods are existing async APIs.
- `git_manager: Arc<GitManager>`; real source-control-backed PR host.
- `usage: Arc<UsageService>`; real provider usage readers.

Commands facade methods:
- `server_settings() -> Result<ServerSettings, CommandError>`
- `update_server_settings(ServerSettingsPatch) -> Result<ServerSettings, CommandError>`
- `update_provider_instance(ProviderInstanceMutation) -> Result<ServerSettings, CommandError>`
- `git_status(&str) -> Result<VcsStatusResult, CommandError>`
- `git_branches(&str) -> Result<Vec<String>, CommandError>`,
  `git_list_refs(&VcsListRefsInput) -> Result<VcsListRefsResult, CommandError>`,
  `git_switch_branch(VcsSwitchRefInput) -> Result<VcsSwitchRefResult, CommandError>`
- `usage_summary(&UsageSummaryInput) -> Result<UsageSummary, CommandError>`
- `pending_approval_count() -> Result<usize, CommandError>` (approval requests on non-deleted/non-archived threads).

Draft undo exact seam: runtime does not own the UI's `ComposerDraftStore`.
`composer::discard_composer_draft::DiscardableDraftStore` has no production
implementation (only tests). UI owner must implement it for its shared draft/session
store, retain `DiscardedDrafts<St::Session>` alongside notices, and execute
`restore_discarded_draft(&mut store, &discarded, &releaser, discard_id)` from
`UndoPlan::RestoreDraft`. The same store/registry/releaser must back discard and undo;
runtime cannot restore into another copy of the composer store.

No cargo, tests, clippy run here: parent owns integrated build. Rustfmt and `git diff --check` only; parent owns compile validation.

## Available backend limitations

- **Cursor history is now real**: `write/usage.rs` adapts existing
  `CursorUsageAccounts` + `CursorAccountReader` + real `ReqwestDashboard` and
  `CursorKeychain::layer` into `ProviderUsageReader::Scan`. It honors saved
  `cursorKeychainUsageEnabled`, file credentials, source errors, account cache and
  `awaitRefresh`. No synthetic parser or totals. Only Cursor history is available;
  Claude/Codex/OpenCode have no concrete history reader. UI must label that scope.
- `Commands::usage_readiness() -> UsageReadiness` supplies `history_driver_kinds`
  (currently `["cursor"]`) and `unavailable_history_driver_kinds` for that label.
- `Commands::provider_snapshots() -> Vec<ServerProvider>` is a synchronous real
  snapshot for limits/readiness (also streamed by `CommandBus::providers`).
- `Commands::refresh_usage_limits().await -> Vec<ServerProvider>` probes the actual
  providers; consume each snapshot's `usage_limits`, auth/install/availability state.
- `Commands::can_consume_reset_credit(&ProviderInstanceId) -> bool` controls whether
  redemption is available. `consume_reset_credit(ProviderConsumeResetCreditInput)`
  calls the actual instance hook and refreshes readiness; unsupported instances and
  external sources return explicit errors. No cross-server source is introduced.
- Settings persist with ServerSettingsService (secret store + file watcher), then publish
  through the DesktopSettingsHandle consumed by provider hydration. External edits feed
  the same bridge. Settings facade signatures match WIRE-settings/live_commands.rs.
- New data directories seed supplied startup settings; existing settings use real service
  migration/normalization/materialization. `TURIN_CLAUDE_BINARY` remains process-local.
- Managed Codex installer shares the driver's actual installer/lease state; a retained
  subscription refreshes real provider snapshots when installation settles.
- Review-guide generation uses actual TextGeneration provider adapter and reads current
  settings each request. GitHub auth uses real `gh`/environment/settings credentials.
- `storage_cleanup: Arc<StorageCleanup>` supports real artifact/log cleanup on demand and
  hourly. Worktree deletion explicitly reports unavailable when policies request it:
  live workspace lease, session, deleted-thread and PR snapshot ports are not complete.
  `StorageCleanup::files_only` avoids interpreting empty fixtures as live state.
- `config: ServerConfig` exposes actual settings/keybindings/DB paths; `worktrees_dir`
  matches review-guide disposable worktrees. Shell builds EnvironmentInfo from this.
- Settings bundle mapping: `source_control = rt.source_control_discovery.clone()`,
  `provider_registry = rt.providers.registry.clone()`, other same-named fields clone
  directly. Shell owns EnvironmentInfo/platform/client-settings initialization.
- Settings facade waits for explicit instance reconciliation before returning (the
  DesktopSettingsHandle watcher still handles external settings changes).

## Native settings invocation (follow-up)

```rust
let input = ProviderAuthStartInput {
    instance_id,
    method_id,
    return_url: None,
    callback_mode: Some(ProviderAuthCallbackMode::Server), // contracts/wire enum
};
let states = runtime.provider_auth.subscribe(&input.instance_id, SETTINGS_AUTH_OWNER_SESSION);
let state = runtime.provider_auth.start(&input, SETTINGS_AUTH_OWNER_SESSION).await?;
```

Run the start and stream on the bus's Tokio runtime. Keep the same owner for all
operations and open the published Browser interaction URL using the existing settings
Open browser action. Server mode finishes via its local callback without a manual
completion call. Existing `complete(instance, flow, callback_url, owner)` remains a
validated manual fallback for a pasted redirect URL; do not fabricate success on open.
The runtime host policy also makes the current settings Client invocation work unchanged.
Cursor's existing explicit CLI/ACP login ignores callback mode and refreshes its snapshot.
Claude/OpenCode have no runtime auth controller; respect real setup capabilities and
report that sign-in must be configured through their CLI/environment.

Parent/integrator: wire `LiveUsageBackend::limits_environments` to cached provider
snapshots instead of its current empty Vec, `refresh_limits` to the facade, and render
history scope from `usage_readiness`. Keychain opt-in already uses the persistent patch.
UI remains outside this worker's ownership.
