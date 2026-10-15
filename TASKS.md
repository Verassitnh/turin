# Task board

Resume notes: docs/PM_HANDOFF.md

PM (Claude Opus 5.5) owns this file. Agents don't edit it; they report back.

## Roster and budget

**Haiku everywhere (user, 2026-10-15 ~13:30 UTC): "just go crazy with haiku, its not worth it with the others."** All backend/logic work and QC run on Claude Haiku 5.5 (effort high). Cursor/Codex/OpenRouter are not worth pursuing now. UI stays on Sonnet (max 2). Opus = PM only, minimal.

**Claude budget (user, 2026-10-15 ~12:55 UTC): Claude usage nearly out.** Use
Cursor Auto, Codex and Haiku as much as possible; be very sparing with Opus
and Sonnet. UI may now be written by **Sonnet** (replaces the Opus-only UI
rule); max 2 Sonnet UI agents at once. Backend → Cursor Auto (`cursor`/`default`)
or Codex. Small fixes/cleanups → Haiku. QC workflows → Haiku for every role
(`models: {review: haiku, verify: haiku, lead: haiku, lookAndFeel: haiku}`).
PM (Opus) keeps its own calls minimal.

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

**Cursor Auto out (2026-10-15 ~13:25 UTC):** "You've hit your usage limit … set a Spend Limit to continue with Auto. Resets 10/22/2026." All Cursor tasks die on their next call. OpenCode free models refuse external clients ("free tier can only be used from within OpenCode"). Capacity left: Claude (low; Sonnet UI + Haiku QC only) and Codex (stuck: needs provider restart in T3). Waiting on user: Cursor spend limit / Codex restart / OpenRouter credits. On restore: relaunch each Cursor task as a fresh thread from its worktree's git state (+ docs/handoff/*).

**Codex 401s (2026-10-15):** T3's Codex app-server gets the access token once at launch; when the Codex daemon refreshes `~/.codex/auth.json` the old token dies → every Codex turn 401s. Fix: kill T3's `codex app-server -c model_provider=...` process; T3 respawns it with the fresh token. Codex provider stuck at 13:05 UTC ("Codex App Server process exited with code 0"; manual app-server works) → needs Codex provider restart in T3 settings. T09/T10/T08b/T05b moved to Cursor Auto (fresh threads). Old threads then can't resume ("Insufficient context allowance for the provider handoff": T3 replays the whole history into the new session) → relaunch as fresh delegate_task threads that rebuild state from git.

**Outage (2026-10-15 12:18 UTC): Grok hit Cursor's plan usage limit ("You're out of usage. Switch to Auto"). T06, T10a, T11, T12, T23 moved to Cursor Auto (`cursor`/`default`) in their own threads; trailer `Model: cursor-auto (cursor)`. Back to grok-4.7 when usage resets.**

**Throughput rule (user, 2026-10-14): run the maximum number of agents the
dependency graph allows, everything async.** Never wait on a merge when a
dependent can branch from the in-flight branch it needs (rebase later; QC fix
rounds merge in). Caps: Codex 4, Opus UI 4, Grok 3, GLM 3, Sonnet 2. **Outages (2026-10-14 ~20:15): GLM out of OpenRouter credits, Codex at ChatGPT subscription usage limit → Sonnet cap 5, Grok cap 6 until they return.** Codex hit its limit again ~05:30 (2026-10-15). Grok cap lowered to 4 after a burst of 'Provider turn failed' with 6 concurrent (2026-10-15 ~05:00). QC
workflows unlimited. Cheap models still go first on mechanical work. Cheap
models go first on anything mechanical; escalate to Codex only on failure or
when the logic is subtle (state machines, protocol framing, concurrency).

## CPU policy (user, 2026-10-15)

