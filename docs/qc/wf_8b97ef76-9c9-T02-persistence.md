# QC wf_8b97ef76-9c9 — T02 persistence

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 42 · refuted: 2

## Lead report

MEDIUM

1. [medium] crates/persistence/sql/{017,021,022,027,028,032-036,038-043,045,047-049,051,054}_*.sql:1 and crates/persistence/sql/023_*.sql
   Problem: Upstream checks `PRAGMA table_info` before each `ADD COLUMN`, and 023 swallows errors on each ALTER with `Effect.catch`. Both guards were dropped. A DB that already has the column but no ledger row (partially migrated, dev or preview build) now fails with "duplicate column name". The whole batch rolls back and `Db::open` fails. Upstream migrates the same DB without error. This was reproduced for 017, 027, 049 and 051. There is no Deviations row.
   Fix: Add a guarded kind, e.g. `MigrationKind::AddColumnIfMissing{table,column,ddl}`, or a per-ALTER `pragma_table_info` pre-check, for every guarded migration. Make 023 ignore errors from each ALTER. Port the upstream tests 027_028_ProviderInstanceIdColumns, 049, 051 and 054 (the re-run / partially migrated cases).

2. [medium] crates/persistence/src/migrations.rs:115
   Problem: `migrate()` runs in a deferred `BEGIN`. Upstream uses `BEGIN IMMEDIATE` (nodeSqliteClient.ts:888). If two processes migrate at once, the INSERT into the ledger fails at once with SQLITE_BUSY_SNAPSHOT, which `busy_timeout` cannot wait out and which the `is_constraint` "already running" path does not catch. The db.rs module doc claims BEGIN IMMEDIATE.
   Fix: Use `BEGIN IMMEDIATE` (or `transaction_with_behavior(Immediate)`).

3. [medium] crates/persistence/src/orchestration_event_store.rs:1135-1190 (LiveHub subscribe/publish), 310 (EventSubscription)
   Problem: A dropped subscription is never removed from `hub.subs`. It is only pruned after `failed` is set by overflow. Each dead subscriber keeps receiving a serialize plus a deep clone per publish, holding up to 1000 events (unbounded bytes for agent events, see item 4). Upstream PubSub subscriptions are scope-released.
   Fix: Unregister by `id` on drop: a Drop guard owned by `EventSubscription` or the drive task, or a closed flag / `Weak` that `publish` checks.

4. [medium] crates/persistence/src/orchestration_event_store.rs:1166, 1234, 1249-1270
   Problem: The live-stream byte budget measures events through hand-written `Serialize` impls. `StoredAgentEvent` writes only sequence, commandId and type, with no payload. `ProjectEvent` omits metadata and aggregate fields. So the 8 MiB cap never trips for agent events; a probe buffered 40 x 1 MiB with no error. Upstream measures `JSON.stringify` of the whole event. Size is also recomputed per subscriber and again in `drain_after`, while upstream caches it once per event.
   Fix: Remove the partial Serialize impls. Size the full wire-shaped event (payload plus all fields) once per publish. Store `(Arc<ApplicationStoredEvent>, usize)` in `LiveBuf.items` and subtract the stored size in `drain_after`.

5. [medium] crates/persistence/src/orchestration_event_store.rs:315-345 (drive_stream), 641-722 (read_agent_events / read_application_events)
   Problem: Replay runs every 500-row page inside one `db.read` closure and collects the result into a single Vec before sending anything. Memory is O(history), and the single reader connection is held for the whole replay (a concurrent read waited about 500 ms in a probe). Upstream uses a lazy `Stream.paginate`; OrchestrationEventStore.test.ts:761 asserts at most one page is retained. There is no Deviations row.
   Fix: In `drive_stream`, loop one `db.read` per 500-row page, send that page, advance the cursor, and stop at `high`. Port the 1501-event page-retention test. Consider a paged API for `read_agent_events` when no limit is given.

6. [medium] crates/persistence/src/db.rs:126-183 (write/read/lock_mutex)
   Problem: A panic inside a `Db::write` or `Db::read` closure poisons the std Mutex. Every later call then permanently returns `Migration{Failed,"SQLite connection lock poisoned"}`. The panicking call itself is reported as `Closed`. Main's PORTING.md maps sync state to `parking_lot::Mutex`.
   Fix: Use `parking_lot::Mutex` (or `unwrap_or_else(PoisonError::into_inner)`); the Transaction drop has already rolled back. Report JoinError panics as their own error variant.

7. [medium] crates/persistence/Cargo.toml:8; orchestration_command_receipts.rs:58,60; orchestration_event_store.rs:44-80; provider_session_runtime.rs:41; errors.rs:11
   Problem: The `turin-contracts` dependency was removed. ThreadId, CommandId, EventId, ProjectId, RunId, NodeId and RawEventId are all `String`, although the `id!` newtypes exist and the mapping table requires them. There is no Deviations row; only payloads were accepted as interim `Value`.
   Fix: Restore the dependency and use the id newtypes in the public row and input structs (`aggregate_id` may stay a String or an enum), or add a Deviations row.

8. [medium] crates/persistence/src/behavior_tests.rs, migrations_test.rs, errors.rs
   Problem: None of the 23 upstream `Migrations/*.test.ts` files or `Errors.test.ts` are ported.
   Fix: Port them, in this priority order:
   - 055 application-event-source baseline (hand-written Rust in application_event_source.rs)
   - 055 upgrade-from-53 and contiguous ids
   - both 050 backfill cases
   - the guard tests in item 1
   - the pure Errors.test.ts cases against `sqlite_failure_detail` / `sql_error`
   - then the SQL-only data migrations (016, 024, 026, 046, ...)

9. [medium] crates/persistence/src/db.rs:300-400 and PORTING.md:157
   Problem: The bundled SQLite is 3.50.2 (libsqlite3-sys 0.35.0), so `start_checkpoint_worker` always warns and returns None. The checkpoint thread, the 10000-frame backstop and the idle flush are dead, untested code, and the Deviations row describes them as live behaviour. The latent bugs in that code:
   (a) The idle path polls `wal_checkpoint(NOOP)` every 100 ms forever (`wait_timeout` loop at 372-385). Upstream does a one-shot idle check after the last request.
   (b) The writer is set to `wal_autocheckpoint=10000` before the spawn. If the spawn, the open (344-349), `checkpoint_fullfsync` (350-358) or a poisoned-lock exit fails, nothing restores 1000. Upstream's `handBack` does.
   Fix: Either bump to a libsqlite3-sys that bundles SQLite 3.51 or newer and add a worker-start test, or rewrite the Deviations row to say plainly that the worker is always off. In both cases:
   - make the idle check one-shot after a kick (`cv.wait()` until dirty, then a single `wait_timeout`);
   - open and configure the checkpoint connection before raising the writer's autocheckpoint, or signal failure back so the writer is reset to 1000.

LOW

10. [low; silent permanent data loss if hit] crates/persistence/src/initialize_v2_database.rs:67
    Problem: `backup.step(-1)?` ignores `Ok(Busy)` and `Ok(Locked)`, so a partial or empty snapshot can be hard-linked as statev2.sqlite. Because `destination.exists()` is then true, the import is never retried. Upstream publishes only a complete snapshot.
    Fix: Use `run_to_completion`, or loop until `StepResult::Done` with backoff. Treat anything else as an import error so nothing is linked.

11. [low] crates/persistence/src/legacy_pull_request_key.rs:84-122, 194-222
    Problem: The migration-50 parser is hand-rolled rather than WHATWG:
    - query and fragment stay in the path (Azure `?_a=overview` falls back to the wrong repository);
    - the scheme match is case-sensitive (uppercase scheme gives host "unknown");
    - the default port is kept in the authority;
    - the fallback rejects non-http schemes;
    - `as_i64` rejects `12.0`;
    - Rust `trim` and ASCII-only lowercasing differ from JS `trim` and `toLowerCase`.
    Fix: Use the `url` crate: match regexes against `path()`, use `host_str()`, and build the authority as host plus a non-default port. Accept integral f64. Use `js_trim` and `to_lowercase`. Add test cases checked against upstream.

