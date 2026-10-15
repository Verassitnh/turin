# QC wf_66cd96fc-6ed — N3 text generation (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 6 · refuted: 4

## Lead report

1. MEDIUM. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/text_generation_operations.rs:450-535 (review_guide_stop, called via generate_review_guide at :348-352). One malformed field (empty title/note/path/question, or line <= 0) fails the whole review guide as invalid structured output. Upstream's generator is permissive, and PullRequestReviewGuideService's normalizer drops or clamps these items. Required fix: keep the generator permissive and move the drop/clamp rules to the normalizer, or drop only the bad item here. Either way, add a PORTING.md Deviations row if the strict behaviour stays, and add review_guide_stop tests.

2. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/claude_text_generation.rs:149-155. CLAUDE_CONFIG_DIR is set to expand_home_path(...) without absolutizing. Upstream ClaudeHome.ts:25-27 applies path.resolve, so a relative homePath resolves against a different cwd in the child. Required fix: absolutize the expanded path (std::path::absolute or join with the process cwd), add a relative-homePath test, and log it in the PORTING.md Deviations row near line 324 if it stays.

3. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/thread_title_context.rs:12. There is no #[cfg(test)] module. limit_title_message's budget-0 and length<=budget assertions from upstream threadTitleContext.test.ts:42-44, and the head/tail split, are untested. Required fix: port those assertions plus a head/tail case as a #[cfg(test)] module. The logic itself matched upstream in the verifier's differential check.

4. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/command.rs:57-58, 76-89. The process group is SIGKILLed only on timeout, so drop and normal-exit paths leave group members alive. Caveat: one verifier refuted this, because the drop path is documented at PORTING.md:318 and text generation is one-shot. Treat as optional. If fixed, add a drop guard or a group sweep after wait, and update the Deviations row.

5. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/codex_text_generation.rs:413-424 (also :434-437). Blocking std::fs::write, read_to_string and tempfile tempdir run inside async fn run. PORTING.md:102-105 requires async IO. Required fix: use tokio::fs::write and tokio::fs::read_to_string. Apply the same to the tempdir calls in claude_text_generation.rs:246-248 and cursor_text_generation.rs:234-236, or log an exception in Deviations (tempfile has no async API).

6. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/cursor_text_generation.rs:125-127. The tokio::spawn stderr drain drops its JoinHandle, which violates PORTING.md:93 ("Never drop a bare JoinHandle"). Required fix: store the handle in CursorAgentChild and abort it on drop, as crates/acp/src/process.rs does with ProcessGuard. Impact is small because the drain ends on stderr EOF after the group SIGKILL.

Excluded as known: the OpenCode 2.x session leak and the acp writer leak.

VERDICT: PASS (no critical/high)

## Confirmed findings (raw)

### [low] Claude CLAUDE_CONFIG_DIR not resolved to an absolute path, unlike upstream
- `crates/text-generation/src/claude_text_generation.rs:151` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust: `environment.insert("CLAUDE_CONFIG_DIR", expand_home_path(home_path, &self.options.home_dir))` with no path resolution. Upstream ClaudeHome.ts:25-27 `resolveClaudeHomePath`: `return path.resolve(expandHomePath(homePath, home));`, used by makeClaudeEnvironment (ClaudeHome.ts:44-53) which ClaudeTextGeneration.ts:105 calls. A relative homePath (TrimmedString in contracts settings.ts:687) resolves against the server cwd upstream, but in Rust it is passed through unchanged and resolved by the child, whose cwd is the project checkout (commit/PR/guide requests) or a fresh temp dir (titles). Claude then reads a different config dir (missing credentials or wrong settings). Not listed in the PORTING.md Deviations row for ClaudeTextGeneration environment (line 324), which only says CLAUDE_CONFIG_DIR is set when homePath is set.
- Fix: Resolve the expanded path to an absolute path before inserting it (std::path::absolute, or join with the process cwd), matching path.resolve. Add a test with a relative homePath, or log the difference in Deviations.

