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
- `source_control_host: Arc<ServerSourceControlHost>`.
- `git: Arc<GitVcsDriverCore>`; branch/status/switch methods are existing async APIs.
- `git_manager: Arc<GitManager>`; real source-control-backed PR host.
- `usage: Arc<UsageService>`; real provider usage readers.

Incoming Commands facade methods:
- `server_settings() -> Result<ServerSettings, CommandError>`
- `update_server_settings(ServerSettingsPatch) -> Result<ServerSettings, CommandError>`
- `update_provider_instance(ProviderInstanceMutation) -> Result<ServerSettings, CommandError>`
- `git_status(&str) -> Result<GitStatusResult, CommandError>` (concrete result TBD)
- `git_branches(&str)`, `git_switch_branch(VcsSwitchRefInput)` (existing git contracts)
- `usage_summary(&UsageSummaryInput) -> Result<UsageSummary, CommandError>`
- `pending_approval_count() -> Result<usize, CommandError>` (all live requests).

Draft undo: runtime does not own composer DraftStore. Existing seam is UI-owned
`composer::draft_store::DraftStore` plus `discard_composer_draft`; shell/composer owner
must retain and restore discarded draft there. Runtime must not invent another store.

No cargo, tests, clippy run here: parent owns integrated build. Final names/types and
limitations will be updated before completion.
