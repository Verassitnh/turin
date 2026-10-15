# QC wf_3daef7bb-ee4 — T13a orchestration projections (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 5 · refuted: 0

## Lead report

1. LOW maintenance_tests.rs:142-186 (assert at :179). The compaction test seeds only 4 v2 events and asserts deleted_receipt_count == 0, so the legacy-receipt delete branch never runs. Imported v1 event deletion, the >500-event multi-page discovery, and verify/rebuild equivalence after compaction are also untested. The code matches upstream ProjectionMaintenance.ts:234-362 on reading, but nothing executes it. Fix: port upstream FoundationPersistence.test.ts:817-1019. Seed more than 500 superseded thread-state events, one imported v1 thread with a legacy receipt, and one pending v1 thread that must survive. Assert the exact remaining event ids and receipts, then run verify and rebuild and assert the thread projection is unchanged. Alternatively, log the omission as a Deviation with a reason.

2. LOW maintenance_tests.rs:70-107. The rebuild test checks only verify flags, sequences and missing_thread_ids. It never reads thread content, has no mid-replay failure test for rollback, has no >500-event replay, and appends with run_id None so the run metadata branch is unexercised. Rollback is correct by construction (maintenance.rs rebuild_in runs inside Db::write, db.rs:141-151, and an early `?` drops the transaction, which rolls back). Fix: assert the rebuilt thread title and that the projection equals the pre-rebuild getThreadProjection. Add a failure injection on one replayed event that expects an error and a byte-identical prior projection. Add a multi-page replay case and a case with run_id Some. Reference: upstream FoundationPersistence.test.ts:142-220.

3. LOW store/mod.rs:197-199 (get_thread_projection) against PORTING.md:353. The call passes `false` to read_for_thread, so read_projection's several statements (canonical.rs:1149-1235, including the fork-source recursion) run in autocommit, and a concurrent commit can tear the result. PORTING.md:353 implies a consistent snapshot for this path. Upstream ProjectionStore.ts:4110-4111 is also unwrapped, so the Rust code matches upstream and only the doc is wrong. No row loss. Fix: either pass `true` and record it as a Deviation, or narrow the PORTING.md:353 row to the methods that use read_tx and list get_thread_projection as an unwrapped upstream-matching read.

4. LOW upstream_tests.rs:440. `#[ignore = "stale-row shell perf case; ..."]` is on builds_shell_snapshots_without_decoding_full_turn_item_payloads. The test passes under --ignored. The "timing marker" it cites is not defined anywhere in the checkout. Upstream ProjectionStore.test.ts:4085-4202 has no timing assertion, and the skip is not in the Deviations table. Fix: remove the #[ignore] attribute and the timing-marker comment.

5. LOW upstream_tests.rs:843. `#[ignore]` is on keeps_fork_visible_items_stable_after_a_source_run_is_rolled_back, and the test passes under --ignored. The stated reason covers only the traceSqlStatements assertions and the cursor walks, which the body already leaves out with comments (about line 1380). The ported non-cut assertions (bounded fork window about 1116-1150, anchored parent page) therefore never run by default. Fix: remove the attribute and keep the omitted sections as comments.

No two of the five findings duplicate each other; items 1 and 2 share a file but cover different tests.

Verdict: PASS (no critical or high findings; five low-severity test-coverage, test-hygiene and doc issues, and no confirmed data-loss defect in the code).

## Confirmed findings (raw)

