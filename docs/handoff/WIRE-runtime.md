# WIRE-runtime API contract

Worktree: `turin-wt/WIRE-runtime`, branch `task/WIRE-runtime`.

Available immediately: `Commands::runtime(&self) -> Arc<WriteRuntime>` (owned clone).
All backend calls run on Tokio; never block GPUI renders.

Incoming public `WriteRuntime` services (same names will be retained):
- `provider_auth: Arc<ProviderAuthService>`; `start/respond/complete/cancel/logout/stream`
  use existing `turin_providers::provider_auth_service` signatures. Use one stable
  owner session ID for each settings UI's auth flow; explicit Client callback mode
  where supported. No HTTP callback listener is supplied by this runtime.
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

Incoming Commands facade methods:
- `server_settings() -> Result<ServerSettings, CommandError>`
- `update_server_settings(ServerSettingsPatch) -> Result<ServerSettings, CommandError>`
- `update_provider_instance(ProviderInstanceMutation) -> Result<ServerSettings, CommandError>`
- `git_status(&str) -> Result<VcsStatusResult, CommandError>`
- `git_branches(&str) -> Result<Vec<String>, CommandError>`,
  `git_list_refs(&VcsListRefsInput) -> Result<VcsListRefsResult, CommandError>`,
  `git_switch_branch(VcsSwitchRefInput) -> Result<VcsSwitchRefResult, CommandError>`
- `usage_summary(&UsageSummaryInput) -> Result<UsageSummary, CommandError>`
- `pending_approval_count() -> Result<usize, CommandError>` (approval requests on non-deleted/non-archived threads).

Draft undo: runtime does not own composer DraftStore. Existing seam is UI-owned
`composer::draft_store::DraftStore` plus `discard_composer_draft`; shell/composer owner
must retain and restore discarded draft there. Runtime must not invent another store.

No cargo, tests, clippy run here: parent owns integrated build. Final names/types and
limitations will be updated before completion.

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
- Storage cleanup is being added; worktree deletion requires missing live lease/session
  seam and will report unavailable; artifact/log cleanup will use real filesystem.
