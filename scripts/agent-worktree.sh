#!/usr/bin/env bash
# Create (or reuse) an isolated worktree for one agent task.
# Usage: scripts/agent-worktree.sh <task-id>   e.g. T01-contracts
set -euo pipefail
task="${1:?task id required}"
root="$(git rev-parse --show-toplevel)"
wt="$(dirname "$root")/turin-wt/$task"
if [ ! -d "$wt" ]; then
  git -C "$root" worktree add -q -b "task/$task" "$wt" main
  ln -sfn "$(cd "$root/../upstream" && pwd)" "$wt/upstream"
fi
echo "$wt"
