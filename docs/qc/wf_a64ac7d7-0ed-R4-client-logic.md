# QC wf_a64ac7d7-0ed — R4 composer + client-runtime logic

Reviewer: Claude QC workflow (Sonnet 5.5 reviewers + skeptics, Sonnet lead) (dimension reviewers, 2 skeptics per finding).
Confirmed: 13 · refuted: 14

## Lead report

FIX LIST (deduplicated, ranked)

1. MEDIUM, crates/app/src/composer/prompt_stash_store.rs:107-108
   - Problem: `records: Vec<ComposerContextRecord>` uses a plain serde decode. Upstream uses `ForwardCompatibleArray`, which drops only the undecodable elements. In Rust, one record that fails the untagged `ComposerContextRecord` decode fails the whole `PersistedPromptStashState`. `read_persisted` then returns None and `new()` loads an empty queue. The next `stash_entry` overwrites the stored key, so every earlier stashed prompt is lost. PORTING.md:59 requires the `forward_compatible_array` mapping here.
   - Fix: annotate the field with `#[serde(default, skip_serializing_if = "Vec::is_empty", with = "turin_contracts::base_schemas::forward_compatible_array")]`. Alternatively, expose `composer_context::records_field::decode` (currently `pub(crate)`). Add a test in prompt_stash_store_tests.rs where one malformed record sits next to valid entries and the other entries and records still restore.

2. LOW, crates/app/src/state/file_metadata.rs:360-367 (`remember`)
   - Problem: `thread.entries.get(key).and_then(|e| e.value.clone())` inside `let Some(previous) = ... else { continue }` unwraps the inner Option. A retained entry cached as `None` (a missing path) is skipped and never refreshed. Upstream tests the entry wrapper (`retained?.get(key)`), which is truthy even when its value is null. `lookup` returns retained entries even after expiry, so the thread keeps reporting the file as missing until `invalidate`.
   - Fix: look up the entry itself with `thread.entries.get(key)` and `continue` only when the entry is absent. Merge only when `partial && next.is_some() && previous.value.as_ref().map(|v| v.kind) == next.map(|n| n.kind)`. Otherwise store `value.clone()`. Add a test: a thread retains None for P, then `remember_file` or `remember_entries` runs for P, then the thread reads P and gets the file.

3. LOW, crates/app/src/state/file_metadata.rs:166-180 (`metadata_many`)
   - Problem: `begin_request` inserts tokens into `state.in_flight`. They are removed only after `get_metadata().await` returns or on the Err branch. Dropping the future mid-await leaks them. Upstream uses `Effect.ensuring`. `invalidate` and `reset` keep scanning the leaked tokens.
   - Fix: hold the tokens in a drop guard that calls `end_request`. Add a test that drops the future mid-request and asserts `in_flight` is empty.

4. LOW, crates/app/src/composer/attachment_upload_queue.rs:660 and :720 (also `new` at :323)
   - Problem: `pump` and the reconnect `on_change` callback call bare `tokio::spawn`. `start`, `retry` and `finish` are sync. The GPUI main thread is not inside the runtime (main.rs only passes `runtime.handle()` and never enters it). The first UI call will panic with "no reactor running". Nothing constructs the queue outside tests yet, so this is latent.
   - Fix: take a `tokio::runtime::Handle` in `AttachmentUploadQueue::new` and use `handle.spawn` in both places. Add a test that calls `start()` from a plain `std::thread` with no runtime entered.

5. LOW, crates/app/src/composer/attachment_upload_queue.rs:893-898 (`run_cycle`)
   - Problem: `cancelled` is read under one `job.inner` lock and `attachment_id` is set under a second lock. `cancel()` on another thread can run between them. It sets `cancelled`, sees `attachment_id == None` and skips `delete_pending`. `run_cycle` then stores the id, `resolve_upload_url` returns None, and `run_upload` returns on `cancelled`. The minted pending upload is leaked until the server-side sweep. Upstream does both steps in one JS turn.
   - Fix: check `cancelled` and set `attachment_id` in a single critical section. If the job is cancelled, call `delete_pending` on the minted id.

6. LOW, crates/app/src/composer/attachment_upload_queue.rs:124-133 (`upload_mime`)
   - Problem: it returns `Some("")` for a File with an empty mimeType. Upstream's `!mimeType` check fails the job with "Unsupported image type". Rust proceeds to mint with an empty mime.
   - Fix: return None for a File whose lowercased mime is empty. Add a test.

