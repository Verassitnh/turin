# Task board

Resume notes: docs/PM_HANDOFF.md

PM (Claude Opus 5.5) owns this file. Agents don't edit it; they report back.

## Roster and budget

**Current policy (user, 2026-10-15 ~23:55 UTC; supersedes the table below):**
- Claude-plan usage is scarce, Cursor usage is plentiful. Implementation goes through the `cursor` provider: `composer-2.5` (bulk ports), `grok-4.7` (backend). Cursor only runs Grok and Composer models. Sonnet (claudeAgent, scarce) only for the critical path; no cap otherwise.
- `claudeAgent` Haiku: Workflow QC and small fixes only. PM (Opus) stays lean.
- Never Codex. Pushing to GitHub (origin) is allowed. Cloud agents only if the Mac is truly resource-bound.
- Never interrupt running threads to switch models; switch on the next continuation.
- Builds: rustc-throttle (5 slots) + sccache shared cache (`sccache.noindex`, 20G).

| Agent | Model | Cost | Use for |
|---|---|---|---|
| PM | Claude Opus 5.5 | $$$$ | Planning, briefs, merges, conflict calls. No implementation. |
| Codex | gpt-6-sol (medium; high only for orchestrator/adapters) | $$$ | Hardest ports: orchestrator core, provider adapters, protocol work |
| UI | Claude Opus 5.5 (high) | $$$$ | **All UI** (every task that touches `crates/app`). User rule: no other model writes UI. |
| QC | Claude Opus 5.5 (QC workflow) | $$$$ | Quality-control gate on every branch before merge, plus foundation audits. |
| Claude | Sonnet 5.5 (high) | $$ | Backend overflow when Codex is saturated; never UI |
| Grok | grok-4.7 via Cursor (fast) | $ | Mid-size self-contained crates: persistence, git, source-control |
| GLM | glm-5.3 via OpenCode | ¢ | Bulk mechanical translation: contracts, shared utils, reducers, test ports |
| Haiku | Claude Haiku 5.5 | ¢ | Small fixes, clippy/fmt cleanups, review passes |

**Model policy (user, 2026-10-15): use Sonnet and Haiku wherever reasonable.**
Opus only for UI rendering/look-and-feel work and look-and-feel QC. View-model
logic, backend, docs and QC review/verify go to Sonnet (reviewers, leads) and
Haiku (skeptics, mechanical edits). Grok carries the bulk of backend ports
while Codex/GLM are out. Keep concurrent Claude agents ≤ 8 and QC workflows ≤ 2
to stay under the Claude session limit (hit once on 2026-10-14 ~20:45).

**Throughput rule (user, 2026-10-14): run the maximum number of agents the
dependency graph allows, everything async.** Never wait on a merge when a
dependent can branch from the in-flight branch it needs (rebase later; QC fix
rounds merge in). Caps: Codex 4, Opus UI 4, Grok 3, GLM 3, Sonnet 2. **Outages (2026-10-14 ~20:15): GLM out of OpenRouter credits, Codex at ChatGPT subscription usage limit → Sonnet cap 5, Grok cap 6 until they return.** Codex hit its limit again ~05:30 (2026-10-15). Grok cap lowered to 4 after a burst of 'Provider turn failed' with 6 concurrent (2026-10-15 ~05:00). QC
workflows unlimited. Cheap models still go first on mechanical work. Cheap
models go first on anything mechanical; escalate to Codex only on failure or
when the logic is subtle (state machines, protocol framing, concurrency).

## Storage policy (user, 2026-10-15)

Disk hit 100% once. Lean dev/test profiles via `../.cargo/config.toml`
(line tables only, no dep debug info). When a task merges, PM removes its
worktree (`git worktree remove`) — branches stay on origin. QC/scratch builds
share `CARGO_TARGET_DIR=/tmp/turin-scratch-target`; PM sweeps `/tmp` after QC
runs. Never keep copies of user databases.

## Integration branch

`stage` = main + every in-flight foundation branch (T01, T02b, T08, T09a, T10a, T11a, T12a). PM re-merges it when those move; dependent tasks base on it and `git merge stage` when told. Never merged to main directly — tasks merge to main individually after QC.

## Workflow

1. PM creates a worktree: `scripts/agent-worktree.sh <task-id>` →
   `../turin-wt/<task-id>` on branch `task/<task-id>`.
