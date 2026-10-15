# QC wf_51d214b1-870 — T12 cursor adapter (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 24 · refuted: 3

## Lead report

HIGH

1. crates/provider-cursor/src/session.rs:1300-1316 (register_handlers, closures at 1302-1376, 1382-1470, 1481-1500; SessionInner held at 305; Drop at 168-175). Reference cycle: SessionInner -> AcpClient handler map -> closures -> Arc<SessionInner> means SessionInner never drops. The cursor-agent process group (ProcessGuard) is never SIGKILLed, including when open_session initialize fails (959-966). Pending permission, ask and plan waiters (1373, 1422, 1466) never resolve, because on_termination (client.rs:110) only calls fail_prompts. Fix: capture Weak<SessionInner> in the handler closures and upgrade per call. Fail SessionInner.pending from a termination hook. In crates/acp/src/protocol.rs:327-333, make the writer loop exit on termination (it holds Arc<ConnectionInner>, which owns write_tx). Add a test that dropping CursorSession kills the child.

2. crates/provider-cursor/src/driver.rs:216-226, 113-114, 135-147. Snapshot is a stub: installed false, auth Unknown, models Vec::new(), usage_limits None, snapshot_for_cwd and refresh_models None, apply_usage_limits a no-op. Upstream driver.ts:220-300 wires status, usage limits, skills and model catalog. Fix: port status (probe_initialize, status_from_auth_methods), model discovery (session.rs:366 list_models), usage limits (read_cursor_account_usage) and the skills probe, or add PORTING.md Deviations rows that cut each one.

3. crates/provider-cursor/src/session.rs:836-867 (emit_update; id `cursor-item-{ordinal}` at 842; chunk-only text at 865; Completed at 859; streaming true at 867). Each agent_message_chunk becomes a separate AssistantMessage with only its own text, and no finalizing update is sent. events.rs:59-66 parses messageId but emit_update never uses it. Fix: key one segment per ACP messageId (else per turn until a non-text update), accumulate text, send running/streaming:true while streaming, and send completed/streaming:false at segment end. The Thought branch (869-892) has the same defect.

4. crates/provider-cursor/src/session.rs:891 (`_ => return`), 799-824 (emit_runtime_request), 1459-1465 (CursorEvent::Plan), 1487-1491 (Todos). Plan, todo and standard ACP plan updates never reach ProviderAdapterV2Event. The plan runtime request has no markdown or steps, so the user approves a blank plan. PlanUpdated is never emitted, and capabilities.rs:83-86 advertise emits_plan_updated, emits_todo_list and emits_proposed_plan as true. Fix: emit PlanUpdated with markdown and steps, plus ProposedPlan and TodoList turn items and node.updated (kind plan/todo_list), per upstream provider-cursor adapter.ts:1337-1502. Also map the ACP plan update in emit_update.

5. crates/provider-cursor/src/session.rs:799-824 (emit_runtime_request), 1366-1372 (approval options), 1416-1421 (user-input questions). Approval and user-input requests emit only RuntimeRequestUpdated. No ApprovalRequest or UserInputRequest turn item carries prompt, options or questions, so the card cannot offer Allow-for-thread, Allow-always or Decline. Fix: emit node.updated, runtime_request and turn_item next to each request, using derive::approval_turn_item (provider-core id_allocator.rs:504) and the user-input equivalent. One verifier rated this medium because no orchestrator consumer exists yet.

MEDIUM

6. crates/provider-cursor/src/session.rs:891 (with 1305-1306). CursorUpdate::Tool never reaches ProviderAdapterV2Event. It goes only to the internal CursorEvent::Update channel, while capabilities.rs:66-70 advertise streams_tool_output, emits_tool_started, emits_tool_completed and emits_tool_output as true. Fix: map to node.updated (tool_call) and command_execution/file_change/file_search items per upstream adapter.ts:1120-1300, or set the capabilities false and log Deviations. No other Turin adapter emits tool items yet, so this is partly a pipeline-wide gap.

