#!/usr/bin/env bash
# Screenshot the Turin window for a demo scenario.
#   scripts/shot.sh docs/screenshots/T19/list-dark.png TURIN_APPEARANCE=dark TURIN_PULL_REQUESTS_DEMO=list
# Launches target/debug/turin with the given env, waits for the window, captures exactly that
# window, then stops the process it started (by the pid captured at spawn).
set -euo pipefail
out="$1"; shift
delay="${SHOT_DELAY:-4}"
mkdir -p "$(dirname "$out")"
env "$@" target/debug/turin >/dev/null 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT
sleep "$delay"
bin="${TMPDIR:-/tmp}/turin-window-id"
if [ ! -x "$bin" ]; then
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun swiftc -O scripts/window-id.swift -o "$bin"
fi
id=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
  id="$("$bin" "$pid" 2>/dev/null || true)"
  [ -n "$id" ] && break
  sleep 1
done
[ -n "$id" ] || { echo "no window for pid $pid" >&2; exit 1; }
screencapture -x -o -l"$id" "$out"
echo "$out"