7. LOW, crates/app/src/composer/composer_context_records.rs:104-113, called at :516-517, :533-535, :578 and :781-783
   - Problem: `must_context_id`, `must_attachment_id` and `must_nonempty` panic. The builders `terminal_context_record`, `review_comment_context_record`, `attachment_context_record`, `uploaded_attachment_context_record` and `build_message_context` can crash the app on a blank name, mime type, terminal id or section id, or on an invalid attachment id. Upstream only copies fields and fails later with a catchable encode error. Nothing in production calls these yet, so this is latent.
   - Fix: return `Result` or `Option` from the builders and surface the error at the send boundary. At minimum, `uploaded_attachment_context_record` should return None for an invalid id. If panicking is kept, add a PORTING.md Deviations row documenting the preconditions.

8. LOW, docs/handoff/R4.md:11, :14, :49 and the Done table
   - Problem: "State at handoff" still cites port commit da0c739 and "2987 passed". HEAD is 2052e59a and the gate gives 3056 passed. The `safe_log` row says 4 tests but safe_log_tests.rs has 5. `state/errors/error_graph.rs` is missing from the Done table.
   - Fix: update the commit and test count, set `safe_log` to 5, and add an `error_graph.rs` row.

9. LOW and cosmetic (optional; each was upheld by only 1 of 2 verifiers)
   - attachment_upload_queue.rs:824 (after `run_cycle` in `run_upload`): upstream sets `job.abort = null` after the transfer ends. Rust leaves a stale abort closure, so a later `cancel()` (:626-628) invokes it on a finished transfer. Fix: add `job.inner.lock().abort = None;` right after `run_cycle` returns, before the cancelled and result checks.
   - attachment_upload_queue.rs:1078-1084: private `parse_scoped_thread_key` duplicates `state::models::parse_scoped_thread_key` (models.rs:325). Its behaviour matches upstream and it has a single caller (:1009). Fix: leave it as is, or reuse the models helper and compare `env.as_str()`. This is not a PORTING.md violation.

VERDICT: PASS (no critical or high findings; fix item 1 before merge, since it can silently destroy all stashed prompts).

## Confirmed findings (raw)

### [low] remember() skips retained thread entries whose cached value is null, so a retained missing-file entry never updates
- `crates/app/src/state/file_metadata.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Rust (line 361): `let Some(previous) = thread.entries.get(key).and_then(|entry| entry.value.clone()) else { continue; };`. Upstream fileMetadata.ts `remember`: `const previous = retained?.get(key); if (retained && previous) retained.set(key, {...entry, value: partial && value && previous.value?.kind === value.kind ? {...previous.value, ...value} : value})`. `previous` is the CachedMetadata object, which is truthy even when `previous.value === null`. Rust unwraps the inner `Option` instead, so an entry cached as `None` (a missing path) is skipped.
- Fix: Look up the retained entry (`thread.entries.get(key)`) and `continue` only when the entry is absent. Then compute `merged` as `partial && next.is_some() && previous_value.as_ref().map(|v| v.kind) == next.map(|n| n.kind)` ? merge : `value.clone()`. A null previous value then takes the new value, as upstream does. Add a test: a thread retains None for P, then `remember_file`/`remember_entries` for P, then the thread reads P and gets the file.

### [low] Prompt stash `records` decodes with a plain Vec instead of ForwardCompatibleArray, so one undecodable record wipes the whole persisted stash
- `crates/app/src/composer/prompt_stash_store.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Line 107-108: `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub records: Vec<ComposerContextRecord>`. Upstream StashEntrySchema has `records: Schema.optionalKey(ForwardCompatibleArray(ComposerContextRecord))`, which drops the elements it cannot decode. In Rust a single record that fails the untagged `ComposerContextRecord` decode makes `serde_json::from_value` fail for the whole `PersistedPromptStashState`. `read_persisted` then returns None and `new()` falls back to an empty queue, so every stashed prompt is lost.
- Fix: Decode `records` with a drop-invalid codec. `contracts::composer_context::records_field::decode` already does this per record. It is `pub(crate)` in turin-contracts, so either expose it or use `base_schemas::forward_compatible_array` via `#[serde(with = ...)]` on the field. Add a test with one malformed record next to a valid entry.

### [low] Upload job `abort` is never cleared after the transfer ends (upstream sets `job.abort = null`)
- `crates/app/src/composer/attachment_upload_queue.rs:` · dimension: fidelity · upheld 1/2
- Evidence: Upstream runUpload: after `runAttachmentUploadCycle` returns, `job.abort = null;` runs before any result handling. In Rust `run_cycle` stores `inner.abort = Some(begun.abort)` (line 945) and nothing clears it. A failed job stays in `jobs` for retry or reconnect-retry. A later `cancel()` (line 626-628) then calls the stale `abort` of an already finished transfer.
- Fix: After `run_cycle` returns in `run_upload`, run `job.inner.lock().abort = None;`, before the cancelled/result checks.

