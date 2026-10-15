# QC wf_5bf4fe41-dd5 — T12a ACP crate

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 13 · refuted: 1

## Lead report

1. MEDIUM, crates/acp/src/protocol.rs:127-136, Drop at ~628
   - Problem: The writer, reader and handler tasks each hold a strong `Arc<ConnectionInner>`, and `ConnectionInner` owns `write_tx` and the JoinHandles. `rx.recv()` never returns None and `Drop for ConnectionInner` (which aborts the tasks) never runs. Dropping the client leaks the writer task and the child's ChildStdin fd, and the peer never sees stdin EOF. Probes: 50 spawn+drop cycles raised open fds from 10 to 60, and a duplex peer read timed out instead of returning Ok(0). This also breaks PORTING.md:93 (tasks owned by a JoinSet/AbortOnDropHandle tied to the owner).
   - Fix: Tasks must not keep `ConnectionInner` alive. Use `Weak<ConnectionInner>`, or put the JoinHandles and `write_tx` in a small outer handle whose Drop aborts the tasks and is the only thing `Connection` clones. Add a test that the peer sees stdin EOF after drop and that the fd count is stable.

2. MEDIUM, crates/acp/src/protocol.rs:519-548 (reader_loop ~367-424)
   - Problem: `transform_session_update` (519) and `(inner.on_notification)(..)` (524, 540, 548) run inline on the reader task. A panic in a session/update, ext-notification or transform callback kills the reader without calling `terminate`. Pending requests then hang forever, and writes are still accepted. Reproduced: an outstanding `ext_request` hung past the 2s timeout. Upstream wraps handlers in `isolateNotificationHandler` (shared.ts) and terminates when the reader dies (protocol.ts:718-739, test "terminates when a callback on the reader dies").
   - Fix:
     - Wrap only the `on_notification` invocation in `AssertUnwindSafe(..).catch_unwind()`, log the panic and continue.
     - Upstream terminates (does not log-and-continue) on a transform defect, so leave the transform unguarded.
     - Give `reader_loop` a drop guard that calls `terminate` on any exit path. Add tests for both cases.
   - Related, same area: `compat.rs:370/377/384` call `session.unwrap()` in `normalize_v2_initialize`. A v2 initialize response with info but no `capabilities.session` panics in the caller's task. Return an error instead.

3. MEDIUM, crates/acp/src/protocol.rs:161-173 (`next_notification`), 305 (`terminate`), 315 (`push_notification`)
   - Problem: Lost wakeup. The code checks the queue and `terminated`, then creates `notify_waiters.notified()`. `notify_waiters()` stores no permit, so a push or terminate landing in that gap is lost. Stress repro: 7 of 7 runs of 200k items hung. A lost terminate wakeup hangs the consumer forever.
   - Fix: Create `let n = notify.notified(); tokio::pin!(n); n.as_mut().enable();` before checking the queue and `terminated`, then await it. Alternatively use `notify_one` or an mpsc/watch channel. Add a multi-thread stress test (the existing test at protocol.rs:876 uses a current_thread runtime and cannot hit this).

4. MEDIUM, crates/acp/src/client.rs:323-351 (`prompt`)
   - Problem: `prompts.insert` (332) is cleared only on the rpc error path (339) and after `rx.await` (349). If the future is dropped (timeout, select!, abort, interrupt), the entry stays. The next `prompt` for that session then fails with "already has an active prompt" until an idle state_update arrives. This is the interrupt-then-replacement-prompt path. Reproduced: spawn `prompt(s1)`, abort it, call `prompt(s1)` again, and it fails. Upstream uses `Effect.ensuring` (client.ts:1317).
   - Fix: Add a drop guard (like `PendingGuard`, protocol.rs:216-245) that calls `clear_prompt(session_id, token)` on drop, armed for the whole function body. Add an abort-then-reprompt test.

