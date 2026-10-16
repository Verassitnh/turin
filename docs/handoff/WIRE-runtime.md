# WIRE-runtime API contract

Worktree: `turin-wt/WIRE-runtime`, branch `task/WIRE-runtime`.

Available immediately: `Commands::runtime(&self) -> Arc<WriteRuntime>` (owned clone).
All backend calls run on Tokio; never block GPUI renders.

Public `WriteRuntime` services:
- `provider_auth: Arc<ProviderAuthService>`; `start/respond/complete/cancel/logout/subscribe`
  use existing `turin_providers::provider_auth_service` signatures. Use one stable
  owner session ID for each settings UI's auth flow; explicit Client callback mode
  where supported. IMPORTANT: native desktop auth should use Server callback mode;
  Codex's own auth controller spawns a loopback callback listener in Server mode.
  WIRE-settings currently forces Client mode, which needs a desktop receiver for
  callback URLs. Runtime supplies no such receiver. No phantom welcome URL remains.
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

- `usage_summary` returns an explicit unavailable error: every retained provider driver
  inherits `usage() -> None`; production transcript formats/readers are absent. Only test
  stand-ins exist. Runtime owns real UsageService and is ready for real driver readers;
  do not call its raw `read_summary` to bypass the facade's unavailable check.
- Settings persist with ServerSettingsService (secret store + file watcher), then publish
  through the DesktopSettingsHandle consumed by provider hydration. External edits feed
  the same bridge. Settings facade signatures match WIRE-settings/live_commands.rs.
- New data directories seed supplied startup settings; existing settings use real service
  migration/normalization/materialization. `TURIN_CLAUDE_BINARY` remains process-local.
- Managed Codex installer shares the driver's actual installer/lease state.
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
