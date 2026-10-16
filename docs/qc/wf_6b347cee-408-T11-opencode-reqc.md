# QC wf_6b347cee-408 — T11 OpenCode adapter re-QC

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 14 · refuted: 1

## Lead report

Fix list (medium first, then low; duplicates merged, the two v1 approval-prompt entries folded into one):

1. MEDIUM. crates/provider-opencode/src/adapter/v2.rs:2095 (finish). Turn end never settles open foreground tool or subagent items, and never settles root text blocks. Upstream: adapter.ts:1738-1757. Fix: track open call ids per V2Turn (insert on Running, remove on terminal). In finish(), emit completed (completed turn) or interrupted for each open call, settle open foreground subagents via settle_background, and close open root text blocks as final. Test: tool.called with no success, then execution.interrupted, expects the item interrupted.

2. MEDIUM. v2.rs:2109-2125 (finish). A background child's pending request is cancelled at turn end. Upstream keeps it open unless the terminal status is failed: adapter.ts:1728-1737. Fix: skip child-owned requests whose child is in guard.background unless the terminal status is Failed. Test: background child asks permission, parent turn completes, request stays pending and answerable.

3. MEDIUM. v2.rs:2028-2038 (abandon_request) and v2.rs:2095 (finish). Only guard.threads is searched, so an undelivered reply from a subagent's request does not fail the child turn or interrupt the child session. Child turns live in guard.child_turns. Upstream: adapter.ts:4248, 1862-1877. Fix: when the session is a child with an active child turn, call finish_child_turn(Failed) and interrupt the child with the 2s timeout. Test: child permission reply fails undelivered, expects the child turn failed and an interrupt sent to the child session. The call site is v2.rs:2917.

4. MEDIUM. crates/provider-opencode/src/adapter/v1.rs:1480 (v1 approval body). prompt is None and patterns is never read. Upstream: server/adapter.ts:1804-1812. Fix: prompt = patterns.join("\n"), or the permission name when patterns is empty. Leave options None; that is correct, since upstream v1 approval items have no options. Test: v1_permission_waits_as_a_node_and_item_then_settles_on_reply (session_tests.rs:1367) must assert the prompt. This is the known open item. The v1 options gap is not a defect; the prompt gap is.

5. MEDIUM. v2.rs:522-556 (fail_stream). Background entries, background_calls, wakes and reports are never cleared. Upstream failAll: adapter.ts:2732-2746. Fix: settle each guard.background entry with settle_background(Failed), drain background_calls, set wake.dropped on all wakes, clear reports. Test: stream failure with a running background subagent, expects its item failed.

6. MEDIUM. v2.rs:400-470 (reconcile_now). Background subagents whose child ended during the stream outage are never settled. Upstream: adapter.ts:2890-2908, with LOST_BACKGROUND at 684-685. Fix: for each guard.background entry whose child is absent from session_active, settle Interrupted with the lost-background text and remove it from background and background_calls. Also discard wakes and clear reports for reconciled sessions. Test: reconnect with a background child gone.

7. MEDIUM. crates/provider-opencode/src/adapter/records.rs:492 versus v1.rs:2358. The display id uses the "Question N" fallback header, but the lookup id uses the raw header. Blank-header answers are sent as [] and silently dropped. Upstream: OpenCodeRuntime.ts:456-465 and 587-589, which derive the id from the raw header in both places. Fix: derive the id from the js-trimmed raw header in both places, keeping the fallback for display only. Test: v1 round trip with a blank or BOM-only header.

8. LOW. v2.rs:3276-3283 (stop_background). A 404 SessionNotFound on interrupt counts as not reached, so the child stays Running and retries fail forever. Upstream: adapter.ts:3641-3647 maps it to Effect.void, so it counts as reached, then settles interrupted (3669-3673). Fix: treat SessionNotFound as reached. Test: 404 on child interrupt during a restart Stop, expects the item interrupted. The client path is crates/provider-opencode/src/v2_client.rs:264.