### [low] Empty-string file MIME type is not rejected as 'Unsupported image type'
- `crates/app/src/composer/attachment_upload_queue.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Upstream: `const mimeType = job.image.type === "file" ? job.image.mimeType.toLowerCase() : ...find(...); if (!mimeType) { setUploadState(... reason: "Unsupported image type") }`. An empty string is falsy, so a file with `mimeType === ""` fails. Rust `upload_mime` (line 124-133) returns `Some(mime)` for any File, including `Some("")`. The upload then proceeds and mints with an empty mime type.
- Fix: In `upload_mime`, return `None` for a File when the lowercased mime is empty.

### [low] parse_scoped_thread_key reimplemented locally instead of using the existing port
- `crates/app/src/composer/attachment_upload_queue.rs:` · dimension: fidelity · upheld 1/2
- Evidence: Lines 1078-1084 define a private `parse_scoped_thread_key(key) -> Option<(String, String)>` (the `parseScopedKey` rules). `crates/app/src/state/models.rs:325` already exports `pub fn parse_scoped_thread_key(key: &str) -> Option<(EnvironmentId, ThreadId)>`. PORTING.md asks for one copy of each helper.
- Fix: Call `state::models::parse_scoped_thread_key` and compare the environment id, or move the shared splitting rule into one place and have both call it.

### [low] Record builders panic (must_nonempty / must_attachment_id / must_context_id) where upstream only builds a plain object
- `crates/app/src/composer/composer_context_records.rs:` · dimension: fidelity · upheld 2/2
- Evidence: `terminal_context_record` calls `must_nonempty(&context.terminal_id)` and `must_nonempty(&context.terminal_label)`. `review_comment_context_record` calls `must_nonempty` on `section_id` and `file_path`. `attachment_context_record` (lines 781-783) calls `must_attachment_id`, and `must_nonempty` on name and mime type. Each of these `.unwrap_or_else(panic!)`. Upstream `terminalContextRecord`, `reviewCommentContextRecord` and `attachmentContextRecord` just copy the fields, and schema validation happens later at encode time. In Rust an attachment with an empty name or mime type, or an id outside `[A-Za-z0-9_-]{1,128}`, crashes the app while building a message. Upstream would give an encode error at send.
- Fix: Have the builders return `Result` (or the unvalidated draft shape) and surface the contract error at the send boundary. At minimum, document the panic as a precondition in the PORTING.md row.

### [low] Retained thread entry holding a missing-path (null) value is never refreshed by remember()
- `crates/app/src/state/file_metadata.rs:` · dimension: correctness · upheld 2/2
- Evidence: Lines 361-367: `let Some(previous) = thread.entries.get(key).and_then(|entry| entry.value.clone()) else { continue; };`. Upstream fileMetadata.ts `remember` tests the entry object, not its value: `const previous = retained?.get(key); if (retained && previous) retained.set(key, {...entry, value: partial && value && previous.value?.kind === value.kind ? {...previous.value, ...value} : value})`. A retained `CachedMetadata { value: None }` (a path the server reported missing) is truthy upstream and gets overwritten. In the Rust it is skipped.
- Fix: Look up the retained entry itself (`thread.entries.get(key)`), then merge only when `previous.value` is `Some` and `partial` and the kinds match. Otherwise store `value.clone()`. Example: a chip's path was missing in thread T, so T's retained entry is `None`. Then `remember_file` or `remember_entries` runs, or another thread fetches it. Upstream updates T's entry to the file. Rust leaves it `None`. `lookup` returns retained entries even after expiry, so the chip keeps showing the file as missing until `invalidate`.

### [medium] Prompt stash `records` decodes strictly, so one bad record drops every stashed prompt
- `crates/app/src/composer/prompt_stash_store.rs:` · dimension: correctness · upheld 2/2
- Evidence: Line 108 declares `pub records: Vec<ComposerContextRecord>` with plain serde. Upstream declares `records: Schema.optionalKey(ForwardCompatibleArray(ComposerContextRecord))`, which drops only the undecodable elements. In Rust, `ComposerContextRecord` is `#[serde(untagged)]` (crates/contracts/src/composer_context.rs:698). A known-kind record with a broken payload fails both variants, so `read_persisted` (`serde_json::from_value(state).ok()?`) returns None for the whole payload. `PromptStashStore::new` then sets `entries` to empty. The next `stash_entry` writes `[new]` over the stored key, so all earlier stashed prompts are lost permanently. The contracts crate already provides tolerant decoding (`forward_compatible_array`, `records_field`).
- Fix: Annotate the field with `#[serde(default, skip_serializing_if = "Vec::is_empty", with = "turin_contracts::base_schemas::forward_compatible_array")]`. Add a test that a stored payload with one malformed record still restores the other entries.

