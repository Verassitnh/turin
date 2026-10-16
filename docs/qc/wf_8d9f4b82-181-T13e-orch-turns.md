# QC wf_8d9f4b82-181 — T13e orchestration turn services

Reviewer: Claude QC workflow (Sonnet 5.5 reviewers + skeptics, Sonnet lead) (dimension reviewers, 2 skeptics per finding).
Confirmed: 13 · refuted: 8

## Lead report

FIX LIST (ranked; no critical or high findings)

LOW, in-scope code

1. crates/orchestration/src/core/provider_turn_start_service.rs:1421-1432 (`settled_handoffs`, merged duplicate; upheld 2/2 on both reports)
   - Problem: Rust uses `delivery.as_ref().is_some_and(..)`. Upstream ProviderTurnStartService.ts:984-989 uses optional chaining. When the running thread has no native id and a handoff has no delivery, upstream evaluates `undefined === undefined && undefined !== "pending"` and counts the handoff as settled. Rust never does. Only `legacy_recovered_run_ids` (Rust ~1480-1494) differs, which skews the attachment-token part of the native-context estimate.
   - Fix: Match the optional semantics. Compare `delivery.map(|d| d.native_thread_id) == running_native_id` and accept when `delivery.map(|d| d.status) != Some(Pending)`. Alternatively, add a Deviations row saying the quirk is intentionally not reproduced.

2. provider_turn_start_service.rs:655, 657, 771, 1401, 1697 and run_execution_service/service.rs:524 (upheld 2/2)
   - Problem: These lines use `str::trim` and `trim_start`. Upstream uses JS `.trim()` and `.trimStart()` at ProviderTurnStartService.ts:258/373/375/446/970 and RunExecutionService.ts:1441. JS strips U+FEFF and keeps U+0085. Rust does the opposite. So `"\u{feff}/compact"` is not detected as /compact, `"\u{85}/x"` is treated as a slash command, and the persisted /logout `command_execution.input` differs.
   - Fix: Use `turin_contracts::base_schemas::js_trim` (crates/contracts/src/base_schemas.rs:31) and `js_trim_start` (crates/provider-core/src/js.rs:29) at every listed site.

3. crates/orchestration/src/core/context_handoff_service.rs:335 (`compact_text`, upheld 2/2)
   - Problem: `text.split_whitespace()` differs from upstream `replace(/\s+/g, " ").trim()` for U+0085 and U+FEFF. The Deviations row (PORTING.md:1081) does not justify this.
   - Fix: Split on the existing `is_js_whitespace` (about line 393) and trim with the same predicate. Update the Deviations row.

4. Dropped upstream why-comments (PORTING.md rule 1, upheld 2/2). Restore these four:
   - provider_turn_start_service.rs:1589, `native_thread_has_turns`. Restore "Legacy accepted attempts have no native id. They count only before a replacement, while no accepted attempt records a native identity." (upstream 1252-1253).
   - provider_turn_start_service.rs:1249, `running_provider_thread.context_usage = handoff_usage`. Restore the note that replacing a native thread clears its usage, not the selected model's capacity (upstream 1069).
   - run_execution_service.rs:579, `can_route_related_subagent`. Restore that a running subagent stays with its launching run, and that taking it over would store its events twice (upstream 178-183).
   - run_execution_service.rs:408, `ProviderSessionUpdated => false`. Restore that the session manager already persists process-wide status once per attached app thread (upstream 379-380).

5. crates/orchestration/src/core/run_execution_service/ingestion_tests.rs (upheld 2/2)
   - Problem: Upstream RunExecutionService.test.ts:4460, "releases ingestion after idle subagent rows and items settle", is not ported. docs/handoff/T13e.md:33 claims the file is ported "in full" apart from the listed exceptions. By reading, `is_subagent_work_active` and `is_turn_item_work_active` match upstream, so this is a coverage gap and not a behaviour bug.
   - Fix: Port the case. The `scenario` helper already supports `keep_open`, `root_subagent` and `root_item`. The expected order is [subagent:running, turn_item:running, subagent:idle, turn_item:idle, root-finalized]. Otherwise, log it in Deviations and correct the handoff line.

