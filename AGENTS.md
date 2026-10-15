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
  `cursor`, `opencode`). The hook is installed repo-wide via
  `git config core.hooksPath scripts/git-hooks` (already set).
- Pulling in `main` when asked: `git merge main -m "merge: main into <task>" -m "Model: …"`
  (merge commits need the trailer too).

- **Storage (user rule): disk space is tight.** Build only the crates you own
  (`-p`). Lean debug profiles come from `../.cargo/config.toml` — don't
  override them. Throwaway crates/builds go under `/tmp/turin-scratch-<task>/`
  and must reuse `CARGO_TARGET_DIR=/tmp/turin-scratch-target` (shared); delete
  your scratch dir when done. Never copy user databases anywhere.

- **CPU (user rule): never pin the machine.** Every rustc goes through
  `../.cargo/rustc-throttle` (global cap of 5 compilers, nice 10; set in
  `../.cargo/config.toml`, mirrored in `/tmp/.cargo/config.toml` for scratch
  crates). Don't override `RUSTC_WRAPPER`, `CARGO_BUILD_JOBS` or `build.jobs`;
  run one cargo command at a time; a build that waits for a slot is normal —
  don't kill or retry it. `target/` is a symlink into `../turin-targets.noindex/`:
  never `rm -rf target` (use `cargo clean -p <crate>`).

- **Never search the whole disk.** No `find /`, `find ~`, or recursive grep
  over `/`, `$HOME`, `~/.bun`, `~/Library` (a watchdog kills them after 20s;
  they pinned the CPU). Upstream's npm dependencies are installed read-only in
  `./upstream/node_modules` (e.g. `effect`, `@effect/*`, `@pierre/diffs`,
  `@tiptap/*`): read sources there. Search only inside your worktree,
  `./upstream`, or `~/.cargo/registry/src` for Rust crates.
