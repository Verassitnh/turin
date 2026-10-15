# QC wf_e19abae9-25e — T02b persistence fix round re-QC

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 4 · refuted: 0

## Lead report

1. HIGH, `crates/persistence/src/db.rs:201-210` (`Inner::drop`; the wait is in `checkpoint_loop`, ~458-528, around 466-471)
   - Problem: `Inner::drop` sets `checkpoint_notify.stop` and calls `cv.notify_one()` without holding `notify.lock`. `checkpoint_loop` checks `stop`/`pending` under that lock, then enters an untimed `cv.wait`. A drop that lands between the check and the wait loses the wakeup. `Checkpoint::drop` (db.rs:68-73) then blocks forever in `thread.join()`.
   - Evidence: Reviewers reproduced the hang several times with `drop(Db::open(path).await)` loops, and `sample` showed the main thread in `Inner::drop -> Checkpoint::drop -> join` and the worker parked in `Condvar::wait`. With the lock taken around `stop`+notify, 1800 and 3000-iteration loops completed with 0 hangs.
   - Why it is a regression: the SQLite 3.53.2 bump (item 9 fix) now starts the worker for every file `Db`. On main the worker never started. The 65 tests never drop a file `Db` right after open, so they miss it.
   - Fix:
     - In `Inner::drop`, take `notify.lock` (use `unwrap_or_else(|e| e.into_inner())`) before `stop.store(true, SeqCst)`, and call `cv.notify_all()` while holding it. Release the lock before the join, as `kick()` already does.
     - Optionally also make the first `cv.wait` a `wait_timeout`, so a missed signal is bounded.
     - Add a regression test that opens and drops a file `Db` a few hundred times under a timeout.

2. LOW, `crates/persistence/src/application_event_source.rs:39-40`
   - Problem: item 19 is only partly fixed. The `ON orchestration_events(application_event_version, sequence)` line of `idx_orchestration_events_application_sequence` is indented 8 spaces in the batch string. Upstream (ApplicationEventSource.ts:42-43) uses 4 spaces. SQLite stores the CREATE text verbatim, so `sqlite_master.sql` differs from upstream. PORTING.md:49 requires byte-for-byte DB shape, and there is no Deviations row for this.
   - Fix: indent the `ON ...` line to exactly 4 spaces (or build it with an explicit `\n    ON ...`). Add a test that compares `sqlite_master.sql` for this index against upstream's text. The ledger-table DDL already matches.

VERDICT: FAIL (one high-severity defect: the Db drop deadlock in item 1)

## Confirmed findings (raw)

### [high] Db drop can deadlock forever: lost wakeup between stop flag and checkpoint thread's condvar wait
- `crates/persistence/src/db.rs:` · dimension: fix-verification · upheld 2/2
- Evidence: Inner::drop (db.rs:203-207) does `stop.store(true)` then `cv.notify_one()` WITHOUT holding `notify.lock`, then joins the checkpoint thread. checkpoint_loop (db.rs:~467) checks `stop` under the lock and then calls `cv.wait(guard)` with no timeout. If the drop lands between that check and the wait, the notify is lost and join() blocks forever. Reproduced against this checkout with a throwaway binary in /tmp/t02b-qc: a loop of `drop(Db::open(path).await)` hung ('HANG at 47', 'HANG at 117', 'HANG at 7' on three runs). Separately, 6 concurrent `open` processes left one hung. `sample` of the hung process shows the main thread in `Inner::drop -> JoinInner::join` and `turin-sqlite-checkpoint` parked in `Condvar::wait` inside `checkpoint_loop`. The 65 unit tests pass because they never drop a file Db right after open. This regression comes from the item 9 fix, which made the worker start for the first time on SQLite 3.53.
- Fix: In Inner::drop, take `checkpoint_notify.lock` (ignore poison) before setting `stop` and calling `notify_one`/`notify_all`, then release it before the join. This is the same pattern `kick()` already uses. Alternatively make the first idle wait a `wait_timeout` so a missed signal is bounded. Add a test that opens and drops a file Db a few hundred times under a timeout.

