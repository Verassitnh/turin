# U3d handoff: thread actions, right-panel state, menu plan, new-thread decisions

Branch `task/U3d-thread-actions`, from `task/U3b-shell-state` (`9cd3f81`). Nothing pushed.
Gate: `scripts/check.sh -p turin` exit 0 at the final tree. `cargo test -p turin`: 1645 passed, 0 failed.
Clippy `-D warnings` clean for `turin`, including tests.

## Ported

| upstream | Rust | tests |
|---|---|---|
| `rightPanelStore.ts` (+ 1242-line test) | `state/right_panel_store.rs`, `_tests.rs` | 50 |
| `hooks/threadUndo.ts` (+ test) and `showThreadUndoNotice.ts` queue | `hooks/thread_undo.rs`, `_tests.rs` | 11 |
| `hooks/useThreadActions.ts` (+ `.test`, `.undo.test`, `.permissions.test`) | `hooks/use_thread_actions.rs`, `_tests.rs` | 42 |
| `hooks/useThreadActionMenu.ts` (+ test) | `hooks/use_thread_action_menu.rs` (plan) | 14 |
| `lib/chatThreadActions.ts` (+ test) | `hooks/chat_thread_actions.rs`, `_tests.rs` | 11 |
| `hooks/useHandleNewThread.ts` (pure decisions only) | `hooks/use_handle_new_thread.rs`, `_tests.rs` | 13 |
| `worktreeCleanup.ts` (orphan rule) | `state/worktree_cleanup.rs` | 6 |

Sidebar `handle_thread_menu` (`components/sidebar/sidebar_actions.rs`) dispatches on `ThreadMenuPlan`.
The inline gate, snooze-prefix parse and copy-path fallback were removed from it.

Counts are `#[test]` functions in the new files. Upstream `it.each` rows expand further.

## Seams

- `ThreadCommands` (`use_thread_actions.rs`): one method per orchestration command. T22 implements
  it. Methods are synchronous, so T22 runs `ThreadActions` on a background task.
- `ThreadActionHost`: store reads, capability flags, settings, route, confirm dialog, navigation,
  local-state clearing, toasts. T22 implements it.
- `ThreadUndo` and `ThreadUndoNotices`: shared handles, `Arc<Mutex<_>>`. Every owner must use the
  same pair. `expire()` replaces the 5 s timer. `current_notice()` replaces the zustand store.
- `RightPanelStore::to_persisted_value` / `load_persisted`: JSON shape for the settings task. The
  `{state, version}` envelope and the file I/O are the caller's.

## Decisions

- Operate-grant gate is applied by `ThreadActions::operate`, before `ThreadCommands`. Upstream
  applies it in the command wrapper. Every `ThreadCommands` implementation keeps the gate.
- Undo is `UndoPlan` data, not a closure. Running a plan goes through the same methods as the
  original action, so gates and claims apply again.
- Right-panel surfaces kept: `diff`, `pull-requests`, `pull-request:*`, `file`, `attachment`.
  Dropped: `preview`, `device`, `terminal`, `files`. The `file` tree is cut, but attachments still
  open in threads.
- Sidebar menu passes `can_operate: true`. Turin is one environment, and the process holds its grant.
- Chat header menu: the plan supports it (`ThreadMenuSurface::ChatHeader`), but no Rust chat-header
  menu caller exists yet. It is not wired.
- `useHandleNewThread` orchestration (store writes, router, awaited project-file read) is not ported.
  It depends on `composerDraftStore`, which is not ported. Its test file is store and router mocks, so
  it is not ported either.

## Upstream cases skipped

Skipped cases exercise only cut surfaces. Each is marked in the test file:

- `rightPanelStore.test.ts`: browser tab, device, terminal, `files` explorer, `reconcileBrowserSurfaces`.
  Some cases were adapted with a kept surface in place of the cut one. Those are commented in place.
- `useThreadActions.undo.test.ts`: none skipped.
- `showThreadUndoNotice.test.ts` and `useThreadActions.test.ts` have no browser or terminal cases.
  Their restore-result cases are ported in `use_thread_actions_tests.rs`: a failed restore toasts
  and releases its claim, a rejected restore toasts the error, and an interrupted restore does not
  toast. They are kept behavior, not cut-list skips.

## Not done

- `rename` and `fork` are not on `ThreadCommands`. Upstream's `useThreadActions` does not call them.
- Undo `commit` callbacks: no caller in the ported surface passes one.
- `useHandleNewThread` async handler: needs the composer draft store.

## Notes for the PM

- Commit trailer: task text said `Model: claude-haiku-5-5 (claude)`. `AGENTS.md` names the provider
  instance `claudeAgent`. The commits use the task's form. The hook accepts either.
- Gate ran once at the final tree. The eight commits were not each gated. Each commit only
  registers modules that it contains.
- `PORTING.md` Deviations rows: one per changed upstream file, appended before `## DEDUP`.