9. LOW. v2.rs:940-956 (emit_tool). metadata.status overrides the event type for plain tools. Upstream: adapter.ts:2300-2306 always completes plain successes, and only the subagent branch (2291-2298) reads metadata.status. Fix: restrict the metadata.status read to the subagent branch, or log it as a Deviation. Also map error.type "aborted" to Interrupted for plain tools (upstream adapter.ts:2321; fixture opencode2_interrupt). Test: plain success with metadata.status "running" completes.

10. LOW. v2.rs:2716-2721 (start_turn running-wake branch). The taken wake is replayed without wake.dropped being set, so its continuation can still dispatch. Upstream: adapter.ts:3494 and 3559-3563, with offerWake returning none at 1713. Fix: wake.dropped.store(true) before replay. Latent until T13e. Test: prompt takes a running wake, then the offer's dispatch is refused.

11. LOW. v2.rs:1492-1497 (collect_report). The status ignores guard.stopped_children, so a late report from an unreached child settles as Completed or Cancelled, not Interrupted. Upstream: adapter.ts:2461-2467. Fix: use Interrupted when the child is in stopped_children, before the outcome match. Test: interrupt times out, then the child reports.

12. LOW. v2.rs:1322-1330 (park_or_wake). Events of announced subagents are not parked while the parent wake is held, so their transcript is dropped. Upstream holderOf: adapter.ts:2475-2484 and 2564-2572. Note: the v2.rs:1319 doc comment wrongly says holderOf is implemented. Fix: route announced-child events to the parent's held wake, in order.

13. LOW, disputed. v2.rs:2661 (start_turn) versus the take at v2.rs:2712-2721. A live event can overtake parked events in the gap between the active-set and the wake take. One verifier says upstream has the same ordering (adapter.ts runWake/startTurn around 3922-4031), so this may be an inherited race rather than a port defect. Consider it optional; it is not a blocker.

Adjacent items from verifier notes, not in the confirmed list, needing a check before filing:
- v2.rs:1195-1215 (show_ask): a background child's permission asked after the parent turn has ended is silently dropped. Upstream shows it under the background turn: adapter.ts:2181-2184, 1021-1030.
- v2.rs:611-615 and 746-750 (link_child): reads "id" rather than "sessionID" and "parentID" for session.created. Upstream: adapter.ts:2547-2558. Verify against the real OpenCode session.created payload first.
- v2.rs:522-556 (fail_stream): child-session turns in child_turns are never finished. Same fix as item 5.

Verdict: PASS (no critical or high findings; the seven medium items above must still be fixed before merge).

## Confirmed findings (raw)

### [medium] Turn end never settles open foreground tool or subagent items (finding 1 exemption implemented, settlement missing)
- `crates/provider-opencode/src/adapter/v2.rs:2095` · dimension: fix-verification · upheld 2/2
- Evidence: finish() (v2.rs:2095-2240) only cancels requests, closes text blocks (drain at :1765) and an open compaction. It never emits a terminal item for a tool call still Running, and V2Turn tracks no open-call set (emit_tool only records tool_started, v2.rs:962). Upstream finishTurn (upstream/packages/provider-opencode/src/server/v2/adapter.ts:1748-1753) runs emitTool(..., terminal.status==='completed' ? 'completed' : 'interrupted') for every id in turn.tools, and 1754-1757 settles every non-background call. Result: a foreground bash/subagent call that gets no session.tool.success before a Stop or failure stays Running in T3. Verified by reading: no test in session_tests.rs covers a tool item open at turn end.
- Fix: Track open tool call ids per V2Turn (insert on first Running event in emit_tool, remove on terminal). In finish(), emit a TurnItemUpdated/NodeUpdated with completed (on completed turns) or interrupted for each open call, and settle open foreground subagents via settle_background with the terminal status. Add a test: tool.called with no success, then execution.interrupted, expects the item to be interrupted.

