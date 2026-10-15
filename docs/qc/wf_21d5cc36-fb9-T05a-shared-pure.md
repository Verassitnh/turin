# QC wf_21d5cc36-fb9 — T05a shared pure (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 11 · refuted: 1

## Lead report

Fix list (ranked; no critical or high findings; no duplicates merged, since each item is a distinct defect):

1. MEDIUM. crates/shared/src/shell.rs:90. `exec_file_sync` joins the stdout reader after the child exits, with no deadline, so a backgrounded grandchild holding the pipe keeps the call alive past `timeout`. Upstream `execFileSync` returns ETIMEDOUT. Fix: read stdout on a thread and wait with `recv_timeout` against the same deadline. Spawn the child in its own process group (`process_group(0)`) and kill the group on timeout. Return the timeout error without joining a reader that can still block. Latent: no in-crate caller yet, but login-shell hydration (shell.rs ~381-390) will hit it.

2. MEDIUM. crates/shared/src/drainable_worker.rs:98, and keyed_coalescing_worker.rs:120-122. A panic in `process(item).await` kills the spawned worker loop with no `catch_unwind`. `drain()` never resolves and queued items never run. In the keyed worker the key stays in `active_keys`, so `drain_key` hangs. Upstream uses `catchCause`/`ignoreCause` plus `ensuring`. Fix: wrap each item in `AssertUnwindSafe(fut).catch_unwind()`, log it as `WorkerItemError::Die`, and run the completion bookkeeping (`OutstandingGuard` drop, `complete_key`) on both paths. Add a test with a real panic, not only an explicit `Die` return. Latent: no production caller yet.

3. LOW. crates/shared/src/composer_inline_tokens.rs:310-315 and 392-393. `scan_file_link_tokens` advances only when `scan_file_link_at` returns Some. A rejected candidate (empty path, external scheme, label != basename) returns None, so the scan rescans inside the rejected span. Upstream `matchAll` consumes it. Example: "[x [y.txt](y.txt) " yields a mention in Rust but nothing upstream. Fix: have `scan_file_link_at` return the consumed end even on rejection, always advance to it, and push only accepted tokens. Add a test for "[x [y.txt](y.txt) " expecting no tokens.

4. LOW. crates/shared/src/prompt_links.rs:304. `Url::parse(format!("{origin}/new"))` yields "//new" for a trailing-slash origin and keeps the base path. Upstream `new URL("/new", origin)` gives "https://app.t3.codes/new" and "http://localhost:5173/new". Fix: `Url::parse(origin)?.join("/new")`. Add tests for a trailing slash and a path origin. Latent: only the test caller exists.

5. LOW. crates/shared/src/prompt_links.rs:323-327 (`js_search`). A bare "?" gives `url.query() == Some("")`, so the code emits "?". Upstream `URL.search` is "". This affects `prompt_link_app_path` (about lines 347 and 365) and the thread branch (about 353-358), and `prompt_link_deep_link` (about 389) via `js_search`. Fix: return "" when `query()` is None or Some(""). Add tests for "/new?" and the deep-link form.

6. LOW. crates/shared/src/composer_inline_tokens.rs:99 (and 70-86). The sent-prompt `SKILL_MENTION_PATTERN` (`(?=\s|$)`, upstream composerInlineTokens.ts:39) is not ported. Only the while-typing `followed_by_whitespace` variant exists, so "use $ui" differs. Upstream consumers are CodexAdapterV2 and provider-opencode (ClaudeSkillDispatch and provider-cursor define their own copies). Fix: add a sent-prompt variant that also accepts end-of-text, exported for provider crates. Otherwise log the deferral in PORTING.md Deviations.

7. LOW. crates/shared/src/js_string.rs:151. Non-ASCII whitespace takes weight `c as u32`, so U+2003 and U+3000 sort after ASCII letters. ICU puts all whitespace first, and the doc comment (lines 124-127) and PORTING.md:210 say whitespace < punctuation < digits < letters. Fix: give every JS whitespace char a weight below `COLLATION_PUNCTUATION`, keeping ASCII controls as they are. Add a test for `locale_compare("\u{2003}x", "ax") == Less`.

