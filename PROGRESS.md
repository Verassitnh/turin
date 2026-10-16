# Turin progress — 2026-10-16

Desktop-only Rust/GPUI fork; local runtime, GitHub only, no T3 Connect.
AI providers retained: Claude, Codex, Cursor, OpenCode.

## Landed

- Integrated all 12 pending branches: T13c, T22b, T22c, T09d, R3, S1,
  S2, S3, S3c, S6, C1, C2 (including their dependency branches).
- Imported already-written uncommitted source from C1/S1/S2/S3c/S6.
  Original worktrees remain untouched and dirty; avoid importing them twice.
- Composer 2.5 (Cursor) resolved merge/compile errors. Code checkpoint:
  `f16f72de` on `task/INT-1`, subsequently landed on main.
- `cargo check --workspace` passed. No tests, clippy, visual QC, app launch,
  or release build were run in this integration session (budget decision).
  Earlier provider smoke passes were on separate branches; integrated tests
  still need rerunning. Build log: `/tmp/turin-int-compile.log` (temporary).

## Completeness

Header-based inventory: 1,158 KEEP/UNASSIGNED upstream files, 377,297 lines.
813 direct mappings (77.0% of lines), 129 partial mentions (15.0%),
216 without mappings (8.0%). These measure documentation evidence, not
functional completeness; consolidated modules and deliberate cuts can be
false gaps. Largest unmatched area: web components (19,707 lines).
See [remaining mappings](docs/handoff/coverage-remaining.txt),
[inventory](docs/inventory/upstream-coverage.md), and [deviations](PORTING.md).
Recount: `python3 scripts/coverage-progress.py` (uses current HEAD).
`task/T13c-D2` remains unmerged: its PR command implementation was carried
forward in `c3af15fa`; old stage/native-agent branches are not integration targets.

## Next work

1. Finish live wiring: auth/install/source-control hosts; dock approval badge;
   discarded-draft undo; lineage/selection/subagent/usage-recovery panels;
   composer context records, page-scroll and citation flow; shell toasts;
   PR guide refresh, thread links and stacks; C2 icons/animations/question
   attachments and remaining shell states. Several ports exist only as demos
   or unmounted components. Compilation does not establish these work.
2. Reconcile 216 missing and 129 partial mappings with actual Rust modules
   and intentional cuts. Check Codex catalog/snapshot/usage/text-generation
   parity and settings/PR/diff/sidebar gaps against the inventory.
3. Deduplicate assistant citation controller versus `pure_rules`; finish
   PR autolink copy/open-confirmation/preview behavior where retained.
4. When budget permits: run provider smoke tests on integrated main, targeted
   orchestration tests, launch with seeded dev DB, check light/dark screens,
   then full tests/clippy and macOS bundle build.

## Resume

Main checkout: `turin/`; integration worktree: `turin-wt/INT-1`.
Build: `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo check --workspace`.
Run: same environment, `cargo run -p turin`.
Reuse existing target dirs; keep rustc/test throttles. About 35 GiB free at
handoff. Source worktree snapshots: `/tmp/turin-int-import-*` (temporary).
Native agent shared modules were already present; scratch `diff_tmp.rs` and
uncommitted screenshots were excluded. TASKS.md/PM_HANDOFF.md contain stale
historical statuses; this file is the current resume point.