12. [low] crates/persistence/src/provider_session_runtime.rs:3-4, 337-379
    Problem: `runtime_mode`, `provider_instance_id` and `last_seen_at` are not validated, so rows upstream would skip or reject are returned. `decode_raw` uses `row.get::<String>`, so a type error fails the whole `list` instead of skipping the row. The module doc says the behaviour matches upstream.
    Fix: Read raw columns as `Value` or `Option`. Validate the RuntimeMode literals, the ProviderInstanceId slug and the timestamp in `decode_runtime`, so `list` skips bad rows and `get` fails. Otherwise add a Deviations row and fix the doc.

13. [low] crates/persistence/src/orchestration_event_store.rs:355-430, 813-874
    Problem: `append_project_event` accepts any `event_type`, which later breaks strict replay. `append_agent_events` does not check `is_known_v2_event_type` either. Three error labels are wrong:
    - `row_to_project` reports `appendProjectEvent:decode` during reads;
    - `row_to_agent` reports `readAgentEvents:decode` when called from application reads;
    - `keep_row` reports `readApplicationEvents:decode` when called from agent reads.
    Fix: Reject unknown types with a Decode error before inserting. Pass the caller's operation name into `keep_row`, `row_to_project` and `row_to_agent`.

14. [low] crates/persistence/src/orchestration_event_store.rs:1172-1181
    Problem: The buffer-overflow error is built from a fake `SQLITE_NOMEM`, so it renders as "SQLITE(7) out of memory" and loses the resume text.
    Fix: Construct `PersistenceSqlError{operation:"...:buffer", detail:None, correlation:None}` directly.

15. [low] crates/persistence/src/orchestration_event_store.rs:985
    Problem: `... .max(0) + 1` overflows when max_events >= i64::MAX: it panics in debug and gives an unlimited LIMIT in release.
    Fix: Clamp to MAX_SAFE_INTEGER as at lines 648 and 754, or use `saturating_add(1)`.

16. [low] crates/persistence/src/db.rs:127-160
    Problem: `spawn_blocking` runs before the std mutex is taken, so N concurrent callers park N blocking-pool threads. Callers cancelled while waiting still run their write. Upstream and the mapping table use `Semaphore(1)`.
    Fix: Acquire an owned `tokio::sync::Semaphore(1)` permit per connection before `spawn_blocking`, and move the permit into the closure.

17. [low] crates/persistence/src (orchestration_event_store.rs:373, 470, 788; orchestration_command_receipts.rs:73, 147; provider_session_runtime.rs:168; pull_request_files_viewed.rs:44; application_event_source.rs:270, 314)
    Problem: No `prepare_cached` anywhere. Upstream keeps a 200-entry prepare cache. Agent-event appends recompile the statement per event while the writer is held.
    Fix: Use `prepare_cached` and call `set_prepared_statement_cache_capacity(200)` on the writer and reader.

18. [low, optional] crates/persistence/src/orchestration_event_store.rs:153-164
    Problem: `PromptHints::remember` uses `order.retain`, which is O(n) per repeated tick. Upstream's Map delete+set is O(1).
    Fix: Use `IndexMap` with `shift_remove` plus insert, or the `lru` crate.

19. [low] crates/persistence/src/migrations.rs:17-22; application_event_source.rs:39-40
    Problem: Schema text differs from upstream:
    - the ledger DDL has an unquoted `effect_sql_migrations` (upstream stores `"effect_sql_migrations"`);
    - the indentation of `idx_orchestration_events_application_sequence` differs;
    - the bundled SQLite is built with STAT4, so `PRAGMA optimize` creates `sqlite_stat4`, which upstream DBs do not have.
    Fix: Quote the identifier in ENSURE_TABLE and copy upstream's whitespace for the index. Add a Deviations row for `sqlite_stat4`, or build without STAT4.

20. [low] crates/persistence/src/orchestration_event_store.rs:74, 406; PORTING.md:162; main PORTING.md:75-76
    Problem: Agent-event `occurredAt` is `DateTimeUtc` upstream but is stored exactly as the caller passed it. The Deviations reason ("UtcDateTime not on branch") is stale after a rebase. Main's PORTING.md contradicts itself: the line-75 rule makes `IsoDateTime` a pass-through String, while line 76 maps every `*_at` column to `UtcDateTime`. Upstream project `occurredAt`, receipts `acceptedAt` and provider `lastSeenAt` are `IsoDateTime` pass-through.
    Fix: After rebasing, type only agent-event `occurred_at` as `turin_contracts::UtcDateTime`. Keep project, receipt and provider `*_at` as String and narrow the Deviations row. On main, narrow line 76 to columns whose upstream schema is DateTimeUtc / DateTimeUtcFromString.

21. [low] PORTING rule 1 (dropped "why" comments)
    Problem: The comments that explain why were dropped in these places:
    - crates/persistence/src/pull_request_files_viewed.rs:1-80: why the scope includes host and viewer, the 500-row limit, the read-one-past ordering, and why a set batch is atomic;
    - provider_session_runtime.rs:223: "stale payloads / only recordImportedTranscript";
    - migration_manifest.rs:344-349: the id-55 preview / "Future V2 schema changes need new migrations" note;
    - sql/027 header: why the column is nullable and not backfilled;
    - db.rs:236-262: the PRAGMA rationale.
    Fix: Copy the upstream comments next to the matching code.

22. [low] crates/persistence/src/migration_manifest.rs:1; sql/055_OrchestrationV2.sql; migrations_test.rs:1; behavior_tests.rs:1; legacy_pull_request_key.rs:1-3
    Problem: The modules lack `//! Port of upstream/<path>` headers or use incomplete paths. The manifest says the SQL is "Generated" but no generator is committed. The 549-line 055 file has no per-section source markers.
    Fix:
    - Add full `upstream/...` Port-of headers.
    - Commit the extraction script, or reword the comment.
    - Add `-- Port of upstream/.../Migrations/OrchestrationV2/<file>.ts` markers per section in 055.

23. [low, process] branch-level; PORTING.md:154
    Problem: The whole port is one 7k-line commit (cbd6ae6). `git merge-tree` reports a PORTING.md conflict with main in the Deviations table. The branch's rows use upstream paths in the File column, while main's rows use Turin paths.
    Fix: Not a blocker. When merging main, keep both sets of rows and normalize the File column. Commit per module in future tasks.

VERDICT: PASS (no critical or high findings). Items 1-9 should be fixed before T23 (opening real T3 databases).

## Confirmed findings (raw)

### [medium] 23 legacy migrations lost their upstream 'column already exists' guards, so real T3 databases that upstream migrates now fail to open
- `crates/persistence/sql/027_ProviderSessionRuntimeInstanceId.sql:1` · dimension: fidelity · upheld 2/2
- Evidence: Upstream 017, 021, 022, 027, 028, 032-036, 038-043, 045, 047-049, 051 and 054 check `PRAGMA table_info(...)` before each `ALTER TABLE ... ADD COLUMN` (for example 027: `if (!columns.some((column) => column.name === "provider_instance_id")) { ALTER ... }`). Upstream 023 wraps each of its 4 ALTERs in `.pipe(Effect.catch(() => Effect.void))`. Every Turin SQL file is a bare `ALTER TABLE ... ADD COLUMN`, and no Deviations row covers this. Upstream has tests for exactly this case: `027_028_ProviderInstanceIdColumns.test.ts` ('continues when provider_session_runtime was partially migrated') and `051_ProjectionThreadMessageContext.test.ts` ('accepts context added by an earlier development migration'). Turin has no port of either. I reproduced both with a throwaway binary in /tmp that calls `run_migrations`: migrate to 26, `ALTER TABLE provider_session_runtime ADD COLUMN provider_instance_id TEXT`, then migrate to 28 gives `ERR Migration "27_ProviderSessionRuntimeInstanceId" failed: SQL error in runMigrations.script: SQLITE(1) SQL logic error` and the ledger stays at 26. Migrate to 50, add `context_json`, then migrate to 51 fails the same way. Running the upstream migration code under Node against the same DBs gives `ok` both times. This blocks T23 (opening real or preview-built T3 databases). (A fresh-DB schema comparison came out identical after whitespace normalization, and the ledger ids and names match; the gap is only these guards.)
- Fix: Restore the guards. Give these migrations a `MigrationKind` that runs `PRAGMA table_info(<table>)` and skips each `ADD COLUMN` whose column already exists, or split them into per-column steps with a Rust guard. For 023, swallow any error from each ALTER, as upstream does. Port the 027_028 and 051 tests.

