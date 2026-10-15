#!/usr/bin/env bash
# Create (or reuse) an isolated worktree for one agent task.
# Usage: scripts/agent-worktree.sh <task-id> [base-branch]   (base defaults to main)
set -euo pipefail
task="${1:?task id required}"
base="${2:-main}"
root="$(git rev-parse --show-toplevel)"
wt="$(dirname "$root")/turin-wt/$task"
if [ ! -d "$wt" ]; then
  git -C "$root" worktree add -q -b "task/$task" "$wt" "$base"
  ln -sfn "$(cd "$root/../upstream" && pwd)" "$wt/upstream"
  # Build output lives under ../turin-targets.noindex (Spotlight skips it).
  mkdir -p "$(dirname "$root")/turin-targets.noindex/$task"
  ln -sfn "$(dirname "$root")/turin-targets.noindex/$task" "$wt/target"
fi
echo "$wt"
