#!/usr/bin/env bash
# Builds a release binary and wraps it in Turin.app (macOS only).
#
# Usage: scripts/bundle-macos.sh [output-dir]   (default: $CARGO_TARGET_DIR or target)/bundle
#
# Offline: only cargo (deps already in the cache), sips, iconutil, codesign and
# plutil, all shipped with macOS / Xcode. The icon is rebuilt from
# assets/prod/black-macos-1024.png on each run.
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "bundle-macos.sh: macOS only" >&2
  exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

# Capture the caller's cwd before entering the repo. A relative output dir is
# resolved against that cwd, including a path that does not exist yet.
caller_pwd="$PWD"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

target_dir="${CARGO_TARGET_DIR:-target}"
case "${1:-}" in
  "") out_dir="$target_dir/bundle" ;;
  /*) out_dir="$1" ;;
  *) out_dir="$caller_pwd/$1" ;;
esac
app="$out_dir/Turin.app"
source_png="assets/prod/black-macos-1024.png"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"

cargo build --release -p turin

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target_dir/release/turin" "$app/Contents/MacOS/turin"

# iconset -> .icns. Sizes and @2x names are the ones iconutil expects.
iconset="$(mktemp -d "${TMPDIR:-/tmp}/turin-iconset.XXXXXX")/Turin.iconset"
trap 'rm -rf "$(dirname "$iconset")"' EXIT
mkdir -p "$iconset"
while read -r px name; do
  sips -z "$px" "$px" "$source_png" --out "$iconset/$name" >/dev/null
done <<'EOF'
16 icon_16x16.png
32 icon_16x16@2x.png
32 icon_32x32.png
64 icon_32x32@2x.png
128 icon_128x128.png
256 icon_128x128@2x.png
256 icon_256x256.png
512 icon_256x256@2x.png
512 icon_512x512.png
1024 icon_512x512@2x.png
EOF
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Turin.icns"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>Turin</string>
  <key>CFBundleDisplayName</key>
  <string>Turin</string>
  <key>CFBundleIdentifier</key>
  <string>dev.turin.app</string>
  <key>CFBundleExecutable</key>
  <string>turin</string>
  <key>CFBundleIconFile</key>
  <string>Turin</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad-hoc signature so the bundle launches on Apple Silicon without a developer ID.
codesign --force --sign - "$app"
codesign --verify --strict "$app"

echo "Built $app"