### [medium] Migrations run in a deferred BEGIN; upstream takes the write lock up front with BEGIN IMMEDIATE
- `crates/persistence/src/migrations.rs:115` · dimension: fidelity · upheld 2/2
- Evidence: Turin: `conn.execute_batch("BEGIN")`. Upstream runs the migrator in `sql.withTransaction(run)`, and nodeSqliteClient.ts:888 sets `beginTransaction: options.readonly === true ? "BEGIN" : "BEGIN IMMEDIATE"`. The comment there gives the reason: "A deferred BEGIN only takes the write lock at the first write. If another process commits after this transaction's first read, that write fails at once with SQLITE_BUSY_SNAPSHOT, which busy_timeout cannot wait out." In Turin, if two processes open the same file at once (Sqlite.ts: "CLI and server write from separate processes"), one reads `latest`, the other commits migrations, and the first one's INSERT into effect_sql_migrations fails with BUSY_SNAPSHOT. `Db::open` then fails instead of waiting. The db.rs module doc claims the writer uses BEGIN IMMEDIATE, but the migration path does not.
- Fix: Use `BEGIN IMMEDIATE` in `migrate()` (or `transaction_with_behavior(Immediate)`).

### [low] V1 to V2 import ignores Busy/Locked from backup.step and can publish an incomplete snapshot as statev2.sqlite
- `crates/persistence/src/initialize_v2_database.rs:67` · dimension: fidelity · upheld 2/2
- Evidence: `backup.step(-1)?; Ok(())`. In rusqlite 0.37 (backup.rs:262-270), `step` returns `Ok(StepResult::Busy)` / `Ok(StepResult::Locked)` for SQLITE_BUSY/LOCKED, so neither becomes an error here. Upstream uses `NodeSqlite.backup(database, snapshotPath)`, which keeps stepping until DONE and rejects on failure, and it publishes only a complete snapshot ("Publish only a complete snapshot"). If T3 holds a lock on state.sqlite during import, Turin hard-links a partial or empty snapshot.sqlite as the V2 database. Because `destination.exists()` is then true, the import is never retried.
- Fix: Use `backup.run_to_completion(...)`, or loop on `step` until `StepResult::Done` with a sleep on Busy/Locked. Treat anything else as an import error so nothing is linked.

### [low] Migration 50 backfill replaces WHATWG `new URL` with a hand-rolled parser and produces different PR keys than upstream
- `crates/persistence/src/legacy_pull_request_key.rs:194` · dimension: fidelity · upheld 2/2
- Evidence: Upstream `parseChangeRequestUrl` matches against `new URL(targetUrl).pathname` (query and hash removed, scheme case-insensitive, default port dropped). Its fallback uses `new URL(linked.url).hostname`. Turin's `parse_http_url` uses `strip_prefix("https://")` / `("http://")` and keeps `path: path.to_owned()`, which includes `?query#hash`. Checked with node: `https://dev.azure.com/org/proj/_git/web/pullrequest/7?_a=overview` has pathname `/org/proj/_git/web/pullrequest/7`, so upstream stores host `dev.azure.com` and repository `org/proj/_git/web`. In Turin the regex `(?:/|$)` fails on `?`, so it falls back to `linked.repository` (for example `web`). `HTTPS://github.com/o/r/pull/1` gives host `github.com` upstream but `unknown` in Turin. For Forgejo, `https://h:443/o/r/pulls/5` gives authority `h` upstream but `h:443` in Turin. Separately, `parse_legacy_linked_pull_request` uses `as_i64()`, which rejects `12.0`, while upstream's `Number.isInteger(12.0)` accepts it. None of this has a Deviations row, and the rows written to projection_thread_pull_requests differ from what T3 would write.
- Fix: Parse with the `url` crate (WHATWG-compliant): match regexes against `url.path()`, use `host_str()` for the hostname and `host` plus non-default port for the authority. Accept integral f64 numbers. Add cases for a query string, an uppercase scheme and a default port, checked against upstream.

### [medium] Live stream loads the whole replay range into one Vec; upstream pages it 500 rows at a time and tests that only one page is retained
- `crates/persistence/src/orchestration_event_store.rs:323` · dimension: fidelity · upheld 2/2
- Evidence: `drive_stream` calls `read_application_events(... through_sequence: high ...)`, which collects every row from `after_sequence` up to the high-water mark into `Vec<ApplicationStoredEvent>` before sending any of them (`read_agent_events` also returns a Vec). Upstream `catchUpApplicationEvents` is a lazy `Stream.paginate` with READ_PAGE_SIZE=500. OrchestrationEventStore.test.ts:761 ('releases consumed application replay pages') asserts that at most 1 page is alive while 1501 events stream. A subscriber starting at sequence 0 on a large history holds the whole event log in memory, and nothing is delivered until all of it is decoded. There is no Deviations row.
- Fix: Page inside `drive_stream`: query 500 rows, send them, advance the cursor, and repeat up to `high`. The public read APIs can keep returning Vec, but replay should stream page by page.

### [low] Live hub keeps subscriptions after the EventSubscription is dropped, and its byte budget leaves out the event payload
- `crates/persistence/src/orchestration_event_store.rs:1135` · dimension: fidelity · upheld 2/2
- Evidence: `LiveHub::subscribe` pushes a clone of the LiveSub into `subs`. `publish` removes an entry only when `state.failed.is_some()`. Dropping `EventSubscription` aborts the task but leaves the entry, which keeps buffering up to 1000 events / 8MB until it overflows. Upstream releases the PubSub subscription when the stream ends (`Stream.ensuring(Scope.close(subscriptionScope, Exit.void))`). The byte budget uses `serde_json::to_string(event)` through custom `Serialize` impls (lines ~1247-1270): ProjectEvent writes only sequence/eventId/type/payload, and StoredAgentEvent writes only sequence/commandId/type with no payload. Upstream measures `JSON.stringify` of the whole event. As a result the 8MB limit never applies to agent events.
- Fix: Remove the hub entry when EventSubscription drops (for example a Drop impl that removes it by `id`). Measure the full event, including payload and metadata, for the byte budget, or record a Deviation if stream budgets are cut.

### [low] Checkpoint thread wakes every 100ms forever and does not reset wal_autocheckpoint when it fails
- `crates/persistence/src/db.rs:376` · dimension: fidelity · upheld 2/2
- Evidence: Upstream arms the idle `finish` timer only when a request arrives (`clearTimeout(idle); idle = setTimeout(finish, idleMs)` inside `on("message")`), so an idle app does no checkpoint work. Turin loops on `cv.wait_timeout(guard, CHECKPOINT_IDLE)` and runs `checkpoint_if_due(&conn, true)` (a `PRAGMA wal_checkpoint(NOOP)` query) on every timeout, about 10 times a second for the life of the Db. Upstream's `handBack` also runs `PRAGMA wal_autocheckpoint = 1000` on the writer when the worker errors or exits. Turin sets the writer to 10000 before spawning the thread, and when `checkpoint_loop` fails to open its connection or set `checkpoint_fullfsync` (lines 344-358) it just `return`s. The writer then stays at 10000 with no worker. The Deviations row describes neither behavior.
- Fix: Run the idle check once per kick (keep a 'pending idle' flag cleared after `finish`). When the thread exits early, restore `wal_autocheckpoint = 1000` on the writer, for example by having the thread report failure back to the opener.