5. LOW (should land before T12 consumes it), crates/acp/src/client.rs:84-111 and 59-63; protocol.rs:175, 593-603
   - Problem: `AcpClient` exposes no termination accessor or hook, and no `on_incoming_request`, `on_outgoing_response` or `on_outgoing_response_failure`. The write ack is consumed internally at protocol.rs:603. The upstream adapter uses `onTermination` for runtime restart (adapter.ts:2106, 6069) and the outgoing-response hooks for native permission acknowledgement (adapter.ts:2130-2170). There is also no `on_elicitation_complete`, so `pending_elicitation_complete` (client.rs:55, 679) grows unbounded. None of this is in the PORTING.md Deviations table.
   - Fix:
     - Add `on_termination` and `on_outgoing_response[_failure]` hooks (plus `on_incoming_request`) to `ConnectionConfig`/`AcpClient`, and a public `termination_error()` or wait-for-termination.
     - Add `on_elicitation_complete` and drain or bound the buffer.
     - Log any hook you deliberately omit in the Deviations table.
     - `logIncoming`/`logOutgoing`/`logger` are not used on the Cursor path and are fine to defer, but log that too.

6. LOW, crates/acp/src/client.rs:412-433 (`on_session_update`)
   - Problem: Buffered updates are replayed in a detached `tokio::spawn` while live updates dispatch inline on the reader. Replay and live delivery can interleave or invert. A scratch probe with 2000 buffered and 50 live updates on a multi-thread runtime came out of order in 40 of 40 rounds. Upstream flushes inline in the registering fiber (client.ts:1388-1390). `tokio::spawn` also panics if called outside a runtime when the buffer is non-empty.
   - Fix: Replay inline in the registering call, or serialize replay and live dispatch (a tokio Mutex sequencer, or a flush-in-progress flag that makes live updates append to the pending buffer until the replay finishes). A parking_lot lock held across an async handler will not work. Add an ordering test.

7. LOW, crates/acp/src/process.rs:98-135 (`exit_error`) and 147-148 (Drop)
   - Problem: `exit_error` reaps the leader via `child.wait()`, but Drop later runs `kill(-pid, SIGKILL)` and a blocking `waitpid(pid)` unconditionally. If the group is empty and the pid has been recycled, the kill can hit an unrelated process group. The `waitpid` can also steal another child's exit status and block a tokio worker. Rare, because it needs pid wraparound.
   - Fix: Do not skip the group kill after reaping, because that leaks surviving group members (PORTING.md:213 requires the kill). Instead kill the group in `exit_error` before `child.wait()`, while the zombie leader still reserves the pgid. Track the reaped state so Drop skips `waitpid` (and `kill` if already done). Move the blocking `waitpid` off the async worker or make it non-blocking.

8. LOW, crates/acp/src/process.rs and protocol.rs:402-411 (termination path)
   - Problem: Termination is detected only on stdout EOF. If the leader dies but a grandchild holds the stdout pipe (`sh -c 'sleep 20 & read x; exit 3'`), or the child closes stdout but stays alive, pending requests hang. Probed: still pending after 4s. Upstream is also EOF-driven, so this is not a regression. It is the cursor-agent helper-process scenario that the process group was added for.
   - Fix: Spawn a watcher that awaits child exit and calls `terminate` with ProcessExited (and kills the group), racing the reader EOF. If you decide to keep upstream parity instead, log it in the Deviations table.

VERDICT: PASS (no critical or high findings; 4 medium items should be fixed before T12 depends on this crate)

## Confirmed findings (raw)

