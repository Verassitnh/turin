# Turin progress — 2026-10-17

Desktop/local Rust + GPUI fork; GitHub only, no T3 Connect.
AI providers retained: Claude, Codex, Cursor, OpenCode.

## Implemented

Step 1 live wiring is integrated (work branch: `task/WIRE-integration`):

- Runtime auth/install/settings and real Git/branch services.
- Live settings and PR routes: list/detail/actions/reviews/diffs/guides,
  linked threads and stacks; removed live PR demo-fixture injection.
- Onboarding readiness, project discovery and session-import adapters.
- Dock attention badge, draft discard/undo, shell toasts and responsive panels.
- Chat lineage/selection/subagent/usage-recovery panels; composer context,
  citations, scrolling and pending-question attachments; file icons.
- Live usage limits; Cursor historical usage reader.
- Fresh settings seeding fixed (`c494eff7`). Client-settings jobs now use
  the app's Tokio handle and onboarding propagates save failures (`8e8e4507`).
- Sidebar settings watcher uses GPUI debounce and view-owned cancellation;
  client preferences respect `TURIN_DATA_DIR` (`3139ec37`).

## Verification

- `cargo check -p turin` passed after the crash fixes.
- Signed release bundle rebuilt successfully after both runtime fixes.
- Fresh-profile smoke: real agent readiness → project discovery → Do not import
  → main shell succeeds; onboarding completion persists. Live Settings and PR
  empty-state routes open without panic. Actual session import was not exercised.
- Installed bundle: `/Users/jackson/Applications/Turin.app`; previous bundle:
  `/Users/jackson/Applications/Turin.pre-wire-20261017.app`.
- No broad tests/clippy, provider-spending flows or live PR mutations run.

## Remaining

1. Verify real session import, provider turns, auth/install, settings round trips
   and PR actions end to end. Compilation does not establish daily-use readiness.
2. Known limits: historical usage is Cursor-only; some provider login paths use
   CLI/environment credentials. Managed-worktree cleanup is unavailable.
   Onboarding has no folder picker.
   Favicon/license coverage and remaining UI parity/polish need follow-up.
3. Reconcile upstream mapping inventory (earlier snapshot: 216 missing,
   129 partial); review Codex parity, citation-controller duplication and
   retained PR autolink/preview behavior. See `docs/handoff/WIRE-*.md`,
   `docs/inventory/upstream-coverage.md` and `PORTING.md`.

## Resume

Main checkout: `turin/`; integration: `turin-wt/WIRE-integration`.
Build env: `PATH=$HOME/.cargo/bin:$PATH`,
`DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`,
`CARGO_TARGET_DIR=<workspace>/turin-targets.noindex/turin`.
Run one Cargo command at a time; retain rustc throttles.
Bundle: `scripts/bundle-macos.sh` → `$CARGO_TARGET_DIR/bundle/Turin.app`.
Temporary logs: `/tmp/turin-sidebar-watch-{check,release}.log`;
`/tmp/turin-import-final-smoke.log`.
Earlier worktree snapshots remain untouched; don't import branches twice.
