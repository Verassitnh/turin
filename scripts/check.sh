#!/usr/bin/env bash
# Gate every task must pass before handing back.
# Usage: scripts/check.sh                 # whole workspace (PM merges)
#        scripts/check.sh -p turin-git    # only the crates you own (+ deps)
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
scope=("$@")
[ ${#scope[@]} -eq 0 ] && scope=(--workspace)
cargo fmt --all --check
cargo build "${scope[@]}" --all-targets
cargo test "${scope[@]}"
cargo clippy "${scope[@]}" --all-targets -- -D warnings
