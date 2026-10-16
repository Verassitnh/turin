# QC wf_8fab4f86-275 — T22a app live read path

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 10 · refuted: 1

## Lead report

1. MEDIUM. crates/app/src/app_runtime/feeds.rs:33-38. The shell feed is terminal on any stream error (live-hub overflow at 1000 items / 8 MiB, or a db read error in persistence/orchestration_event_store.rs:415/419/441). mod.rs:127-132 spawns it once and nothing resubscribes, so the sidebar stays stale until restart. Required fix: supervise the feed. On Err, back off (upstream rpc/client.ts subscribeDynamic, expected-failure retry: 250 ms doubling to 30 s) and re-run load_shell plus the stream from the new snapshot sequence. The thread feed has the same terminal return (feeds.rs:135-139, 152-156), recoverable only by close and reopen. If not fixed, log the choice in docs/handoff/T22.md Deviations.

2. MEDIUM. crates/orchestration/src/projection/store/snapshots.rs:34-40 (BoundedThreadSnapshot has no watermark field) and crates/app/src/app_runtime/feeds.rs:113 (`latest_local_turn_ordinal: None`). No crate computes the watermark. The client (crates/app/src/state/orchestration_v2_projection.rs:174-186) uses it for partial windows with no local rows, so a late update to an unloaded older item is appended as newest. Required fix: add `latest_local_turn_ordinal: Option<u64>` to BoundedThreadSnapshot, computed as the max ordinal over the full projection.turn_items before bounded_turn_items replaces them (snapshots.rs ~76-92), and pass Some(..) at feeds.rs:113. Add the upstream threadHistoryPaging.test.ts:462 case. Impact is narrow.

