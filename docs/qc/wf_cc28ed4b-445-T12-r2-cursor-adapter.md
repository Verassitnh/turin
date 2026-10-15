# QC wf_cc28ed4b-445 — T12 cursor adapter re-QC (Haiku)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 16 · refuted: 0

## Lead report

Checkout root: /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter (paths below are relative to it)

Fix list, ranked. Prior-finding items 1, 2, 10, 11, 22 and 24 are the same defects re-raised, so they are merged below. Finding 2 (accountUsage page-failure) was fixed and is not listed.

MEDIUM
1. crates/provider-cursor/src/session.rs:1563 and :1566. Approval card prompt and title are `request.title` only. When toolCall.title is absent, the card is blank except for its buttons. Fix: derive the prompt as rawInput command/cmd, then title, then a session fallback (mirror permissions.rs parse logic). Use it for ApprovalRequest.prompt and the card title. Upstream runtimeModel.ts:1247-1250 and adapter.ts:4837. Also prefer the command over title when both exist, as upstream does.
2. crates/provider-cursor/src/session.rs:1674-1684. create_plan emits ProposedPlan markdown and the proposed_plan artifact from `parsed.plan` with no trim or empty check, so a blank plan can be approved. Contested: one verifier says the upstream Cursor path is also untrimmed. The brief requires that a blank plan is never approved, so fix: trim; if empty, fall back to overview or todos, else reply cancelled without opening a card. Apply the same trimmed value to the artifact at :1683. Trigger (Cursor sending plan "") is unverified.
3. PORTING.md:250 (and the Deviations table). No Deviations row for text generation. driver.rs:250-292 returns errors for commit, PR, branch, title and review-guide generation. Fix: add a provider-cursor row naming those cuts, the reason, and the upstream textGeneration.ts reference.
4. crates/provider-cursor/src/events.rs:123-125 and :1419, session.rs:669. usage_update/state_update is parsed and dropped, so context_usage is always None. The context window meter (PORTING.md:168, kept feature) gets no Cursor data, and no Deviations row covers it. Fix: store the usage snapshot and emit contextUsage, as upstream provider-acp adapter.ts:4119-4160 does. Or add a Deviations row. Emission by cursor-agent 2026.10.14 is unverified, so confirm before treating as blocking.

LOW
5. crates/provider-cursor/src/events.rs:1407-1415, :1222, :1382-1391. ACP plan updates are always Running and Draft, so Completed and Active are never produced (only drained at turn end). Fix: derive ACP plan status from steps (Completed when non-empty and all completed, else Active), and node status to match. Keep plan_artifact_status for update_todos (session.rs:1718-1726). Upstream adapter.ts:3577-3583 and 3600-3601.
6. crates/provider-cursor/src/events.rs:1150-1156. A completed tool is removed from self.tools, so a later tool_call_update loses title, kind and rawInput and resets started_at. A status-less late update re-emits as Running. Fix: keep tool state for the turn, as upstream does (adapter.ts:3105-3108). Also filter finish() (events.rs:1292-1297) to pending/running tools only, mirroring acpCompletedTurnShouldTerminalizeTool (adapter.ts:1368-1376), or completed tools flip to interrupted on interrupt.
7. crates/provider-cursor/src/session.rs:1578-1584, with permissions.rs:188-199 and :119-122. An AcceptForSession grant is inserted even when the answer is Cancelled (no allow_once option). Later identical requests are silently cancelled (session.rs:1543-1550). Fix: insert the grant only when answer_for_decision returns Selected, or omit AcceptForSession when allow_once is absent.
8. crates/provider-cursor/src/acp_config.rs:54-68 (callers session.rs:365-372 and :476-479). No drop guard: if the wrapped future is dropped mid-await, the restore never runs and ~/.cursor cli-config edits persist. Latent, since no current caller drops it. Fix: a TurnGuard-style drop guard that restores (armed/disarmed).
9. crates/provider-cursor/src/acp_config.rs:65-67, :76, :95, :105, :122, :161-163. Blocking std::fs runs inline in async with_user_config on the tokio path, which violates PORTING.md:102-108. The account_usage.rs:172-173 precedent uses spawn_blocking. Fix: spawn_blocking around snapshot and restore, keeping order, or tokio::fs, and log a Deviations row if kept. This merges the prior finding 22 and the new-defect item.
10. crates/provider-cursor/src/events.rs:746-751. Execute input falls back to "" via unwrap_or_default. Verifiers disagree on the fix. One says use the literal "Command" (upstream adapter.ts:3458). The other says upstream's effective fallback is "Ran command" through the summary path (runtimeModel.ts, toolActivity.ts:275-276, 438), so "Ran command" is the better target. Pick after checking upstream output.
11. PORTING.md:250. The cut-tests row omits adapter.test.ts (9 cases; `:374` lint/dir-tree projection and `:945` `<unknown path>` are kept features, not cut), CursorAgentSdk.test.ts, and CursorKeychain.test.ts. Fix: name these files and state which adapter cases are ported or cut.
12. crates/provider-cursor/src/account_usage.rs:166 and crates/provider-cursor/src/driver.rs:243-245. read_cursor_account_usage has no non-test caller, and the accountCache and CursorUsageAccounts layer is not in any Deviations row. The driver.rs comment says "usage limits come from the dashboard", which is wrong (upstream usageLimits.ts uses api2.cursor.sh). Fix: correct the comment, and document the reader as unwired and why in PORTING.md.
13. events.rs:776 (optional). diff_str is set only on failed edits. Upstream stores it on success (adapter.ts:3466-3476), but its client projection drops it, so observable behaviour matches. Optional parity fix only.

