# Task board

Resume notes: docs/PM_HANDOFF.md

PM (Claude Opus 5.5) owns this file. Agents don't edit it; they report back.

## Roster and budget

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
rounds merge in). Caps: Codex 4, Opus UI 4, Grok 3, GLM 3, Sonnet 2. **Outages (2026-10-14 ~20:15): GLM out of OpenRouter credits, Codex at ChatGPT subscription usage limit → Sonnet cap 5, Grok cap 6 until they return.** QC
workflows unlimited. Cheap models still go first on mechanical work. Cheap
models go first on anything mechanical; escalate to Codex only on failure or
when the logic is subtle (state machines, protocol framing, concurrency).

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
| wf_4a9d9be1-c7c | T03 fix round re-QC | aborted: Claude session limit → rerun pending |
| wf_e19abae9-25e | T02b persistence fix round | running (Sonnet/Haiku) |
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
| T02b | Persistence QC fix round (15 medium, 27 low) | `crates/persistence` | Grok | done (23/23) → re-QC wf_e19abae9-25e |
| T03 | Design system + static app shell (theme, fonts, tokens, layout) | `crates/app` | Opus | QC FAIL (1 high) → fix round r4 active; sidebar items moved to T15 |
| T04 | Protocol research: Claude Agent SDK control protocol, `@cursor/sdk`, OpenCode v2 client | `docs/protocols/` | Codex | re-QC wf_9dc37bfd-861 FAIL (3 high: Cursor ext result nesting, OpenCode v1 dir routing, OpenCode auto perms) → T04c (Haiku) + implementers steered |

### Wave 2: backend leaves (after T01)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T04c | Apply T04b re-QC corrections to protocol docs | `docs/protocols` | Haiku | active |
| T05a | Shared utils, contract-free files | `crates/shared` | GLM → Sonnet → Grok | r3 active (Grok) |
| T05b | Shared utils, contract-dependent files | `crates/shared` | GLM | todo (after T01) |
| T06 | Git: vcs drivers, GitManager, GitWorkflowService, checkpointing, workspace | `crates/git` | Grok | r2 active (core execute/parsers/checkpoints done) |
| T07 | Source control: core + GitHub driver, PR service, review guides, viewed files | `crates/source-control` | GLM → Sonnet | r3 active (resume after Claude limit) |
| T08 | Provider core: adapter trait, registry, catalog, model manifests, auth/credentials | `crates/provider-core` | Codex → Sonnet → Grok | r3 active (Grok) |

### Wave 3: engines (after wave 2)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T09a | Codex app-server protocol: codegen types from JSON schema + stdio JSON-RPC client | `crates/codex-app-server` | Codex → Grok | done → QC pending |
| T09 | Codex provider: CodexAdapterV2 + provider/Codex* | `crates/provider-codex` | Codex (high) | todo |
| T10a | Claude CLI protocol crate (replaces @anthropic-ai/claude-agent-sdk) | `crates/claude-protocol` | Codex → Grok | done → QC pending |
| T10 | Claude provider: ClaudeAdapterV2 + provider/Claude* | `crates/provider-claude` | Codex (high) | todo |
| T11a | OpenCode transport: version probe, v1+v2 server runtimes, HTTP clients, SSE | `crates/provider-opencode` | Grok | done (r2 routing/perms/ledger, live smoke ok) → QC pending |
| T11 | OpenCode adapters (v1 then v2) | `crates/provider-opencode` | Grok | todo |
| T12a | ACP protocol crate (port of effect-acp) | `crates/acp` | Codex → Grok | done (nested Cursor shapes) → QC pending |
| T12 | Cursor provider via `cursor-agent acp` (AcpAdapterV2 Cursor flavor) | `crates/provider-cursor` | Grok | todo |
| T13 | Orchestrator: event sink/store, projections, run execution, turn services | `crates/orchestration` | Codex (high), split into sub-tasks | todo |
| T14 | Client view-model reducers (`client-runtime/state`) | `crates/app/src/state` | Opus → Sonnet | r2 active (logic → Sonnet per model policy) |

### Wave 4: UI (after T03 + T14)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T15 | Sidebar (projects, threads, status, snooze, search, drag) | `crates/app/src/components/sidebar` | Opus | r2 active (resume after Claude limit) |
| T16 | Thread view: timeline, markdown, tool cards, approvals, wrapping | `crates/app/src/components/chat` | Opus | r2 active (resume; merging task/T16-logic) |
| T17 | Composer: editor, mentions, model picker, attachments, stash, queue | `crates/app/src/composer` | Opus | todo |
| T18 | Diff: changed files tree, turn/thread diffs, checkpoints | `crates/app/src/diff` | Opus | todo |
| T19 | GitHub PR views: list, detail tabs, checks, reviews, stacks, guide | `crates/app/src/pull_request` | Opus | todo |
| T20 | Settings + keybindings UI | `crates/app/src/settings` | Opus | todo |

### Wave 5: glue

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T21 | Hooks crate (orchestrator wiring later; UI parts → Opus) | `crates/hooks` | GLM → Sonnet | done (merged; re-QC PASS) |
| T21c | Hooks trust covers all of `.turin/` + docs/test fixes from re-QC | `crates/hooks` | Sonnet | active |
| T22 | App wiring: runtime bridge, end-to-end smoke with each provider | `crates/app` | Opus | todo |
| T23 | T3 database import | `crates/persistence` | Grok | active (snapshot of live statev2.sqlite, read-only) |

### Brand

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T24 | Turin app icon: T3 Code icon with a single T | `assets/**` | Opus | done (merged; T at 1.0×). 1 low (--keep-macos-png normalisation) → r4 |
| T24b | Wire icon: dev Dock icon, bundle .icns/actool, in-app mark | `crates/app` | Opus | todo (after T24 + T03 merge) |