### [low] ProviderSessionRuntime decoding differs from upstream: runtime_mode is not validated, and a raw-column type error fails the whole list
- `crates/persistence/src/provider_session_runtime.rs:337` · dimension: fidelity · upheld 2/2
- Evidence: Upstream reads rows with `ProviderSessionRuntimeRawDbRowSchema`, where every column except threadId is `Schema.Unknown`. It then decodes them with `ProviderSessionRuntime` (`runtimeMode: RuntimeMode` literal, `providerInstanceId: ProviderInstanceId`). `list` skips rows that fail with a warning ("one stale row must not disable every consumer"). In Turin, `decode_raw` uses `row.get::<String>` for provider_name/adapter_key/runtime_mode/status/last_seen_at, and a conversion error there propagates as `PersistenceError::sql("...list:query")` and fails the whole list. `runtime_mode` is kept as a plain `String`, so rows upstream would skip (unknown runtime mode) are returned. The module doc nevertheless says "matching upstream", and there is no Deviations row.
- Fix: Read raw columns as `rusqlite::types::Value` (or `Option<String>`) and validate in `decode_runtime`, including the RuntimeMode literals (or the `turin_contracts` enum once available), so bad rows are skipped instead of failing the list.

### [low] appendProjectEvent drops request validation, and read errors carry the wrong operation labels
- `crates/persistence/src/orchestration_event_store.rs:355` · dimension: fidelity · upheld 2/2
- Evidence: Upstream `AppendProjectEventRequestSchema` restricts `type` to `project.created|project.meta-updated|project.deleted` and returns a PersistenceDecodeError for anything else. Turin's `append_project_event` inserts any `event_type` with aggregate_kind 'project'. A wrong type is committed, and every later strict `read_application_events` then fails in `keep_row` (unknown project type), which breaks replay for all subscribers. This literal check does not depend on typed payloads. Error labels: `row_to_project` uses `"OrchestrationEventStore.appendProjectEvent:decode"` (lines 857/861) for rows decoded during reads, and `keep_row` (line 830) reports `readApplicationEvents:decode` even when called from `read_agent_events`. Upstream uses `readApplicationEvents:decode` and `readAgentEvents:decode` respectively.
- Fix: Reject event types outside the three project literals with a Decode error before inserting. Pass the caller's operation name into `keep_row` and `row_to_project`.

### [low] Upstream comments that explain why were dropped (PORTING rule 1)
- `crates/persistence/src/pull_request_files_viewed.rs:1` · dimension: fidelity · upheld 2/2
- Evidence: These comments are missing from the Turin modules and SQL files: PullRequestFilesViewed.ts on why scope includes host and viewer ("signing in as somebody else must not inherit their ticks"), the MAX_FILES_VIEWED_ROWS rationale, ordering by path and reading one row past the limit, and why a set batch is one transaction; ProviderSessionRuntime.ts "Runtime writes can carry stale payloads. Only recordImportedTranscript may change source records" above the upsert SQL; Migrations.ts at id 55 "Released as 53 and 54 in V2 previews ... Preserve this migration's schema. Future V2 schema changes need new migrations."; the 027 header on why provider_instance_id is nullable and not backfilled; and the Sqlite.ts PRAGMA rationale (busy_timeout, synchronous NORMAL, checkpoint_fullfsync, journal_size_limit, optimize 0x10012), which `apply_writer_pragmas` does not carry. `grep -rn 'stale payloads|signing in as somebody|Future V2 schema' crates/persistence/src` finds nothing. PORTING.md rule 1 requires keeping comments that explain why.
- Fix: Copy these comments next to the corresponding Rust constants, SQL strings, manifest entries and SQL files.

### [medium] Live-stream 8 MiB byte budget never trips: the size it measures leaves out event payloads
- `crates/persistence/src/orchestration_event_store.rs:1166` · dimension: correctness · upheld 2/2
- Evidence: LiveHub::publish sizes each event with `serde_json::to_string(event)`. The hand-written Serialize impls write only `sequence/commandId/type` for StoredAgentEvent (line 1261) and `sequence/eventId/type/payload` for ProjectEvent (line 1249, no metadata). The agent-event payload is never counted. Upstream LiveStreamBudget.serializedSize does `Buffer.byteLength(JSON.stringify(value))` on the whole event. Probe (/tmp/qc-t02-probe): I subscribed, then published 40 agent events with 1 MiB payloads each. Output: `B delivered 40 x 1MiB events (40 MiB buffered); err=None`. Upstream fails the stream once the buffer passes 8 MiB. As written, a slow subscriber can buffer up to 1000 events of any size.
- Fix: Measure the full serialized event, including payload and metadata, in the same shape the transport sends (a derived Serialize that matches the wire JSON), or serialize once per publish and share the size across subscribers.

### [medium] Dropped subscriptions are never removed from LiveHub and keep receiving clones of every published event
- `crates/persistence/src/orchestration_event_store.rs:1160` · dimension: correctness · upheld 2/2
- Evidence: `LiveHub::subscribe` pushes `clone_sub(&sub)` into `self.subs`. `publish` removes a subscriber only when `state.failed.is_some()` (`subs.retain(|sub| { ... if state.failed.is_some() { return false; } ...`). LiveSub has no Drop impl, and EventSubscription's drop only aborts the task. A closed stream therefore stays in `subs`. It keeps cloning every committed event into its VecDeque until it reaches 1000 items, and only the next publish after that removes it. Because the byte budget is ineffective (finding above), each abandoned subscriber can hold 1000 full events. Every publish also re-serializes each event once per subscriber, dead ones included. Upstream PubSub subscriptions are scoped and released when the stream ends.
- Fix: Remove the hub entry when the subscription ends: give LiveSub a Drop impl or a weak handle that removes it by `id`, or have publish skip and remove entries whose receiver is closed (for example, hold a `Weak` to the state).

### [medium] A panic inside a Db::write/read closure poisons the connection mutex, and every later DB call fails
- `crates/persistence/src/db.rs:176` · dimension: correctness · upheld 2/2
- Evidence: `write` runs `f(&tx)` while holding `lock_mutex(&inner.writer)`. A panic poisons that std Mutex, and `lock_mutex` maps poisoning to a permanent `Migration{Failed, "SQLite connection lock poisoned"}`. The JoinError is reported as `PersistenceError::Closed` (line 140). Probe: `db.write(|_| panic!())` returned `persistence connection closed`. The next `db.write(..)` and `db.read(..)` both returned `SQLite connection lock poisoned`. One panicking store call takes down all persistence for the rest of the process. Unwinding drops the Transaction, which rolls back, so the connection itself is still usable.
- Fix: Recover the guard after a poisoned lock (`lock().unwrap_or_else(PoisonError::into_inner)`): the rusqlite Transaction drop has already rolled back, so the connection is consistent. Or use parking_lot::Mutex. Report a panic as its own error variant, not as `Closed`.

### [medium] Migrations dropped upstream's column-exists guards (PRAGMA table_info checks), so re-adding an existing column fails
- `crates/persistence/sql/017_ProjectionThreadsArchivedAt.sql:1` · dimension: correctness · upheld 2/2
- Evidence: Upstream 017, 021, 022, 027, 028, 032, 033, 034, 035, 036, 038, 039, 040, 041, 042, 043, 045, 047, 048, 049, 051 and 054 each read `PRAGMA table_info(...)` and run `ALTER TABLE ... ADD COLUMN` only when the column is missing. The extracted SQL files run the ALTER unconditionally (e.g. 017: `ALTER TABLE projection_threads ADD COLUMN archived_at TEXT;`, 021: seven unconditional ADD COLUMNs). Probe: I migrated to 16, added `archived_at`, then migrated to 17. Result: `Err("Migration \"17_ProjectionThreadsArchivedAt\" failed: SQL error in runMigrations.script: SQLITE(1) SQL logic error")`. Upstream succeeds here. The PORTING.md Deviations rows on this branch do not mention it. This matters for T23, which opens real or forked T3 databases.
- Fix: Keep the guards. Add a migration kind (or a per-statement wrapper) that checks `pragma_table_info` before each ADD COLUMN, as upstream does, or generate those migrations as Rust functions. Add a test that runs each guarded migration against a table that already has the column.