OUT OF T12 DIFF (crates/acp, unchanged on this branch; track as PM items or fix in a separate change)
14. crates/acp/src/protocol.rs:326-333. writer_loop holds Arc<ConnectionInner>, which owns write_tx, so the writer task and its ChildStdin fd never exit. Fix: use a weak handle or a termination signal.
15. crates/acp/src/process.rs:145-148. Drop runs kill(-pid) and waitpid(pid) with no reaped check. Fix: gate only the waitpid on a reaped flag, and keep the group kill (PORTING.md:266).

VERDICT: PASS (no critical or high findings; items 1 to 4 are medium and should be fixed or logged before merge).

## Confirmed findings (raw)

### [low] Reference-cycle fix is partial: the ACP writer task still leaks (finding 1/18)
- `crates/acp/src/protocol.rs:327` · dimension: prior-findings · upheld 2/2
- Evidence: Fixed in provider-cursor: handler closures hold Weak<SessionInner> (session.rs:1492-1510, 1527-1533) and dropping CursorSession kills the child (test session.rs:2434 dropping_the_session_kills_the_agent_process passes). Not fixed: protocol.rs:327-333 writer_loop(write, rx, inner: Arc<ConnectionInner>) still holds the strong Arc that owns write_tx, so the writer task never exits. `git diff main HEAD -- crates/acp` is empty, so the acp-side item from the original fix list is not on this branch.
- Fix: In crates/acp, make writer_loop exit on connection termination (weak handle or termination signal), or record the acp item as an open PM item in PORTING/handoff.

### [medium] Text generation cut has no Deviations row (finding 10)
- `PORTING.md:250` · dimension: prior-findings · upheld 2/2
- Evidence: Sign-in is wired (driver.rs:144 auth: Some(sign_in::controller(...))), but driver.rs:250-257 text_error() returns 'Cursor ACP does not generate commit messages or titles.' for commit, PR, branch, title and review-guide generation. No PORTING.md row describes that cut. The only mention is inside the '(tests not ported)' row at line 250, which lists textGeneration.test.ts as a test file, not as a cut feature.
- Fix: Add a provider-cursor Deviations row: commit, PR, branch, thread-title and review-guide text generation are cut, with the reason and the upstream textGeneration.ts reference.