2. Agent works only in that worktree, only on files it owns.
3. Agent runs `scripts/check.sh -p <crates>` until green, commits on its branch,
   appends rows to the Deviations table in PORTING.md, reports back:
   what was ported, what was skipped and why, bugs found/fixed, open questions.
4. **QC gate (Opus 5.5).** PM runs the QC workflow on the branch: parallel
   reviewers per dimension (upstream fidelity, correctness, JSON/DB
   compatibility, concurrency/perf, tests, conventions), each finding
   adversarially verified. Confirmed findings go back to the task's agent as
   a fix round (same model; new round id). Repeat until QC returns clean.
   Script: `scripts/qc/turin-qc.workflow.js` (args: worktree, scope,
   upstream, context, optional dimensions; add a look-and-feel dimension for
   UI branches).
5. PM merges into `main` with
   `git merge --no-ff task/<id> -m "merge: <id> <title>" -m "Model: <model of the work> (<provider>)"`, pushes `main` to `origin`
   (github.com/Verassitnh/turin, private), rebases waiting branches.
6. PM also mirrors every `task/*` branch to `origin` whenever it checks in on
   agents, so in-progress work is visible and backed up. Agents never push. After each wave merges,
   QC runs a cross-crate audit on `main` (seams between crates, duplicated
   logic, drift from upstream).

## QC log

| Run | Target | Result |
|---|---|---|
| wf_c926ef06-3ca | T03 design system + shell (+ look-and-feel) | FAIL: 1 high, 4 medium, 27 low. docs/qc/wf_c926ef06-3ca-T03-design-system.md |
| wf_9dc37bfd-861 | T04b protocol docs fix round | FAIL: 5 high, 13 medium, 6 low. docs/qc/wf_9dc37bfd-861-T04b-protocols.md |
| wf_342ba162-dd7 | T24 icon fix rounds | PASS: 1 low (fixed, merged) |
| wf_4a9d9be1-c7c | T03 fix round re-QC | aborted: Claude session limit → rerun as wf_5a0d0e56-76b |
| wf_5bf4fe41-dd5 | T12a ACP crate | running |
| wf_118a9a5d-526 | T11a OpenCode transport | running |
| wf_5a0d0e56-76b | T03 fix round re-QC (Sonnet verify, Opus look-and-feel) | PASS: 6 low → T03c. docs/qc/wf_5a0d0e56-76b-T03-r2-design-system.md |
| wf_e19abae9-25e | T02b persistence fix round | FAIL: Db drop lost-wakeup deadlock (high) + index whitespace (low). docs/qc/wf_e19abae9-25e-T02b-persistence.md |
| wf_6497dded-82d | T04c protocol-doc application check (Sonnet/Haiku) | PASS: 3 low. docs/qc/wf_6497dded-82d-T04c-protocols.md |
| wf_54f7c42f-d77 | T10a Claude protocol (SDK fidelity, correctness) | FAIL: 1 high, 5 medium, 16 low. docs/qc/wf_54f7c42f-d77-T10a-claude-protocol.md |
| wf_44e1924e-b5c | T21b hooks fix round | PASS: 1 medium (script files unhashed), 1 low → T21c. docs/qc/wf_44e1924e-b5c-T21b-hooks.md |
| wf_e8a1a7c1-d53 | T21 hooks (spec, lifecycle, security/trust, process) | FAIL: 2 high, 5 medium, 6 low. docs/qc/wf_e8a1a7c1-d53-T21-hooks.md |
| wf_d6d58e6c-789 | T24 app icon (visual fidelity, reproducibility) | PASS: 1 medium, 6 low; T scale needs user decision. docs/qc/wf_d6d58e6c-789-T24-icon.md |
| wf_8b97ef76-9c9 | T02 persistence | PASS: 15 medium, 27 low → T02b. docs/qc/wf_8b97ef76-9c9-T02-persistence.md |
| wf_b2f290b9-219 | T00a fix round (base_schemas helpers) | PASS: 6 low → T00c |
| wf_42618e43-71a | T04 protocol docs (accuracy, completeness, Rust viability) | FAIL: 7 high, 18 medium → T04b. Cursor via ACP viable. docs/qc/wf_42618e43-71a-T04-protocols.md |
| wf_6ce02f2f-38c | foundation on `main` (base_schemas, PORTING, scripts) | FAIL: 34 confirmed (3 high). Report: docs/qc/wf_6ce02f2f-38c-foundation.md. Fix → T00 |

