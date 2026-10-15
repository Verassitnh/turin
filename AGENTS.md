# Agents

Read PORTING.md before touching code. It is the source of truth for upstream
mapping, Effect→Rust conventions, the cut list, and the Deviations log.
TASKS.md is the PM's board; don't edit it.

- You work in your own worktree (`../turin-wt/<task-id>`, branch `task/<task-id>`).
  Never touch `main` or other worktrees.
- Upstream reference checkout: `./upstream` (symlink). Read-only.
- Build env: `export PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`
  (GPUI needs the full Xcode toolchain for the Metal shader compiler).
- Gate: `scripts/check.sh -p <your crates>` must pass before you report back.
- Only edit files you own. You may add dependencies to your crate's
  Cargo.toml; prefer `x.workspace = true` and add new entries to the root
  `[workspace.dependencies]` only when needed (PM resolves conflicts).
- **Commit regularly** on your branch: after every module/logical unit, and
  never sit on more than ~1 hour of uncommitted work. Conventional Commits.
  Don't push.
- **Every commit names its model** with a trailer (enforced by the
  `commit-msg` hook in scripts/git-hooks):

      Model: <model-id> (<provider>)

  e.g. `git commit -m "feat(contracts): port vcs" -m "Model: gpt-6-sol (codex)"`.
  Use your exact model id and T3 provider instance (`codex`, `claudeAgent`,
  `cursor`, `opencode`).