### [medium] Review guide: one malformed stop field fails the whole guide, where upstream tolerates and filters it
- `crates/text-generation/src/text_generation_operations.rs:464` · dimension: logic-fidelity · upheld 2/2
- Evidence: review_guide_stop applies `text(&stop.title)?` (l.464), `text(&attention.note)?` (l.487), `text(&excerpt.path)?` (l.496), `line(excerpt.start)?` (l.501), `text(&file.path)?`, `line(note.line)?` (l.523), `text(&note.title)?`, and `text(question)` inside `.collect::<Result<_,_>>()?` (l.531-533). Any empty string or line <= 0 propagates Err, and generate_review_guide (l.348-352) then fails the whole request with 'returned invalid structured output'. Upstream's prompt schema accepts these (CodexTextGeneration/textGenerationPrompts.ts l.256-283: Schema.String and Schema.Int with no non-empty or positive constraint). Upstream's normalizer then drops them: pullRequestReviewGuideNormalize.ts:127-158 filters stops with empty title, attention with empty note (nulled), questions that are empty, and notes with empty title or outside the excerpt range; it clamps excerpt lines to the hunks (l.76-84, 114). The Rust comment at l.447-449 documents the stricter behaviour, but the PORTING.md Deviations table has no row for it.
- Fix: Mirror the upstream split: keep the generator permissive (empty strings and out-of-range lines pass through), and apply the drop/clamp rules where the normalizer lives, or port those filters here so a single bad field drops only that item. Log the change in Deviations if it stays.

### [low] limit_title_message has no test; upstream tiny-budget and head/tail cases are not ported
- `crates/text-generation/src/thread_title_context.rs:12` · dimension: logic-fidelity · upheld 2/2
- Evidence: thread_title_context.rs has no #[cfg(test)] module, and no test in the crate asserts the truncated output (grep for 'Content truncated' finds only the const). Upstream packages/provider-core/src/server/threadTitleContext.test.ts:42-44 asserts `limitTitleMessage("x".repeat(100), 0)` is "" and that for budgets 1..39 the result length is <= budget. Rust's budget <= TRUNCATED.len() branch and the head/tail split (div_ceil) are untested, although the initial thread title prompt depends on them for messages over 8,000 units.
- Fix: Port the two upstream assertions (budget 0 gives "", and length <= budget for budgets 1..40) plus a head/tail case as a #[cfg(test)] module in thread_title_context.rs.

### [low] Process group is only signalled on timeout, so the cancel (drop) and success paths leave group members running
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/command.rs:57` · dimension: safety-and-lifecycle · upheld 1/2
- Evidence: command.rs:57 `.kill_on_drop(true)` and :58 `.process_group(0)`. The group is signalled only in the timeout arm at :83-86 (`signal_process_group(i64::from(pid), libc::SIGKILL)`). If the `run_command` future is dropped (cancellation), only the direct child gets SIGKILL through kill_on_drop, so tool subprocesses in the child's group are orphaned. On normal exit (:77-81) the group is never swept, so any member that closed its stdout/stderr before exiting keeps running. PORTING.md's child-process rule says the group is killed on timeout/shutdown, and its Deviations row for command.rs documents only the timeout case.
- Fix: Add a Drop guard that holds the pid and signals the group with SIGKILL, disarmed only after a clean wait. Alternatively, after `wait_with_output` returns, signal the group once to sweep stragglers. Update the Deviations row to describe the chosen behaviour.

### [low] Blocking std::fs and tempdir calls run inside async fns
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/codex_text_generation.rs:413` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: codex_text_generation.rs:413-414 `write_temp_file(operation, &schema_path, &schema_json)?;` and `write_temp_file(operation, &output_path, "")?;` call `std::fs::write` from `async fn run` (line 398). Line 424 `std::fs::read_to_string(&output_path)` is also blocking inside the async fn. Similar sync `tempfile::Builder::tempdir()` calls run in async paths at codex_text_generation.rs:434-437, claude_text_generation.rs:246-248, and cursor_text_generation.rs:234-236. The same module already uses `tokio::fs::metadata` at :184, so the port is inconsistent with PORTING.md's 'everything that can be async is async' rule.
- Fix: Use `tokio::fs::write` / `tokio::fs::read_to_string`, and create temp dirs with `spawn_blocking` or `tokio::fs` equivalents. Keep the `TempDir` drop-based cleanup.

### [low] ACP stderr drain task is detached by dropping its JoinHandle
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/N3-text-generation/crates/text-generation/src/cursor_text_generation.rs:125` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: cursor_text_generation.rs:125-127: `tokio::spawn(async move { let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await; });`. The JoinHandle is dropped immediately, so the task is detached. PORTING.md's Fiber/forkScoped row says 'Never drop a bare JoinHandle (it detaches, unlike Effect's scoped interruption)' and asks for a JoinSet or AbortOnDropHandle tied to the owner. The task ends only on stderr EOF, so it can outlive the connection if the child's group is not reaped.
- Fix: Hold the handle in `CursorAgentChild` (or wrap it in `tokio_util::task::AbortOnDropHandle`) so that dropping the connection aborts the drain task.
