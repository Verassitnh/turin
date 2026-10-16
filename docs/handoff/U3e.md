# U3e handoff: ChatView.logic batch 2 (QC-ready)

Branch `task/U3e-chatview-logic`, worktree `turin-wt/U3e-chatview-logic`. Commit trailer: `Model: composer-2.5 (cursor)`. Nothing pushed.

## Done

All batch-2 predicates from U3c's list are in `crates/app/src/components/chat_view_logic/`, plus pure ports the PM called out for QC:

- `get_started_thread_model_change_block_reason` (`model_change.rs`)
- `should_refocus_composer_on_window_focus` (`refocus.rs`, `FocusableElement` trait instead of DOM `Element`)
- `restore_plan_follow_up_composer` (`plan_follow_up.rs`, restore record + plain-text collapse until `composer-logic` segments exist)

Upstream test coverage now includes:

- Full `resolveComposerProviderSelection` / `deriveLockedProvider` matrix (`composer_logic_matrix.rs`, 20 tests)
- All `hasServerAcknowledgedLocalDispatch` cases (`dispatch.rs`, 6 tests)
- `deriveCommittedServerUserMessageIds` fixture (`dispatch.rs`, 1 test)
- `resolveComposerInteractionMode` four cases (`composer.rs`)
- Refocus, model-change block, plan follow-up restore tests

**83** tests in `cargo test -p turin chat_view_logic` (was 44). Full **`cargo test -p turin`**: **2843 passed**, 1 ignored (after merge with `task/U3c-chat-find`).

## Still not ported (Deviations in PORTING.md)

Browser/preview/proactive helpers, terminal mount/toast/blob URL helpers, attachment URL async helpers, atom `waitForStartedServerThread`, environment reconnect timers, and small command predicates deferred to orchestration/view wiring. See PORTING.md rows appended after the batch-2 entry.

## Gate

`scripts/check.sh -p turin` green at branch tip.