### [low] Attachment queue uses bare tokio::spawn from synchronous start/retry/release paths
- `crates/app/src/composer/attachment_upload_queue.rs:` · dimension: correctness · upheld 2/2
- Evidence: Line 660 (`pump`) and line 720 (reconnect watcher callback) call `tokio::spawn(...)`. `pump` is reached from the sync `pub fn start`/`retry`/`finish`. The reconnect `on_change` callback comes from `watch_connection` and may run on any thread. `tokio::spawn` panics with 'there is no reactor running' when the caller is not inside a runtime context. The rest of the crate passes an explicit `tokio::runtime::Handle` (e.g. `spawn_client_state(&Handle, ..)` in app_runtime), and the GPUI main thread is not a tokio worker. The queue holds no Handle.
- Fix: Give `AttachmentUploadQueue::new` a `tokio::runtime::Handle` and use `handle.spawn`, including inside the connection-change callback. Add a test that calls `start()` from a plain `std::thread` with no runtime entered.

### [low] Cancel can race the minted-id bookkeeping and leak the minted upload
- `crates/app/src/composer/attachment_upload_queue.rs:` · dimension: correctness · upheld 2/2
- Evidence: Lines 893 and 898 take `job.inner` twice: `let cancelled = job.inner.lock().cancelled;` and later `job.inner.lock().attachment_id = Some(...)`. Upstream's `onMinted` runs in one JS turn. On the multi-threaded runtime, `cancel()` can run between the two locks. It sets `cancelled`, reads `attachment_id == None`, and so does not call `delete_pending`. `run_cycle` then stores the id, `resolve_upload_url` returns None because the job is cancelled, and `run_upload` returns on `cancelled`. The minted pending upload is never deleted, so it lingers on the server until the 24h sweep.
- Fix: Check `cancelled` and set `attachment_id` under a single `job.inner` lock. Decide cancel-or-continue from that one critical section, and delete the mint if cancelled.

### [low] fn metadata_many leaks in-flight tokens when its future is dropped
- `crates/app/src/state/file_metadata.rs:` · dimension: correctness · upheld 2/2
- Evidence: Lines 166-175: `begin_request` inserts tokens into `state.in_flight`. They are removed only after `get_metadata(..).await` returns, or via `end_request` on the Err branch. Upstream wraps the request in `Effect.ensuring(...)` so tokens are removed on interruption. Dropping the `metadata_many` future mid-request (view closed, task cancelled) leaves `valid: true` tokens in `in_flight` forever. Memory grows without bound, and `invalidate`/`reset` scan all of them.
- Fix: Hold the tokens in a drop guard that calls `end_request`, or use a scope guard around the await.

### [low] Context record builders panic on invalid data instead of returning an error
- `crates/app/src/composer/composer_context_records.rs:` · dimension: correctness · upheld 1/2
- Evidence: `must_nonempty`, `must_attachment_id` and `must_context_id` (lines 100-110) call `panic!`. They are reached from `attachment_context_record` (line 781: `must_attachment_id(&bound.attachment_id)`, plus `must_nonempty` on name and mime type), `terminal_context_record` (line 516) and `review_comment_context_record` (line 533). Inputs include attachment ids (rejected if they contain a dot, per the handoff note), blank file names and MIME types, and blank terminal ids/labels or review section/file paths. `uploaded_attachment_context_record` returns Option, so callers expect a graceful None. Upstream fails only at schema-encode time with a catchable error.
- Fix: Return `Result` (or `Option`) from the record builders and let the send/copy boundary surface the error. At minimum, `uploaded_attachment_context_record` should return None when the id is not a valid `ContextAttachmentId`.

### [low] R4 handoff doc is stale against the final tree
- `docs/handoff/R4.md:` · dimension: tests-process · upheld 2/2
- Evidence: "State at handoff" still says: "Port commit: `da0c739 feat: port composer placeholder`" and "`scripts/check.sh -p turin` ... green. 2987 passed, 0 failed, 1 ignored." The branch HEAD (2052e59a) adds the Sonnet T17/T14 ports. My run of `scripts/check.sh -p turin` gives "3056 passed; 0 failed; 1 ignored". The test column lists `safe_log` as 4, but `safe_log_tests.rs` has 5 `#[test]`s. The doc also does not list `state/errors/error_graph.rs`, which the diff adds.
- Fix: Update the State section to the final commit and the 3056 count, fix the `safe_log` count, and add `error_graph.rs` to the Done table.

## Fix round

All 9 fix items closed by turin-R4-r3-sonnet in commits 158e6e51..fab6439e. Gate: 3064 passed.

**Final verdict: PASS**