### [low] Migration-50 legacy PR key parser does not match WHATWG URL parsing, so backfilled (host, repository) keys come out wrong
- `crates/persistence/src/legacy_pull_request_key.rs:194` · dimension: correctness · upheld 2/2
- Evidence: `parse_http_url` splits at the first '/', so the path keeps `?query` and `#fragment`. The route regexes then fail on `(?:/|$)`. It also keeps a default port in `authority`, matches the scheme case-sensitively, and does not trim. `http_hostname` returns None for non-http schemes, where upstream uses `new URL(url).hostname`. Probe: a legacy row `{repository:"web",number:7,url:"https://dev.azure.com/org-a/project/_git/web/pullrequest/7?_a=overview"}` was backfilled as `host=dev.azure.com repository=web`. Upstream parses `pathname=/org-a/project/_git/web/pullrequest/7` and stores `repository=org-a/project/_git/web`, as confirmed with node. Likewise `https://codeberg.org:443/o/r/pulls/5` gives Turin authority `codeberg.org:443` vs upstream `url.host`=`codeberg.org`. The wrong key is written into the projection_thread_pull_requests primary key.
- Fix: Parse with the `url` crate (WHATWG): use `Url::parse`, `scheme()` in {http, https}, `host_str()` for the host, `path()` for the regex input, and `host + port_or_known_default` handling to match JS `url.host`. Use `host_str()` for the fallback hostname with any scheme.

### [low] agent_replay_stats overflows i64 when max_events >= i64::MAX
- `crates/persistence/src/orchestration_event_store.rs:985` · dimension: correctness · upheld 1/2
- Evidence: `let limit = i64::try_from(max_events).unwrap_or(i64::MAX).max(0) + 1;` With `max_events = u64::MAX` (or `i64::MAX as u64`), this is `i64::MAX + 1`, which panics in debug builds. In release it wraps to i64::MIN, and SQLite treats a negative LIMIT as unlimited. Upstream computes `Math.floor(maxEvents) + 1` without overflow.
- Fix: Use `saturating_add(1)` (or clamp to MAX_SAFE_INTEGER, as read_agent_events does).

### [low] Live-buffer overflow error is reported as SQLite 'out of memory'
- `crates/persistence/src/orchestration_event_store.rs:1175` · dimension: correctness · upheld 2/2
- Evidence: On overflow, publish builds `PersistenceError::Sql(sql_error(..., &SqliteFailure(ffi::Error::new(SQLITE_NOMEM), Some("The live event buffer is full..."))))`. `sqlite_failure_detail` keeps only the code, so the client sees `SQL error in OrchestrationEventStore.streamApplicationEvents:buffer: SQLITE(7) out of memory` and the 'resume from last sequence' text is lost. Upstream wraps LiveStreamBufferError, which has no SQLite condition, so its detail is undefined and the message is `SQL error in OrchestrationEventStore.streamApplicationEvents:buffer`.
- Fix: Construct `PersistenceSqlError { operation: "...:buffer", detail: None, correlation: None }` directly instead of faking a SQLite failure.

### [low] If the checkpoint thread cannot open its connection, the writer stays at wal_autocheckpoint=10000
- `crates/persistence/src/db.rs:345` · dimension: correctness · upheld 2/2
- Evidence: `start_checkpoint_worker` sets `PRAGMA wal_autocheckpoint = {WRITER_BACKSTOP_FRAMES}` (line 318) before spawning. If `checkpoint_loop` then fails `Connection::open` or the fullfsync pragma, it only logs a warning and returns. Nothing restores the writer to 1000 frames, so the WAL grows 10x before any checkpoint. Upstream `handBack` resets `wal_autocheckpoint = CHECKPOINT_FRAMES` when the worker errors or exits. The idle path also differs: Turin re-runs `checkpoint_if_due(idle=true)` on every 100ms timeout forever, while upstream's `finish` fires once, 100ms after the last request.
- Fix: Open and configure the checkpoint connection before raising the writer's autocheckpoint (open it in start_checkpoint_worker and move it into the thread). Or have the thread signal failure so the writer is reset to 1000. Make the idle flush one-shot after the last kick.

### [low] Upstream's column-exists guards were dropped from 22 migrations, so re-running one on a DB that already has the column fails
- `crates/persistence/sql/049_ProjectionThreadsActiveOrderKey.sql:1` · dimension: wire-compat · upheld 2/2
- Evidence: Upstream 017, 021, 022, 027, 028, 032-036, 038-043, 045, 047-049, 051 and 054 only add a column after checking for it, e.g. 042: `const columns = yield* sql`PRAGMA table_info(projection_threads)`; if (!columns.some((column) => column.name === "linked_pull_request_json")) { ALTER TABLE ... }`. Upstream 049_ProjectionThreadsActiveOrderKey.test.ts tests this case directly: "Recovery may run the same migration against a database that already has the column". Turin's SQL files run `ALTER TABLE ... ADD COLUMN` with no check (for example 042, 047, 051, 027, 033). I reproduced it with a /tmp scratch crate: migrate to 48, run `ALTER TABLE projection_threads ADD COLUMN active_order_key TEXT`, then migrate to 49. The result is `Err(Migration { kind: Failed, message: "Migration \"49_ProjectionThreadsActiveOrderKey\" failed: SQL error in runMigrations.script: SQLITE(1) SQL logic error" })` and the whole batch rolls back. Upstream makes no change in this case. The Deviations table does not list this.
- Fix: Add a `MigrationKind::AddColumnIfMissing { table, column }` (or a small Rust pre-check) that runs `PRAGMA table_info` and skips the ALTER when the column exists, matching each guarded upstream migration. Add the upstream 049/054 re-run cases as tests.

### [medium] Hand-written Serialize impls for stored events drop the payload, so the 8 MiB live-stream byte budget never trips
- `crates/persistence/src/orchestration_event_store.rs:1261` · dimension: wire-compat · upheld 2/2
- Evidence: `impl serde::Serialize for StoredAgentEvent` writes only `sequence`, `commandId` and `type`. `impl Serialize for ProjectEvent` (line 1249) writes only `sequence`, `eventId`, `type` and `payload`. LiveHub::publish (line 1166) and event_bytes (line 1234) size events with `serde_json::to_string(event)` and check them against LIVE_STREAM_MAX_BYTES. Upstream LiveStreamBudget.ts measures `Buffer.byteLength(JSON.stringify(value))` of the full event: OrchestrationV2StoredEvent `{sequence, commandId, event:{id, threadId, type, occurredAt, payload, runId?, ...}}`. Scratch test: a StoredAgentEvent with a 1 MB payload serializes to `{"sequence":1,"commandId":null,"type":"message.updated"}` (56 bytes). These public impls also give any other consumer, such as the hooks event JSON, a shape that matches no upstream schema.
- Fix: Remove the partial impls. Either derive Serialize with upstream's field names and nesting (`event: {id, threadId, type, occurredAt, payload, ...optional metadata}` for agent events; all ApplicationProjectEvent fields for project events), or size events with a private function that counts payload length plus fixed fields, and do not implement Serialize.

### [low] Agent event occurredAt (upstream DateTimeUtc) is stored exactly as the caller passed it, not in the canonical .sssZ form
- `crates/persistence/src/orchestration_event_store.rs:74` · dimension: wire-compat · upheld 1/2
- Evidence: `pub occurred_at: String` is bound verbatim at line 406 (`SqlValue::text(&event.occurred_at)`). Upstream sets `occurredAt: Schema.DateTimeUtc` in OrchestrationV2EventBase and stores `encoded.occurredAt` after `encodeV2EventJson`, which is always `YYYY-MM-DDTHH:MM:SS.sssZ`. main:PORTING.md says `DateTimeUtc` / every `*_at` column -> `UtcDateTime` (always encodes .sssZ). The branch's Deviations row says this waits because "`turin_contracts::UtcDateTime` is not on this branch yet". `git show main:crates/contracts/src/base_schemas.rs` now has `pub struct UtcDateTime` (line 179), so after rebase that reason no longer holds. ProviderSessionRuntime.last_seen_at (ORDER BY last_seen_at) and receipts.accepted_at are also plain Strings.
- Fix: After rebasing on main, type `occurred_at` (and the other `*_at` inputs) as `turin_contracts::UtcDateTime`, bind its canonical string, and remove or narrow the Deviations row.