6. Stale module headers (upheld 2/2 each)
   - crates/orchestration/src/core/restart_continuation.rs:4-7 says the effectful half "is not ported yet". Point it at restart_continuation_service.rs, which has `continue_restarted_run`.
   - crates/orchestration/src/core/run_execution_service.rs:3-7 still describes "Slice 1". Rewrite it for the final layout: pure helpers here, effectful layer in service.rs, terminal write in finalize.rs.

LOW, docs and process (outside the T13e code)

7. AGENTS.md:32,38,39,42 (upheld 2/2)
   - Problem: The paths use `../.cargo/...` and `../turin-targets.noindex/`, which do not exist from a worktree. The cap of 5 compilers is wrong, because /tmp/turin-rustc-slots/limit is 4. The test-throttle runner (/tmp/turin-test-slots, default 2) is not documented.
   - Fix: Change the paths to `../../.cargo/...` and `../../turin-targets.noindex/`. Say the cap is read from /tmp/turin-rustc-slots/limit instead of hard-coding it. Document the test-throttle runner.

8. scripts/git-hooks/commit-msg:10 (upheld 2/2)
   - Problem: The hook regex accepts any provider string. 42 of 71 branch commits use `(claude)` where AGENTS.md:25-26 requires `claudeAgent`.
   - Fix: Restrict the provider to the allowed set (codex|claudeAgent|cursor|opencode, plus human if applicable) and use `(claudeAgent)` from now on.

OPTIONAL (only 1 of 2 verifiers upheld these; not blocking)

9. provider_turn_start_service/handoff_session.rs:~549-555 (`DeliverySession::compact_thread` always returns `Some`)
   - Problem: On `/compact`, `deliver_and_start` runs `deliver_context_handoffs` before it hits `unsupported_compaction`. Upstream fails before any delivery. The side effect needs a provider that has `inject_history` but no `compact_thread`, and no real adapter is like that.
   - Fix: Either probe `session.compact_thread(..).is_some()` before delivery, or note the ordering in the PORTING.md:1084 row.

10. service.rs, finalize.rs, settle.rs and handoff_session.rs headers
    - Problem: They lack the literal `//! Port of upstream/<path>` form. Each header already names the upstream file and line range, and the parent modules have the Port-of line.
    - Fix: Cosmetic. Prefix each with `//! Port of upstream/apps/server/src/orchestration-v2/<File>.ts (lines N-M)`.

11. PORTING.md:503 (`## DEDUP`)
    - Problem: It splits the Deviations table. The T13e rows at 1071-1102 render under the DEDUP 3-column header. The split also exists on main.
    - Fix: Repeat the `| File | Upstream | Turin | Why |` header after the DEDUP section, or move DEDUP below the table.

VERDICT: PASS (no critical or high findings; items 1-8 should be fixed, 9-11 are optional)

## Confirmed findings (raw)

