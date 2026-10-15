# PM handoff (resume point)

Written by the PM (Claude Opus 5.5) before an app restart on 2026-10-14.
Read this, TASKS.md and PORTING.md to resume.

## Paths
- Main checkout: `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin` (branch `main`, origin = github.com/Verassitnh/turin, private)
- Upstream: `../upstream` (pingdotgg/t3code@ec1313c8)
- Agent worktrees: `../turin-wt/<task-id>` (branch `task/<task-id>`)
- QC workflow script: `scripts/qc/turin-qc.workflow.js` (run via the Workflow tool with `scriptPath`; args: worktree, scope, upstream, context, optional dimensions)
- Build env: `PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`

## Delegated tasks (T3 delegate_task; check with task_status)
| Task | Model | taskId suffix (prefix `node:delegated-task:command%3Amcp%3Ae30e345b-6629-40c5-a2b3-27cc96f9edcf%3Adelegate-task%3A`) | State at handoff |
|---|---|---|---|
| T01 contracts | openrouter/z-ai/glm-5.3 (opencode) | `turin-T01-r1` | running; told to commit WIP + merge main (T00a landed) |
| T03 design system | claude-opus-5-5 (claudeAgent) | `turin-T03-r2-opus` | running; told to commit WIP |
| T02 persistence | grok-4.7 (cursor) | `turin-T02-r1` | done; branch in QC |
| T04 protocols | gpt-6-sol (codex) | `turin-T04-r1` | done; branch in QC |
| T00a helpers | gpt-6-sol → claude-sonnet-5-5 | `turin-T00a-r2-sonnet` | merged to main |

Child thread ids are the same strings with `node:` replaced by `thread:`.

After the restart the MCP session prefix changed for new tasks:
`node:delegated-task:command%3Amcp%3A51608ea3-eaef-438d-89cb-e132943c1fa6%3A…`
| T03 r3 (resume in same child thread) | claude-opus-5-5 | `…%3Athread-send%3Aturin-T03-r3-resume` | running |
| T09a codex-app-server | gpt-6-sol (codex) | `…%3Adelegate-task%3Aturin-T09a-r1` | running |

QC runs wf_8b97ef76-9c9 (T02) and wf_42618e43-71a (T04) were resumed after the restart (same run ids).

## QC workflow runs in flight (lost if the session dies; re-run with same args)
| Run | Target | Worktree | Notes |
|---|---|---|---|
| wf_42618e43-71a | T04 protocol docs | `../turin-wt/T04-protocols` | custom dimensions: accuracy, completeness, rust-viability (Cursor via `cursor-agent acp`? OpenCode 1.19 vs 2.x) |
| wf_8b97ef76-9c9 | T02 persistence | `../turin-wt/T02-persistence` | default dimensions; schema diff vs upstream |

## Decisions pending
- Cursor provider: leaning `cursor-agent acp` (ACP over stdio) by porting upstream effect-acp + AcpAdapterV2, instead of in-process `@cursor/sdk`. Waiting on T04 QC.
- OpenCode: installed CLI 1.19.0 vs upstream's 2.x client; pick adapter generation after T04 QC.

## Next actions
1. Collect T04 + T02 QC results → fix rounds or merge.
2. When T01 finishes → QC → merge → dispatch wave 2: T05 shared (GLM), T06 git (Grok), T07 source-control (Grok), T08 provider-core (Codex, now logged in again).
3. T00c (Sonnet): QC low items in base_schemas after T01 merges.
4. Push task branches to origin on every check-in.


## Update 2026-10-14 ~20:10

- OpenRouter credits exhausted: GLM tasks T01, T05a, T07 died; work saved (GLM-attributed WIP commits) and reassigned to Sonnet 5.5 (T01 r2, T05a r2, T07 r2). T21 fix round (T21b) on Sonnet too. Ask the user to top up OpenRouter before using GLM again.
- T06 (Grok), T07 (Sonnet), T08 (Codex) are based on task/T01-contracts and are waiting for the PM to say "merge T01" once T01 compiles.
- T24 icon: QC PASS; user asked to see 1.0x vs 1.25x T before deciding (agent producing assets/prod/compare-scale-options.png).
- Live QC reports in docs/qc/. Board in TASKS.md is current.


## Update 2026-10-15 ~04:30

- Claude session limit hit ~20:45 on 10-14; all Claude tasks died, WIP saved per worktree (model-attributed). Codex usage reset; GLM still out (OpenRouter credits).
- Model policy (user): Sonnet/Haiku wherever reasonable; Opus for UI rendering + look-and-feel QC only. QC workflow takes `models` arg (default review sonnet, verify haiku, lead sonnet).
- Integration branch `stage` (worktree ../turin-wt/stage) = main + T01 + T02b + T08 + T09a + T10a + T11a + T12a. Re-merge when those move; dependents `git merge stage` on instruction.
- New MCP session prefix for tasks: `node:delegated-task:command%3Amcp%3Aa4bbaa6c-c3a6-42ee-89f1-02d82165c892%3Adelegate-task%3Aturin-<id>` (resumes in old threads use `…%3Athread-send%3A…`).
- Running: Codex T13a, T13b, T09, T10 · Grok T06, T08, T05a, T23, T11, T12 · Sonnet T01(r3), T07(r3), T14, T21c · Opus T15(r2), T16(r2) · QC T02b re-QC (wf_e19abae9-25e), T10a (wf_54f7c42f-d77).
- Queued QC: T04c light verify, T09a, T11a, T12a, T03 re-QC rerun.