7. crates/provider-cursor/src/account_usage.rs:274 (prefetch spawned into `ahead`), 294-300 (`?` returns before abort_ahead), 167-180 (60s timeout drops the future). Prefetch JoinHandles detach, so cookie-bearing page requests keep running up to PAGE_TIMEOUT (10s). This breaks PORTING.md:93-94. The timeout path at 180 also returns account_key None, where upstream keeps it (accountUsage.ts ~373-381). Fix: hold handles in a JoinSet or AbortOnDropHandle, or use a Drop guard that aborts all. Keep the account key on timeout. Add the upstream page-failure test (accountUsage.test.ts:371-411), which asserts no request is left open.

8. crates/provider-cursor/src/session.rs:751-757 (terminal_status), 1167-1181 (interrupt_turn). User interrupt sends session/cancel, the stop reason is "cancelled", and the turn is reported as Cancelled, never Interrupted (provider_adapter.rs:82-87). Upstream adapter.ts:2054-2058 returns interrupted when an interrupt was requested. Fix: set an interrupted flag on ActiveTurn in interrupt_turn, and map a cancelled stop reason to Interrupted when it is set. Extend the test at 1921-1975, which checks only stop_reason.

9. crates/provider-cursor/src/session.rs:930-934 (open_session reads only cwd), 397-405 (set_mode, no caller), driver.rs:208 (show_interaction_mode_toggle Some(false)). approval_policy, sandbox_policy, runtime_mode and interaction_mode are ignored. Plan interaction mode is a silent no-op, and the toggle is hidden (upstream status.ts:29 shows it; adapter.ts:317, 2214 apply mode "plan"). Fix: apply plan via session/set_mode on each turn and enable the toggle. Log the runtime-mode gap in PORTING.md Deviations, or wire the policy.

10. crates/provider-cursor/src/driver.rs:119 (auth: None), session.rs:351 (sign_in, no production caller), driver.rs:150-196 (text generation errors), driver.rs:230 (supports_text_generation Some(false)). Sign-in is unreachable from ProviderInstance (upstream driver.ts:282 exposes auth.controller). Commit, PR, branch and title generation is cut with no Deviations row, although thread title is a kept feature. Fix: wire a ProviderAuthController or log the cut, and add a Deviations row for text generation.

11. Test coverage. The page-failure test (accountUsage.test.ts:371-411) is not ported, and the account_usage.rs tests (about 759-1060) do not cover it. The accountCache and usageLimits features are unported with no Deviations row. Fix: port the page-failure test first, and list every cut test file in Deviations.

LOW

12. session.rs:271-283, 987-989. provider_session() returns the unpublished Starting record with static capabilities, because publish_session (624-636) only emits an event. Fix: store the published session (Mutex or OnceLock) and return it.

13. capabilities.rs:132-137 vs session.rs:1254-1267. can_read_thread_snapshot is true when loadSession is advertised, but read_thread_snapshot always errors. Fix: set it false, or implement the read via session/load replay, and log the choice.

14. session.rs:933, 1017, 1064. The cwd fallback "." goes to session/new and session/load, but the schema says cwd must be absolute (crates/acp/schema/schema-v1.21.0.json). No current caller passes None. Fix: resolve to an absolute path, then add a test asserting an absolute cwd when None is passed.

15. extensions.rs:75-80. Cancelled todos map to Pending and blank content falls back to the id. Upstream filters both (adapter.ts:514-527). Only the public CursorSession::recv exposes these steps.

16. account_usage.rs:585-597. billed_event_identity hashes serde_json f64 values, giving "1700000000000.0" where JSON.stringify gives "1700000000000", so dedupeKey differs from upstream. The js_number_string helper at account_usage.rs:709 can be reused. Log it or fix it.

17. permissions.rs:140-149. The `_q` substring test flags ids like tool_queue_3. The flag has no consumer. Fix: match the `_q\d+$` suffix or delete the field.

18. capabilities.rs:102-105. The context handoff flags are false while supports_provider_switching_via_handoff (line 35) is true. Upstream sets them true (provider-acp adapter.ts:610-613). supports_mcp_tools (line 71) is also unlogged. Log them in Deviations or align them.

19. session.rs:268, 282-309, 1304-1306. The unbounded CursorEvent channel grows with every streamed update, and its only consumers are in-crate tests. Fix: remove it or bound it.

20. permissions.rs:126-137 with session.rs:1342-1347. If a request has neither title nor rawInput, its grant key is only the kind. After one AcceptForSession, every later request of that kind is auto-allowed. Fix: record no key, and always show the card, when both fields are absent.