### [low] Index DDL whitespace in application_event_source still differs from upstream (item 19 only partly fixed)
- `crates/persistence/src/application_event_source.rs:` · dimension: fix-verification · upheld 2/2
- Evidence: Upstream ApplicationEventSource.ts:42-43 stores `CREATE INDEX idx_orchestration_events_application_sequence\n    ON orchestration_events(application_event_version, sequence)`, with `ON` indented 4 spaces. Turin lines 39-40 put `ON` at 8 spaces (the Rust source indentation). A fresh DB's sqlite_master shows `CREATE INDEX idx_orchestration_events_application_sequence\n        ON orchestration_events(application_event_version, sequence)`. The ledger-table quoting half of item 19 is fixed, but this stored schema text still does not match byte for byte.
- Fix: Indent the `ON ...` line to 4 spaces inside the batch string, or build the string with explicit `\n    ON ...`. Add an assertion on `sqlite_master.sql` for this index.

### [high] Db drop can hang forever: checkpoint thread misses the stop wakeup (lost wakeup), now reachable because the SQLite bump starts the worker
- `crates/persistence/src/db.rs:` · dimension: regressions · upheld 2/2
- Evidence: `impl Drop for Inner` (db.rs ~line 188) does `self.checkpoint_notify.stop.store(true, SeqCst); self.checkpoint_notify.cv.notify_one();` without taking `notify.lock`. `checkpoint_loop` checks `!pending && !stop` under that lock and then calls `cv.wait(guard)`. A notify landing between the check and the wait is lost, so the thread sleeps forever and `Checkpoint::drop` blocks in `thread.join()`. `kick()` takes the lock; only the stop path does not. Reproduced with a throwaway binary in /tmp/qcr/prog (outside the checkout): `for _ in 0..3000 { let db = Db::open(&p).await.unwrap(); drop(db); }`. It hung in 3 of 3 runs within about 0-100 iterations. `sample` showed the main thread in `Inner::drop -> Checkpoint::drop -> __ulock_wait` and the `turin-sqlite-checkpoint` thread in `checkpoint_loop -> Condvar::wait`. On main (bundled SQLite 3.50) the worker never started, so this latent race was unreachable. After the bump to 3.53.2 every file `Db` starts the thread. Any drop of the last `Db` clone close to when the thread parks (shortly after open, or after the idle flush) deadlocks. `cargo test` (65 tests) does not catch it.
- Fix: Set `stop` while holding `notify.lock`, as `kick()` does: `{ let _g = notify.lock.lock().unwrap_or_else(|e| e.into_inner()); notify.stop.store(true, SeqCst); notify.cv.notify_all(); }`. Add a regression test that opens and drops a file `Db` a few hundred times.

### [low] Application-event-source index text still differs from upstream (8-space indent vs 4); fix 6df92d3 did not match the upstream text
- `crates/persistence/src/application_event_source.rs:` · dimension: regressions · upheld 2/2
- Evidence: Lines 39-40 have `CREATE INDEX idx_orchestration_events_application_sequence\n        ON orchestration_events(application_event_version, sequence)`. Upstream ApplicationEventSource.ts:42-43 has the `ON` line indented 4 spaces. I diffed sqlite_master from a fresh Turin DB against a DB built by running upstream's migrations under node. The stored SQL for this index is `'...\n        ON ...'` (Turin) vs `'...\n    ON ...\n  '` (upstream). Every other table, index and view differs only by trailing whitespace, which `execute_script` trims. The ledger DDL now matches. The only extra objects are the documented sqlite_stat1 and sqlite_stat4. QC item 19 asked for upstream's whitespace; commit 6df92d3 changed the indentation but not to upstream's.
- Fix: Indent the `ON ...` line with 4 spaces so that the line matches upstream's, or drop the claim from the commit and the QC response. Functionally harmless.