### [low] settledHandoffs drops handoffs with no delivery when the running thread has no native id
- `crates/orchestration/src/core/provider_turn_start_service.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Rust (1421-1432): `handoff.delivery.as_ref().is_some_and(|delivery| Some(delivery.native_thread_id.as_str()) == running_native_id... && delivery.status != Pending)`. Upstream ProviderTurnStartService.ts:984-989: `handoff.toProviderThreadId === providerThread.id && handoff.delivery?.nativeThreadId === runningProviderThread.nativeThreadRef?.nativeId && handoff.delivery?.status !== "pending"`. When `runningProviderThread.nativeThreadRef?.nativeId` is undefined (upstream handles a null native ref at 880 and 847), a handoff with no `delivery` satisfies `undefined === undefined && undefined !== "pending"` and is in upstream's settledHandoffs. Rust requires `delivery` to be Some, so it is excluded.
- Fix: Match optional semantics: treat a missing delivery as `None` and compare `delivery.map(|d| d.native_thread_id) == running_native_id`. Accept the handoff when `delivery.map(|d| d.status) != Some(Pending)`. This only changes `legacy_recovered_run_ids` (FullThreadSummary handoffs), which feeds the native-context attachment estimate. If the quirk is judged accidental, add a Deviations row instead.

### [low] compact_text splits on Rust Unicode whitespace instead of JavaScript \s
- `crates/orchestration/src/core/context_handoff_service.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Line 335: `text.split_whitespace()`. Upstream `text.replace(/\s+/g, " ").trim()`. In node, "a\u0085b﻿c d" becomes "a\u0085b c d": JS \s excludes U+0085 (NEL) and includes U+FEFF (BOM), while Rust split_whitespace does the opposite. The Deviations row says only "splits on Unicode whitespace" and claims the output is identical. The same file already has an exact `is_js_whitespace` (about line 396) for the legacy-import summary, so the two paths are inconsistent. The fork-delta summary text for items containing NEL or BOM differs from upstream.
- Fix: Split on `is_js_whitespace` code units (or `char::is_whitespace` minus U+0085 plus U+FEFF) in compact_text, so it matches `/\s+/` and `trim()`.

### [low] Several upstream why-comments were dropped
- `crates/orchestration/src/core/provider_turn_start_service.rs:` · dimension: fidelity · upheld 2/2
- Evidence: PORTING.md ground rule 1 says to keep comments that explain why. Missing from the Rust: (1) provider_turn_start_service.rs:1589 `native_thread_has_turns`, upstream 1252-1253: "Legacy accepted attempts have no native id. They count only before a replacement, while no accepted attempt records a native identity." (2) The `budgetProviderThread` note (upstream 1069): "Replacing a native thread clears its usage, not the selected model's capacity". The Rust folds this into `running_provider_thread.context_usage = handoff_usage` with no comment. (3) run_execution_service.rs:579 `can_route_related_subagent`: upstream 178-183 explains that a running subagent stays with the run that launched it and taking it over "would store its events twice"; Rust keeps only "only a completed subagent's child thread is resumed". (4) run_execution_service.rs:408 `ProviderSessionUpdated => false`: upstream 379-380 explains the session manager already persists process-wide status once for every attached app thread.
- Fix: Restore the four comments next to the corresponding Rust code.

### [low] DeliverySession offers compact_thread unconditionally, so a /compact on a provider without compaction injects handoff history before failing
- `crates/orchestration/src/core/provider_turn_start_service/handoff_session.rs:` · dimension: correctness · upheld 1/2
- Evidence: handoff_session.rs ~L505: `fn compact_thread(..) -> Option<..> { Some(Box::pin(self.start(input, true))) }` (always Some). `HandoffPlan::deliver_and_start` runs `deliver_context_handoffs(..)` (persist pending -> `inject` -> persist injected) and only afterwards hits `session.compact_thread(prompted)` -> `None => Err(unsupported_compaction(..))`. Upstream `makeDeliverySession` (ProviderTurnStartService.ts L181-216) spreads `compactThread` only `session.compactThread === undefined ? {} : {...}`, so RunExecutionService (L1390-1400) fails with 'This provider does not support context compaction.' BEFORE any delivery. The PORTING.md row accepts the always-Some shape but not the ordering side effect.
- Fix: In `deliver_and_start`, when `compact` is true probe `session.compact_thread(..).is_some()` (drop the unawaited future, as is already done for the `inject_history` probe) and return `unsupported_compaction` before calling `deliver_context_handoffs`, or give DeliverySession a compact_thread that returns None when `self.inner.compact_thread(..)` is None.