Never pin the machine at 100%. `../.cargo/config.toml` routes every rustc
through `../.cargo/rustc-throttle` (global slot cap in
`/tmp/turin-rustc-slots/limit`, default 5; nice 10; jobs 4 per cargo).
`/tmp/.cargo/config.toml` mirrors it for scratch crates (recreate after
reboot). Build output lives in `../turin-targets.noindex/` (Spotlight skips
it); each worktree's `target` is a symlink there.

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
| wf_1f8bff22-abb | T16 thread view (Haiku-only: logic fidelity, look-and-feel) | running |
| wf_0fd743c1-ca2 | T01 contracts (Haiku-only: wire fidelity, tests/rules) | running |
| wf_5bf4fe41-dd5 | T12a ACP crate | see docs/qc/wf_5bf4fe41-dd5-T12a-acp.md → T12a r3 fixes |
| wf_118a9a5d-526 | T11a OpenCode transport | see docs/qc/wf_118a9a5d-526-T11a-opencode-runtime.md → fixes in T11 r4 |
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
| T00c | QC low items: UtcDateTime parse bounds/leap/zone-less, private field; `forward_compatible_*::optional` | `crates/contracts/src/base_schemas.rs` | Cursor Auto | done (merged into T01; UtcDateTime field now private) |
| T00i | Upstream coverage inventory → docs/inventory/upstream-coverage.md | docs | Cursor Auto | done (merged): 40.4% of kept lines ported (54.9% incl. partial); 36.1% of all needed; 135 unassigned files → N1–N11 |
| N1 | App runtime: settings + secrets, keybindings file, data dir, environment descriptor, storage cleanup | `crates/runtime` | Haiku | active |
| N2s | Project services (server): identity, favicons, scratch projects, agent-session scan/import | `crates/projects` | Haiku | active |
| N2u | Onboarding / welcome UI | `crates/app` | Sonnet | todo |
| N3 | Text generation (titles, commit messages, PR bodies, branch names) per provider | provider crates | Cursor Auto | todo (after adapters) |
| N4 | Usage page: service (scan, aggregate, pricing) + UI | `crates/usage` + `crates/app` | Haiku (N4s backend active) + Sonnet (UI todo) | N4s active |
| N5 | Editor / Open-in: launcher, app icons, prefs, picker, file context menu | `crates/launcher` + `crates/app` | Haiku (N5s backend active) + Sonnet (UI todo) | N5s active |
| N6 | Git UI (branch toolbar/picker, git actions) + ReviewService | `crates/review` + `crates/app` | Haiku (N6s backend active) + Sonnet (UI todo) | N6s active |
| N7 | GitHubMediaFetch (folded into T07) | `crates/source-control` | Haiku | in T07 |
| N7b | App shell, routing, command palette, UI-state stores (fold into T22?) | `crates/app` | Sonnet | todo |
| N8 | Provider update UI | `crates/app` | Sonnet | todo |
| N9 | Native macOS shell: menu, window lifecycle, quit hold, dock badge | `crates/app` | Sonnet | todo |
| N10 | Prompt links (keep/cut: user decision) | `crates/app` | — | decide |
| N11 | Project content search dialog | `crates/app` | Sonnet | todo |
| DEDUP | Collapse 28 upstream files ported in >1 crate; delete 10 ports of cut files | all | Cursor Auto | todo (after wave merges) |
| T01 | Port all kept contracts modules | `crates/contracts` | GLM → Sonnet | done (9f37c0a, 545 tests) → QC queued |
| T02 | Persistence: SQLite, migrations, event store, receipts | `crates/persistence` | Grok | done (merged; QC PASS) |
| T02b | Persistence QC fix round (15 medium, 27 low) | `crates/persistence` | Grok | done (merged with T02c) |
| T03 | Design system + static app shell (theme, fonts, tokens, layout) | `crates/app` | Opus | done (merged; re-QC PASS) |
| T03c | T03 re-QC lows: contrast deviation row, 2 themePalette tests, send tooltip label, etc. | `crates/app/src/theme`, composer_surface | Haiku | done (merged) |
| T04 | Protocol research: Claude Agent SDK control protocol, `@cursor/sdk`, OpenCode v2 client | `docs/protocols/` | Codex → Haiku | done (merged; T04c verify PASS) |

