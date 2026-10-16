#!/usr/bin/env bash
# Re-captures wizard and recovery PNGs under docs/screenshots/U2/.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BIN="${TURIN_BIN:-$ROOT/target/debug/turin}"
WINID_BIN="${TMPDIR:-/tmp}/turin-window-id"
if [ ! -x "$WINID_BIN" ]; then
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun swiftc -O "$ROOT/scripts/window-id.swift" -o "$WINID_BIN"
fi
WINID="$WINID_BIN"
DELAY="${TURIN_SHOT_DELAY_MS:-4500}"
OUT="$ROOT/docs/screenshots/U2"

shot() {
  local appearance=$1 file=$2
  shift 2
  echo "→ $file ($appearance) $*"
  env TURIN_APPEARANCE="$appearance" TURIN_SHOT="$OUT/$file" TURIN_WINID="$WINID" \
    TURIN_SHOT_DELAY_MS="$DELAY" "$@" "$BIN"
}

# Wizard (fresh install + mock wizard steps)
for appearance in dark light; do
  shot "$appearance" "${appearance}-wizard-agents.png" \
    TURIN_FIRST_RUN=fresh TURIN_WIZARD_PROVIDERS=mixed TURIN_WIZARD_T3=found
  shot "$appearance" "${appearance}-wizard-import.png" \
    TURIN_FIRST_RUN=fresh TURIN_WIZARD_STEP=import TURIN_WIZARD_PROVIDERS=mixed TURIN_WIZARD_T3=found
  shot "$appearance" "${appearance}-wizard-command.png" \
    TURIN_FIRST_RUN=fresh TURIN_WIZARD_STEP=command TURIN_WIZARD_PROVIDERS=signin TURIN_WIZARD_T3=none
done

# Recovery: stalled connection (dark) and settings read failure (light)
shot dark dark-recovery.png TURIN_FIRST_RUN=stalled
shot light light-recovery.png TURIN_FIRST_RUN=settings-failed

echo "Done."
