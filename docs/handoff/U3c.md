# U3c handoff: chat find, embedded scripts, markdown clipboard, chat logic

Branch `task/U3c-chat-find`, worktree `turin-wt/U3c-chat-find`, from `task/U3b-shell-state`
(U3a merged in). Commit trailer: `Model: claude-haiku-5-5 (claude)`. Nothing pushed.

## Done (upstream → Rust)

| upstream | Rust | new tests |
|---|---|---|
| `components/chat/threadFind.ts` | `components/chat/thread_find.rs` | 3 |
| `components/chat/threadFindPosition.ts` (+ test) | `components/chat/thread_find_position.rs` | 4 (all 3 upstream cases, geometry as plain values; 1 added) |
| `components/chat/threadFindHighlights.ts` (pure half, `collectThreadFindRanges`, fold rules) | `components/chat/thread_find_highlights.rs` | 8 (2 upstream `collectThreadFindRanges` cases; 6 added) |
| `components/chat/useThreadFind.ts` (pure state) | `components/chat/thread_find_state.rs` | 12 (no upstream test file) |
| `components/chat/useThreadFindNavigation.ts` (pure rules) | `components/chat/thread_find_navigation.rs` | 9 (no upstream test file) |
| `lib/embeddedScripts.ts` (tree-sitter-bash replaces `unbash`) | `components/chat/embedded_scripts.rs` | 8 (every upstream `it`/`it.each` case) |
| `markdown-clipboard.ts` (over Turin's `Block` document) | `components/chat/markdown_clipboard.rs` | 18 (the upstream cases that map to the document; see below) |
| `components/chat/useAssistantCitationTarget.ts` (pure rules) | `components/chat/assistant_citation_target.rs` | 9 (no upstream test file) |
| `components/ChatView.logic.ts` (first batch) | `components/chat_view_logic.rs` | 14 (upstream cases for these functions) |

85 new `#[test]` functions. `cargo test -p turin`: 1589 passed, 0 failed.

## Not done

1. **`ChatView.logic.ts`, batch 2 onward.** Only the first batch is in `chat_view_logic.rs`:
   `resolveEffectiveInteractionMode`, `resolveSendEnvMode`, `resolveBackgroundDraftWorkspaceOptions`,
   `branchMismatchKey`, `shouldShowBranchMismatchBanner`, `shouldShowPlanFollowUpPrompt`, and the
   session dismissal. Still to port (upstream tests in `ChatView.logic.test.ts` not ported):
   `threadHasStarted` / `threadShellHasStarted`, `deriveLockedProvider`,
   `createLocalDispatchSnapshot` / `hasServerAcknowledgedLocalDispatch`,
   `resolveThreadSwitchTimeline` and the held-timeline helpers (generic over `T`),
   `resolveDraftHeroState`, `resolveWorktreeSetupProgress`, `shouldReleaseTimelineAnchorForToolActivity`,
   `resolveComposerProviderSelection`, `resolveComposerInteractionMode`, `deriveComposerSendState`,
   `buildLocalDraftThread`, `resolveDraftPromotionNavigationTarget`, `waitForRevertedMessage`.
   Skipped as cut: the `resolveProactive…` preview helpers, the `Antigravity*` send block,
   `reconcileMountedTerminalThreadIds` and `buildExpiredTerminalContextToastCopy` (terminal cut), and
   the blob-URL helpers (web-only). Target type mapping: `ProviderInteractionMode` and `ThreadEnvMode` come
   from `turin_contracts`; the thread and timeline types go on `state/models.rs` and `timeline/types.rs`.
2. **`threadFindHighlights.test.tsx`** (DOM, 276 lines) is not ported. The two `collectThreadFindRanges`
   cases are ported as node-tree tests. The CSS Highlight caching cases depend on the DOM observer.
3. **`markdown-clipboard.test.ts`**: the cases in the 18 ported tests cover every block and inline rule.
   Not ported, because the rule they test is DOM markup Turin does not emit: the `line`-span inline code
   cut, the `data-markdown-copy` card, and the `select-none` header chrome. See PORTING.md Deviations.
4. **`hooks/useCopyToClipboard.ts`**: skipped, per PM decision. Deviations row added.
5. **`text/html` clipboard flavor** is not produced (GPUI clipboard is text-only).
6. **No call sites yet.** Every new module is `#[allow(dead_code)]` with a comment, as in U3a. Remove
   the allows when the chat view, the find bar or the copy action calls each module.

## Deviations

Rows added to PORTING.md's Deviations table (`threadFindHighlights`, `threadFindPosition`,
`useThreadFind`, `useThreadFindNavigation`, `embeddedScripts` parser and positions, `markdown-clipboard`
DOM walk and html flavor, `ChatView.logic` first batch, `useCopyToClipboard`, `useAssistantCitationTarget`).

Key points:
- `embeddedScripts`: tree-sitter-bash 0.23 (already pinned by gpui-kit) replaces `unbash`. Positions are
  UTF-8 byte offsets, not UTF-16. Any tree with an error returns no scripts.
- `markdown-clipboard`: serializes Turin's `Block`/`Inlines`; selections over code copy their text.
- `useThreadFind`: response identity compares by value.

## Gate

- `cargo fmt --all --check`: clean.
- `cargo clippy -p turin --all-targets -- -D warnings`: clean.
- `cargo test -p turin`: 1589 passed.
- `scripts/check.sh -p turin` run at the branch tip; result in the final report.

## Notes for the next agent

- Check Rust string literals in test fixtures with `grep -n '\\'` after editing (escape decoding issue noted in U3a).
- `rustfmt --edition 2024 <file>` for single files; avoid `cargo fmt -p turin` on the whole crate.
- Cargo builds take about 10 minutes on first run after dependency changes; later runs are quick.