3. MEDIUM. crates/orchestration/src/projection/store/snapshots.rs (no #[cfg(test)] module; bounded_turn_items at :100, get_bounded_thread_snapshot at :71). PORTING.md rule 5 is unmet: the upstream buildBoundedThreadProjection cases in threadHistoryPaging.test.ts (408-741) are not ported. Missing cases: inherited-only watermark (462), run_interrupt_request retention (498, 520), local-row double charge (549). Required fix: port those as #[cfg(test)] tests beside bounded_turn_items and get_bounded_thread_snapshot, with the watermark test passing once item 2 lands. Record any remaining divergence in the T22.md Deviations table. Message and plan trimming (610, 641) is already logged at T22.md:103 and is excluded.

4. LOW. crates/app/src/app_runtime/feeds.rs:40-46 (the shell refetch error path, merged from two duplicate entries). On a per-aggregate refetch error the event is logged and dropped, the cursor has already advanced, and the row stays stale until the next event for that aggregate. Upstream ws.ts:1025-1051 fails the stream instead. Required fix: retry the refetch with bounded backoff. If it still fails, fail the shell feed and resync (see item 1). Otherwise add a Deviations row with the reason.

5. LOW. crates/app/src/app_runtime/mod.rs:111-116 (also :90-99). A shell load, open, or data-dir failure logs and returns without sending ClientStateEvent::ShellFeed(FeedStatus::Failed). The store stays Empty, and the dropped request receiver silently ignores later OpenThread requests. Required fix: send Failed before returning in the serve() branch. For the run() branches, either send Failed or document the intent (the doc comment at :91-97 covers the DB-open case).

6. LOW. crates/app/src/app_runtime/feeds.rs:124-152. The thread feed forwards every raw event. Upstream ws.ts:787-790 pipes it through coalesceThreadLiveStream (50 ms window, 512 cap). The port exists at crates/orchestration/src/projection/thread_live_event_coalescer.rs but has no call site, and PORTING.md (~line 501) says the window is kept. Required fix: wire the coalescer into the thread feed (pump task, rebuild Event items), or add a Deviations row. Final state is unchanged; only intermediate frames differ.

7. LOW. crates/app/src/app_runtime/mod.rs:140. `Some(_) = tasks.join_next()` discards the JoinError, so a panicking feed never sends ThreadFeed(Failed) and the thread stays Connecting or Synchronizing. Required fix: match `Some(Err(e)) if e.is_panic()`, log with tracing::error! including the thread id, and send ThreadFeed Failed for that thread (tag tasks with their thread id). Do not log aborts from requests.rs:30-32 and :40-43. No reachable panic was found, so this is latent.

8. LOW. crates/orchestration/src/projection/store/snapshots.rs:78-82. The byte budget is not ported: there is no windowBudget or dependency reserve, and local rows are charged once. This matters only for threads with no user turn (upstream threadHistoryPaging.ts ~229), where the window can be up to about 2x upstream. It is an intentional cut under the socket-frame rule (thread_history_paging.rs:9-11, PORTING.md:36-37), but the T22.md:102 row "Same result" is inaccurate. Required fix: correct that Deviations row to name the byte-budget cut and the no-user-turn case.

Dropped: mod.rs:61 blocking std::fs::create_dir_all. Verdicts split, and the PORTING.md rule it was measured against is misquoted. It is one startup mkdir and not a concrete violation.

Checkout: /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T22a-live-read

PASS (no critical/high; highest is medium)

## Confirmed findings (raw)

### [medium] Bounded thread snapshot never carries latestLocalTurnOrdinal watermark
- `crates/app/src/app_runtime/feeds.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: feeds.rs:113 `latest_local_turn_ordinal: None,`. BoundedThreadSnapshot (crates/orchestration/src/projection/store/snapshots.rs:34-40) has no such field, and no crate computes it (grep for latest_local_turn_ordinal in crates/orchestration returns nothing). Upstream threadHistoryPaging.ts buildBoundedThreadProjection returns `latestLocalTurnOrdinal = computeLatestLocalTurnOrdinal(input.projection.turnItems)` (max ordinal over the FULL projection), and ThreadStream.ts boundedSnapshotResponseFields forwards it. The Turin client (crates/app/src/state/orchestration_v2_projection.rs:174-186, is_below_partial_window) relies on it: with None and a window that has no local rows, oldest_local_turn_ordinal returns None, so a late turn-item update for an unloaded older item is not dropped and is appended as newest. Upstream test threadHistoryPaging.test.ts:462 ('carries a full-projection watermark when the bounded window is inherited-only') asserts exactly this. Not listed in docs/handoff/T22.md Deviations.
- Fix: Add latest_local_turn_ordinal: Option<u64> to BoundedThreadSnapshot, computed as max ordinal over the full projection.turn_items before the window replaces them (port computeLatestLocalTurnOrdinal), and pass Some(..) in feeds.rs:113 instead of None. Add the upstream:462 test.

### [low] Bounded snapshot byte budget not ported for threads without user turns
- `crates/orchestration/src/projection/store/snapshots.rs:` · dimension: logic-fidelity · upheld 1/2
- Evidence: snapshots.rs:78-82 calls select_recent_timeline_window(&projection.visible_turn_items, seq, None, &projected_row_encoded_bytes). Upstream threadHistoryPaging.ts buildBoundedThreadProjection instead computes windowBudget = max(0, maxEncodedBytes - dependencyReserve - controlBytes - 1024), passes windowPolicy {maxUserTurns, maxItems: 75, maxEncodedBytes: windowBudget}, and rowEncodedBytes = projectedRowBoundedSnapshotEncodedBytes (local rows charged twice, nested item counted again). selectOlderTimelinePage only charges bytes when turnLimit is undefined (no user_message turn start in the window), so for a thread with no user turns the byte cap applies. Turin uses the full 1 MiB cap and single-count bytes, so it can send a larger window than upstream. The handoff Deviations table says 'Same result' and does not mention the byte budget. Orchestration crate has no bounded_turn_items budget or dependency-reserve code (grep for dependency_reserve/window_budget/payload_budget returns nothing).
- Fix: Port the windowBudget computation and bounded row cost (projectedRowBoundedSnapshotEncodedBytes) into get_bounded_thread_snapshot, or log the difference in the Deviations table with the reason.

### [medium] Upstream bounded-projection unit tests not ported
- `crates/orchestration/src/projection/store/snapshots.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: snapshots.rs has no #[cfg(test)] module. bounded_turn_items (snapshots.rs:100) has no unit test. The only coverage is crates/app/src/app_runtime/tests.rs (4 read-path tests that check counts such as 22 of 26 items). Upstream upstream/apps/server/src/orchestration-v2/threadHistoryPaging.test.ts has pure-logic cases for buildBoundedThreadProjection at lines 408-741, including 462 (inherited-only watermark), 498 and 520 (run_interrupt_request retention outside the window), 549 (local row.item double-charge), 610 (message table not carried), 641 (control details omitted), and 687 (oversized actionable state). PORTING.md rule 5 requires porting pure projection tests.
- Fix: Port the interrupt-retention, inherited-only watermark, and message/plan trimming cases as #[cfg(test)] tests next to bounded_turn_items and get_bounded_thread_snapshot. Where the Turin implementation diverges, record it in the Deviations table.

### [low] ThreadLiveEventCoalescer ported but not wired into the thread live feed
- `crates/app/src/app_runtime/feeds.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: feeds.rs:125-129 streams events with `stores.sink.stream(StreamOptions{..})` and forwards each event raw. Upstream ws.ts:787-790 pipes eventStreamFrom through `coalesceThreadLiveStream` (50 ms window, 512-update cap, running tool updates coalesced). crates/orchestration/src/projection/thread_live_event_coalescer.rs is a full port but has no call sites (grep for coalesce_live_tool_updated_events / ThreadLiveEventCoalescer finds only its own file and mod.rs). This deviation is not in docs/handoff/T22.md. The client receives intermediate running tool-update events that upstream drops.
- Fix: Wrap the feed's event stream with ThreadLiveEventCoalescer in feeds::thread, or record the omission in the Deviations table with the reason.

### [low] Shell refetch failure silently drops the update instead of failing the feed
- `crates/app/src/app_runtime/feeds.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: feeds.rs:40-46: on Err from shell_item, the code runs tracing::warn!("dropped a shell update") and continues. Upstream ws.ts projectShellItems uses Effect.forEach(..., {concurrency: 8}) over projectItem/getThreadShell, so a failed refetch fails the shell stream rather than skipping one delta. In Turin, a sidebar entry whose refetch failed stays stale until another event for that aggregate arrives, and the handoff Deviations table does not mention it.
- Fix: Either surface the error as FeedStatus::Failed on the shell feed like the thread feed does, or document the choice in the Deviations table with the reason.

### [medium] Shell feed dies permanently on any stream error and is never restarted
- `crates/app/src/app_runtime/feeds.rs:36` · dimension: robustness · upheld 2/2
- Evidence: feeds.rs:33-38: `Err(error) => { ... sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(message))); return; }`. serve() (mod.rs:128) spawns feeds::shell exactly once and nothing re-subscribes. The error source is drive_stream in crates/persistence/src/orchestration_event_store.rs: `sub.fail_if_over_budget()?` (415, 441) when the live hub overflows LIVE_STREAM_MAX_ITEMS=1000 / 8 MiB, and `db.read(...).await?` (419-420) on any transient SQLite error. The reducer then calls set_stream_error (crates/app/src/state/client_state.rs:166), so the sidebar stays stale until restart. Upstream does not have this gap: client-runtime/src/connection/supervisor.ts runs a retry ladder (retryDelayMs, lines ~141-149) and re-subscribes with a fresh snapshot. The same terminal behaviour applies to thread feeds (feeds.rs:134-139 and 151-156 return after ThreadFeed Failed with no retry, so the open thread stops updating).
- Fix: Wrap the shell feed in a supervisor loop: on Err, back off (same ladder as upstream retryDelayMs) and re-run load_shell + stream from the new snapshot_sequence, instead of returning. Do the same for the thread feed on live-stream error by re-running the snapshot and stream. Alternatively, keep the Failed status and re-issue the subscription on the next OpenThread or on a retry request.

### [medium] Shell update dropped on a transient refetch error, with no retry
- `crates/app/src/app_runtime/feeds.rs:43` · dimension: robustness · upheld 2/2
- Evidence: feeds.rs:40-46: `Err(error) => { tracing::warn!(..."dropped a shell update"); }`. The comment says "the next event for it retries", but the event is consumed and not re-queued. If the refetch of a project or thread row fails once (for example a reader busy timeout, or a decode error), and no later event touches that aggregate (for example a freshly created thread with no activity), the sidebar never shows that row until restart. This is a lost event on the live path.
- Fix: Do not drop the event. Retry the refetch with a bounded backoff before giving up. If it still fails, treat it like a feed failure (see the supervisor finding) so the feed resyncs from a fresh shell snapshot rather than silently diverging.

### [low] Shell snapshot failure is only logged; the store is never told the feed failed
- `crates/app/src/app_runtime/mod.rs:111` · dimension: robustness · upheld 2/2
- Evidence: mod.rs:111-116: `Err(error) => { tracing::error!(..."failed to read the shell snapshot"); return; }`. No ClientStateEvent::ShellFeed(FeedStatus::Failed) is sent, unlike the feed error paths in feeds.rs. The store keeps its initial shell state with no error shown. The `run` branches at mod.rs:90-99 have the same gap for the no-data-dir and DB-open failures. The request receiver is dropped on return, so later OpenThread requests are silently ignored.
- Fix: Before returning, send sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(message))) (and the equivalent for the open and data-dir failures), so the UI can show that the sidebar failed to load rather than sitting in its initial state.