### [medium] A panic in a session/update, ext-notification or transform callback kills the reader task silently and hangs all pending requests
- `crates/acp/src/protocol.rs:` · dimension: fidelity · upheld 2/2
- Evidence: dispatch_notification awaits `(inner.on_notification)(notification)` (lines 524, 540, 548) directly on the reader task, and `transform_session_update` (line 519) runs there too. Nothing catches a panic, so the reader task dies without calling `inner.terminate`. Probe: `on_session_update(|_| async { panic!() })`, then an outstanding `ext_request`. The agent sent a session/update followed by the response for that request. Result: panic printed and `REQUEST HUNG after handler panic` (2s timeout). Pending requests are never failed and later writes are still accepted. Upstream wraps every notification handler in `isolateNotificationHandler` (shared.ts), which logs the defect and keeps the reader alive. Upstream also terminates the connection if the reader itself dies (protocol.ts 718-739, test 'terminates when a callback on the reader dies'). Rust does neither. Only the request handler path catches panics (handler_loop 566).
- Fix: Wrap the on_notification invocation and the transform in `AssertUnwindSafe(..).catch_unwind()`, log the panic and continue (matching isolateNotificationHandler). Also make reader_loop terminate the connection on any exit path, for example with a drop guard that calls `terminate`.

### [medium] Connection tasks hold Arc<ConnectionInner> that owns their own channel and JoinHandles, so dropping the client leaks a task and a file descriptor
- `crates/acp/src/protocol.rs:` · dimension: fidelity · upheld 2/2
- Evidence: `writer_loop(write, write_rx, Arc::clone(&inner))` (line 127) holds `inner`, and `inner.write_tx` is the only sender for `write_rx`. `rx.recv()` therefore never returns None, `Drop for ConnectionInner` (which aborts tasks, line 628) never runs, and the writer task owning the child's ChildStdin lives forever. Probe: spawn `/bin/cat` through `turin_acp::spawn` and drop the client, 50 times. Result: `fds before 10 after 60`, one leaked fd per client, after the process group was killed. Turin runs one ACP process per thread, so this is a per-thread leak of a task, a pipe fd and the connection state.
- Fix: Have the tasks hold only what they need (the receiver, the writer, `Weak<ConnectionInner>`), or make `AcpClient`/`Connection` Drop abort the tasks explicitly or close `write_tx`. Upstream scopes these fibers with `forkScoped`.

### [low] Dropping a v2 prompt() future leaves a stale active-prompt entry, so later prompts on the session fail
- `crates/acp/src/client.rs:` · dimension: fidelity · upheld 2/2
- Evidence: prompt() inserts `prompts.insert(session_id, (token, tx))` (line 332), then awaits `rpc_request(...)` (335) and `rx.await` (344). `clear_prompt` is only called on the rpc error path (339) and after rx completes (350). If the caller's future is dropped or aborted while waiting (select!, timeout, task abort, i.e. interrupt-on-cancel), the map entry stays. The next `prompt` for that sessionId returns `ACP session '...' already has an active prompt.` until an idle state_update arrives. Upstream wraps this in `Effect.ensuring` (client.ts ~1325-1331), so the entry is deleted on interruption.
- Fix: Add a drop guard (like PendingGuard) that calls `clear_prompt(session_id, token)` when the future is dropped.

