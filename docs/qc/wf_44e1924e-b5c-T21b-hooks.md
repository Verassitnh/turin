# QC wf_44e1924e-b5c — T21b hooks fix round re-QC

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 3 · refuted: 0

## Lead report

1. MEDIUM, crates/hooks/src/config.rs:202, docs/hooks.md:45-63 and :161, PORTING.md:199-202 and :239 (Deviations row)
   - Problem: The trust hash covers only the bytes of `.turin/hooks.toml`.
     - Scripts that an approved command runs, such as `sh ./setup.sh` or `./scripts/new-worktree.sh`, are not hashed.
     - A `git pull` or agent edit that changes only the script runs new code with no re-approval.
     - The PoC reproduced this: after approval, rewriting `setup.sh` made `dispatch` return `Some(0)` and create the marker file.
     - It is worst with `cwd = "thread"`, where a relative script path resolves inside the agent-writable worktree.
     - The docs example `./scripts/new-worktree.sh` and the PORTING rationale "clone/pull/agent edit" imply broader coverage than exists.
     - This gap is not logged in Deviations.
   - Required fix (docs-only is acceptable, because the hash matches the QC item 1 spec and direnv has the same limit):
     - State in docs/hooks.md (Trust section), the PORTING.md Trust paragraph and the Deviations row that approval covers only the `hooks.toml` bytes and the command strings. Files those commands invoke are not covered and can change without re-approval.
     - Qualify the "Any change ... an agent edit revokes the approval" wording to say it applies to the file only.
     - Recommend inline commands, or scripts outside the repo (for example global `~/.turin/hooks.toml` with scripts under `~/.turin/hooks/`).
     - Mark or replace the `./scripts/new-worktree.sh` example (docs/hooks.md:161) with a note that it relies on trusting the repo's scripts, and avoid relative-script examples for `cwd = "thread"`.
     - Optional hardening: store hashes of project-tree files that commands reference and re-check them on dispatch.

2. LOW, docs/hooks.md:145-147, crates/hooks/src/runner.rs:636-647 (and the test near runner.rs:1950-1980), PORTING.md:95 and the Deviations table
   - Problem: After `sh` exits and the 250ms drain expires with a pipe still held, `kill_group()` (runner.rs:646-647, kill(-pgid, SIGKILL)) kills the whole group.
     - A detached job (`sleep 600 >/dev/null 2>&1 &`) is therefore also killed when a sibling still holds stdout/stderr. This was reproduced.
     - The docs say a job that redirects its output "keeps running", which is wrong in this mixed case.
     - The only test, `output_detached_background_job_is_left_running`, has no pipe holder.
     - PORTING.md:95 is silent on normal exit, and the Deviations table has no row for the drain behaviour. Upstream (processRunner.ts) waits for pipe EOF and does no group kill.
   - Design verdict: keep the current behaviour and make no code change. It is acceptable that a pipe-detached job survives a normal exit. It matches the documented daemon use case. Killing the group on every exit would break `nohup server >/dev/null 2>&1 &`. The failure mode is an early kill, which is the safe direction.
   - Required fix:
     - docs/hooks.md: say that if anything still holds the pipes after the drain, the entire group is killed, including jobs that redirected their output. Tell authors who want a surviving daemon to start it in its own session (`setsid`).
     - Add a mixed-case test: one detached job plus one pipe-holding sibling, asserting both are dead.
     - PORTING.md:95: add a line saying that on normal exit, after shell exit plus a 250ms drain, the group is killed only if something still holds the stdout/stderr pipes.
     - Add a Deviations row for this behaviour, since upstream waits for EOF with no group kill.

VERDICT: PASS (no critical/high; one medium and one low, both fixable in docs and tests)

## Confirmed findings (raw)