### [low] Compaction test omits legacy receipt/import deletion and multi-page discovery
- `crates/orchestration/src/projection/maintenance_tests.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust test at lines 142-186 asserts `assert_eq!(report.deleted_receipt_count, 0);` (line 179), so the legacy-receipt delete branch never runs. No test covers imported v1 event deletion (imported_legacy_thread == 1) or more than COMPACTION_PAGE_SIZE (500) events. Upstream apps/server/src/orchestration-v2/FoundationPersistence.test.ts:817-1019 ('compacts superseded state events, imported v1 events, and legacy receipts') asserts deletedEventCount >= 507 across pages, deletedReceiptCount >= 1, removal of the imported v1 events and the legacy receipt, and that verify/rebuild after compaction leave getThreadProjection deepEqual to the pre-compaction value. These are the destructive paths the user flagged as the main data-loss risk.
- Fix: Port the upstream FoundationPersistence compaction test into maintenance_tests.rs: seed >500 superseded thread-state events, one imported v1 thread with a legacy receipt, one pending v1 thread that must survive, assert the exact remaining event ids and receipts, then run verify, rebuild and assert the thread projection is unchanged.

### [low] Rebuild test does not check projection content or rollback on failure
- `crates/orchestration/src/projection/maintenance_tests.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: rebuild_replays_the_event_log_and_restores_missing_threads (lines 70-107) asserts only verify flags, expected/projection sequence and missing_thread_ids. Upstream FoundationPersistence.test.ts:142-220 ('rebuilds event history one bounded page at a time') checks the rebuilt thread title, and that a rebuild failing mid-replay (applied == 500) leaves the prior projection title unchanged. Rust rebuild_in runs in one Db::write transaction (crates/persistence/src/db.rs:141-151, `f(&tx)?` returns early and drops the tx), so rollback is likely correct, but no test pins it.
- Fix: Add a test that seeds a thread, corrupts or injects a failure into one replayed event (or a page-boundary event), calls rebuild, asserts it errors, and asserts the thread projection is byte-identical to before. Add a rebuild equality assertion on getThreadProjection for the successful path.

### [low] Ignored test passes when run and its ignore reason has no basis in the repo
- `crates/orchestration/src/projection/store/upstream_tests.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Line 440: #[ignore = "stale-row shell perf case; not a timing assertion upstream, run manually"] on builds_shell_snapshots_without_decoding_full_turn_item_payloads. Running the crate test binary with --ignored reports this test ok. Upstream ProjectionStore.test.ts:4085-4202 has no timing assertion; it checks shell itemCount/visibleItemCount/status and that getThreadProjection fails. The doc comment's 'requested timing marker' is not defined anywhere in the checkout (grep of crates/, .github, Cargo.toml finds nothing). PORTING.md rule 5 says pure-logic projection tests are ported, not skipped.
- Fix: Remove the #[ignore] attribute and the 'timing marker' comment so the test runs by default.

### [low] Ignore on fork-visibility test hides ported assertions that already pass
- `crates/orchestration/src/projection/store/upstream_tests.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Line 843: #[ignore = "omits the SQL statement tracing assertions and the cut buildBoundedThreadProjection cursor walks ..."] on keeps_fork_visible_items_stable_after_a_source_run_is_rolled_back. Running --ignored reports ok. The stated reason covers only the traceSqlStatements assertions (upstream ProjectionStore.test.ts:5105-5118) and the cursor walks (around 5385-5450). The ported non-cut assertions in the same test (bounded fork window at about line 1117-1150, anchored parent page) are therefore not run by default.
- Fix: Drop the #[ignore] attribute, since the test passes as ported. If the tracing and cursor-walk sections must stay out, leave them commented out with the existing note, not behind an ignore on the whole test.

### [low] get_thread_projection runs a multi-statement read without a transaction, contrary to the Deviations row
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T13a-orch-projections/crates/orchestration/src/projection/store/mod.rs:` · dimension: safety-and-concurrency · upheld 2/2
- Evidence: store/mod.rs:197-199: `self.read_for_thread(thread_id, false, move |conn| { read_projection(conn, &id, &[], None) })`. read_projection (store/canonical.rs:1149-1200) issues several statements: read_canonical_projection, then source_window and the fork-parent recursion, which read other threads. The flag `false` means no read_tx, so each statement autocommits on the reader and a writer can commit between them. PORTING.md:353 says multi-statement reads 'run inside one read transaction (read_for_thread(.., true, ..)), so the snapshot is consistent', and get_thread_snapshot (mod.rs:219-232) does use `true`. Upstream getThreadProjection (ProjectionStore.ts:4110-4111) is also unwrapped, so the Rust code matches upstream, but the Deviations row and its 'consistent snapshot' claim are not true for this method. Effect is a transiently torn projection under concurrent writes, not row loss.
- Fix: Either pass `true` in get_thread_projection (mod.rs:197) so it uses read_tx, and record that as a deviation from upstream, or narrow the PORTING.md:353 row to the methods that actually use read_tx and list get_thread_projection as an unwrapped read that matches upstream.