8. LOW. crates/shared/src/logging.rs:450-460 (`js_integer`), used by prune at about 216-223 and 388-395. It accepts only trimmed ASCII digits and returns None on i64 overflow. Upstream `Number()` plus `Number.isInteger` also prunes "1e3", "0x10", "1.0", "+5" and 20-digit suffixes. Fix: implement JS StringToNumber for the decimal, exponent, hex, octal and binary forms, then check integrality. Otherwise log the deviation. Impact is limited to stray files in the log directory.

9. LOW. crates/shared/src/logging.rs:311-336 (`AsyncRotatingFileSink::write`) and 262-304 (`open`). Blocking std::fs calls (append, rotate, read_dir prune, metadata) run on tokio workers while holding the tokio Mutex. Upstream `makeAsyncRotatingFileSink` is documented as not blocking the event loop. Fix: wrap the sync helpers in `spawn_blocking`, following the convention at crates/persistence/src/db.rs:97-152. Or make the type sync and log the deviation. No callers exist yet.

10. LOW. crates/shared/src/js_string.rs:186-206 (`is_js_whitespace_char`). This duplicates the private `is_js_whitespace` at crates/contracts/src/base_schemas.rs:17-30. The sets match exactly, and PORTING.md:47 says to use base_schemas and never hand-roll. Fix: make the contracts predicate `pub` and delete the copy. This needs a small cross-crate change outside crates/shared. `js_trim_end` (proposed_plan_text.rs:42-44) calls the js_string copy, so there are two copies in total, not three. `js_trim_end` has no existing base_schemas equivalent, so it can be added as a new helper.

11. LOW. PORTING.md:219 (the "not ported (T05a)" row). Four contract-free, unported files have no row: devProxy.ts, cliRelease.ts, legacyCliLauncher.ts, thirdPartyLicenses.ts. Fix: add rows for each saying why it is not ported. The cut-feature files (relayUrl, relaySigning, connectAuth, PtyAdapter, htmlRender, qrCode and others) are already covered by the whole-feature cut list (about PORTING.md:147-160), so no row is needed for them.

VERDICT: PASS (no critical or high findings; items 1 and 2 are medium and latent, with no in-crate callers yet, but should be fixed before the consumers land).

## Confirmed findings (raw)

### [low] Rejected file-link candidate is rescanned, creating mention tokens upstream never produces
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/composer_inline_tokens.rs:310` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream composerInlineTokens.ts uses text.matchAll(FILE_LINK_TOKEN_REGEX); a regex match that is then rejected by `continue` (empty path, external scheme, or label !== basename) still consumes its span, so matching resumes after it. Rust scan_file_link_tokens (lines 310-315) only advances past a span when scan_file_link_at returns Some. scan_file_link_at returns None at line 392-393 for the rejected case, so the loop does index += 1 and rescans inside the rejected span. Probe run with upstream under node: collectComposerInlineTokens("[x [y.txt](y.txt) ") returns [] and collectComposerInlineTokens("[a [b.md](b.md) and [c.md](c.md) ") returns only c.md. The Rust port would also emit a mention for [y.txt](y.txt) (start 3) and [b.md](b.md) (start 3), which upstream drops.
- Fix: Make scan_file_link_at return the consumed end index even when it rejects the candidate (e.g. return (end, Option<token>)), and in scan_file_link_tokens always set index to that end, pushing only accepted tokens. Add a test with "[x [y.txt](y.txt) " expecting no tokens.

### [low] build_new_thread_link joins origin with "/new" by string concat, not URL resolution
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/prompt_links.rs:304` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream promptLinks.ts: `const url = new URL("/new", input.origin ?? DEFAULT_HOSTED_APP_URL);`. Rust: `Url::parse(&format!("{origin}/new"))`. Node check: new URL("/new", "https://app.t3.codes/").toString() gives "https://app.t3.codes/new", but the Rust form gives "https://app.t3.codes//new". Likewise a base with a path, "http://localhost:5173/app/x", resolves to "http://localhost:5173/new" upstream but "…/app/x/new" in Rust. Shared links built from a trailing-slash origin are broken.
- Fix: Parse the origin and call `.join("/new")` (or `Url::parse(origin)?.join("/new")`) so the path is replaced, matching WHATWG URL resolution.