### [low] The client does not expose the transport hooks and termination signal that the upstream adapter depends on
- `crates/acp/src/client.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Upstream AcpClientOptions exposes `onTermination`, `onIncomingRequest`, `onOutgoingResponse`, `onOutgoingResponseFailure` and `logIncoming`/`logOutgoing`/`logger`. The AcpAdapterV2 code being ported uses them: adapter.ts 2106-2170 acknowledges native permission responses via onOutgoingResponse/onOutgoingResponseFailure, and onTermination drives runtime restart. In Rust, `Connection::termination_error()` and the `on_termination` callback exist only inside protocol.rs. `AcpClient` has no public termination accessor or hook (grep of `pub fn` in client.rs lines 76-630). Its only observable signals are failed requests or `next_notification()` returning None. There is no on_incoming_request, no response-written acknowledgement (the writer's `ack` is consumed internally at protocol.rs 603), no elicitation/complete registration (`pending_elicitation_complete` is buffered forever with no `on_elicitation_complete`), and no MCP/fs/terminal core handler registration (`insert_core` is private). PORTING.md Deviations (lines 210-213) does not list these cuts.
- Fix: Add `on_termination`, `on_incoming_request`, `on_outgoing_response` and `on_outgoing_response_failure` hooks to ConnectionConfig/AcpClient and a public `termination_error()`/wait-for-termination. Add `on_elicitation_complete`. Or record each cut in the Deviations table.

### [low] next_notification can miss a wakeup between the empty-queue check and Notify::notified()
- `crates/acp/src/protocol.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Lines 161-173 pop the queue, check `terminated`, and only then call `self.inner.notify_waiters.notified().await`. `push_notification` (line 315) and `terminate` (line 305) use `notify_waiters()`, which stores no permit. A push or terminate that lands between the checks and the creation of the Notified future is lost, and the consumer sleeps until the next notification. After termination it may sleep forever. I did not reproduce it in 100k iterations because the window is very narrow. It follows from tokio's documented Notify contract (create and `enable()` the Notified future before checking the condition).
- Fix: Create `let notified = notify.notified(); tokio::pin!(notified); notified.as_mut().enable();` before checking the queue and terminated state, or use `notify_one()` or a watch/mpsc channel.

