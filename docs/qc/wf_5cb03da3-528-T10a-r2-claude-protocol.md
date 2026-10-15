# QC wf_5cb03da3-528 — T10a claude protocol re-QC (Haiku)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 7 · refuted: 0

## Lead report

1. [low] session.rs:311-339 (await_control_response): the unmatched check (312-321) and the pending insert (323-335) are separate lock sections. read_loop (596-612) checks pending first and buffers to unmatched on a miss. A response landing in the gap is never re-checked and the await blocks until EOF. Latent, since nothing in crates/ calls it. Fix: do the unmatched lookup and the pending insert under one lock, or insert into pending first and then re-check unmatched before awaiting. Add a test that injects the response between the two steps. (Merges the two duplicate race findings.)

2. [low] session.rs:512 (exit_error, `exit.code.unwrap_or(0)`), fed by session.rs:244-246 and wait_for_exit at 399-412: when the reaper has not reported a status after the 2s wait, or child.wait() failed (reaper 534-541), the spawn error reads "exited with code 0" for a child that may still be alive. Fix: when both code and signal are None, emit "Claude Code process exited" with no code, or a distinct "closed stdout before initialize" message. Add a fake-CLI test that closes stdout and sleeps longer than 2s, asserting the message does not contain "code 0". (Merges the two duplicate exit-diagnostic findings.)

3. [low] session.rs:637-642 (read_loop, `Some(Err(e))` breaks the loop) with the LinesCodec framing near session.rs:575: one non-UTF-8 stdout line stops the reader, fails pending requests and emits Eof while the CLI is alive. PORTING.md's malformed-stdout row says reading continues, and upstream readline decodes lossily. Fix: split stdout as raw bytes and use from_utf8_lossy (or a per-line error that does not break). Keep the 32 MiB cap as the only reader-fatal condition. Practically unreachable from the Node CLI.

4. [low] session.rs:452-456 (Drop calls signal_group(self.pid, SIGKILL)): self.pid is cleared only in shutdown (394), so after the reaper has reaped the leader, Drop can signal a stale pgid that may be reused. Fix: in Drop, take the pgid from state.pgid (cleared by the reaper), or clear self.pid when the reaper runs.

5. [low] session.rs:650-663 (replay_pending): both pending lists are dispatched for either subtype. Upstream sdk.mjs filters pending_permission_requests to can_use_tool and pending_user_dialog_requests to request_user_dialog. Fix: filter each list by its own subtype.

Prior findings: the HIGH (control requests hang after EOF) is fixed. The closed flag is set in fail_all (session.rs:151-160) and checked in request (264), the send path (289) and await_control_response (325). I did not re-run the 31 tests or the probes.

Verdict: PASS (no critical/high; five low items, two of which are duplicates merged above).

## Confirmed findings (raw)

### [low] await_control_response can hang: unmatched check and pending insert are not atomic with read_loop's buffering
- `crates/claude-protocol/src/session.rs:311` · dimension: prior-findings · upheld 2/2
- Evidence: await_control_response takes the `unmatched` lock and returns on a hit (session.rs:312-321), then in a separate critical section inserts into `pending.requests` (session.rs:323-335). read_loop handles a control_response by first trying `pending.requests.remove(id)` and, on a miss, pushes into `unmatched` (session.rs:596-612). If the response arrives after the unmatched check and before the pending insert, it is buffered in `unmatched` and never looked at again, and the later insert waits on `receiver.await` forever. The prior HIGH fix (closed flag) covers only the post-EOF case, not this window. No caller in crates/ uses await_control_response yet, so impact is latent.
- Fix: Do the unmatched lookup and the pending insert under one critical section, and have read_loop push into `unmatched` while it still holds the `pending` guard (same lock order in both places). Add a regression test that delivers the response between the two steps, or at least a stress test that calls await_control_response while the fake CLI replies immediately.

### [low] Exit diagnostic reports 'exited with code 0' when the exit status is unknown
- `crates/claude-protocol/src/session.rs:506` · dimension: prior-findings · upheld 2/2
- Evidence: wait_for_exit returns ProcessExit{code:None,signal:None} when exit_status is still None after the 2s wait (session.rs:399-413, fallback at 409-412). exit_error then formats `exit.code.unwrap_or(0)` (session.rs:510-513), so the spawn error reads 'Claude Code process exited with code 0' even though the child is still alive (for example, a CLI that closes stdout but keeps running, which the prior QC reproduced) or its wait() failed (reaper uses `.wait().await.ok()` at session.rs:535, mapped to None by process_exit at 484-490). This is a false failure code on the spawn error path that the prior finding 2 fix was meant to make accurate.
- Fix: Represent an unknown status explicitly: when both code and signal are None, emit 'Claude Code process exited' with no code (or 'exit status unknown'), not 0. Optionally wait longer or check try_wait() before formatting. Add a test where the fake CLI closes stdout and sleeps, and assert the message does not contain 'code 0'.