### [low] ProviderSessionRuntime decode accepts rows that upstream rejects or skips, despite its 'matching upstream' doc comment
- `crates/persistence/src/provider_session_runtime.rs:351` · dimension: wire-compat · upheld 2/2
- Evidence: decode_runtime checks only `status` and the two JSON columns. `runtime_mode` is copied as any String (line 365), and `provider_instance_id` is not checked against the slug. Upstream decodes with ProviderSessionRuntimeDbRowSchema: `runtimeMode: RuntimeMode` (Literals approval-required|auto-accept-edits|auto|full-access) and `providerInstanceId: NullOr(ProviderInstanceId)` (TrimmedNonEmptyString with a max length and slug pattern). In `list` it skips rows that fail (`provider.session.runtime.row-skipped`), and `getByThreadId` returns PersistenceDecodeError. A row with runtime_mode='yolo' is skipped upstream but returned by Turin. The module doc (line 4) says this matches upstream.
- Fix: Validate runtime_mode against the four upstream literals (or an enum) and provider_instance_id against the slug pattern in decode_runtime, so list skips and get fails the same rows. Otherwise, correct the doc comment and add a Deviations row.

### [low] Migration-50 backfill URL and trim handling differs from WHATWG URL and JS trim, so it can write different projection_thread_pull_requests keys
- `crates/persistence/src/legacy_pull_request_key.rs:194` · dimension: wire-compat · upheld 2/2
- Evidence: Upstream uses `new URL(...)` (hostname and pathname, with the default port removed from `url.host`), `.trim()` (JS) and `.toLowerCase()`. Turin's parse_http_url accepts only a lowercase `https://` or `http://` prefix, treats the query and fragment as part of the path, keeps `:443` in the authority (line 215), and lowercases ASCII only (line 121). Rust `str::trim` does not strip U+FEFF (line 84). Checked with node: `new URL("HTTPS://github.com/Org/Repo/pull/12").hostname === "github.com"`, where Turin falls back to host "unknown". `new URL("https://codeberg.org:443/org/repo/pulls/5").host === "codeberg.org"`, where Turin's forgejo key host is "codeberg.org:443". `"﻿".trim() === ""` in JS but not in Rust, so a row upstream skips gets inserted.
- Fix: Parse with the `url` crate (WHATWG-compliant): use host_str() and path() for regex matching, and host plus a non-default port() for the authority. Use `js_trim` from base_schemas and `to_lowercase()` to match JS.

### [low] Fresh-DB sqlite_master text differs from upstream in two places (cosmetic)
- `crates/persistence/src/migrations.rs:18` · dimension: wire-compat · upheld 2/2
- Evidence: I built a DB from SQL extracted from upstream migrations and compared sqlite_master against a Turin-migrated DB. All 196 objects and the 62 ledger ids and names match after whitespace normalization. Two raw-text differences remain. (1) Effect 4.0.4 Migrator (`/tmp/qc-effect404/node_modules/effect/src/sql/Migrator.ts:160`) creates `CREATE TABLE IF NOT EXISTS ${sql(table)} (...)`, which quotes the identifier as "effect_sql_migrations"; Turin's ENSURE_TABLE leaves it unquoted. (2) application_event_source.rs:39 creates idx_orchestration_events_application_sequence with different indentation (`          ON orchestration_events(...)` vs upstream `    ON ...`). Neither changes behavior.
- Fix: For byte-identical schema text, quote the table name in ENSURE_TABLE and copy upstream's whitespace for the index in application_event_source.rs.

### [medium] Dropped event-stream subscribers stay in LiveHub and keep receiving deep-cloned events until they overflow
- `crates/persistence/src/orchestration_event_store.rs:1158` · dimension: perf-concurrency · upheld 2/2
- Evidence: `LiveHub::subscribe` (l.1135) pushes a `LiveSub` clone into `subs`. Nothing ever removes it: `LiveSub.id` is only copied in `clone_sub` (l.1195), `EventSubscription` has no Drop impl that unregisters, and `publish` (l.1160) only drops a sub once `state.failed.is_some()`. Dropping the `EventSubscription` aborts the task but leaves the buffer registered. On every later publish that buffer gets `event.clone()` (a deep serde_json::Value copy, l.1185) plus a JSON serialization (l.1166). This only stops after 1,000 items, when the item cap marks the sub failed. I tested it with a throwaway program in /tmp: after `drop(store.stream_application_events(0))`, 900 publishes of 64 KiB payloads went into the dead subscriber (about 56 MiB kept in memory) with no overflow. Upstream uses `PubSub.subscribe(committedEvents)`, which is scoped, so the subscription goes away when the stream's scope closes.
- Fix: Unregister on drop. Give `EventSubscription` (or a guard owned by the drive task) a Drop impl that removes the sub with `id` from `hub.subs`. Or keep a `Weak`/`Arc` strong count, or a closed flag that `publish` checks before buffering.

### [medium] Live-stream byte budget leaves out agent-event payloads, so the 8 MiB cap never applies to V2 events
- `crates/persistence/src/orchestration_event_store.rs:1262` · dimension: perf-concurrency · upheld 2/2
- Evidence: The budget measures each event as `serde_json::to_string(event).len()` (l.1166, l.1235). The hand-written `impl Serialize for StoredAgentEvent` (l.1262-1269) writes only `sequence`, `commandId` and `type`, with no payload. So each agent event counts as about 50 bytes however large its payload is, and `LIVE_STREAM_MAX_BYTES` (8 MiB) can never trip for agent events. In the /tmp probe, 900 events with 64 KiB payloads each (56 MiB) were buffered for one subscriber with no overflow. Upstream `serializedSize(payload)` (LiveStreamBudget.ts) measures the whole projected wire object, payload included, so the memory bound is real. Right now one subscriber can hold 1,000 x (largest payload) bytes, and every subscriber holds its own copy.
- Fix: Measure the full event, payload and metadata included: serialize the complete `StoredAgentEvent`/`ProjectEvent` the way they go on the wire, or add `payload_json.len()` carried over from the row. Compute the size once per event, not once per subscriber (see the next finding).

### [low] publish() serializes and deep-clones every event once per subscriber while holding the hub lock; drain re-serializes it
- `crates/persistence/src/orchestration_event_store.rs:1166` · dimension: perf-concurrency · upheld 2/2
- Evidence: Inside `subs.retain(...)`, which holds the `subs` mutex and each sub's state mutex, every event is serialized with `serde_json::to_string(event)` (l.1166) and deep-cloned with `state.items.push_back(event.clone())` (l.1185). That is O(subscribers x events) full JSON encodes plus Value clones on the commit hot path. `drain_after` serializes each event a second time just to subtract its size (`event_bytes(&event)`, l.1221/1235). Upstream shares immutable event objects across subscriptions and caches each object's size once in a `WeakMap` (`serializedSizes`, LiveStreamBudget.ts). `publish_committed` is called after every write commit, and with the payload fix above these encodes cover full payloads.
- Fix: Compute each event's size once in `publish`, outside the per-sub loop. Store `Arc<ApplicationStoredEvent>` (or `(Arc<_>, usize)`) in `LiveBuf.items` so subscribers share one allocation. Keep that stored size for the decrement in `drain_after` instead of serializing again.