### [low] A panicking feed task is silently discarded by the request loop
- `crates/app/src/app_runtime/mod.rs:140` · dimension: robustness · upheld 2/2
- Evidence: mod.rs:140: `Some(_) = tasks.join_next(), if !tasks.is_empty() => {}`. The JoinError (including a panic from feeds::thread or feeds::shell) is discarded without logging. A panic in a thread feed never sends ThreadFeed(Failed), so the open thread stays in its loading state with no log line.
- Fix: Match on the join result: `Some(Err(e)) if e.is_panic()` should log via tracing::error! with the thread id if one is known, and send ThreadFeed Failed for thread tasks (for example by tagging each spawned task with its thread id).

### [low] Blocking std::fs call on the tokio worker inside an async fn
- `crates/app/src/app_runtime/mod.rs:61` · dimension: robustness · upheld 1/2
- Evidence: mod.rs:60-62: `pub async fn open(...) { std::fs::create_dir_all(data_dir).with_context(...)?; ...`. This is blocking filesystem I/O on a tokio worker thread. The rest of the path (Db::open_configured, initialize_v2_database) correctly uses spawn_blocking, so this one call is inconsistent with the 'all IO off the executor' rule in PORTING.md.
- Fix: Use `tokio::fs::create_dir_all(data_dir).await`, or move it inside the spawn_blocking that open_configured already runs (initialize_v2_database could create the parent directory).
