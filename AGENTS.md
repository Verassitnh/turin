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
- Commit on your branch with Conventional Commits messages. Don't push.