### [medium] Turn end cancels a background subagent's pending request (upstream keeps it open unless the turn failed)
- `crates/provider-opencode/src/adapter/v2.rs:2109` · dimension: fix-verification · upheld 2/2
- Evidence: finish() builds `doomed` from every pending request whose session is the turn's session or a child of it (v2.rs:2109-2125) and settles each as cancelled, with no background check. Upstream finishTurn (upstream/packages/provider-opencode/src/server/v2/adapter.ts:1728-1737): `entry.sessionId === state.sessionId || (entry.turn === turn && !(background && terminal.status !== 'failed'))`, where background = callsAbove(entry.sessionId).some(call => call.background). So a background child's permission survives a completed or interrupted turn upstream, but T3 cancels it here while OpenCode still waits on it and no reply is sent. This is the same background exemption that finding 1 asked for, applied to requests.
- Fix: In the doomed filter, skip child-owned pending requests whose child is in guard.background (or whose call is background) unless the terminal status is Failed, matching adapter.ts:1728-1737. Add a test: background child asks permission, parent turn completes, request stays pending and answerable.

### [medium] Undelivered answer for a subagent's request does not end the subagent's turn
- `crates/provider-opencode/src/adapter/v2.rs:2028` · dimension: fix-verification · upheld 2/2
- Evidence: abandon_request (v2.rs:2028-2038) returns early unless guard.threads has an active turn for session_id, and finish() (v2.rs:2095+) also only looks in guard.threads. A child session's turn lives in guard.child_turns (finish_child_turn at v2.rs:1751 is the only child finisher). In respond_to_runtime_request, abandon is called with pending.session_id, which is the child for a subagent's request (v2.rs:2990-2999 path). Upstream abandonRequest(asker, ...) with asker = threads.get(entry.sessionId) ?? entry.state (upstream/packages/provider-opencode/src/server/v2/adapter.ts:4248, 4304) finishes the subagent's turn as failed and interrupts its session (adapter.ts:1862-1877). Here the child turn stays Running and OpenCode stays blocked on the unanswered request. This path is new in the QC fix pass (finding 5).
- Fix: In abandon_request, when the session is a child with an active child turn, call finish_child_turn(Failed) and interrupt the child session with the 2s timeout, as adapter.ts:1862-1877 does. Add a test: child permission reply fails undelivered, expects the child turn failed and an interrupt sent to the child session.

### [low] Stop treats an already-gone background child as not reached, leaving its item Running
- `crates/provider-opencode/src/adapter/v2.rs:3276` · dimension: fix-verification · upheld 2/2
- Evidence: stop_background: `let reached = matches!(tokio::time::timeout(INTERRUPT_TIMEOUT, self.client.interrupt(&child, None)).await, Ok(Ok(_)));` then `if !reached { continue; }` (v2.rs:3276-3283). V2Client::interrupt expects only 200 (v2_client.rs:264-266), so a 404 SessionNotFound returns Err and the child stays in guard.background, its item Running. Upstream stopBackground (upstream/packages/provider-opencode/src/server/v2/adapter.ts:3641-3647) maps SessionNotFoundError to Effect.void, which counts as reached, so the call is settled as interrupted (adapter.ts:3671-3673).
- Fix: Treat a SessionNotFound (404) reply from interrupt as reached, as adapter.ts:3641-3647 does. Add a test: background child session returns 404 on interrupt during a restart-Stop, expects its subagent item to settle interrupted.

### [medium] v1 approval request item drops the prompt (known open item: options None is correct for v1)
- `crates/provider-opencode/src/adapter/v1.rs:1480` · dimension: fix-verification · upheld 2/2
- Evidence: v1.rs:1480 sets `prompt: None` for the approval body. The v1 code never reads `patterns` (grep finds no `patterns` in v1.rs). Upstream v1 (upstream/packages/provider-opencode/src/server/adapter.ts:1807-1811) builds the approval item with `prompt: permission.patterns.length === 0 ? permission.permission : permission.patterns.join('\n')`. The known open item is partly misdirected: upstream's v1 approval item has no `options` field at all (adapter.ts:1800-1812), so v1 `options: None` at v1.rs:1481 matches upstream. The real v1 gap is the prompt, which is the same class as fixed v2 finding 8.
- Fix: Read props.permission and props.patterns in the v1 permission path and set prompt to patterns.join('\n'), or the permission name when patterns is empty, as adapter.ts:1807-1811. Leave options None (no upstream v1 options). Add a test on v1_permission_waits_as_a_node_and_item_then_settles_on_reply asserting the prompt.

