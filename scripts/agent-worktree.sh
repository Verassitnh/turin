#!/usr/bin/env bash
# Create (or reuse) an isolated worktree for one agent task.
# Usage: scripts/agent-worktree.sh <task-id> [base-branch]   (base defaults to main)
set -euo pipefail
task="${1:?task id required}"
base="${2:-main}"
root="$(git rev-parse --show-toplevel)"
# Worktrees, targets and the upstream checkout live next to each other in the
# workspace dir: the parent of the resolved `upstream` checkout (the repo itself
# may live elsewhere, e.g. ~/turin with an absolute `upstream` symlink).
upstream="$(cd "$root/upstream" && pwd -P)"
ws="$(dirname "$upstream")"
wt="$ws/turin-wt/$task"
if [ ! -d "$wt" ]; then
  git -C "$root" worktree add -q -b "task/$task" "$wt" "$base"
  ln -sfn "$upstream" "$wt/upstream"
  # Build output lives under ../turin-targets.noindex (Spotlight skips it).
  mkdir -p "$ws/turin-targets.noindex/$task"
  ln -sfn "$ws/turin-targets.noindex/$task" "$wt/target"
fi
echo "$wt"