### [low] Test-cut Deviations row omits upstream adapter.test.ts and CursorAgentSdk.test.ts (finding 11)
- `PORTING.md:250` · dimension: prior-findings · upheld 2/2
- Evidence: Upstream has packages/provider-cursor/src/server/adapter.test.ts (8 tests) and CursorAgentSdk.test.ts. The row lists accountCache, usageLimits, skills, auth, CursorSdk*, credentialStore, textGeneration and transportFailure, but not adapter.test.ts or CursorAgentSdk.test.ts. Rust ports only the failed-search case literally (handoff T12.md); the other adapter cases are new tests, and the omitted upstream files are not logged as cut. The accountUsage page-failure test is ported (account_usage.rs:1177 failed_page_read_leaves_no_request_open), so that part is fixed.
- Fix: Add adapter.test.ts and CursorAgentSdk.test.ts to the cut-test row, or list which upstream adapter cases were ported.

### [low] Account-usage reader is documented as ported but has no caller (finding 2)
- `crates/provider-cursor/src/account_usage.rs:166` · dimension: prior-findings · upheld 1/2
- Evidence: read_cursor_account_usage (account_usage.rs:166) is exported from lib.rs, and grep finds no non-test caller in crates/. PORTING.md (usage limits row, plus the '(account usage)' row) says 'Dashboard usage records are ported', but driver.rs:245 says 'Cursor's usage limits come from the dashboard, which is not ported.' The docs disagree, and the reader is unwired.
- Fix: Either wire the reader to a consumer, or change the PORTING row to say the reader is unwired and why.

### [low] Blocking std::fs config IO still runs on the async turn path (finding 22)
- `crates/provider-cursor/src/acp_config.rs:65` · dimension: prior-findings · upheld 2/2
- Evidence: with_user_config is async but calls snapshot_user_config(cursor_home)? (acp_config.rs:65) and restore_user_config (acp_config.rs:67) inline. Those call std::fs::read/write/remove_file (acp_config.rs:76, 95, 105, 122, 161-163). It runs from apply_model_selection (session.rs:356-375), which start_turn calls every turn (session.rs:1355). The branch did not change it: the handoff lists it as left for T12d, and the T12d commit 6f49b04 only moved the credential read in account_usage.rs:173.
- Fix: Wrap snapshot and restore in tokio::task::spawn_blocking (keeping the snapshot/restore order), or log the decision in Deviations. The original QC excluded this item, but the count of 24 includes it.

### [low] ProcessGuard kills and waits on a possibly reaped pid (finding 24, out of this crate)
- `crates/acp/src/process.rs:139` · dimension: prior-findings · upheld 1/2
- Evidence: Drop for ProcessGuard still runs libc::kill(-pid, SIGKILL) and libc::waitpid(pid, null, 0) at process.rs:145-148 with no reaped check. `git diff main HEAD -- crates/acp` is empty, so the branch does not fix this. Out of this branch's crate scope, but the original finding is still open.
- Fix: Set an AtomicBool reaped flag in exit_error and skip the kill and waitpid once it is set, or reap only through tokio Child. Track as a PM item in crates/acp.

### [medium] create_plan card and proposed_plan artifact carry the plan string unchecked, so an empty plan can be approved
- `crates/provider-cursor/src/session.rs:` · dimension: new-defects · upheld 1/2
- Evidence: session.rs:1674-1678 builds ItemBody::ProposedPlan { markdown: parsed.plan.clone(), .. } with no trim or empty check, and session.rs:1682-1684 emits stream.proposed_plan(&plan_id, &request_id, &parsed.plan, now). crates/acp/src/cursor.rs:128 declares `pub plan: String` (required, so "" decodes). The user is then asked to accept a card whose body is blank. Upstream upstream/packages/provider-acp/src/server/adapter.ts:2292-2293 does `const markdown = input.planMarkdown.trim(); if (markdown.length === 0) return;`, so an empty plan never becomes a proposed-plan card. docs/protocols/cursor-acp.md (create_plan params) lists overview? as an optional field that the card ignores as a fallback.
- Fix: Trim parsed.plan. If it is empty, fall back to parsed.overview, then to the todo list text. If nothing remains, reply rejected or cancelled without opening a card, as upstream's captureProposedPlan returns without emitting. Apply the same trimmed value to the proposed_plan artifact at session.rs:1683.

