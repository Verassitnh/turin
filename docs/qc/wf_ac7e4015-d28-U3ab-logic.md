# QC wf_ac7e4015-d28 — U3a+U3b chat and shell logic

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 3 · refuted: 0

## Lead report

1. [low] PORTING.md:424 (also docs/handoff/U3a.md:22): the row says "Upstream has no test file for this module". Upstream has `components/chat/timelineScrollAnchoring.test.tsx` with 18 cases. Required fix: correct both docs to name that file. Then either port the five untested observeTimelineRun cases (upstream test lines 209, 224, 229, 256, 261) as `#[test]`s in `crates/app/src/components/chat/timeline_scroll_anchoring.rs` (existing tests at ~281-345), or log each skipped case in the Deviations table. Upstream cases at lines 200, 238 and 246 are only covered indirectly and should be noted too. The observe logic itself matches upstream `timelineScrollAnchoring.ts:10-35`, so no behavioural change is needed.

2. [low] `crates/app/src/state/ui_state_store.rs:116-117` (sanitize_boolean_record), `:138-139` (sanitize_timestamp_record), `:169` and `:174` (sanitize_thread_changed_files_expanded), and the nested turns guard near `:179`: these accept only `Value::Object`. Upstream `upstream/apps/web/src/uiStateStore.ts:91, 106, 189, 195` use `typeof !== "object"`, which accepts arrays, so `Object.entries` yields index keys (e.g. `[true]` gives `{"0": true}` upstream and `{}` in Rust). Required fix: either accept arrays and iterate indices as string keys in these four guards, or add a Deviations row. Impact is low because upstream's `persistState` only writes objects. Add a test if you fix it.

3. [low] `crates/app/src/components/chat/assistant_text_selection.rs:70, 77, 88, 103-104`: `String::from_utf16_lossy` turns a lone surrogate at a selection boundary into U+FFFD, while upstream `assistantTextSelection.ts:57, 76-77` keeps it. Reviewers disagreed on impact. Upstream's href serialization (`URLSearchParams`) also converts lone surrogates to U+FFFD, so persisted citations match. The only real gap is in-memory matching, and the DOM half is not ported. Required fix: add a PORTING.md Deviations row for this file, in line with the existing lone-surrogate rows (~746, 749, 807). Changing the storage type to `Vec<u16>` is not required.

Verdict: PASS (no critical or high findings; three low-severity items above).

## Confirmed findings (raw)

### [low] Deviation row claims timelineScrollAnchoring has no upstream test file; upstream tests are only partly ported
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/U3b-shell-state/PORTING.md:424` · dimension: logic-fidelity · upheld 2/2
- Evidence: PORTING.md:424 says 'Upstream has no test file for this module; tests cover each function here'. Upstream has upstream/apps/web/src/components/chat/timelineScrollAnchoring.test.tsx (18 cases in describe blocks at lines 32, 63, 179, 270). Upstream cases with no Rust counterpart and no logged deviation: 'stays synchronized: keeps the end position while a stale cached run is replaced' (test line 209), 'does not anchor when an existing run's user message arrives after its run' (224), 'waits for a new run's user message and frames it only once' (229), 'frames the first new run in an initially empty thread' (256), 'waits for a queued run to start before framing it' (261). Rust tests in crates/app/src/components/chat/timeline_scroll_anchoring.rs:281-345 only cover the opening, later-run, queued/unhydrated and no-message/other-thread cases. The observeTimelineRun logic itself matches upstream line by line; the gap is test coverage and an inaccurate deviation note.
- Fix: Correct the PORTING.md row to name the upstream test file. Port the five cases above as #[test]s in timeline_scroll_anchoring.rs, or log each skipped case in the Deviations table.

### [low] UI state sanitizers reject JS arrays that upstream's typeof-object check accepts
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/U3b-shell-state/crates/app/src/state/ui_state_store.rs:116` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust sanitize_boolean_record (line 116-117) matches only Value::Object; sanitize_timestamp_record (138-139) and sanitize_thread_changed_files_expanded (166-174) do the same. Upstream upstream/apps/web/src/uiStateStore.ts:90-93, 105-108 and 186-189 use `if (!value || typeof value !== 'object') return {}`, and typeof [] === 'object', so Object.entries() runs over an array's index keys. A persisted projectExpandedById of [true] yields {"0": true} upstream and {} in Rust. JSON written by persistState never contains arrays here, so the practical impact is low.
- Fix: Accept Value::Array in the three sanitizers and iterate its indices as string keys to match Object.entries, or log the difference in PORTING.md Deviations.

### [low] Assistant citation selector replaces lone UTF-16 surrogates with U+FFFD in stored quote and context
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/U3b-shell-state/crates/app/src/components/chat/assistant_text_selection.rs:70` · dimension: logic-fidelity · upheld 1/2
- Evidence: create_assistant_text_selector builds text/prefix/suffix with String::from_utf16_lossy (lines 70, 77, 88 for the quote and the boundary slices, plus the prefix and suffix slices). Upstream upstream/apps/web/src/lib/assistantTextSelection.ts createAssistantTextSelector keeps a JS string slice, so a selection boundary inside a surrogate pair yields a lone surrogate in text, prefix or suffix. Rust stores U+FFFD, so the saved selector no longer matches the live text. The splitsSurrogatePair guard in the context path only covers prefixStart and suffixEnd, not the quote boundaries. This divergence is not in the PORTING.md Deviations table, which only says positions are UTF-16.
- Fix: Store the quote and context as Vec<u16> (or a lossless wtf-8-style type) to match JS, or add a Deviations row explaining that a boundary inside a surrogate pair becomes U+FFFD and why that cannot be matched later.