## Waves

Status: `todo` · `active` · `review` · `done` · `blocked`

### Wave 1: foundations (no cross-deps beyond `base_schemas`)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T00 | Foundation fixes from QC wf_6ce02f2f-38c (docs part done by PM; helpers: Codex died on auth expiry → Sonnet r2) | `crates/contracts/src/base_schemas.rs` | Codex → Sonnet | done (merged) |
| T00c | QC low items: UtcDateTime parse bounds/leap/zone-less, private field; `forward_compatible_*::optional` | `crates/contracts/src/base_schemas.rs` | Sonnet | todo (after T01 merges, to avoid conflicts) |
| T01 | Port all kept contracts modules | `crates/contracts` | GLM → Sonnet | r3 active (resume after Claude limit; merging t01-aud-a..e) |
| T02 | Persistence: SQLite, migrations, event store, receipts | `crates/persistence` | Grok | done (merged; QC PASS) |
| T02b | Persistence QC fix round (15 medium, 27 low) | `crates/persistence` | Grok | done (merged with T02c) |
| T03 | Design system + static app shell (theme, fonts, tokens, layout) | `crates/app` | Opus | done (merged; re-QC PASS) |
| T03c | T03 re-QC lows: contrast deviation row, 2 themePalette tests, send tooltip label, etc. | `crates/app/src/theme`, composer_surface | Haiku | active |
| T04 | Protocol research: Claude Agent SDK control protocol, `@cursor/sdk`, OpenCode v2 client | `docs/protocols/` | Codex → Haiku | done (merged; T04c verify PASS) |

### Wave 2: backend leaves (after T01)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T04c | Apply T04b re-QC corrections to protocol docs | `docs/protocols` | Haiku | done (merged). 3 low stale-note fixes → T04d (Haiku) |
| T02c | Persistence: Db drop deadlock + index DDL whitespace (from T02b re-QC) | `crates/persistence` | Haiku | done (merged; test catches bug, 6.5k cycles clean) |
| T05a | Shared utils, contract-free files | `crates/shared` | GLM → Sonnet → Grok → Haiku | r4 active (Haiku) |
| T05b | Shared utils, contract-dependent files | `crates/shared` (new modules only) | Codex | active (Codex re-auth) |
| T06 | Git: vcs drivers, GitManager, GitWorkflowService, checkpointing, workspace | `crates/git` | Grok | r2 active (core execute/parsers/checkpoints done) |
| T07 | Source control: core + GitHub driver, PR service, review guides, viewed files | `crates/source-control` | GLM → Sonnet | r3 active (resume after Claude limit) |
| T08 | Provider core: adapter trait, registry, catalog, model manifests, auth/credentials | `crates/provider-core` | Codex → Sonnet → Grok | done (stable trait + registries) → QC queued |
| T08b | Provider core remainder: subagentProjection, mcpToolPresentation, settings watcher → reconcile, model manifest fetch, NDJSON event log, ProviderHost live wiring | `crates/provider-core` | Codex | active (Codex re-auth) |

### Wave 3: engines (after wave 2)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T09a | Codex app-server protocol: codegen types from JSON schema + stdio JSON-RPC client | `crates/codex-app-server` | Codex → Grok | done → QC pending |
| T09 | Codex provider: CodexAdapterV2 + provider/Codex* | `crates/provider-codex` | Haiku | T09d r2 active (ChatGPT auth, managed runtime); then combined T09+T09d QC |
| T10a | Claude CLI protocol crate (replaces @anthropic-ai/claude-agent-sdk) | `crates/claude-protocol` | Codex → Grok | QC FAIL (1 high: requests hang after EOF; exit status not surfaced) → r3 fix (Grok) |
| T10 | Claude provider: ClaudeAdapterV2 + provider/Claude* | `crates/provider-claude` | Codex | r3 active (Codex re-auth) |
| T11a | OpenCode transport: version probe, v1+v2 server runtimes, HTTP clients, SSE | `crates/provider-opencode` | Grok | done → QC wf_118a9a5d-526 |
| T11 | OpenCode adapters (v1 then v2) | `crates/provider-opencode` | Haiku | QC wf_3c934e73-6da running |
| T12a | ACP protocol crate (port of effect-acp) | `crates/acp` | Codex → Grok | done → QC wf_5bf4fe41-dd5 |
| T12 | Cursor provider via `cursor-agent acp` (AcpAdapterV2 Cursor flavor) | `crates/provider-cursor` | Grok | r1 done (24 tests, local traits) → r2 wiring to real provider-core |
| T13a | Orchestrator read side: ProjectionStore + reads/paging/search/streams | `crates/orchestration/src/projection/**` | Codex → Sonnet | r2 active (Codex usage limit again) |
| T13b | Orchestrator write side: EventSink/Store, outbox/worker, receipts, command policy, thread lifecycle services | `crates/orchestration/src/pipeline/**` | Codex → Sonnet | r2 active (Codex usage limit again) |
| T13c | Orchestrator core + provider run services (Orchestrator.ts, ProviderSessionManager, RunExecution, ingest, recovery, checkpoints, PR reactors) | `crates/orchestration/src/core/**` | Codex (high) | todo (after T13a/b + T08) |
| T14 | Client view-model reducers (`client-runtime/state`) | `crates/app/src/state` | Opus → Sonnet | r2 active (logic → Sonnet per model policy) |