### [medium] Permission approval card prompt is the bare title, with no command or detail fallback
- `crates/provider-cursor/src/session.rs:` · dimension: new-defects · upheld 2/2
- Evidence: session.rs:1563 `prompt: request.title.clone()` and session.rs:1566 `title: request.title.clone()`. When toolCall.title is absent, both are None and the card has no text, even though toolCall.rawInput.command is present. Upstream upstream/packages/provider-acp/src/server/runtimeModel.ts:1247-1253 computes `detail = toolCall?.command ?? toolCall?.title ?? toolCall?.detail ?? "Session <id>"`, and adapter.ts:4862-4863 sets the approval item's `prompt: parsed.detail`.
- Fix: Derive the prompt the way parsePermissionRequest does: command from rawInput (command/cmd), then title, then a session fallback. Use it for ItemBody::ApprovalRequest.prompt and the card title, so the user always sees what they approve.

### [low] ACP plan updates never reach the completed or active plan states that upstream produces
- `crates/provider-cursor/src/events.rs:` · dimension: new-defects · upheld 2/2
- Evidence: events.rs:1407-1415 projects CursorUpdate::Plan through plan_update(..., Running). events.rs:1222 then sets plan_artifact_status(Running), which returns Draft (events.rs:1382-1391). Only Completed maps to Active, and nothing maps to Completed, so an ACP plan whose steps are all done still reports a running node until turn end. Upstream upstream/packages/provider-acp/src/server/adapter.ts:3577-3583 sets `status: completed ? "completed" : "active"` for a todo_list, with completed = every step completed. adapter.ts:3600-3601 then marks the node completed for completed/superseded plans.
- Fix: For ACP plan updates, derive the plan status from the steps as upstream does: Completed when non-empty and every step is completed, otherwise Active. Derive the node status the same way. Keep plan_artifact_status for the cursor/update_todos path, which upstream's SDK path maps differently.

### [medium] usage_update is parsed and then dropped, so the context window meter never receives Cursor usage
- `crates/provider-cursor/src/events.rs:` · dimension: new-defects · upheld 2/2
- Evidence: events.rs:123-125 maps "usage_update" | "state_update" to CursorUpdate::Usage, and events.rs:1416-1420 project() returns Vec::new() for Usage. session.rs:669 always sets `context_usage: None`. Upstream upstream/packages/provider-acp/src/server/adapter.ts:4119-4137 stores the usage snapshot per session and sets context.contextUsage, then adapter.ts:4150-4160 republishes the provider thread with contextUsage. The Turin port does not record this, and PORTING.md has no Deviations row for it, although PORTING.md:168 lists the context window meter as a kept feature.
- Fix: Keep the usage snapshot on SessionInner and the active turn, parse it into the ThreadTokenUsageSnapshot shape, and emit a provider-thread update with contextUsage. If the omission is intentional, add a Deviations row citing adapter.ts:4119-4137.

### [low] with_user_config restores the Cursor config only if the wrapped future completes
- `crates/provider-cursor/src/acp_config.rs:` · dimension: new-defects · upheld 2/2
- Evidence: acp_config.rs:54-68: `let snapshot = snapshot_user_config(cursor_home)?; let result = operation().await; let restored = restore_user_config(cursor_home, &snapshot);`. There is no drop guard. session.rs:365-372 (apply_model_selection) and session.rs:476-479 (open_with_snapshot) pass their set_config and session/new calls through it. If the caller drops the future during operation().await (interrupt, a select/timeout, or a dropped start_turn), restore never runs and ~/.cursor/cli-config.json and acp-config.json keep the model edits. PORTING.md's acp-config row promises that the snapshot is restored, and TurnGuard (session.rs:741-757) shows the pattern used elsewhere.
- Fix: Add a drop guard, like TurnGuard, that performs the restore (or spawns it onto the runtime) when armed, and disarm it after a normal restore. Keep the restore synchronous, so it runs on drop.

