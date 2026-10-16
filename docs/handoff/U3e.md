# U3e handoff: ChatView.logic batch 2

Branch `task/U3e-chatview-logic`, worktree `turin-wt/U3e-chatview-logic`, from `task/U3c-chat-find`. Commit trailer: `Model: composer-2.5 (cursor)`. Nothing pushed.

## Done (upstream → Rust)

Extended `crates/app/src/components/chat_view_logic/` (batch 1 stays in `mod.rs`):

| Upstream | Rust module | New `#[test]` |
|---|---|---|
| `threadHasStarted`, `threadShellHasStarted`, `deriveLockedProvider` | `thread.rs` | 4 |
| `rememberCheckoutIsRepo`, `recallCheckoutIsRepo` | `checkout_memory.rs` | 1 |
| `resolveDraftHeroState`, `shouldDockDraftHeroForSubmission`, `resolveDraftPromotionNavigationTarget`, `buildLocalDraftThread` | `draft.rs` | 5 |
| Held timeline helpers + `resolveThreadSwitchTimeline`, `threadKeysShareEnvironment`, `isPaintOnlyThreadTimeline` | `timeline_hold.rs` | 5 |
| `timelineHasEphemeralPreviewUrls`, `collectUserMessageBlobPreviewUrls` | `preview_urls.rs` | 2 |
| `findRecordedWorktreeSetup`, `resolveVisibleWorktreeSetup`, `resolveWorktreeSetupProgress`, `worktreeSetupAgentStarted` | `worktree.rs` | 2 |
| `derivePhase`, `createLocalDispatchSnapshot`, `deriveCommittedServerUserMessageIds`, `hasServerAcknowledgedLocalDispatch` | `dispatch.rs` | 0 |
| `deriveComposerSendState` | `terminal_send.rs` | 3 |
| `shouldReleaseTimelineAnchorForToolActivity` | `anchor.rs` | 2 |
| `resolveComposerProviderSelection`, `resolveComposerInteractionMode`, `getAntigravitySendBlockReason`, `resolveSelectableProviderInstanceEntry` | `composer.rs`, `provider_instances.rs` | 2 |
| `waitForRevertedMessage` | `revert.rs` | 4 |

30 new tests in batch 2 (44 total in `chat_view_logic`). `cargo test -p turin`: **1619 passed**.

## Not done / partial

- **`hasServerAcknowledgedLocalDispatch` / `deriveCommittedServerUserMessageIds`**: logic ported; upstream’s large `describe` blocks not yet mirrored (add next).
- **`resolveComposerProviderSelection` / `deriveLockedProvider`**: core selection ported; many upstream `it.each` catalog cases still to copy (Antigravity continuation groups, imported history matrix).
- **Cut (per U3c / PORTING)**: proactive preview/PR helpers, `reconcileMountedTerminalThreadIds`, `buildExpiredTerminalContextToastCopy`, blob URL helpers, `shouldRefocusComposerOnWindowFocus`, `restorePlanFollowUpComposer`, `getStartedThreadModelChangeBlockReason`, `scheduleEnvironmentReconnectWarning`, and other DOM/React-only exports in `ChatView.logic.ts`.
- **Call sites**: still `#[allow(dead_code)]` on `components/chat_view_logic` until the chat view imports these modules.

## Gate

`scripts/check.sh -p turin` green at branch tip.

## Notes

- Worktree snapshots live in `worktree.rs` as faithful structs; swap for `turin_contracts` types when T01 grows `WorktreeSetupSnapshot`.
- `wait_for_reverted_message` expects the caller to supply projection reads (tests use `Arc<Mutex<…>>`); no `appAtomRegistry` port in this batch.
