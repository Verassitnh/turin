# QC wf_1d598789-d6c — T13b orchestration pipeline (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 8 · refuted: 1

## Lead report

1. low: crates/orchestration/src/pipeline/thread_management_service.rs:1105,1109 (comment 384-387). The settle-after-run task captures `Arc::clone(self)` while the struct owns the JoinSet that holds the task, so the struct cannot drop and the task is never aborted. The wait runs up to 24h, and `thread.settle` can then fire from a torn-down service. Fix: pass a `Weak` or an inner state that does not own the JoinSet, and correct the comment, which says the service aborts its tasks on drop. Latent: `settle_thread` has no production caller here.

2. low: crates/orchestration/src/pipeline/event_sink.rs:569-574 (also write_conditional 591-607 and run-current near 811). Publish runs only after `db.write(..).await`. If the caller is dropped after the blocking transaction starts, the events commit but are never published, so live subscribers and outbox notify are skipped until restart or the 30s poll. Fix: run the write and publish in an owned task and await its JoinHandle, as attachment_claims.rs:131-150 does. Also log the divergence from upstream's rollback-on-interrupt in PORTING.md Deviations. Latent: no production caller drops these futures yet.

3. low: crates/orchestration/src/pipeline/thread_settlement_service.rs:961-966 and 625-626. A panic in a sweep item kills the DrainableWorker task (crates/shared/src/drainable_worker.rs:98, outside stated scope), and the next `drain()` then hangs forever, silently stopping the 60s sweep. Upstream DrainableWorker.ts:65-69 skips failed items and keeps going. Fix: wrap `process(item)` in `catch_unwind` in DrainableWorker, or in run_sweep, and log the panic as a failed item. No reachable panic was found in pipeline code itself.

4. low: crates/orchestration/src/pipeline/user_facing_errors.rs:104-106. The selection takes the last non-generic message without first-occurrence dedupe. Upstream dedupes with `indexOf` and then takes `.at(-1)`. For [X, Y, X] upstream returns Y and Rust returns X. Fix: dedupe to first occurrence before taking the last element, and add a test for [X, Y, X]. Not in Deviations.

5. low: crates/orchestration/src/pipeline/user_facing_errors.rs:83-88. The detail/message fallback uses `get("detail").or_else(...)`, so a blank or non-string detail drops the message. Upstream uses `textValue(detail) ?? textValue(message)`. For {detail:"", message:"Real"} upstream yields "Real" and Rust yields None. Fix: `["detail","message"].iter().find_map(|k| value.get(*k)?.as_str().map(str::trim).filter(|s| !s.is_empty()))`. No live caller yet.

6. low: crates/orchestration/src/pipeline/user_facing_errors.rs:18 (match table 12-17). `strip_suffix("Agent")` is case-sensitive, but upstream `/Agent$/i` is not, so "myagent" renders "Myagent" and not "My". The match table also differs from upstream PROVIDER_DISPLAY_NAMES: acpRegistry should be "ACP Registry" (Rust gives "AcpRegistry") and muse should be "Muse Code" (Rust gives "Muse"). Fix: strip the suffix case-insensitively and align the table with upstream display names. Add tests for both.

7. low: crates/orchestration/src/pipeline/resource_cleanup_service.rs:40-48 (module doc lines 3-4). `cleanup_terminals` and `cleanup_previews` are unconditional no-ops. The doc calls this upstream's defaultValue, but upstream's live layer calls terminals.close(deleteHistory) and previews.close. PORTING.md has no ResourceCleanup Deviations row, although docs/handoff/T13b.md:39 promises one. Fix: add the Deviations row, correct the doc comment, and name the consumer of the terminal.cleanup and preview.cleanup effects (thread_deletion.rs:358-364). No user data is lost, since terminal and preview are cut.

8. low: crates/orchestration/src/pipeline/thread_settlement_service/pure_tests.rs:~248 (helper 96-110). The upstream assertion with autoSettleKeepPullRequests=true plus a merged PR is not ported, because the helper hard-codes keep_pull_requests=false. Production logic is correct. Fix: set keep_pull_requests=true on that call and assert at(-4 DAY).

