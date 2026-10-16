# QC wf_ad6bdf98-949 — T19 PR views (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 1 · refuted: 0

## Lead report

1. LOW: crates/app/src/pull_request/pull_request_file_order_logic.rs:98-111. Module-path collisions resolve differently from upstream on unsorted input. Rust sorts `source_paths` with `locale_compare` (line 105) before the `by_module_path` insert loop, so the last sorted file wins. Upstream pullRequestFileOrder.logic.ts:157-162 builds the same map from Map insertion (input) order, so the last input file wins. Example: input [src/z.tsx, src/z.ts, src/b.ts], with src/b.ts importing "./z". Upstream resolves to src/z.ts. Rust resolves to src/z.tsx. Reachable only with unsorted input; patch order from git or GitHub is normally path-sorted, so the two agree there. Not covered by PORTING.md Deviations (line 388 covers only path resolution and tier ordering).
   Fix: preserve input order. Rust's `by_path: HashMap` (line 97) discards input order before the sort, so replace it with an insertion-ordered structure (Vec with dedupe-by-last-wins, matching JS Map.set semantics). Build `by_module_path` and `by_base_name` from that input-order source list, and sort a separate copy only for the Kahn pass and `sourcePositions`. Add a test with unsorted input that has a module-path collision, and log the fix in PORTING.md if it is not made.

Verdict: PASS (no critical or high findings).

## Confirmed findings (raw)

### [low] File-order import map built from sorted paths, upstream builds it in input order
- `crates/app/src/pull_request/pull_request_file_order_logic.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream upstream/apps/web/src/components/pullRequest/pullRequestFileOrder.logic.ts:157 `const sourcePaths = [...byPath.keys()].filter(...)` keeps Map insertion (input) order, and lines 161-162 use that list for `byModulePath.set(stripExtension(path), path)` so the last input file wins on a module-path collision. Rust line 100-105 sorts `source_paths` with locale_compare before the loop at lines 108-112 that inserts into `by_module_path`, so the last *sorted* file wins instead. Concrete case: input [src/z.tsx, src/z.ts, src/b.ts] with src/b.ts importing "./z". Upstream resolves the import to src/z.ts and orders z.ts, b.ts, z.tsx; Rust resolves to src/z.tsx and orders z.ts, z.tsx, b.ts. The divergence needs unsorted input, since git/GitHub listings are normally path-sorted, so the impact is limited.
- Fix: Build by_module_path and by_base_name from the input-order source list (as upstream's sourcePaths does) and sort a separate copy only for the Kahn pass and sourcePositions. Add a test with an unsorted input that has a module-path collision.