21. session.rs:1121-1130. start_turn clears active_turn only after the await. Dropping the future leaves the turn active, and AcpClient::prompts (client.rs:323-349) keeps a stale entry. start_turn also has no active-turn guard, which upstream has (adapter.ts:2190-2196). No current caller, so this is latent.

Excluded: the blocking std::fs config IO in acp_config.rs:65-67 is not worth reporting, since the repo already does this. Out of scope (crates/acp, not provider-cursor): process.rs:139-149 gates the kill and waitpid after the child is reaped. Gate only the waitpid on a reaped flag, and keep the group kill.

Verdict: FAIL (5 high-severity findings: items 1-5).

## Confirmed findings (raw)

### [medium] Tool calls never reach orchestration events
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:891` · dimension: logic-fidelity · upheld 2/2
- Evidence: emit_update (session.rs:836-901) matches only CursorUpdate::Text and Thought; `_ => return,` at line 891 drops CursorUpdate::Tool, Plan, Commands, Config, Mode, Usage. Tool updates go only to CursorEvent::Update on the recv() channel (session.rs:1305-1306), which ProviderSessionRuntime::events() never carries. Upstream adapter.ts:1120-1300 emits tool_call nodes plus command_execution / file_change / file_search turn items. PORTING.md 'Kept features' requires tool cards; docs/protocols/cursor-acp.md:68 requires tool card updates. No Deviations row covers this.
- Fix: Map CursorUpdate::Tool to node.updated (kind tool_call) and to command_execution / file_change / file_search turn items per adapter.ts:1120-1300, with streaming output. Any variant that is intentionally not ported needs a Deviations row.

### [high] Plan, todo and proposed-plan content never reaches orchestration
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:1459` · dimension: logic-fidelity · upheld 2/2
- Evidence: CursorEvent::Plan (markdown, steps) is sent only on the recv() channel (session.rs:1459-1465). cursor/update_todos is sent as CursorEvent::Todos only (session.rs:1487-1491, register_side). The plan runtime request built by emit_runtime_request (session.rs:799-824) has no markdown or steps, and ProviderAdapterV2Event::PlanUpdated (provider_adapter.rs:147) is never emitted. Upstream adapter.ts:1360-1501 emits node.updated kind plan, plan.updated, a proposed_plan turn item and a todo_list. docs/protocols/cursor-acp.md:68 requires plan card updates.
- Fix: Emit ProviderAdapterV2Event::PlanUpdated with the markdown and steps, plus TurnItem ProposedPlan and TodoList items, per adapter.ts:1360-1501. Cards cannot render without this content.

### [high] Approval and user-input requests carry no options, prompt or questions
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:799` · dimension: logic-fidelity · upheld 2/2
- Evidence: emit_runtime_request (session.rs:799-824) builds OrchestrationV2RuntimeRequest with only id, kind, status, decision and answers. The option list from permissions::approval_options (permissions.rs:95-122) reaches only CursorEvent::Approval (session.rs:1366-1372), and questions from extensions::user_input_questions only reach CursorEvent::UserInput (session.rs:1416-1421). Both are recv()-only. The contract carries these in TurnItem ApprovalRequest (prompt, options; orchestration_v2.rs:2806-2838) and UserInputRequest (orchestration_v2.rs:2649). PORTING.md Kept features lists approvals and pending user input.
- Fix: Alongside each RuntimeRequestUpdated, emit a TurnItem ApprovalRequest with prompt and options (permissions::approval_options) and a UserInputRequest with the questions (extensions::user_input_questions).

### [high] Assistant text is split into one message per ACP chunk
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:842` · dimension: logic-fidelity · upheld 2/2
- Evidence: emit_update derives a new id per chunk: `format!("cursor-item-{ordinal}")` (line 842), with ordinal incremented per update. Each item carries only the chunk (`text: text.clone()`, line 865), status Completed (line 859), completed_at set (line 862), and streaming: true (line 867). The ACP messageId is parsed (events.rs:62-65) but never used. No finalizing item update is sent when the turn ends (only TurnItemUpdated at session.rs:895-900, and TurnSettled at 564). Upstream adapter.ts:940-1030 keeps one segment per item id with accumulated text, streaming: !completed, and status running until completed.
- Fix: Keep one assistant segment per message (keyed by ACP messageId, else per turn until a non-text update). Accumulate text, send status running with streaming true while chunks arrive, and send a completed, streaming:false update when the segment ends.