### [medium] Stream replay loads the whole history into memory and holds the single reader connection for the entire replay
- `crates/persistence/src/orchestration_event_store.rs:323` · dimension: perf-concurrency · upheld 2/2
- Evidence: `drive_stream` makes one `db.read(move |conn| read_application_events(conn, ... through_sequence: high))` call. `read_application_events` runs every 500-row page inside that one closure and collects them all into a `Vec` before the first event is sent. `Db::read` keeps the reader `Mutex<Connection>` locked for the whole closure (db.rs:152-158). Upstream `catchUpApplicationEvents` is a lazy `Stream.paginate`: each page is its own query that takes the reader semaphore per statement, so other reads interleave and memory stays at one page. In the /tmp probe with 21,000 x 4 KiB events, a full replay took 510 ms and a concurrent `latest_application_sequence()` waited 504 ms behind it. A client reconnecting from sequence 0 on a real T3 database (hundreds of thousands of events) would stall every other read and buffer the full log in RAM. The same applies to `read_agent_events` with no limit.
- Fix: Paginate in `drive_stream`. Loop on `db.read` with one 500-row page per call (cursor = last sequence) and send each page before fetching the next. Optionally expose a page-at-a-time API for `read_application_events`/`read_agent_events` instead of returning the whole Vec.

### [low] Checkpoint thread polls SQLite every 100 ms forever, even when the app is idle
- `crates/persistence/src/db.rs:376` · dimension: perf-concurrency · upheld 2/2
- Evidence: `checkpoint_loop` calls `notify.cv.wait_timeout(guard, CHECKPOINT_IDLE)` (100 ms) in a loop, and on every timeout runs `checkpoint_if_due(&conn, true)`, which runs `PRAGMA wal_checkpoint(NOOP)`. With no writes the timeout fires every time, so the thread wakes and queries the WAL index 10 times a second for as long as the app runs. Upstream arms a one-shot `setTimeout(finish, idleMs)` only after a check request (`clearTimeout(idle); idle = setTimeout(...)` inside `parentPort.on("message")`), so an idle app does no checkpoint work. For a desktop GPUI app this is a constant wake-up source that costs battery.
- Fix: Do only one idle check after activity. Track a `dirty_since_idle` flag that `kick` sets. Use `cv.wait()` with no timeout while it is clear, and `wait_timeout(100ms)` only while it is set. Clear it after the idle `checkpoint_if_due` runs.

### [low] Concurrent writes each take a blocking-pool thread that parks on the std writer mutex
- `crates/persistence/src/db.rs:128` · dimension: perf-concurrency · upheld 2/2
- Evidence: `Db::write` calls `tokio::task::spawn_blocking(move || { let mut conn = lock_mutex(&inner.writer)?; ... })`. With N concurrent writers, N blocking-pool threads start and all but one block on `std::sync::Mutex::lock` (or on `inner.writer` from `Db::read` when there is no reader). `:memory:` reads take the same path at l.157. Upstream waits on `Semaphore.make(1)` (nodeSqliteClient.ts:812) without taking a thread. A burst of provider events can fill tokio's blocking pool, 512 threads by default, and starve other `spawn_blocking` users such as fs and git work.
- Fix: Acquire an async gate first, e.g. `tokio::sync::Mutex<()>` or a `Semaphore(1)` (one for the writer, one for the reader). Hold its owned guard into the `spawn_blocking` closure, so only one blocking thread per connection exists at a time.

### [low] Statements are re-prepared on every call; upstream keeps a 200-entry prepare cache
- `crates/persistence/src/orchestration_event_store.rs:470` · dimension: perf-concurrency · upheld 2/2
- Evidence: `append_agent_events` calls `conn.prepare(&sql)` for every event in the batch (l.470). `append_project_event` (l.373), `query_rows` (l.788, once per 500-row page), the receipts and provider-session repos, and `pull_request_files_viewed::set` (`conn.execute` per file) also prepare fresh each time. No `prepare_cached` is used anywhere in the crate, and the rusqlite cache capacity is never set. Upstream nodeSqliteClient uses `Cache.makeWith(prepare, { capacity: options.prepareCacheSize ?? 200 })`. The agent-event append is the hottest write path (one per provider event) and runs inside the writer transaction, so each compile lengthens the time the writer is held.
- Fix: Use `conn.prepare_cached(...)` for the fixed SQL strings. The two `agent_insert_sql` variants and the `query_rows` shapes are few and stable, so they can be cached too. Call `conn.set_prepared_statement_cache_capacity(200)` on the writer and reader to match upstream.

### [low] PromptHints::remember scans the 4096-entry order queue on every repeated subagent tick
- `crates/persistence/src/orchestration_event_store.rs:155` · dimension: perf-concurrency · upheld 1/2
- Evidence: `if self.values.contains_key(&key) { self.order.retain(|existing| existing != &key); }` is O(PROMPT_HINT_CAP) with string comparisons. Repeated progress ticks for the same subagent are the case this cache exists for (upstream comment: "Progress ticks repeat an unchanged prompt"), so the worst case is the common case. It runs inside the writer transaction, and `hints` is locked twice per event (l.454 and l.494). Upstream uses a JS `Map` with delete+set, which is O(1) LRU reordering.
- Fix: Use an O(1) LRU, such as the `lru` crate or an `IndexMap` with `shift_remove`/insert. Or store a generation counter per key and drop stale `order` entries lazily.

### [low] Process: entire persistence port landed as a single commit
- `crates/persistence/src/lib.rs:1` · dimension: perf-concurrency · upheld 2/2
- Evidence: `git log --oneline main..HEAD` shows one commit: `cbd6ae6 feat: port the sqlite persistence layer`, covering 82 files and +7043 lines (Db, all migrations, event store, repos, tests). Not a blocker, per the task brief.
- Fix: In future tasks, commit in reviewable steps (e.g. Db + pragmas, migrations 001-054, V2 migrations, each repository).

### [medium] About 22 upstream migrations lost their column-existence guards, and this is not logged in Deviations
- `crates/persistence/sql/027_ProviderSessionRuntimeInstanceId.sql:1` · dimension: process · upheld 2/2
- Evidence: Upstream 027_ProviderSessionRuntimeInstanceId.ts: `const columns = yield* sql`PRAGMA table_info(provider_session_runtime)`; if (!columns.some((column) => column.name === "provider_instance_id")) { ALTER TABLE ... }`. Turin's 027 SQL is a bare `ALTER TABLE provider_session_runtime ADD COLUMN provider_instance_id TEXT;`. The same guard was stripped from 017, 021, 022, 028, 032-036, 038-043, 045, 047-049, 051 and 054. Each of these is MigrationKind::Sql and runs through execute_batch with no check. Reproduced: on a table that already has provider_instance_id, running 027.sql fails with `Parse error near line 1: duplicate column name: provider_instance_id`. Migrations run in one transaction, so the whole migrate rolls back and Db::open fails. Upstream has tests for exactly these cases: `027_028_ProviderInstanceIdColumns.test.ts` ("continues when provider_session_runtime was partially migrated") and `051_ProjectionThreadMessageContext.test.ts` ("accepts context added by an earlier development migration"). The Deviations table does not mention this change, and it breaks the T23 goal of opening real T3 databases.
- Fix: Add a MigrationKind for guarded ALTERs, or encode the guard in the manifest (for example `add_column_if_missing(table, column, ddl)`), so each upstream PRAGMA table_info check is reproduced exactly. Port the 027_028 and 051 tests.