### [medium] Trust hash covers only hooks.toml; scripts it invokes run unapproved after modification (git pull / agent edit)
- `crates/hooks/src/config.rs:` · dimension: security · upheld 2/2
- Evidence: config.rs:202 `Some(approved) if approved == hash` gates only the bytes of .turin/hooks.toml. docs/hooks.md:58 and PORTING.md:199-202 claim a `git pull`, branch switch, or agent edit invalidates approval, and the docs' own example (docs/hooks.md ~L161) approves `command = "./scripts/new-worktree.sh"`, a repo-tracked file that is not hashed. PoC in /tmp/qc21/src/crates/hooks/tests/poc.rs: approve hooks.toml with `command="sh ./setup.sh"` (blocking thread.created) via trust_project_hooks(hash). Then rewrite only setup.sh to `touch /tmp/qc21/PWNED`. runner.dispatch(ThreadCreated) returns exit_code Some(0) and /tmp/qc21/PWNED exists. config.rs L202 still says Trusted, no Pending update, no re-approval. Cause: only the repo-supplied hooks.toml is hashed, not the files its commands execute from the project tree.
- Fix: Narrow the guarantee or widen the gate. Cheapest: state in docs/hooks.md, PORTING.md Trust, and the Approve UI that approval covers the command strings only, not files they invoke, and recommend commands that live outside the repo (e.g. ~/.turin/hooks/). Stronger: record hashes of referenced project files (or of the whole tracked tree) with the approval and re-check them on dispatch. Alternatively, refuse project hooks whose command resolves to a path under the project root unless that file's hash is stored too. Either way, remove or qualify the 'git pull / agent edit revokes approval' wording and the script-based worktree example.

### [low] Trust hash covers only hooks.toml, not the repo scripts it runs, and the docs example invites that bypass
- `docs/hooks.md:` · dimension: fix-verification · upheld 2/2
- Evidence: The approval is SHA-256(.turin/hooks.toml bytes) (config.rs:HooksConfig::load, trust.rs). The docs example is a project hook with command = "./scripts/new-worktree.sh" (hooks.md:161). Nothing in the Trust section (hooks.md:45-63) says the referenced script is unhashed. A `git pull`, or an agent in workspace-write mode editing scripts/new-worktree.sh, runs new code on the next event with the hook file byte-identical and no re-approval. This defeats the QC item 1 threat (agent edit or git pull leading to unsandboxed execution). It is worst for `cwd = "thread"` hooks, which run the agent-writable worktree copy. The new text 'Any change to the file ... an agent edit — revokes the approval' reads as full protection, and Review what you approve (hooks.md:61) lists only command strings. I checked this by reading the code. The attack is not run end to end.
- Fix: This is the same limit direnv has, so the code can stay. Do three things in the docs. (1) Say that approval covers only hooks.toml bytes, and that scripts the commands call (./scripts/*) are not covered, so a changed script runs without re-approval. (2) Tell users to put approved logic inline in the command, or in the global ~/.turin/hooks.toml with scripts outside the repo, and mark the repo-script example as relying on trusting the repo's scripts. (3) Add one sentence about this residual risk to the PORTING.md trust paragraph and the Deviations row.

### [low] Drain-expiry kill takes the whole process group, so a detached (output-redirected) job is also killed if any sibling still holds the pipes; docs say it keeps running
- `crates/hooks/src/runner.rs:` · dimension: fix-verification · upheld 2/2
- Evidence: runner.rs:646-647 calls `self.kill_group()` when `leftover_group` is true, which sends kill(-pgid, SIGKILL) to every group member. docs/hooks.md:145-147 says 'Anything still holding the hook's stdout/stderr ... is killed. A job that redirects its output away ... keeps running.' The mixed case contradicts this. I ran the hook `sleep 600 >/dev/null 2>&1 & echo $! > a.pid; sleep 601 & echo $! > b.pid` in a /tmp copy. The result was `QC detached alive=false holder alive=false`. The redirected daemon was killed because its sibling held stdout. Whether a daemon survives therefore depends on whether an unrelated sibling job keeps the pipes open. The test output_detached_background_job_is_left_running covers only the case with no pipe holder.
- Fix: Keep the design (see the nohup judgement below). Correct the docs to say that if anything still holds the pipes after the drain, the entire group is killed, including jobs that redirected their output. Add a mixed-case test that asserts this. Tell hook authors who want a daemon that survives to start it in its own session (`setsid`/`nohup setsid ... &`), which is the only reliable way out of the group. PORTING.md:95 only says the group is killed on timeout/shutdown and is silent on normal exit. Add one line there: after shell exit plus a 250ms drain, the group is killed only if something still holds the stdout/stderr pipes.