### [low] await_control_response can miss a response that arrives between its two lock sections (lost wakeup, hang)
- `crates/claude-protocol/src/session.rs:` · dimension: protocol-and-lifecycle · upheld 2/2
- Evidence: session.rs:312-321 checks `unmatched` under one lock and releases it; session.rs:323-335 then inserts into `pending` under a different lock. read_loop (session.rs:596-612) looks up `pending` first and, on a miss, pushes the response to `unmatched`. A control_response that lands in the gap between the two sections goes only to `unmatched`, and the await then waits on a pending entry that nothing will ever fill, so it hangs (no timeout, and fail_all only runs at EOF). No callers in the tree yet (grep finds only the definition), so this is latent.
- Fix: Check `unmatched` and insert into `pending` while holding the same lock, or insert into `pending` first and then re-check `unmatched` before awaiting. Add a unit test that injects the response between the two steps.

### [low] Spawn failure reports 'exited with code 0' while the child is still alive
- `crates/claude-protocol/src/session.rs:` · dimension: protocol-and-lifecycle · upheld 2/2
- Evidence: session.rs:244-246 calls wait_for_exit() on the init-failure path. wait_for_exit (session.rs:399-413) waits 2s for the reaper and then falls back to ProcessExit{code:None,signal:None}. exit_error (session.rs:506-513) prints `exit.code.unwrap_or(0)`, so the timeout case reads as 'exited with code 0'. Probe: a fake CLI that answers nothing and then blocks on stdin, after emitting one invalid-UTF-8 stdout line, gave `Claude Code process exited with code 0` after ~2s. The child was still running at that point (it was killed only when the session was dropped).
- Fix: When the reaper has not reported an exit, do not invent a code. Use a distinct message such as 'Claude Code closed stdout before initialize', or kill the child and wait for the real status before formatting. Only use 'code N' or 'signal N' when a real ExitStatus exists.

### [low] Invalid UTF-8 on one stdout line kills the reader, which PORTING.md says must not happen
- `crates/claude-protocol/src/session.rs:` · dimension: protocol-and-lifecycle · upheld 2/2
- Evidence: session.rs:637-642: `Some(Err(e)) => { send(SessionEvent::Error(...)); break; }`. A non-UTF-8 line makes LinesCodec return an io InvalidData error, so the reader stops, pending requests fail, and Eof is emitted while the CLI stays alive. PORTING.md's malformed-stdout row promises 'The line becomes SessionEvent::Error and reading continues'. The pinned SDK reads stdout with readline (sdk.mjs ProcessTransport readMessages: `lze({input:this.processStdout})`), which decodes lossily, logs 'Non-JSON stdout' and continues. Probe: an init reply that follows a line containing byte 0xFF gave a spawn error instead of a session.
- Fix: Read raw bytes per line (for example LinesCodec is not usable; use a bytes-based splitter or `String::from_utf8_lossy` on each line) so an invalid line becomes a parse-level SessionEvent::Error and the loop continues. Keep the 32MiB cap as the only reader-fatal condition.

### [low] Drop signals a stale pid after the reaper has already reaped the leader
- `crates/claude-protocol/src/session.rs:` · dimension: protocol-and-lifecycle · upheld 2/2
- Evidence: Drop (session.rs:452-456) calls `signal_group(self.pid, kill_signal())`. self.pid is cleared only in shutdown (session.rs:394). The reaper (session.rs:534-542) reaps the leader and takes `state.pgid` (session.rs:536), so the Drop path can still hold a pid that has been reaped. If the CLI exits on its own and the session is then dropped without shutdown(), a later killpg(pid) can hit a new unrelated process group whose leader reuses that pid.
- Fix: In Drop, use the group id taken from state (`self.state.pgid.lock().unwrap().take()`), which the reaper clears once the leader is reaped, instead of self.pid. Or clear self.pid when the reaper runs.

### [low] Replay of pending prompts is not filtered per list as the SDK does
- `crates/claude-protocol/src/session.rs:` · dimension: protocol-and-lifecycle · upheld 2/2
- Evidence: session.rs:651-663 dispatches any item whose subtype is can_use_tool OR request_user_dialog from BOTH `pending_permission_requests` and `pending_user_dialog_requests`. Pinned sdk.mjs: `processPendingPermissionRequests(e){for(let t of e)if(t.request.subtype==="can_use_tool")this.handleControlRequest(t)...}` and `processPendingUserDialogRequests(e){for(let t of e)if(t.request.subtype==="request_user_dialog")...}`. A request_user_dialog listed under pending_permission_requests is dispatched by Turin but ignored by the SDK, so the host can receive a dialog the SDK would never surface.
- Fix: Match the SDK: replay only can_use_tool from pending_permission_requests and only request_user_dialog from pending_user_dialog_requests.