### [medium] Failed page read leaves prefetch tasks running with the session cookie
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/account_usage.rs:294` · dimension: logic-fidelity · upheld 2/2
- Evidence: Prefetch tasks are spawned at line 274 into `ahead`. At line 294-296, `outcome.map_err(...)?` returns early without calling abort_ahead (abort is only at line 299 for Denied and line 323 on success). tokio JoinHandle drop detaches the task, so queued pages keep sending the WorkosCursorSessionToken cookie with 10s timeouts. Dropping the future on the 60s outer timeout (line ~178-180) also detaches them. Upstream accountUsage.ts uses Effect.forkScoped so pages are interrupted with the scope, and accountUsage.test.ts:371-411 asserts 'leaving no request open' for a page failure. The Rust tests do not cover this case. The timeout path also drops the account key (line 180 returns empty(), but accountUsage.ts keeps accountKey in its catch).
- Fix: Wrap the handles in a guard whose Drop aborts every JoinHandle, so both the early-return and the outer-timeout paths cancel in-flight requests. Add the upstream page-failure test. Keep the account key on the timeout path.

### [medium] User interrupt is reported as cancelled, not interrupted
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:751` · dimension: logic-fidelity · upheld 2/2
- Evidence: terminal_status (session.rs:751-757) maps only the ACP stop reason and never yields Interrupted. interrupt_turn (session.rs:1167-1181) calls session/cancel, so the prompt returns stopReason "cancelled" and the turn is emitted as Cancelled. Upstream adapter.ts:2050-2056 returns "interrupted" when context.interrupted is set. ProviderAdapterV2TurnTerminalStatus::Interrupted exists (provider_adapter.rs:82-87).
- Fix: Set an interrupted flag on ActiveTurn in interrupt_turn and emit ProviderAdapterV2TurnTerminalStatus::Interrupted when it is set, as adapter.ts:2054 does.

### [low] provider_session() returns the unpublished Starting session
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:987` · dimension: logic-fidelity · upheld 2/2
- Evidence: from_client_with builds provider_session with status Starting and cursor_capabilities() (session.rs:271-283). publish_session (session.rs:624-636) clones it and sends Ready with negotiated capabilities as an event only. ProviderSessionRuntime::provider_session() (session.rs:987-989) returns &self.inner.provider_session, which never changes, so it stays Starting with unnegotiated capabilities.
- Fix: Store the published session (for example in a OnceLock or Mutex-backed cell set by publish_session) and return it from provider_session().

### [low] Capability advertises thread snapshot reads that always fail
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/capabilities.rs:137` · dimension: logic-fidelity · upheld 2/2
- Evidence: negotiate_capabilities sets threads.can_read_thread_snapshot = loadSession (capabilities.rs:132-137), so it becomes true after initialize. read_thread_snapshot (session.rs:1254-1267) always returns Err. PORTING.md Deviations '(session ops)' says read_thread_snapshot returns the snapshot error, so the capability and the behaviour disagree. Upstream adapter.ts:~246 sets canReadThreadSnapshot true only because the SDK implements it.
- Fix: Keep can_read_thread_snapshot false in negotiate_capabilities, or implement the read. Either way, record the decision in Deviations.