### [medium] Upstream's per-migration data tests and Errors.test.ts were not ported
- `crates/persistence/src/behavior_tests.rs:1` · dimension: process · upheld 2/2
- Evidence: Upstream has 23 migration tests that check deterministic SQL data transforms and schema: Migrations/016, 019, 024, 025, 026, 027_028, 029, 031, 035, 040, 041, 042, 046, 047, 049, 050 (x2), 051, 054, 055 (x3: contiguous ids, upgrade from released schema 53, application-event-source baseline, effect-cancellation), 056, 057. Turin's 25 tests cover only Migrations.test.ts (migrations_test.rs), Sqlite/initializeV2/event-store/receipts behaviour, and one unit test for the 050 key function (legacy_pull_request_key.rs:284). Nothing exercises the 016/024/026/046 backfills or the 055 upgrade-from-53 path. Errors.test.ts is also unported, although its cases are pure logic ("keeps SQL operation context without a tautological detail", "reads the condition through a wrapping driver error", "omits a detail for a cause it cannot categorize"); errors.rs has no tests. PORTING rule 5 requires porting pure-logic tests, and these are the tests that would have caught the dropped guards.
- Fix: Port each Migrations/*.test.ts as a #[cfg(test)] case: seed the pre-migration schema with run_migrations(to_migration_inclusive = N-1), insert the upstream fixture rows, migrate to N, and assert the same rows. Port the pure Errors.test.ts cases against sqlite_failure_detail/sql_error.

### [medium] Branded ids replaced by String and the turin-contracts dependency removed, with no Deviations row
- `crates/persistence/Cargo.toml:8` · dimension: process · upheld 2/2
- Evidence: `git diff main...HEAD -- crates/persistence/Cargo.toml` removes `turin-contracts.workspace = true`. At the merge base (dc9d635), contracts/base_schemas.rs:163 already declares `id!(ThreadId, ProjectId, CommandId, EventId, RunId, NodeId, RawEventId, ...)`. The persistence API still uses plain strings everywhere: orchestration_command_receipts.rs:58 `pub command_id: String`, :60 `aggregate_id: String`; orchestration_event_store.rs:44-51 and :71-80 (`event_id`, `thread_id`, `run_id`, `node_id`, `raw_event_id` as String); provider_session_runtime.rs:41 `pub thread_id: String`. PORTING's mapping table (on main and on the branch) says branded ids map to the `id!` newtypes, and main adds "Never hand-write id newtypes". The task only accepts serde_json::Value for payloads as an interim choice, not ids. The Deviations rows cover payloads (line 155) and timestamps (line 162) but not ids.
- Fix: Restore the turin-contracts dependency and use ThreadId/CommandId/EventId/ProjectId/RunId/NodeId/RawEventId in the public row and input structs. If that is deferred, add a Deviations row saying ids are String until callers land.

### [medium] The Deviations row hedges that bundled SQLite "may be older" than 3.51, but it is 3.50.2, so the checkpoint worker is dead code that warns on every open
- `crates/persistence/src/db.rs:310` · dimension: process · upheld 2/2
- Evidence: Cargo.lock pins libsqlite3-sys 0.35.0, whose bundled sqlite3.h has `#define SQLITE_VERSION "3.50.2"`. In start_checkpoint_worker (db.rs:300-315), `if !sqlite_supports_noop_checkpoint(&version) { tracing::warn!(..."SQLite checkpoint worker unavailable..."); return None; }` therefore always returns early. The checkpoint thread, the 10000-frame backstop and the idle flush never run in any build or test, yet the Deviations row (PORTING.md:157) describes them as Turin's behaviour ("a std thread for checkpoints ... 100ms idle flush, and writer autocheckpoint of 10000").
- Fix: Bump rusqlite/libsqlite3-sys to a version that bundles SQLite 3.51 or newer and add a test that the worker starts. Otherwise, rewrite the Deviations row to say plainly that the bundled SQLite is 3.50.2 and the worker is always off, and drop the per-open warn to debug.

### [low] The 'generated' migration SQL has no committed generator and no per-file upstream provenance
- `crates/persistence/src/migration_manifest.rs:1` · dimension: process · upheld 2/2
- Evidence: migration_manifest.rs:1 says `//! Generated migration scripts. SQL is the upstream template text.`, but no generator or extraction script is in the commit (no file under scripts/ or crates/persistence other than sql/ and src/). sql/055_OrchestrationV2.sql is 549 lines concatenated from 11 upstream files (Migrations/OrchestrationV2/{Foundation,ThreadLaunchWorkflows,ApplicationEventSource,EffectCancellation,Subagents,ScheduledTasks,...}.ts). Its only comment is `-- TURIN_APPLICATION_EVENT_SOURCE --` at line 422, so no section can be traced back to its upstream file. PORTING rule 2 requires each port to name its upstream source "so it can be diffed later", and T23 and future upstream syncs depend on re-extracting this SQL.
- Fix: Commit the extraction script (for example scripts/extract-migrations.mjs) and have migration_manifest.rs name it. Add a `-- Port of upstream/apps/server/src/persistence/Migrations/<file>.ts` header to each SQL file and to each section of 055.

### [low] Several modules lack the required `//! Port of upstream/<path>` header
- `crates/persistence/src/migrations_test.rs:1` · dimension: process · upheld 2/2
- Evidence: migrations_test.rs starts directly with `use rusqlite::Connection;` and has no module doc. Its counterpart is Migrations.test.ts. behavior_tests.rs:1 reads `//! Ports of Sqlite.test.ts, initializeV2Database.test.ts, and the persistence repository tests...`, with no upstream paths. legacy_pull_request_key.rs:1 reads `//! Port of the migration-50 backfill in `Migrations/050_...`` instead of the `upstream/` path form. PORTING rule 2: "Every module starts with `//! Port of upstream/<path>`".
- Fix: Add `//! Port of upstream/apps/server/src/persistence/Migrations.test.ts` to migrations_test.rs. Use full upstream/ paths in behavior_tests.rs and legacy_pull_request_key.rs (`upstream/apps/server/src/persistence/Migrations/050_ProjectionThreadPullRequests.ts`, `upstream/packages/shared/src/threadPullRequests.ts`).

### [low] effect_sql_migrations DDL text differs from upstream, the only schema diff against a real T3 DB, and it is not logged
- `crates/persistence/src/migrations.rs:17` · dimension: process · upheld 2/2
- Evidence: I compared sqlite_master of a fresh Turin DB (Db::open, 62 migrations) with a read-only copy of a real upstream statev2.sqlite whose ledger is at 62_ThreadFailureLookupIndex. Every table, index and trigger matches after whitespace normalisation except: real `CREATE TABLE "effect_sql_migrations" (...)` versus Turin `CREATE TABLE effect_sql_migrations (...)` (Effect's Migrator quotes the identifier). Turin also creates `sqlite_stat4`, because the bundled SQLite is built with STAT4, while upstream DBs have none. The task requires the exact upstream schema.
- Fix: Quote the identifier in ENSURE_TABLE (`CREATE TABLE IF NOT EXISTS "effect_sql_migrations" (`). Note the sqlite_stat4 difference in Deviations, or disable STAT4.

### [low] Main's PORTING.md rule "every `*_at` column → UtcDateTime" contradicts its own IsoDateTime pass-through rule for persistence
- `PORTING.md:75` · dimension: process · upheld 2/2
- Evidence: `git show main:PORTING.md` line 74: "`IsoDateTime` (`Schema.String`) | `String`, passed through untouched". Line 75: "`DateTimeUtc` / `DateTimeUtcFromString`, every `*_at` column | `UtcDateTime`: always encodes ...". Upstream OrchestrationEventStore.ts types project events' `occurredAt: IsoDateTime` (AppendProjectEventRequestSchema and ProjectEventPersistedRowSchema), and upstream baseSchemas.ts:47 has `IsoDateTime = Schema.String`. So `orchestration_events.occurred_at` must round-trip untouched for project events. An agent applying the "every `*_at` column" rule would re-encode these values. The branch's timestamps Deviations row (PORTING.md:162, "Caller-supplied strings are stored as given") is correct for those columns.
- Fix: On main, narrow line 75 to columns whose upstream schema is DateTimeUtc/DateTimeUtcFromString, and say explicitly that `*_at` columns typed IsoDateTime stay String pass-through.

### [low] Process: one 7k-line commit, and the PORTING.md Deviations rows conflict with main
- `PORTING.md:154` · dimension: process · upheld 2/2
- Evidence: `git log main..HEAD` shows a single commit, cbd6ae6 "feat: port the sqlite persistence layer" (82 files, +7043). It does carry `Model: grok-4.7 (cursor)`. Main's AGENTS.md requires committing after every module or logical unit. `git merge-tree --write-tree main HEAD` reports `CONFLICT (content): Merge conflict in PORTING.md`, because main appended 3 contracts rows at the same spot. The branch's rows also put upstream paths in the File column, while main's rows use Turin paths (`contracts/base_schemas.rs`).
- Fix: Not a blocker. When merging main, resolve the Deviations conflict by keeping both sets of rows and use a consistent File-column convention. Future tasks should commit per module.