### Wave 4: UI (after T03 + T14)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T15 | Sidebar (projects, threads, status, snooze, search, drag) | `crates/app/src/components/sidebar` | Opus | r2 active (resume after Claude limit) |
| T16 | Thread view: timeline, markdown, tool cards, approvals, wrapping | `crates/app/src/components/chat` | Opus | r2 active (resume; merging task/T16-logic) |
| T17 | Composer: editor, mentions, model picker, attachments, stash, queue | `crates/app/src/composer` | Sonnet+Haiku | done (merged; QC PASS); backlog: queue drag, tasks drawer, usage-limits banner |
| T18 | Diff: changed files tree, turn/thread diffs, checkpoints | `crates/app/src/diff` | Opus | todo |
| T19 | GitHub PR views: list, detail tabs, checks, reviews, stacks, guide | `crates/app/src/pull_request` | Opus | todo |
| T20 | Settings + keybindings UI | `crates/app/src/settings` | Haiku | QC FAIL fixed (T20c); T20d GitHub-token redaction + premerge active |

### Wave 5: glue

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T21 | Hooks crate (orchestrator wiring later; UI parts → Opus) | `crates/hooks` | GLM → Sonnet | done (merged; re-QC PASS) |
| T21c | Hooks trust covers all of `.turin/` + docs/test fixes from re-QC | `crates/hooks` | Sonnet | done (merged; security re-QC queued) |
| T22 | App wiring: runtime bridge, end-to-end smoke with each provider | `crates/app` | Opus | todo |
| T23 | T3 database import | `crates/persistence` | Grok | active (snapshot of live statev2.sqlite, read-only) |

### Brand

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T24 | Turin app icon: T3 Code icon with a single T | `assets/**` | Opus | done (merged; T at 1.0×). 1 low (--keep-macos-png normalisation) → r4 |
| T24b | Wire icon: dev Dock icon, bundle .icns/actool, in-app mark | `crates/app` | Opus | todo (after T24 + T03 merge) |

## PM status 2026-10-15 23:30 UTC
- Merged since last update: T19, T17, N1 (main workspace check green).
- QC running: T11, T13d, U3 (U3a+U3b combined on task/U3b-shell-state).
- Active: T22a (live read path), DEDUP-1, T24b, T10c (QC fixes), T13c-F/D2/D4, T13e r2, T09d r2, T20d, U1/U2 (Sonnet), U3c (chat find/scripts/clipboard), U3d (thread actions/right panel), N1b (runtime async fs), T07 r4.


## PM status 2026-10-15 23:50 UTC
- Coverage (needed ≈ 421.6k upstream lines after cuts): on some branch 71.6% ported + 12.6% partial; on main 47.9% + 7.7%.
- Merged since 23:30: U3a+U3b. QC: T11 FAIL (T11c fixing), T13d FAIL (fix round), U3 PASS (merged).
- New logic bundles: R1 provider registry/auth/maintenance, R2 ACP session runtime + cursor accounts, R3 settings/theme logic, R4 composer/sidebar/client-runtime logic, R5 orchestration extras (context handoff, checkpoint rollback, workflow*) + storage cleanup.
- Queued for Sonnet (after U1/U2): settings UI (theme editor etc.), chat UI leftovers (T16 tsx), ui primitives (T03 tsx), composer UI leftovers, N8 provider update UI, N7b routes, N9 native shell, N10/N11.