### [low] Plain (non-subagent) tool status now follows metadata.status, overriding the event type
- `crates/provider-opencode/src/adapter/v2.rs:945` · dimension: fix-verification · upheld 2/2
- Evidence: emit_tool's status chain (v2.rs:940-956) reads /metadata/status and /state/metadata/status for every tool, and maps 'running' to Running and 'error'/'failed' to Failed before falling back to the event type. Upstream emitTool for a plain tool success is always 'completed' (upstream/packages/provider-opencode/src/server/v2/adapter.ts:2300-2306: `yield* emitTool(state, turn, event.data.id, 'completed', {output, metadata})`), and only subagent calls read metadata.status (adapter.ts:2287-2298). So a plain tool success whose metadata.status is 'running' stays Running forever here, and one with 'error' becomes Failed where upstream completes it. The real fixture shows `metadata.status: 'completed'` for plain tools (testkit/fixtures/opencode2_tool_call), so this only bites on other payload shapes.
- Fix: Apply metadata.status only inside the subagent branch (name == task/subagent), or for plain tools keep the upstream rule: session.tool.success is completed and session.tool.failed is failed. Add a test: plain tool success with metadata.status 'running' completes.

### [medium] Background subagent items stay Running forever when the event stream fails
- `crates/provider-opencode/src/adapter/v2.rs:522` · dimension: regressions · upheld 2/2
- Evidence: fail_stream (v2.rs:522-556) only finishes active turns as Failed. It never settles guard.background entries (OpenSubagent, inserted at v2.rs ~1040-1050 by the background fix) and never clears wakes or reports. Upstream failAll (upstream/packages/provider-opencode/src/server/v2/adapter.ts:2738-2746) settles every running call as failed, marks wakes dropped, and clears reports with the comment 'Nothing can report a background subagent's end any more.' Before the background-tracking fix, background launches completed at the launch event, so this leak is new.
- Fix: In fail_stream, drain guard.background and background_calls and emit settle_background(open, Failed, None) for each. Mark every wake dropped and clear reports, as failAll does.

### [medium] Reconnect reconcile never settles background subagents whose report was lost
- `crates/provider-opencode/src/adapter/v2.rs:400` · dimension: regressions · upheld 2/2
- Evidence: reconcile_now (v2.rs:400-470) reads session_active and backfills turns, permissions and forms. It has no handling for guard.background. Upstream reconcile (adapter.ts:2880-2896) settles each background call whose child is not active with 'interrupted' and LOST_BACKGROUND ('T3 Code lost its connection to OpenCode while this subagent ran, so its result is not shown.', adapter.ts:684-685). It also discards each session's wakes and clears its reports. In Rust, a subagent that ended while the stream was down never reports, so its item stays Running indefinitely. The background-tracking fix introduced this path.
- Fix: In reconcile_now, for each guard.background entry whose child_session is absent from session_active, settle it with Interrupted and the lost-background text, and remove it from background and background_calls. Discard wakes and clear reports for sessions that were reconciled, as upstream does.

### [low] A user prompt that takes a running wake does not drop the wake's continuation offer
- `crates/provider-opencode/src/adapter/v2.rs:2716` · dimension: regressions · upheld 2/2
- Evidence: start_turn pops the running wake (v2.rs:2716 `wakes.pop()`) and replays it (v2.rs:2721) without setting its `dropped` flag. The offer built in park_or_wake (v2.rs:1422, dispatch_if_current: wake_dispatch(dropped)) still dispatches. The continuation then reaches drain_wake, which finds no wake and finishes a Completed turn with no prompt. Upstream replay() sets `wake.dropped = true` (upstream/packages/provider-opencode/src/server/v2/adapter.ts:3494), and takeRunningWake calls replay (adapter.ts:3559-3563). With dropped set, offerWake returns Option.none (adapter.ts:1713). The consumer of dispatch_if_current is not in this branch (T13e), so this is latent until that lands.
- Fix: In start_turn's running-wake branch, call wake.dropped.store(true, Ordering::SeqCst) before or inside replay, as upstream replay() does.