### [low] cwd "." is sent to session/new and session/load
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:932` · dimension: logic-fidelity · upheld 2/2
- Evidence: open_session (session.rs:930-934), ensure_thread (1015-1019) and resume_thread (1062-1066) fall back to "." when runtime_policy.cwd is None, and that value goes to session/new or session/load. The vendored schema says NewSessionRequest.cwd and LoadSessionRequest.cwd are "Must be an absolute path" (crates/acp/schema/schema-v1.21.0.json). Upstream falls back to process.cwd() in providerSession (adapter.ts).
- Fix: Resolve the fallback to an absolute path (std::env::current_dir()) before session/new or session/load, and add a test that asserts an absolute cwd on the wire.

### [medium] Runtime mode and interaction mode are ignored
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:930` · dimension: logic-fidelity · upheld 2/2
- Evidence: open_session reads only runtime_policy.cwd (session.rs:930-934). approval_policy, sandbox_policy, runtime_mode and interaction_mode are never read (grep finds only the test fixture at session.rs:2026-2030). set_mode (session.rs:397-405) has no caller. driver.rs:208 sets show_interaction_mode_toggle to Some(false), but upstream status.ts:26-30 sets showInteractionModeToggle true. Upstream adapter.ts cursorRuntimeAgentPolicy (about adapter.ts:178-205) maps approval and sandbox modes, and makeCursorAgentOptions sets mode 'plan' for plan interaction. With a full-access runtime the user still gets a card for every permission. PORTING.md:228 documents losing autoReview and sandbox, not that the selected mode is a no-op.
- Fix: Map interaction mode 'plan' to set_mode, and turn the toggle on. Record in Deviations that runtime mode does not change approvals, or wire approvalPolicy 'never' to an explicit host policy.

### [high] Driver snapshot is a hard-coded stub with no status, models, usage limits or skills probes
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/driver.rs:139` · dimension: logic-fidelity · upheld 2/2
- Evidence: driver.rs:220-226 hard-codes installed: false, auth Unknown, models: Vec::new(), usage_limits: None. refresh() and get_snapshot() return the same value (driver.rs:135-141). snapshot_for_cwd (driver.rs:113) and refresh_models (driver.rs:114) are None. apply_usage_limits is a no-op (driver.rs:147). Upstream driver.ts create() wires checkCursorProviderStatus (status.ts), readCursorUsageLimits (usageLimits.ts), probeCursorSkills (skills.ts) and CursorSdkCatalog model discovery. None of those modules exist in the crate, and no Deviations row cuts them.
- Fix: Port status, models (via cursor/list_available_models), usageLimits and skills, or add Deviations rows that cut each one with a reason.

### [medium] Sign-in controller and text generation are missing without a Deviations row
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/driver.rs:119` · dimension: logic-fidelity · upheld 2/2
- Evidence: driver.rs:119 sets auth: None, so CursorSession::sign_in (session.rs:350-357) is unreachable from ProviderInstance. driver.rs:150-196 makes every text-generation method return 'Cursor ACP does not generate commit messages or titles.' Upstream driver.ts exposes auth.controller and textGeneration.ts supports commit, PR, branch and title generation. The credentials Deviations row covers only env/apiKey handling.
- Fix: Build a ProviderAuthController around CursorSession::sign_in, or record that explicit sign-in is cut. Add a Deviations row for text generation.

### [low] Cancelled and blank todos are kept as plan steps
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/extensions.rs:80` · dimension: logic-fidelity · upheld 1/2
- Evidence: plan_steps maps TodoStatus::Cancelled to OrchestrationV2PlanStepStatus::Pending (extensions.rs:80), and falls back to the todo id when content is blank (line 75-78). Upstream adapter.ts:514-527 cursorTodoSteps filters out status cancelled and content.trim().length === 0 before mapping.
- Fix: Filter cancelled and blank-content todos before mapping, as adapter.ts:514-527 does.

### [low] Dedupe keys hash a different number format than upstream
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/account_usage.rs:585` · dimension: logic-fidelity · upheld 2/2
- Evidence: billed_event_identity builds its input with serde_json::json! over an f64 timestamp (account_usage.rs:585-597). serde_json writes integral floats with a trailing .0 (1700000000000.0), while JS JSON.stringify in accountUsage.ts billedEventIdentity writes 1700000000000. The sha256 input, and so the dedupeKey, differs from upstream for the same billed event.
- Fix: Format the timestamp the way JSON.stringify does (integral values without .0), or document the change in Deviations, since the keys cannot match upstream.