### [low] SKILL_MENTION_PATTERN (sent-prompt skill regex) is not ported
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/composer_inline_tokens.rs:99` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream composerInlineTokens.ts exports `SKILL_MENTION_PATTERN` (lookahead `(?=\s|$)`, flags gu) and it is consumed by upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts, upstream/apps/server/src/provider/Drivers/ClaudeSkillDispatch.ts, upstream/packages/provider-opencode/src/server/v2/adapter.ts and upstream/packages/provider-cursor/src/server/skills.ts. grep of crates/ finds no SKILL_MENTION or skill_mention equivalent. Only the while-typing scan (`followed_by_whitespace` only) exists, and PORTING.md Deviations does not record the omission.
- Fix: Add a sent-prompt variant of scan_skill_tokens that also accepts end-of-text after the name (the `$` alternative), exposed for provider crates, and log the choice in Deviations if it is intentionally deferred.

### [low] locale_compare orders non-ASCII whitespace after letters
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/js_string.rs:151` · dimension: logic-fidelity · upheld 2/2
- Evidence: In `key()`, whitespace and control chars get `weight = c as u32` (line 151) while letters get 3_000+ and non-ASCII letters 4_000+. So U+2003 (8195) and U+3000 (12288) sort above ASCII letters. The doc comment states the intended order "whitespace < punctuation < digits < letters" and ICU root collation puts all whitespace first. Example: locale_compare("\u{2003}x", "ax") returns Greater, ICU returns Less. Affects search tie-breakers and semver fallback only for such inputs.
- Fix: Give whitespace a weight below COLLATION_PUNCTUATION (for example map js whitespace to 0..999 by a small table, keeping c as u32 only for ASCII controls).

### [low] js_string-style trailing '?' handling: empty query kept in prompt link search
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/prompt_links.rs:323` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream promptLinkAppPath returns `/new${url.search}`. Node check: new URL("https://app.t3.codes/new?").search is "" (empty string). Rust js_search (lines 323-327) uses url.query(), which is Some("") for a bare '?', and formats "?" so `/new?` is returned where upstream returns `/new`.
- Fix: Return an empty string when url.query() is None or Some("").

### [low] js_integer rejects numeric forms JS Number() accepts, so some overflow backups are not pruned
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/logging.rs:450` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream logging.ts prune: `const suffix = Number(entry.slice(baseName.length + 1)); if (!Number.isInteger(suffix) || suffix <= this.maxFiles) continue;`. JS Number("1e3") is 1000, Number("0x10") is 16, Number("1.0") is 1, Number("+5") is 5, and each is pruned when above maxFiles. Rust js_integer (lines 450-460) accepts only ASCII digits after trim, so e.g. `app.log.0x10` or `app.log.1e3` survive pruning. Also a digit run beyond i64 returns None, while JS treats it as an integer.
- Fix: Implement JS StringToNumber for the decimal/hex/octal/binary/exponent forms (or at least decimal with optional sign, '.' and exponent) and check integrality, so prune matches upstream.

