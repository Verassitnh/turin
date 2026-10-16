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
| Usage page | `commands.usage_summary(input)` |
| Dock badge (approvals) | Prefer `commands.pending_approval_count()`; fallback counts shell rows with `has_pending_approvals` / `has_pending_user_input` |
| Provider list (onboarding, sidebar) | `bus.providers()` watch |

When `WriteRuntime` exposes `git`, `usage`, `source_control`, etc., live
adapters should switch from interim `turin_git::GitVcsDriver` calls to the
Commands facade (same Tokio executor via `bus.task` / `bus.spawn`).

## Settings (WIRE-settings)

Expected entry: `settings::live_window::open(route, open_menu, window, cx, bus)`
(or equivalent from `docs/handoff/WIRE-settings.md` when published).

Shell routes palette/sidebar `OpenSettings`, `OpenProjectSettings`,
`OpenSetting { .. }`, `ToggleThemeEditor` through
`shell::settings_navigation` (overlay or secondary window — match settings
agent’s helper).

## Pull requests (WIRE-pr-ui + WIRE-pr-backend)

**Store**

```rust
pull_request::live_source::start(
    bus: CommandBus,
    client: ClientStateHandle,
    cx: &mut App,
) -> Entity<PullRequestStore>
```

Optional:

```rust
pull_request::live_source::start_with_navigation(
    bus,
    client,
    Arc<dyn Fn(&ThreadId, &mut App) + Send + Sync>,
    cx,
) -> Entity<PullRequestStore>
```

Requires `WriteRuntime.pull_requests: Arc<PullRequestService>` and
`.review_guides: Arc<PullRequestReviewGuideService>` (direct fields, not nested
under `source_control`).

Shell wiring (live app):

```rust
let host = pull_request::live_navigation::mount_route(
    bus,
    client.handle().clone(),
    Some(Arc::new(|thread_id, cx| { /* navigate sidebar to thread */ })),
    search,
    &LocalPreferenceStorage(pr_preferences),
    window,
    cx,
);
layout.set_main_content(Some(host.read(cx).page_view()), cx);
// PullRequestsRouteEvent::OpenPanel → set_right_panel_content(panel_view)
```

**Navigation host**

PR UI supplies `pull_request::live_navigation` to mount list page, detail
panel, and link dialog. Shell calls it once from `AppSidebarLayout` for:

- `SidebarEvent::OpenPullRequestsPage`
- `CommandPaletteEvent::OpenPullRequests`
- `CommandPaletteEvent::LinkPullRequest`
- `CommandPaletteEvent::ShowLinkedPullRequests` (right panel; may reuse thread panel host)

Pattern (from demo): `PullRequestsPage` main column +
`PullRequestPanelHost` right panel; list `Select` opens detail tab.

## Composer draft undo

Runtime does **not** own drafts. Shell/composer keep
`composer::draft_store::DraftStore` and `discard_composer_draft` /
`DiscardedDrafts` on the shell or chat host; no duplicate store in runtime.

## Shell-owned live modules

| Module | Replaces |
|--------|----------|
| `components/sidebar/live_source.rs` | Mock sidebar (wired) |
| `components/command_palette/live_source.rs` | Mock palette (wired) |
| `components/onboarding/live_source.rs` | Mock first-run |
| `components/git_actions_control/live_source.rs` | Mock git actions |
| `components/branch_toolbar/live_source.rs` | Mock branch toolbar |
| `components/usage/live_source.rs` | Mock usage (when runtime usage wired) |
| `shell/environment_coordinator.rs` | Dock badge from shell snapshot |
| `main.rs` | `ui::toast::init` for shell window |

## Deviations until merge

- Git live backends call `GitVcsDriver` directly until `Commands::git_*` lands on
  the merged runtime branch.
- Onboarding `complete_onboarding` writes `settings.json` until
  `update_server_settings` is available.
- PR list/detail routes wait for `pull_request::live_source` / `live_navigation`
  from WIRE-pr-ui.