### [low] ask_question fallback matches any tool id containing _q
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/permissions.rs:140` · dimension: logic-fidelity · upheld 2/2
- Evidence: is_ask_question_fallback (permissions.rs:140-149) returns true when tool_call_id.contains("_q"), a substring test. Ids such as tool_queue_3 match. The result feeds only CursorEvent::Approval.ask_question_fallback (session.rs:1371), which has no consumer in the repo.
- Fix: Match the exact fallback rule documented in docs/protocols/cursor-acp.md (the tool_q prefix or the skip option id), or delete the flag if it is unused.

### [low] Capability constants differ from upstream without Deviations rows
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/capabilities.rs:88` · dimension: logic-fidelity · upheld 2/2
- Evidence: Undocumented differences from upstream adapter.ts:~100-150: plan_deltas_have_item_ids false (line 88, upstream true); emits_subagent_lifecycle false (line 93) and can_wait_for_subagents false (line 94), upstream true; can_generate_summaries, can_consume_handoff_summaries, supports_delta_handoff and supports_full_thread_handoff false (lines 102-105), upstream true; exposes_native_turn_id false (line 50), upstream true. None of these appear in PORTING.md Deviations.
- Fix: Record each divergence in Deviations with its reason, or align the constants with upstream.

### [medium] Upstream Cursor tests are mostly not ported
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T12-cursor-adapter/crates/provider-cursor/src/session.rs:1497` · dimension: logic-fidelity · upheld 2/2
- Evidence: The upstream adapter.test.ts (8 tests, including tool, search and mode mapping at adapter.test.ts:224-945) has no Rust counterpart. Of accountUsage.test.ts (7 tests), the Rust tests cover about 4 and omit the page-failure no-open-request case (accountUsage.test.ts:371-411). status, usageLimits, skills, textGeneration, accountCache and CursorUsageAccounts tests are not ported, and no Deviations row cuts them. PORTING.md ground rule 5 requires pure-logic tests to be ported.
- Fix: Port the pure-logic cases (the page-failure case first) and list any cut test files in Deviations.

### [high] Reference cycle keeps CursorSession alive, so cursor-agent child is never killed and pending cards never resolve
- `crates/provider-cursor/src/session.rs:1300` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: register_handlers(&client, &inner) (session.rs:305) stores Arc<SessionInner> clones in the AcpClient handler map (session.rs:1300-1316). SessionInner owns client: AcpClient (session.rs ~138). The ACP reader/writer/handler tasks hold Arc<ConnectionInner> (crates/acp/src/protocol.rs:135-136), whose on_request callbacks hold the handler map. The cycle is SessionInner -> AcpClient -> Connection -> tasks -> handlers -> SessionInner, so SessionInner::drop (session.rs:168-175, which cancels pending waiters) never runs and AcpClient's ProcessGuard (crates/acp/src/process.rs:139-149, SIGKILL to the process group) is never dropped while the child stays alive. Runtime probe in a scratch crate: dropping AcpClient directly kills a `sleep` child (control: alive=false); dropping CursorSession::from_client leaves it alive (session: alive=true). The same leak hits open_session when initialize fails (session.rs:959-966). Permission, ask and plan waiters in SessionInner.pending (session.rs:1373, 1422, 1466) are only resolved by answer_request, cancel_turn or that dead Drop. Nothing resolves them on child EOF, since on_termination only calls fail_prompts (client.rs:110). Fix: have the handler closures hold Weak<SessionInner> and upgrade per call, and fail SessionInner.pending from a termination hook. The writer task also holds Arc<ConnectionInner>, which owns write_tx (protocol.rs:55-62, 135), so it never exits; that is an acp-crate issue to fix in the same change.
- Fix: Capture Weak<SessionInner> in register_handlers closures (session.rs:1302-1376, 1382-1470, 1481-1500) and upgrade inside each handler; add a termination callback that calls cancel_pending on every pending entry; make the writer loop exit when the connection terminates.

### [low] Session event channel is unbounded and has no drain
- `crates/provider-cursor/src/session.rs:1302` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: inner.events is an unbounded_channel (session.rs:268). Every session update is cloned into it (session.rs:1302-1307: inner.events.send(CursorEvent::Update(update))), and so are approvals, questions, plans, todos, task and image payloads (session.rs:1382-1420, 1481-1500), plus TurnSettled (session.rs:553). The only reader is CursorSession::recv (session.rs:334-336), and no caller exists in the workspace (grep for CursorSession/recv finds none outside this crate). The provider-facing events() stream reads a different channel. Streamed text and raw payloads therefore accumulate for the life of the session.
- Fix: Either remove the CursorEvent channel when nothing consumes it, or make it bounded (mpsc::channel with try_send and a drop policy) and document the single consumer.

### [low] Permission grant key collapses when rawInput or title is absent
- `crates/provider-cursor/src/permissions.rs:126` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: session_grant_key uses raw_input = ...get("rawInput").map(Value::to_string).unwrap_or_default() and title.unwrap_or("") (permissions.rs:126-137). ACP makes rawInput optional (upstream/packages/effect-acp/src/_generated/schema.gen.ts:6559 `rawInput: Schema.optionalKey`, and title is optional too). handle_permission (session.rs:1336-1346) auto-answers any later request whose key matches. So after one AcceptForSession on a request with no rawInput and no title, every later request of the same tool kind gets allow_once with no card, for example any execute or edit call. The PORTING deviation row describes the key as kind+title+rawInput but does not cover the absent-field case.
- Fix: Do not record or match a session grant when the request has no rawInput and no title (return no key and always show the card), or include the full toolCall JSON minus ids in the key.

### [low] Dropped JoinHandles detach account-usage prefetch tasks that keep cookie-bearing requests running
- `crates/provider-cursor/src/account_usage.rs:274` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: read_with_token pushes tokio::spawn handles into a local VecDeque `ahead` (account_usage.rs:264-274). The outer tokio::time::timeout(READ_TIMEOUT) (account_usage.rs:167-170) drops that future on expiry, and the early `?` at account_usage.rs:294-300 returns without calling abort_ahead. Dropping a JoinHandle detaches the task, so up to six prefetch requests keep running with the cookie-bearing request, past the 60s overall bound. This breaks the PORTING rule 'Never drop a bare JoinHandle' and the Effect.timeout rule (the work must actually stop). Each task is still capped by PAGE_TIMEOUT. Note: read_cursor_account_usage has no non-test caller in this branch.
- Fix: Hold the prefetch tasks in tokio_util::task::JoinSet or AbortOnDropHandle so they are aborted when the read future is dropped.

### [low] Blocking std::fs config IO runs on async paths on every turn
- `crates/provider-cursor/src/acp_config.rs:65` · dimension: safety-and-lifecycle · upheld 1/2
- Evidence: with_user_config is async but calls snapshot_user_config (fs::read, acp_config.rs:75-84) and restore_user_config (fs::write/remove_file/create_dir_all, acp_config.rs:149-164) inline (acp_config.rs:65-67). It runs inside apply_model_selection (session.rs:385-395), which start_turn calls on every turn (session.rs:1113), and inside open_with_snapshot (session.rs:484). load_token also uses fs::read_to_string inside async read_cursor_account_usage (account_usage.rs:192). These block a tokio worker thread on the turn path.
- Fix: Wrap the file operations in tokio::task::spawn_blocking (or tokio::fs), keeping the snapshot and restore ordering.

### [low] Dropping the start_turn future leaves the turn active and the agent running
- `crates/provider-cursor/src/session.rs:1129` · dimension: safety-and-lifecycle · upheld 1/2
- Evidence: start_turn sets active_turn (session.rs:1119-1124), then awaits self.prompt (session.rs:1129). It clears active_turn only after the await (session.rs:1130). If the orchestrator drops or aborts this future mid-prompt, no session/cancel is sent, active_turn stays Some, and any permission waiters stay pending. Only interrupt_turn (session.rs:1167-1176) cancels the agent.
- Fix: Use a drop guard that clears active_turn and sends session/cancel (or uses AbortOnDrop semantics) so an aborted turn stops the child's work.

### [low] Out of scope (crates/acp): ProcessGuard kills and waits on a pid that may already be reaped
- `crates/acp/src/process.rs:139` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: exit_error awaits child.wait() on EOF (process.rs:120), which reaps the child. ProcessGuard::drop then calls libc::kill(-pid, SIGKILL) and a blocking libc::waitpid(pid, null, 0) on the same pid (process.rs:145-148) with no check that the child is still ours. If the pid was reused, the kill can hit an unrelated process group and the blocking waitpid can reap or block on an unrelated child of Turin, and it runs on whatever thread drops the client.
- Fix: Record that the child was reaped (e.g. an AtomicBool set in exit_error) and skip the kill and waitpid once it is set, or reap only through the tokio Child.
