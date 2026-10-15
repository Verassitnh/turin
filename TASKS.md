# Task board

PM (Claude Opus 5.5) owns this file. Agents don't edit it; they report back.

## Roster and budget

| Agent | Model | Cost | Use for |
|---|---|---|---|
| PM | Claude Opus 5.5 | $$$$ | Planning, briefs, merges, conflict calls. No implementation. |
| Codex | gpt-6-sol (medium; high only for orchestrator/adapters) | $$$ | Hardest ports: orchestrator core, provider adapters, protocol work |
| Claude | Sonnet 5.5 (high) | $$ | GPUI UI ports (look and feel) |
| Grok | grok-4.7 via Cursor (fast) | $ | Mid-size self-contained crates: persistence, git, source-control |
| GLM | glm-5.3 via OpenCode | ¢ | Bulk mechanical translation: contracts, shared utils, reducers, test ports |
| Haiku | Claude Haiku 5.5 | ¢ | Small fixes, clippy/fmt cleanups, review passes |

Rules of thumb: at most 2 Codex tasks and 2 Sonnet tasks in flight. Cheap
models go first on anything mechanical; escalate to Codex only on failure or
when the logic is subtle (state machines, protocol framing, concurrency).

## Workflow

1. PM creates a worktree: `scripts/agent-worktree.sh <task-id>` →
   `../turin-wt/<task-id>` on branch `task/<task-id>`.
2. Agent works only in that worktree, only on files it owns.
3. Agent runs `scripts/check.sh -p <crates>` until green, commits on its branch,
   appends rows to the Deviations table in PORTING.md, reports back:
   what was ported, what was skipped and why, bugs found/fixed, open questions.
4. PM reviews, merges into `main`, rebases waiting branches.

## Waves

Status: `todo` · `active` · `review` · `done` · `blocked`

### Wave 1: foundations (no cross-deps beyond `base_schemas`)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T01 | Port all kept contracts modules | `crates/contracts` | GLM | todo |
| T02 | Persistence: SQLite, migrations, event store, receipts | `crates/persistence` | Grok | todo |
| T03 | Design system + static app shell (theme, fonts, tokens, layout) | `crates/app` | Sonnet | todo |
| T04 | Protocol research: Claude Agent SDK control protocol, `@cursor/sdk`, OpenCode v2 client | `docs/protocols/` | Codex | todo |

### Wave 2: backend leaves (after T01)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T05 | Shared utils (kept files) | `crates/shared` | GLM | todo |
| T06 | Git: vcs drivers, GitManager, GitWorkflowService, checkpointing, workspace | `crates/git` | Grok | todo |
| T07 | Source control: core + GitHub driver, PR service, review guides, viewed files | `crates/source-control` | Grok | todo |
| T08 | Provider core: adapter trait, registry, catalog, model manifests, auth/credentials | `crates/provider-core` | Codex | todo |

### Wave 3: engines (after wave 2)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T09 | Codex provider: app-server JSON-RPC client + CodexAdapterV2 | `crates/provider-codex` | Codex (high) | todo |
| T10 | Claude provider: CLI stream-json control protocol + ClaudeAdapterV2 | `crates/provider-claude` | Codex (high) | todo |
| T11 | OpenCode provider | `crates/provider-opencode` | Grok | todo |
| T12 | Cursor provider | `crates/provider-cursor` | Grok | todo |
| T13 | Orchestrator: event sink/store, projections, run execution, turn services | `crates/orchestration` | Codex (high), split into sub-tasks | todo |
| T14 | Client view-model reducers (`client-runtime/state`) | `crates/app/src/state` | GLM | todo |

### Wave 4: UI (after T03 + T14)

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T15 | Sidebar (projects, threads, status, snooze, search, drag) | `crates/app/src/sidebar` | Sonnet | todo |
| T16 | Thread view: timeline, markdown, tool cards, approvals, wrapping | `crates/app/src/chat` | Sonnet | todo |
| T17 | Composer: editor, mentions, model picker, attachments, stash, queue | `crates/app/src/composer` | Sonnet | todo |
| T18 | Diff: changed files tree, turn/thread diffs, checkpoints | `crates/app/src/diff` | Grok | todo |
| T19 | GitHub PR views: list, detail tabs, checks, reviews, stacks, guide | `crates/app/src/pull_request` | Sonnet | todo |
| T20 | Settings + keybindings UI | `crates/app/src/settings` | GLM | todo |

### Wave 5: glue

| ID | Task | Owns | Agent | Status |
|---|---|---|---|---|
| T21 | Hooks crate + orchestrator wiring | `crates/hooks` | GLM | todo |
| T22 | App wiring: runtime bridge, end-to-end smoke with each provider | `crates/app` | Codex | todo |
| T23 | T3 database import | `crates/persistence` | Grok | todo |