### Wave 2: backend leaves (after T01)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T04c | Apply T04b re-QC corrections to protocol docs | `docs/protocols` | Haiku | done (merged, incl. T04d stale-note fixes) |
| T02c | Persistence: Db drop deadlock + index DDL whitespace (from T02b re-QC) | `crates/persistence` | Haiku | done (merged; test catches bug, 6.5k cycles clean) |
| T05a | Shared utils, contract-free files | `crates/shared` | GLM → Sonnet → Grok → Haiku | done (316 tests; markdown files live in T16's app/chat/markdown) → QC queued |
| T05b | Shared utils, contract-dependent files | `crates/shared` (new modules only) | Codex | active (Codex re-auth) |
| T06 | Git: vcs drivers, GitManager, GitWorkflowService, checkpointing, workspace | `crates/git` | Grok → Cursor Auto | done (151 tests) → QC queued |
| T07 | Source control: core + GitHub driver, PR service, review guides, viewed files | `crates/source-control` | GLM → Sonnet → Cursor Auto | r3 active (from docs/handoff/T07.md) |
| T08 | Provider core: adapter trait, registry, catalog, model manifests, auth/credentials | `crates/provider-core` | Codex → Sonnet → Grok | done (stable trait + registries) → QC queued |
| T08b | Provider core remainder: subagentProjection, mcpToolPresentation, settings watcher → reconcile, model manifest fetch, NDJSON event log, ProviderHost live wiring | `crates/provider-core` | Codex | active (Codex re-auth) |

### Wave 3: engines (after wave 2)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T09a | Codex app-server protocol: codegen types from JSON schema + stdio JSON-RPC client | `crates/codex-app-server` | Codex → Grok | done → QC pending |
| T09 | Codex provider: CodexAdapterV2 + provider/Codex* | `crates/provider-codex` | Codex | r3 active (Codex re-auth) |
| T10a | Claude CLI protocol crate (replaces @anthropic-ai/claude-agent-sdk) | `crates/claude-protocol` | Codex → Grok → Cursor Auto | r3 fixes done (16 items, 31 tests) → re-QC queued (Haiku) |
| T10 | Claude provider: ClaudeAdapterV2 + provider/Claude* | `crates/provider-claude` | Codex | r3 active (Codex re-auth) |
| T11a | OpenCode transport: version probe, v1+v2 server runtimes, HTTP clients, SSE | `crates/provider-opencode` | Grok | done → QC wf_118a9a5d-526 |
| T11 | OpenCode adapters (v1 then v2) | `crates/provider-opencode` | Grok → Cursor Auto | r3: process leak + unjustified cuts (92 tests) |
| T12a | ACP protocol crate (port of effect-acp) | `crates/acp` | Codex → Grok → Cursor Auto | QC PASS (5 medium, 8 low) → r3 fixes active |
| T12 | Cursor provider via `cursor-agent acp` (AcpAdapterV2 Cursor flavor) | `crates/provider-cursor` | Grok → Cursor Auto | done (26 tests, real provider-core traits) → QC queued |
| T13a | Orchestrator read side: ProjectionStore + reads/paging/search/streams | `crates/orchestration/src/projection/**` | Codex → Sonnet → Cursor Auto | r3 active (from docs/handoff/T13a.md; ~124 tests) |
| T13b | Orchestrator write side: EventSink/Store, outbox/worker, receipts, command policy, thread lifecycle services | `crates/orchestration/src/pipeline/**` | Codex → Sonnet → Cursor Auto | r3 active (from docs/handoff/T13b.md) |
| T13c | Orchestrator core + provider run services (Orchestrator.ts, ProviderSessionManager, RunExecution, ingest, recovery, checkpoints, PR reactors) | `crates/orchestration/src/core/**` | Cursor Auto (Codex when back) | active (base: T13b + T13a + stage) |
| T14 | Client view-model reducers (`client-runtime/state`) | `crates/app/src/state` | Opus → Sonnet → Cursor Auto | r3 active (from docs/handoff/T14.md; 451 tests) |

### Wave 4: UI (after T03 + T14)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T15 | Sidebar (projects, threads, status, snooze, search, drag) | `crates/app/src/components/sidebar` | Opus | done (286 tests) → QC queued |
| T16 | Thread view: timeline, markdown, tool cards, approvals, wrapping | `crates/app/src/components/chat` | Opus | done (294 tests) → QC queued |
| T17 | Composer: editor, mentions, model picker, attachments, stash, queue | `crates/app/src/composer` | Opus → Sonnet | active (base: T16) |
| T18 | Diff: changed files tree, turn/thread diffs, checkpoints | `crates/app/src/diff` | Opus → Sonnet | active (base: T16) |
| T19 | GitHub PR views: list, detail tabs, checks, reviews, stacks, guide | `crates/app/src/pull_request` | Sonnet | paused (Claude budget; next Sonnet UI slot) |
| T20 | Settings + keybindings UI | `crates/app/src/settings` | Opus | todo |

### Wave 5: glue

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T21 | Hooks crate (orchestrator wiring later; UI parts → Opus) | `crates/hooks` | GLM → Sonnet | done (merged; re-QC PASS) |
| T21c | Hooks trust covers all of `.turin/` + docs/test fixes from re-QC | `crates/hooks` | Sonnet | done (merged; security re-QC queued) |
| T22 | App wiring: runtime bridge, end-to-end smoke with each provider | `crates/app` | Opus | todo |
| T23 | T3 database import | `crates/persistence` | Grok → Cursor Auto | done (905 threads / 164,813 events decode, 0 errors) → QC queued |

### Brand

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T24 | Turin app icon: T3 Code icon with a single T | `assets/**` | Opus | done (merged; T at 1.0×). 1 low (--keep-macos-png normalisation) → r4 |
| T24b | Wire icon: dev Dock icon, bundle .icns/actool, in-app mark | `crates/app` | Opus | todo (after T24 + T03 merge) |
