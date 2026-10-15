# U3b handoff — app shell state (pure logic)

Branch `task/U3b-shell-state`, worktree `turin-wt/U3b-shell-state`, from `main`
(`7cab9c4`). Port of the shell stores and decision logic into
`crates/app/src/state/**` and `crates/app/src/components/**`. Nothing pushed.
Env: `export PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

## State at handoff
- Tree clean. Last commit `d64ad18`.
- `scripts/check.sh -p turin`: exit 0. `cargo test -p turin`: 1427 pass, 0 fail.
  Clippy `-D warnings` clean for the `turin` package.
- Upstream files are read-only. `PORTING.md` deviation rows are appended for every
  file below. TASKS.md is untouched.

## Done
| upstream | Rust | tests |
|---|---|---|
| `onboarding/firstRun.logic.ts` | `state/first_run.rs` | 39 (`firstRun.logic.test.ts`, all cases ported) |
| `fileContextMenu.ts` (+ `components/preview/fileExplorerLabel.ts` wording) | `state/file_context_menu.rs` | 15 (7 upstream cases, 8 for the capability and activation decisions upstream does not test) |
| `hooks/useLiveRefresh.ts` (decision half) | `state/live_refresh.rs` | 18 (upstream cases plus cadence state) |
| `sidebarProjectGrouping.ts` | `state/sidebar_project_grouping.rs` | 9. No upstream test file exists |
| `uiStateStore.ts` (state, mutators, persisted JSON) | `state/ui_state_store.rs` | 19 (`uiStateStore.test.ts`, plus an array-index sanitizer case) |
| `components/ProviderUpdateLaunchNotification.logic.ts` | `components/provider_update_launch_notification_logic.rs` + `_tests.rs` | 47 (`.logic.test.ts`, 1149 lines) |

Commits: `412692c` (first-run, file menu, live refresh, sidebar grouping),
`d64ad18` (ui state store), `f5ca1b9` (provider update). The first commit
holds four files, not one per file. Their `mod.rs` lines could not be split
without a non-compiling intermediate state.

Dependency: `crates/app/Cargo.toml` gains `turin-provider-core.workspace = true`
(provider update logic reads the real `ServerProvider`). `Cargo.lock` updated.

Seams for T20/T22: `live_refresh` keeps the DOM and timer glue out (the cadence
takes an epoch-ms clock). `ui_state_store` keeps localStorage I/O, the 500 ms
debouncer and the zustand wrapper out. `file_context_menu` leaves the popup,
the `openInEditor` call and the toast to the caller. `sidebar_project_grouping`
takes the environment-label lookup as a closure.

## Not done (in priority order)

### 1. `rightPanelStore.ts` (1156 lines) + `rightPanelStore.test.ts` (1242 lines)
- Zustand store with ordered surfaces per thread. Pure transitions:
  `updateThread`, `automaticUpdate`, `userAction`, `closeAction`, `upsertSurface`,
  `openProactive` revision guard, `reconcileBrowserSurfaces`, `reconcileFileSurfaces`,
  `moveSurface`, `close*Surfaces`, `setThreadPanelOpen`.
  `migratePersistedRightPanelState` takes persisted versions up to 14.
- Cut list applies: browser/preview, device, terminal are cut (PORTING.md cut
  list). Drop those surfaces and log each in Deviations. Keep `diff`,
  `pull-requests`, `pull-request:*`. Decide `file`/`attachment` surfaces: the
  file tree is cut, but attachments still appear in threads. Note the cut
  `openFile` path in `diff/diff_file_actions.rs`.
- Persistence (`t3code:right-panel-state:v2`) is not in Turin yet. Port the
  migration as a pure function. Wire it when the settings store lands.
- Depends on `closedViewStore.ts` (check for a Rust port first) and
  `rightPanelLayout` `ThreadPanelPresentation`.

### 2. `hooks/useThreadActions.ts` (1046 lines) + three test files (106 + 355 + 223)
- Mostly command dispatch: settle, unsettle, snooze, unsnooze, pin/unpin
  (with confirm), auto-settle, archive, delete, mark unread, undo.
- Pure decisions to port: the operate-scope gate, confirmation copy, undo
  window (`hooks/threadUndo.ts` plus its tests: check for a Rust port first),
  toast titles, the "thread already gone" delete rule.
- Needs a command seam trait (`ThreadCommandPort`) implemented by T22 over the
  orchestration commands. Do not port React or atom glue.

### 3. `hooks/useHandleNewThread.ts` (498 lines) + `useHandleNewThread.test.ts` (319 lines)
- Blocked on the composer draft store. `composerDraftStore.ts` is not ported
  (PORTING.md notes it). Also missing: `lib/chatThreadActions`
  (`resolveNewDraftStartFromOrigin`, `resolveNewThreadModelSelectionOverride`,
  `hasExplicitComposerModelSelection`) and `threadRoutes`.
- Extract the pure decisions first: reuse of an empty stored draft versus an
  invested draft, explicit workspace options that win outright, model and
  interaction-mode carry-over, and env-mode resolution (`resolveNewThreadEnvMode`
  is in `@t3tools/shared/projectSettings`, check for a port first).
- The stale-await bailouts depend on store and route snapshots. Model them as
  an explicit snapshot compared before and after the async read.

### 4. `hooks/useThreadActionMenu.ts` (377 lines) + `useThreadActionMenu.test.ts` (221 lines)
- Mostly glue. The pure parts: building the menu input from the thread shell
  and capability flags (`threadActionRequiresOperate`, `canSnooze`,
  `effectiveSnoozed`, `resolveSnoozePresets`), mapping each clicked id to a
  plan, the `snooze:` prefix parse, `new-thread-on-branch` options, the
  copy-path fallback, the regenerate-title guard, and the archive and delete
  failure toasts.
- `components/thread_action_menu_logic.rs` already ports the item builder.
  The sidebar's `handle_thread_menu` in `sidebar_actions.rs` duplicates some
  of this inline. Port the plan as a pure function and have both consumers use it.
- Settle and snooze in this hook do not navigate away (chat header). The
  sidebar does. Keep that difference explicit in the plan.

## Notes for the PM
- Run the gate on the turin package only. `scripts/check.sh -p turin` passed
  at HEAD.
- The first build of this worktree compiled all dependencies cold. Later
  incremental runs took about a minute.
- Model trailer on every commit: `Model: claude-haiku-5-5 (claudeAgent)`.