Verdict: PASS (no critical or high findings; 8 low, all test-fidelity, hardening, or doc/Deviations gaps, with item 1 the most worth fixing before the settle path is wired up).

## Confirmed findings (raw)

### [low] Dispatch error picks wrong message when the deepest message repeats an earlier one
- `crates/orchestration/src/pipeline/user_facing_errors.rs:104` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust: `messages.into_iter().rev().find(|m| !GENERIC...)` returns the last non-generic message in collection order. Upstream UserFacingErrors.ts dedupes first (`allMessages.indexOf(message) === index`) and then takes `.at(-1)`. For messages [X, Y, X] (e.g. cause {message:'Y', cause:{message:'X'}} under outer message 'X') upstream returns 'Y' and Rust returns 'X'. Verified with node: upstream filter yields ['A','B'] -> 'B'; Rust logic yields 'A'.
- Fix: Dedupe with first-occurrence semantics before selecting, e.g. collect into a Vec, keep only the first index of each distinct message, then take the last kept element (or mirror upstream's indexOf filter exactly). Add a test for [X, Y, X].

### [low] detail/message fallback does not match upstream textValue chain
- `crates/orchestration/src/pipeline/user_facing_errors.rs:83` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust: `value.get("detail").or_else(|| value.get("message")).and_then(|v| v.as_str())`. Upstream: `textValue(value.detail) ?? textValue(value.message)`, where textValue returns undefined for non-strings and blank strings, so a blank or non-string detail falls through to message. For {detail:"", message:"Real"} upstream yields "Real" (verified with node) while Rust yields None (Some("") is picked, then dropped as empty). Same for {detail:5, message:"Real"}.
- Fix: Apply the same per-field check as textValue to detail, falling back to message when detail is absent, non-string, or blank: `["detail","message"].iter().find_map(|k| value.get(*k)?.as_str().map(str::trim).filter(|s| !s.is_empty()))`.

### [low] Provider display-name fallback strips 'Agent' case-sensitively
- `crates/orchestration/src/pipeline/user_facing_errors.rs:18` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust: `id.strip_suffix("Agent")`. Upstream: `instanceId.replace(/Agent$/i, "")`, which is case-insensitive. For a custom instance id such as 'myagent', upstream renders 'My' and Rust renders 'Myagent'.
- Fix: Strip the suffix case-insensitively (e.g. check `id.len() >= 5 && id[id.len()-5..].eq_ignore_ascii_case("agent")`) before trimming and capitalising.

### [low] Terminal and preview cleanup are no-ops with no Deviations entry
- `crates/orchestration/src/pipeline/resource_cleanup_service.rs:40` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust: `pub async fn cleanup_terminals(&self, _thread_id: &str) -> Result<(), ...> { Ok(()) }` and the same no-op for `cleanup_previews`. Upstream ResourceCleanupService.ts calls `terminals.close({threadId, deleteHistory: true})` and `previews.close({threadId})`. The module doc calls this a cut feature, but PORTING.md's Deviations table (ground rule 4) has no ResourceCleanup row, and nothing in the crate calls this service, so the terminal.cleanup and preview.cleanup effects queued by thread_deletion.rs:358-364 have no documented consumer here.
- Fix: Add a Deviations row for resource_cleanup_service.rs (upstream closes terminals with deleteHistory and previews; Turin no-ops because terminal/preview are cut), and state where terminal.cleanup/preview.cleanup effects are handled.

### [low] Upstream merged-PR-with-keep-pull-requests assertion not ported
- `crates/orchestration/src/pipeline/thread_settlement_service/pure_tests.rs:248` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream ThreadSettlementService.test.ts:254-285 ends with `resolveAutoSettlementAt({...input, pullRequest: {state:'merged', mergedAt: at(-1)}})` while `input` has `autoSettleKeepPullRequests: true`, expecting at(-4 DAY). The Rust port builds `input(&thread, Some(merged_pull_request(-1)), Some(3.0), true)`, and that helper sets keep_pull_requests=false, so the keep=true + merged combination is never asserted.
- Fix: Set `keep_pull_requests = true` on that third call (e.g. build the input as in the earlier `keep` binding and override only pull_request) so the upstream assertion is reproduced.

### [low] Settle-after-run task holds an Arc to its own ThreadManagementService, so the service's teardown never aborts it
- `crates/orchestration/src/pipeline/thread_management_service.rs:` · dimension: safety-and-concurrency · upheld 2/2
- Evidence: Line 1105 `let service = Arc::clone(self);` and line 1109 `background.spawn(async move { service.settle_after_run(...) })`, where `background: Mutex<JoinSet<()>>` is a field of ThreadManagementService (line 388). The task owns an Arc to the struct that owns the JoinSet, so the reference cycle keeps the struct alive until the task finishes. The task waits up to SETTLE_AFTER_RUN_WAIT_MS = 24h (line 61, `wait_for_thread` at line 1071). The comment at lines 384-386 says 'the service owns the tasks and aborts them when it drops', which is false for this path. Upstream ThreadManagementService.ts:~940 uses `Effect.forkIn(layerScope)`, which is interrupted when the layer closes. Result: after the owner drops its last external Arc, the wait keeps running, and when the agent's run completes it dispatches `thread.settle` from a service that was already torn down. There is no Drop impl for ThreadManagementService in pipeline/*.rs.
- Fix: Do not capture Arc<Self> in the spawned task. Pass the needed deps (orchestrator, settings, etc.) as a Weak or an Arc of an inner state that does not own the JoinSet. Alternatively, keep the JoinSet in a separate handle that the owner drops. Correct the comment at lines 384-386 to match the actual lifetime.

### [low] Write path publishes only after the commit future resolves, so a dropped caller leaves committed events unpublished
- `crates/orchestration/src/pipeline/event_sink.rs:` · dimension: safety-and-concurrency · upheld 2/2
- Evidence: `write` at lines 569-574: `let appended = self.db.write(move |tx| sink.write_in(tx, input)).await...?; self.publish(&appended, effect_count);`. `write_conditional` (591-607) and the path at 811 follow the same shape. In turin-persistence db.rs:137-151, the commit runs inside `AbortOnDrop::new(spawn_blocking(..))`. If the caller future is dropped after the blocking transaction has started, the transaction still commits, but `publish_committed`, the live fan-out and `outbox.notify_available` never run. Live subscribers miss those events until a restart replay, and claimers only recover on the 30 s liveness poll. No in-crate timeout or select wraps dispatch today, so this needs a dropping caller (for example a GPUI task being cancelled).
- Fix: Run the post-commit publish inside a task that is not tied to the caller's future, for example spawn the `write` body as an owned task and await its JoinHandle, so a dropped caller still gets the publish. The PORTING.md 'uninterruptible region' pattern used for AttachmentClaims applies here too.

### [low] A panic in a settlement sweep is not contained and wedges the periodic sweep permanently
- `crates/orchestration/src/pipeline/thread_settlement_service.rs:` · dimension: safety-and-concurrency · upheld 2/2
- Evidence: `run_sweep` (line 961-966) only handles `Err` from `sweep`, with no catch_unwind. The DrainableWorker item loop (crates/shared/src/drainable_worker.rs:98, `match process(item).await`) has no panic guard either. A panic kills the worker task. The periodic loop at thread_settlement_service.rs:625-626 then calls `inner.worker.drain().await`, which waits for `outstanding == 0`, and the queued items are never processed, so it hangs forever and the 60 s sweep stops silently. Upstream ThreadSettlementService.ts runSweep/sweep use `Effect.catchCause`, which contains defects too. I did not find a reachable panic site in the settlement code (the `expect`s are guarded), so this is a hardening gap rather than a demonstrated crash.
- Fix: Wrap each item in `futures::FutureExt::catch_unwind` (as effect_worker.rs does) inside DrainableWorker, or in run_sweep, and log the panic as a failed item so the worker keeps going.