### [low] Background report settles as Completed even when its child was stopped
- `crates/provider-opencode/src/adapter/v2.rs:1492` · dimension: regressions · upheld 2/2
- Evidence: collect_report settles a background call with the outcome-derived status (v2.rs:1492-1497: Failed, Cancelled, else Completed). It never checks guard.stopped_children. Upstream onReport (adapter.ts:2453-2457) uses 'interrupted' when stoppedChildren.has(childId). stop_background (v2.rs ~3265-3300) leaves a child in stopped_children and in guard.background when interrupt does not answer in time. The child's later report then settles its item as Completed, not Interrupted.
- Fix: In collect_report, if the child is in guard.stopped_children, use Interrupted instead of the outcome-derived status, as upstream does.

### [low] Subagent sessions' events are not parked while their parent holds a wake
- `crates/provider-opencode/src/adapter/v2.rs:1322` · dimension: regressions · upheld 2/2
- Evidence: park_or_wake looks up only guard.threads.get(session_id) (v2.rs:1322-1330), so a subagent session never parks. Its events go to child_execution and are processed live. Upstream holderOf (adapter.ts:2475-2484) parks a subagent's events while the parent thread holds a running wake, and the event handler pushes them to held.events (adapter.ts:2564-2572), so they replay in order with the wake. The v2.rs:1319 doc comment claims 'holderOf' is implemented, but the subagent branch is missing.
- Fix: Mirror holderOf: when the session is a subagent (subagents map or a parent link), find its parent thread. If that thread is idle and its newest wake is running, park the event on that wake and replay it with the wake.

### [medium] v1 question ids use the fallback header, so answers to a blank-header question are dropped
- `crates/provider-opencode/src/adapter/records.rs:492` · dimension: regressions · upheld 2/2
- Evidence: user_input_questions substitutes 'Question N' for a blank header (records.rs ~450-455), then derives the id from that fallback: open_code_question_id(index, &header) (records.rs:492), giving 'question-0-question-1'. v1 answer mapping derives the lookup key from the raw header: `let id = open_code_question_id(index, header);` (v1.rs:2358), giving 'question-0'. The app answers under the displayed id, so answers.get(&id) misses and the answer is lost. Upstream uses openCodeQuestionId(index, question) on the raw header for both (upstream/packages/provider-opencode/src/server/OpenCodeRuntime.ts:456-465), which yields 'question-0' when the header is blank. The U+FEFF fix changed the display id without updating the lookup.
- Fix: Compute the id from the raw (js-trimmed, unfallbacked) header in both places, with the fallback as in OpenCodeRuntime.ts openCodeQuestionId. Add a v1 round-trip test with a blank header.

### [medium] v1 approval items drop the prompt that upstream derives from the permission patterns
- `crates/provider-opencode/src/adapter/v1.rs:1480` · dimension: regressions · upheld 2/2
- Evidence: v1.rs:1480 sets `prompt: None` on the v1 approval body. Upstream v1 runtimeRequestTurnItem sets prompt = patterns.length === 0 ? permission.permission : patterns.join('\n') (upstream/packages/provider-opencode/src/server/adapter.ts:1808-1812). The Rust v1 path never reads the 'patterns' property. The known open item is only partly a gap: options None matches upstream v1, which emits no approval options (no `options:` on the approval item in adapter.ts). The gap is the prompt.
- Fix: Read props.patterns in the v1 permission path and set prompt to the joined patterns, or to the permission name when there are none. Leave options None, as upstream v1 does.

### [low] A live event can overtake parked wake events in the take/replay window
- `crates/provider-opencode/src/adapter/v2.rs:2661` · dimension: regressions · upheld 1/2
- Evidence: start_turn sets thread.active (v2.rs:2661) and releases the inner lock. drain_wake (v2.rs:2685-2687) or the running-wake take (v2.rs:2712-2721) happens later, under a separate lock hold. In between, the reader task's park_or_wake sees active.is_some() (v2.rs:1322-1330, returns false) and processes the new live event first. The older parked events then replay after it, reordering text deltas and tool events. Upstream serialises this with lock.withPermit (adapter.ts runWake, ~3576-3601). The window is narrow but reachable on a multi-threaded runtime.
- Fix: Take the wake and mark the turn as replaying in the same lock hold as thread.active is set, or keep a 'replaying' flag that parks live events until replay completes.