### [medium] exec_file_sync does not enforce its timeout once the child exits (pipe held open by a grandchild)
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/shell.rs:90` · dimension: safety-and-rules · upheld 2/2
- Evidence: Code: the poll loop only enforces the deadline while the direct child is running (lines 78-89); after `try_wait` returns a status, `reader.join()` (line 90) blocks until every holder of the stdout pipe closes it. Probe (standalone copy of lines 63-97, run with `/bin/sh -c "(sleep 15 &); echo hi"` and timeout 1000 ms): returned Ok("hi") after 15008 ms. Upstream Node `execFileSync` with the same command and timeout 1000 returned ETIMEDOUT after 1011 ms. Login-shell hydration (`read_environment_from_login_shell`, line 390, 5000 ms) is the intended caller, and a profile that starts a background daemon (gpg-agent, ssh-agent) would hang startup. PORTING.md requires timeouts to actually stop the work. No in-crate callers exist yet, so this is latent.
- Fix: Do not join the reader without a deadline. Read stdout on the reader thread and wait with `recv_timeout` on a channel bounded by the same deadline. Spawn the child with `process_group(0)` and kill the whole group (`kill(-pgid, SIGKILL)`) on timeout. Return the timeout error without joining a reader that can still block.

### [medium] A panic in a worker item kills the worker task, so drain never resolves and queued items never run
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/drainable_worker.rs:98` · dimension: safety-and-rules · upheld 2/2
- Evidence: `match process(item).await` (line 98) has no catch_unwind, so a panic ends the spawned loop (the same pattern is at keyed_coalescing_worker.rs:120-122, `process(key.clone(), value).await; complete_key(...)`). Probe: a copy of this module with items 1 (panics) and 2 was run under tokio. `drain()` did not resolve within 2 s and item 2 never ran. Upstream DrainableWorker.ts:69-74 uses `Effect.catchCause` plus `Effect.ensuring(outstanding - 1)`, and KeyedCoalescingWorker.ts uses `Effect.ignoreCause` plus `Effect.ensuring(completeKey)`, so the worker survives a defect. In the keyed worker a panic also leaves the key in `active_keys` forever, so `drain_key` for that key hangs.
- Fix: Wrap each item in `AssertUnwindSafe(fut).catch_unwind()` (futures::FutureExt), log the panic as `WorkerItemError::Die`, and run the completion bookkeeping (`OutstandingGuard` drop, `complete_key`) on both paths. Alternatively spawn each item as its own task and join it for the result.

### [low] AsyncRotatingFileSink performs blocking std::fs I/O inside async fns while holding a tokio Mutex
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/logging.rs:311` · dimension: safety-and-rules · upheld 2/2
- Evidence: `pub async fn write` (line 311) calls `self.rotate` (fs::rename, rm_force), `self.append` (append_bytes, OpenOptions) and `self.read_size` (fs::metadata) on the tokio worker thread while holding `self.state.lock().await`. `pub async fn open` (line 262) runs create_dir_all (line 289), prune (read_dir) and read_size synchronously. Nothing is offloaded.
- Fix: Run append, rotate, prune and size reads through `tokio::task::spawn_blocking` (keeping the mutex held across the await is fine for ordering), or make the type explicitly sync like `RotatingFileSink` and drop the async API.

### [low] Whitespace predicate duplicated from contracts base_schemas (third copy in proposed_plan_text)
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/crates/shared/src/js_string.rs:186` · dimension: safety-and-rules · upheld 2/2
- Evidence: `js_string::is_js_whitespace_char` (line 186-206) re-lists the ECMAScript WhiteSpace/LineTerminator set that is private in `turin_contracts::base_schemas::is_js_whitespace` (crates/contracts/src/base_schemas.rs:17-30). The two sets currently match, but nothing ties them together. `proposed_plan_text.rs:42-44` adds a further `js_trim_end` that uses the js_string copy. The PORTING rule says helpers that exist in base_schemas must be used, not hand-rolled.
- Fix: Make `base_schemas::is_js_whitespace` pub and delete the copy in js_string.rs. Add `js_trim_end` next to `js_trim` in base_schemas or js_string, and reuse it from proposed_plan_text.

### [low] Deviations table omits several unported contract-free files and cut-feature files
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T05a-shared-pure/PORTING.md:219` · dimension: safety-and-rules · upheld 2/2
- Evidence: The "not ported (T05a)" row (line 219) lists Net, HostProcess, nodeRuntime, devHome, httpReadiness, mcpResponsePayloads, observability, t3McpToolPresentation, themePalettes, themePreview, schemaYaml, codexAuthCallback. Upstream files with zero contract/effect imports and no crate counterpart and no Deviations row: devProxy.ts (Vite proxy path list, not in the cut list), cliRelease.ts (release artifact naming), legacyCliLauncher.ts (npm boot-service launcher), thirdPartyLicenses.ts (license manifest). Cut-feature files with no row: relayUrl.ts, relaySigning.ts, desktopBootstrapToken.ts, connectAuth.ts, qrCode.ts, htmlRender.ts, PtyAdapter.ts (also desktopAppControl.ts). Checked with `ls upstream/packages/shared/src` against crates/shared/src.
- Fix: Add rows for devProxy, cliRelease, legacyCliLauncher and thirdPartyLicenses saying why they are not ported. Either add the relay, desktop, QR, HTML, PTY and connect files to the cut list or give them one shared row pointing at it.
