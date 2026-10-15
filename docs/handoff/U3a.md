# U3a handoff: chat and thread view logic

Branch `task/U3a-chat-logic`, worktree `turin-wt/U3a-chat-logic`. Ports of upstream
`apps/web/src` pure logic into `crates/app/src/components/chat/`. Env as in T14:
`export PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.
Commit trailer: `Model: claude-haiku-5-5 (claudeAgent)`.

## State at handoff
- Tree clean; all work committed. Last commits: `feat(app): port assistant citation text selector logic`,
  then the PORTING.md deviation rows and this file.
- `cargo test -p turin`: all pass (1336 at the last full run, before the citation selector added 22).
- `cargo clippy -p turin --all-targets -- -D warnings`: clean after the last code change.
- Gate `scripts/check.sh -p turin` is re-run as the final step; see the final report for its result.
- No T16 call site is wired yet. Each new module is `#[allow(dead_code)]` in `components/chat.rs`,
  with a comment saying so. Remove the allow when a view calls the module.

## Done (upstream → Rust)
| upstream | Rust | tests |
|---|---|---|
| `pendingUserInput.ts` | `components/chat/pending_user_input.rs` | 27 (every upstream case, plus helper fixtures) |
| `components/chat/pageScrollController.ts` | `components/chat/page_scroll_controller.rs` | 9 (all upstream cases; pull-based clock harness) |
| `components/chat/timelineScrollAnchoring.ts` | `components/chat/timeline_scroll_anchoring.rs` | 10 (no upstream test file; written here) |
| `components/chat/chatCanvasLayout.ts` (chat lane only) | `components/chat/chat_canvas_layout.rs` | 5 (non-preview upstream cases) |
| `lib/selectionActions.ts` (positioning and timing only) | `components/chat/selection_actions.rs` | 5 (3 upstream cases, 2 added) |
| `lib/assistantTextSelection.ts` (pure text half) | `components/chat/assistant_text_selection.rs` | 22 (upstream selector and finder cases, minus the round-trip assertions) |

T16 already had `timeline/session_logic.rs` (`derive_pending_user_inputs`) and the composer's
pending-input card with element-state drafts (`composer_pending_panels.rs`). Those are separate
from this port. `pending_user_input.rs` is the pure logic the card should call once it moves off
element state. No duplicate was found for the other modules.

Every deviation is in PORTING.md's Deviations table (rows added for each module above, plus the
preview-cut and DOM-glue items).

## Left to do (not started, with the blocker for each)
1. **`components/ChatView.logic.ts`** (1347 lines, `ChatView.logic.test.ts`). Not started. Most
   functions take client-runtime types (`EnvironmentThreadShell`, `presentThreadShell`), Effect
   atoms (`appAtomRegistry`, `waitForAtomValue`), `DraftThreadState` / `ComposerImageAttachment`,
   provider-instance resolution and `Thread`/`ChatMessage` web types. Map each to Turin's
   `state/models.rs` and `timeline/types.rs` first. Pure candidates with the fewest deps:
   `resolveEffectiveInteractionMode`, `resolveSendEnvMode`, `resolveBackgroundDraftWorkspaceOptions`,
   `branchMismatchKey` / `shouldShowBranchMismatchBanner`, `shouldShowPlanFollowUpPrompt`,
   `threadHasStarted`, `deriveLockedProvider`, `createLocalDispatchSnapshot` /
   `hasServerAcknowledgedLocalDispatch`, `resolveThreadSwitchTimeline` and the held-timeline
   helpers (generic over `T`), `resolveDraftHeroState`, `resolveWorktreeSetupProgress`.
   Skip as cut features: `resolveProactive…` preview-panel helpers (preview cut), the
   `Antigravity*` send block (provider cut), `reconcileMountedTerminalThreadIds` and
   `buildExpiredTerminalContextToastCopy` (terminal cut), and the blob-URL helpers (web-only).
2. **`markdown-clipboard.ts`** (411 lines, test exists). DOM serialization of rendered markdown
   (`serializeRenderedMarkdownFragment`, tables to markdown and CSV, `chatMarkdownClipboardPayload`).
   Blocker: it walks the rendered DOM. Port it against the Turin markdown AST
   (`components/chat/markdown/`) instead, and record the change as a deviation.
3. **`lib/embeddedScripts.ts`** (544 lines, test exists). Needs a shell parser that matches
   `unbash`'s `Command` / `Word` / heredoc AST and spans. Turin has no Rust bash parser. Options:
   tree-sitter-bash (already a gpui-kit feature), or a small parser for the subset used. Also needs
   the filetype map from `@pierre/diffs` (`getFiletypeFromFileName`), which `diff/highlight.rs`
   already has for its grammars. Decide the parser before porting.
4. **`lib/selectionActions.ts` `observeSelectionActions`** (DOM gesture observer). Re-derive the
   gesture state machine against GPUI pointer, key and selection events, with the same timer rules
   (`SELECTION_MULTI_CLICK_INTERVAL_MS`). The upstream test file is DOM-mocked; rewrite its cases as
   event sequences.
5. **`hooks/useCopyToClipboard.ts`** (262 lines, test exists). The Effect tagged errors and the
   clipboard-API fallback are web-only. GPUI uses `cx.write_to_clipboard` / `read_from_clipboard`.
   Port only the error vocabulary if a caller needs it; most of the file does not carry over.
6. **`components/chat/useThreadFind.ts`, `threadFindHighlights.ts`, `useThreadFindNavigation.ts`**
   (about 900 lines together). Blockers: `components/chat/threadFind.ts` (`stepThreadFindIndex`,
   `ThreadFindStart`, `ThreadFindPositionReader`) is out of this task's list and not ported, and the
   find state reads server search results (`useServerResults`, `orchestrationEnvironment`). Port
   `threadFind.ts` first. `collectThreadFindRanges` walks DOM text nodes, so port the range
   collection over the Turin timeline text model. Keep the state machine in `rebaseLocalSelection`
   and `step`.
7. **`components/chat/useAssistantCitationTarget.ts`** (191 lines). Mostly React state and history
   paging (`CitationHistoryPage`). Pure parts are the target-resolution rules. Check them against
   `find_assistant_citation_text` (done) before porting.

## Notes for the next agent
- The Edit/Write tools decode `\uXXXX`. Check literals with `od -c` or `grep -n '\\'`. The module
  tests here use `\u{…}` and `\t`/`\n` escapes, and they were checked.
- `rustfmt --edition 2024 <file>` works on a single file; `cargo fmt -p turin` reformats the whole
  crate and should be avoided here.
- `cargo test -p turin <filter>` takes about 10 minutes the first time (throttled build); later runs
  are fast.