## PM status 2026-10-16 00:05 UTC
- Policy switch (see Roster): Cursor for implementation; continuations of critical-path Haiku tasks (T13c-F, T13e, T11c, T13d fix, T22a) move to cursor/claude-sonnet-5-5 when their current runs end (no interrupts).
- New UI wave on Cursor: S1 settings UI leftovers (sonnet, from task/T20-settings), S2 chat UI leftovers (sonnet), S3 ui primitives + composer UI (sonnet), S4 provider update UI + prompt links + content search + open-in (composer-2.5).
- Held for capacity: S5 app shell routes (N7b) + native macOS shell (N9), S6 PR/diff/sidebar leftovers (T19/T18/T15). Cloud agents only if the Mac is truly bound.

## Definition of done (goal set 2026-10-16)
1. Coverage: every KEEP upstream file in docs/inventory/upstream-coverage.md is ported on main (coverage script: `none` = 0, every `partial` resolved or logged as a deviation).
2. QC: every merged branch has a QC PASS report in docs/qc/ (FAIL → fix → re-QC).
3. App works end to end (T22): real data read path, send/stop/interrupt write path, each provider (Claude, Codex, Cursor, OpenCode) smoke-tested against fake CLIs/servers; `cargo run -p turin` with a seeded dev DB.
4. UI QC: look-and-feel workflow over every screen (screenshots light/dark vs upstream classes), all UI bugs fixed, re-checked.
5. Full gate on main: `cargo check --workspace`, per-crate `scripts/check.sh` all green.
Capacity notes: rustc slots 4 (CPU was pinned at 97%), sccache on; cloud agents not useful for UI (macOS-only GPUI) — backend only if needed.

## PM status 2026-10-16 05:45 UTC (final wave)
- Policy: new agents are Sonnet 5.5 by default and Opus for hard work (e.g. composer prompt-editor parity, look-and-feel judging). No new Cursor/Grok work: both are out of usage. Never Codex. Ultracode: Workflow-driven QC, coverage, S3 and UI QC.
- Newly on main (QC PASS + fixes): T11 OpenCode, U3e+U3c chat logic and find, R2 ACP/Cursor, U2 usage/onboarding, S5 app shell, R4 composer + client logic, S2b chat media. App tests: 3512.
- Finished on branch, merging through T22b: T13e turn and restart services (QC PASS, all fixes in).
- In flight:
  - T13c-F orchestration dispatch remainder (Sonnet), with T13c QC workflow wf_5986b045-21a running.
  - T22b write path (approvals, queue, attachments, titles, @search, read-path gaps).
  - T22c provider smoke tests vs fake CLIs (Cursor, OpenCode, Codex).
  - T09d Codex provider: done; QC wf_307b939d-f99 running; remainder: model catalog, snapshotForCwd, usage pushes, codex text generation.
  - S1 settings UI, S2 chat panels, S6 PR/diff/sidebar, R3 theme logic + theme UI (all Sonnet).
  - S3 workflow wf_d3a2d49c-5f6 (primitives; composer prompt editor on Opus).
  - Coverage workflow wf_068e46cb-67c (C1 backend, C2 web).
  - UI QC pass 1 wf_7d19f01b-f48 on UIQC-1.
- Remaining to done:
  1. Merge T13c+T13e+T22b (the app working on main) and T22c.
  2. Merge T09d, S1, S2, S6, R3, S3/S3c, C1, C2 after QC.
  3. UI QC pass 2 on the newly merged screens.
  4. Coverage recount (/tmp/turin-pm/coverage.py).
  5. Full gate on main.

## PM status 2026-10-16 02:45 UTC
- On main (all QC PASS + fixes): T19, T17, N1, N1b, U3a/b, U3d, T10(+T10d), T07, T13d, T20, GITFS, T24b, DEDUP-1, U1. App crate 2838 tests green.
- In flight: T13c (dispatch S4–S6, Sonnet), T13e (RunExecution/turn start, Sonnet), R1 (ProviderRegistry, Sonnet), T22a fixes, T11 (verifier fixing 2 high), S4 (re-QC high fix), U3c fixes → U3e QC, R2 (ACP runtime), R3/R4/R5, T09d, S1/S2/S2b/S3/S5/S6 UI, U2.
- Infra: test-runner throttle (2 slots), rustc slots 4, sccache 6G, disk guard tiers 40/35/25/15/8G with idle-target eviction; fingerprint-safe pruning.