### [low] settled_handoffs drops JS `undefined === undefined` match for handoffs without delivery when the running thread has no native ref
- `crates/orchestration/src/core/provider_turn_start_service.rs:` · dimension: correctness · upheld 2/2
- Evidence: provider_turn_start_service.rs ~L1418-1430: `handoff.delivery.as_ref().is_some_and(|delivery| Some(delivery.native_thread_id) == running_native_id && status != Pending)`. Upstream (TS ~L968-974): `handoff.toProviderThreadId === providerThread.id && handoff.delivery?.nativeThreadId === runningProviderThread.nativeThreadRef?.nativeId && handoff.delivery?.status !== "pending"`. When delivery is undefined and the running provider thread has no nativeThreadRef, both sides are `undefined` and `undefined !== "pending"`, so upstream counts the handoff as settled; Rust never does. That set feeds `legacyRecoveredRunIds` and `nativeContextEstimate` attachment allowance (only when same_native_thread).
- Fix: Mirror the JS comparison: treat `delivery == None` as matching when `running_native_id` is None (e.g. match on `(running_native_id, &handoff.delivery)`), or record in PORTING.md that this is intentionally not reproduced.

### [low] str::trim / trim_start used where upstream calls JS trim(): /compact detection and persisted command text differ for U+FEFF and U+0085
- `crates/orchestration/src/core/provider_turn_start_service.rs:655` · dimension: wire-compat · upheld 2/2
- Evidence: Lines 655, 657, 771, 1401, 1697 and run_execution_service/service.rs:524 use `text.trim().to_lowercase() == "/compact"`, `text.trim_start().starts_with('/')` and `message.text.trim().to_owned()`. Upstream (ProviderTurnStartService.ts:258/375/446/970, RunExecutionService.ts:1441) uses JS `.trim()` / `.trimStart()`. PORTING.md: "Use js_trim, never str::trim, anywhere upstream calls .trim()". Verified with node and rustc: JS `"﻿/compact".trim()` -> "/compact" but Rust `"\u{feff}/compact".trim()` -> "\u{feff}/compact" (not trimmed). JS `"\u0085/x".trimStart().startsWith("/")` -> false but Rust `"\u{85}/x".trim_start().starts_with('/')` -> true. U+0085 is trimmed by Rust but not by JS.
- Fix: Replace each call with turin_contracts::base_schemas::js_trim (and a js_trim_start equivalent via is_js_whitespace) at provider_turn_start_service.rs:655,657,771,1401,1697 and run_execution_service/service.rs:524. Then "/compact" detection and the persisted `command_execution.input` for the local /logout item match upstream.

### [low] Upstream test 'releases ingestion after idle subagent rows and items settle' not ported
- `crates/orchestration/src/core/run_execution_service/ingestion_tests.rs:` · dimension: process · upheld 2/2
- Evidence: upstream RunExecutionService.test.ts:4460 `it.effect("releases ingestion after idle subagent rows and items settle", ...)` (subagent running -> turn_item running -> subagent idle -> turn_item idle -> root terminal, keepEventStreamOpen: true; expects [subagent:running, turn_item:running, subagent:idle, turn_item:idle, root-finalized]). `grep -rn 'idle' crates/orchestration/src/core/run_execution_service/*.rs` finds no matching case. docs/handoff/T13e.md claims the file is 'ported in full except the two MCP tests, the interruption scenarios and the memory test', and PORTING.md has no deviation row for it.
- Fix: Port the case (it exercises the idle-subagent release path of the ingestion loop in service.rs, which is critical-path event ingestion), or add a Deviations row and correct the handoff claim.

### [low] AGENTS.md build/throttle paths and numbers are wrong and would mislead agents
- `AGENTS.md:` · dimension: process · upheld 2/2
- Evidence: AGENTS.md says `../.cargo/config.toml`, `../.cargo/rustc-throttle` and `../turin-targets.noindex/`, but from a worktree (turin-wt/<task>) `..` is turin-wt, where none of these exist. They are at `../../.cargo/` and `../../turin-targets.noindex/` (`ls -d ../.cargo` fails; `ls -d ../../.cargo ../../turin-targets.noindex` succeeds). It also says 'global cap of 5 compilers', but /tmp/turin-rustc-slots/limit is 4. It omits the test-throttle (/tmp/turin-test-slots/limit, default 2) that ../../.cargo/config.toml now sets as the cargo runner for test binaries.
- Fix: Correct the relative paths to `../../.cargo/...` and `../../turin-targets.noindex/`, say the cap is read from /tmp/turin-rustc-slots/limit instead of hard-coding 5, and document the test-throttle runner.