### [low] on_session_update replays buffered updates on a spawned task, so replay can interleave with and be overtaken by live updates
- `crates/acp/src/client.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Lines 412-432 push the handler into `session_updates`, drain `pending_session_updates`, then `tokio::spawn` the replay. From that point the reader sees non-empty handlers and dispatches new updates inline (client_notification_callback, line ~655), so a new update can reach the handler before older buffered ones. Upstream `handleSessionUpdate` returns `flushBufferedNotifications(...)` inline, so the registration effect completes the replay in order (client.ts 'replays buffered notifications' test).
- Fix: Run the replay inline under the same ordering guarantee, for example by holding the handler-state lock or a flush-in-progress flag that makes the live path append to the pending buffer until the replay finishes.

### [low] ProcessGuard::drop signals a stale process group after the leader has already been reaped
- `crates/acp/src/process.rs:` · dimension: fidelity · upheld 1/2
- Evidence: exit_error() (lines 98-122) calls `child.wait()`, which reaps the leader when stdout hits EOF. Drop (lines 135-150) then unconditionally runs `libc::kill(-pid, SIGKILL)` followed by a blocking `libc::waitpid(pid, ..)`. Once the child has been reaped, its pid can be recycled, and `kill(-pid)` could then hit an unrelated process group. The blocking waitpid also runs on whatever async worker drops the client. Upstream relies on scope finalization of the spawned handle, with no raw pid signalling.
- Fix: Track whether the child was already reaped, via the exit_error result or `try_wait`, and skip kill/waitpid in that case. Or only kill the group while the leader is still unreaped.

### [medium] Dropping the client leaks the writer task, ConnectionInner and the child's stdin fd (Drop never runs)
- `crates/acp/src/protocol.rs:` · dimension: correctness · upheld 2/2
- Evidence: Lines 127-136: writer, reader and handler tasks each hold a strong Arc<ConnectionInner>, and ConnectionInner owns write_tx plus the JoinHandles. The abort in `impl Drop for ConnectionInner` (line 628) therefore never fires while the tasks run. writer_loop (line 326) blocks on rx.recv() because the sender lives in the ConnectionInner it holds, so it never exits. Throwaway test with fake peer: after drop(client) on a duplex pair, the agent's read never saw EOF (timeout, expected Ok(0)). Real spawn: 40x spawn(/bin/sleep)+drop raised open fds 10 -> 50, so one stdin fd leaks per session.
- Fix: Tasks should not keep ConnectionInner alive. Either hold a Weak in the tasks, or have a small outer handle (Arc<Tasks>) whose Drop aborts the tasks and is the only thing Connection clones. Alternatively have the writer own the receiver and exit when all Connection handles drop. Add a test that the peer sees stdin EOF after drop.

### [medium] AcpClient::prompt (v2) is not cancel-safe: a dropped future leaves a stale active-prompt entry
- `crates/acp/src/client.rs:` · dimension: correctness · upheld 2/2
- Evidence: prompt() inserts into self.prompts at line 332 and only calls clear_prompt on the error return (339) and normal completion (349). If the future is dropped while awaiting rpc_request or rx (timeout, select!, task abort, interrupt), nothing clears the entry. Upstream uses Effect.ensuring (client.ts ~1318) for exactly this. Throwaway test: start prompt(sessionId s1), abort the task, call prompt(s1) again: it fails with `ACP session 's1' already has an active prompt.`, and that stays true for the life of the connection until an idle state_update for s1 arrives.
- Fix: Use a drop guard (like PendingGuard in protocol.rs) that calls clear_prompt(session_id, token) on drop, armed for the whole function body.

### [medium] next_notification can miss a wakeup and hang with a non-empty queue or after termination
- `crates/acp/src/protocol.rs:` · dimension: correctness · upheld 2/2
- Evidence: Lines 161-173: the queue and terminated checks run first, then `self.inner.notify_waiters.notified().await` creates the Notified future. push_notification (line 315) and terminate (line 305) use notify_waiters(), which stores no permit, so a push or termination between the check and the creation of Notified is lost. Throwaway stress test (multi-thread runtime, 1 notification per iteration, consumer spawned right before the write) hung with the item sitting in the queue at iteration 2360 (200 ms timeout; no later notification would wake it).
- Fix: Create and enable the Notified future before checking the queue (`let n = notify.notified(); tokio::pin!(n); n.as_mut().enable();` then check queue/terminated, then await), or use notify_one/a Notify permit, or switch the queue to an mpsc/watch.

### [low] Pending requests hang if the agent dies but a grandchild still holds stdout (or stdout closes while child lives)
- `crates/acp/src/process.rs:` · dimension: correctness · upheld 2/2
- Evidence: Failure is detected only by reader EOF, then termination_on_eof -> exit_error() awaits child.wait(). Nothing watches child exit directly. Throwaway test: `sh -c 'sleep 20 & read x; exit 3'` then ext_request: still pending after 3 s (the leader has exited, the grandchild holds the stdout pipe). A child that closes stdout but stays alive (`exec 1>&-; sleep 6`) likewise leaves the request pending until the child exits. Upstream is also EOF-driven, so this is not a regression, but it is exactly the cursor-agent helper-process scenario the process group was added for.
- Fix: Spawn a task that awaits child.wait() and calls terminate with the ProcessExited error (and kills the group), racing the reader EOF.

### [low] session/update replay on late handler registration runs on a spawned task, so order is not preserved
- `crates/acp/src/client.rs:` · dimension: correctness · upheld 2/2
- Evidence: on_session_update (line 412-433) drains pending_session_updates and replays them in tokio::spawn, while the reader keeps calling the notification callback inline (client_notification_callback) with handlers now non-empty. Updates that arrive after registration can be delivered before, or interleaved with, the buffered replay. Upstream flushes inline. Also panics if called outside a tokio runtime when pending is non-empty. (My 20+20 test came out ordered because the writes were slow, so this is a code-path concern, not a reproduced failure.)
- Fix: Replay under the same ordering as live delivery, for example hold the handlers lock/sequence while draining, or run live dispatch through the same queue.

### [low] killpg after the leader has been reaped can signal a recycled pgid
- `crates/acp/src/process.rs:` · dimension: correctness · upheld 2/2
- Evidence: ProcessGuard::drop (line 147) does kill(-pid, SIGKILL) unconditionally. exit_error() (line 105-125) may already have reaped the leader via child.wait(). If the group has no remaining members and the pid number has been reused as a pgid, SIGKILL goes to an unrelated process group. Needs pid wraparound, so rare.
- Fix: Record when the leader was reaped and, if no group members remain, skip the kill. Otherwise kill the group at the moment of exit detection, before the pid can be recycled.