### [low] Synchronous file IO runs on async workers inside the Cursor config snapshot and restore
- `crates/provider-cursor/src/acp_config.rs:` · dimension: new-defects · upheld 2/2
- Evidence: acp_config.rs:76 (read_snap: fs::read), 95 (fs::read), 105 (fs::remove_file), 122 (fs::read), and 161-163 (create_dir_all and fs::write) are plain std::fs calls. They run inside with_user_config, an async fn awaited from session.rs:365 and session.rs:476 on tokio workers. account_usage.rs:172-173 wraps the same kind of credential read in tokio::task::spawn_blocking, so the module is inconsistent. PORTING.md's Async-first rule requires IO off the async path.
- Fix: Use tokio::fs for these reads and writes, or wrap the snapshot and restore helpers in spawn_blocking. Keep the ordering so the restore still happens after the operation.

### [low] Successful edits drop the unified diff that upstream puts in diffStr
- `crates/provider-cursor/src/events.rs:` · dimension: new-defects · upheld 1/2
- Evidence: events.rs:776 sets `diff_str: (failed && !output_text.is_empty()).then(|| output_text.clone())`, so diffStr is None for every completed edit. Only old_str and new_str are filled (events.rs:777-782), from the ACP diff content. Upstream upstream/packages/provider-acp/src/server/adapter.ts:3302-3303 computes diffText = acpToolCallDiffPatch(content) ?? textFromUnknown(rawOutput), and adapter.ts:3466-3476 sets diffStr from it for file_change items on success.
- Fix: Build a unified patch from the diff content (acpToolCallDiffPatch, adapter.ts:940-965) and set diff_str for completed edit, delete and move calls too, as upstream does. Keep old_str and new_str as well.

### [low] A completed tool call's state is discarded, so a later tool_call_update rebuilds it from partial fields
- `crates/provider-cursor/src/events.rs:` · dimension: new-defects · upheld 2/2
- Evidence: events.rs:1153-1154: `if completed { self.tools.remove(call_id); }`. A later tool_call_update for the same id then starts from previous = None (events.rs:1146-1148). It loses title, kind and rawInput, and resets started_at to now. Upstream upstream/packages/provider-acp/src/server/adapter.ts:3105 merges with the previous tool state, and grep finds no `tools.delete` anywhere in that adapter.
- Fix: Keep the tool state for the rest of the turn, as upstream does, and remove it only at turn finalisation.

### [low] Granting for the session when no allow_once option exists records a grant that answers cancelled
- `crates/provider-cursor/src/session.rs:` · dimension: new-defects · upheld 2/2
- Evidence: permissions.rs:119-122 always offers AcceptForSession. permissions.rs:197-201 answer_for_decision maps AcceptForSession to allow_once and returns Cancelled when the request has no allow_once option. session.rs:1579-1584 still inserts the grant key when decision == AcceptForSession. Later identical requests then take the auto-answer path at session.rs:1543-1550 with no card, and that path also resolves to Cancelled.
- Fix: Insert the grant only when answer_for_decision returns Selected. Alternatively, omit the AcceptForSession option when allow_once is absent, as approval_options already does for AcceptAlways.

### [low] Execute item input falls back to an empty string where upstream uses the literal Command
- `crates/provider-cursor/src/events.rs:` · dimension: new-defects · upheld 2/2
- Evidence: events.rs:746-751 builds `input` from command/cmd, then title, then `.unwrap_or_default()`, which yields an empty command_execution input. Upstream upstream/packages/provider-acp/src/server/adapter.ts (the execute case, about lines 3453-3460) uses `toolCall.command ?? monitorCommand ?? toolCall.title ?? "Command"`.
- Fix: Use "Command" as the final fallback in place of unwrap_or_default, matching upstream's literal.