### [low] Commit trailers use provider `claude`, which is not a T3 provider instance id
- `scripts/git-hooks/commit-msg:` · dimension: process · upheld 2/2
- Evidence: AGENTS.md: 'Use ... T3 provider instance (`codex`, `claudeAgent`, `cursor`, `opencode`)'. `git log main..HEAD` has 71 commits: 42 carry `Model: claude-sonnet-5-5 (claude)` or `Model: claude-haiku-5-5 (claude)`, and only 29 use `(claudeAgent)`. The hook regex `^Model: [^ ]+ \([^)]+\)$` accepts any provider string, so nothing enforces the instance ids. Every commit does have a trailer.
- Fix: Have the hook check the provider against the allowed set (codex|claudeAgent|cursor|opencode|human ...) so `(claude)` is rejected, and use `(claudeAgent)` in future commits.

### [low] Stale module header: restart_continuation.rs says the effectful half is not ported
- `crates/orchestration/src/core/restart_continuation.rs:` · dimension: process · upheld 2/2
- Evidence: Lines 5-7: 'The effectful half (`continueRestartedRun`, ...) is not ported yet; it reuses the blocked check here.' It is ported in core/restart_continuation_service.rs (commit 844601f6).
- Fix: Update the header to point at restart_continuation_service.rs.

### [low] Stale module header: run_execution_service.rs still describes 'Slice 1'
- `crates/orchestration/src/core/run_execution_service.rs:` · dimension: process · upheld 2/2
- Evidence: Lines 3-7: 'Slice 1 ports the pure helpers ... The effectful `RunExecutionService` follows in file order (see docs/handoff/T13e.md).' The effectful layer is now in run_execution_service/service.rs and finalize.rs.
- Fix: Rewrite the header to describe the final layout: pure helpers here, effectful layer in service.rs, terminal write in finalize.rs.

### [low] Split submodules lack the mandatory `//! Port of upstream/<path>` header
- `crates/orchestration/src/core/run_execution_service/service.rs:` · dimension: process · upheld 1/2
- Evidence: PORTING.md ground rule 2: 'Every module starts with `//! Port of upstream/<path>`'. service.rs starts 'The effectful layer of `RunExecutionService.ts` (488-1510)...'. finalize.rs, provider_turn_start_service/settle.rs and provider_turn_start_service/handoff_session.rs start with '`writeFinalRunEvents` (upstream `RunExecutionService.ts` 583-820)' style text, not the Port-of form that tooling and diffing rely on.
- Fix: Start each of these with `//! Port of upstream/apps/server/src/orchestration-v2/<File>.ts (lines N-M)`.

### [low] PORTING.md Deviations table is split by the '## DEDUP' section; all T13e rows render under the wrong header
- `PORTING.md:` · dimension: process · upheld 1/2
- Evidence: `## Deviations` at line 226, `## DEDUP` at 503 with its own 3-column header `| State file | turin-shared counterpart | Holds |`, then the 4-column Deviations rows continue without a new header from line ~514 to 1102. Every T13e row (RunExecutionService, ContextHandoff*, ProviderTurnStartService, ProviderRuntimeRecoveryService, RestartContinuation, lines 1071-1102) sits in this tail. The same split exists on main (main:PORTING.md 227/557).
- Fix: Move the DEDUP section after the Deviations table, or repeat the `| File | Upstream | Turin | Why |` header after it, so readers and tools see the Deviations rows as one table.
