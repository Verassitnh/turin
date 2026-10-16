# QC wf_7d761296-44a — T24b icon wiring

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 4 · refuted: 1

## Lead report

1. low: crates/app/src/... no. Fix: scripts/bundle-macos.sh:23. Problem: `out_dir` is computed after `cd "$root"` at line 20, so a relative first argument resolves against the repo root, not the caller's cwd. The README (around lines 22-23) says only "Pass a directory as the first argument", which users will read as cwd-relative. Required fix: before the `cd` at line 20, capture `$1` as an absolute path, e.g. `case "${1:-}" in "") out_dir="$root/target/bundle";; /*) out_dir="$1";; *) out_dir="$PWD/$1";; esac`, then quote it everywhere. Alternatively, document the argument as repo-root-relative in the README. Do not use `cd "$1" && pwd || echo ...`, because it fails for non-existent absolute paths.

2. low: PORTING.md:322. Problem: the sidebar brand Deviations row still says the slot renders only the text "Turin". The branch now adds a T glyph (sidebar_chrome.rs:57-69, `svg().path(WORDMARK)`, fixed 8.71x10px) and drops the "3" from upstream T3Wordmark. Ground rule 4 requires this to be logged. Required fix: update the row to say the slot renders the T-only mark from `assets/brand/wordmark.svg` in foreground, followed by "Turin". Note that the mark is fixed px rather than upstream's `h-[1cap] w-auto`, and that the "3" is omitted. Make this edit after item 3 is settled so the row matches the final code.

3. low: crates/app/src/components/sidebar/sidebar_chrome.rs:66-67. Problem: `.w(px(8.71)).h(px(10.))` is fixed px, but the name beside it uses rem-based `TEXT_SM` (theme/tokens.rs:97). The interface font size scales rems (gpui-component root.rs:440 `set_rem_size`), so at size_interface=20 the name is 17.5px and the mark stays 10px. Upstream uses `h-[1cap]`, which is font-relative. Required fix: express the mark in rems, `h = rems(0.625)` (10px at the 16px default) and `w = rems(0.625 * 48.8 / 56)` (about 0.5446rem, 8.71px at default). Confirm the helper name in gpui_kit first. If the px sizing is kept deliberately, log it in the Deviations row from item 2.

4. low: crates/app/src/assets.rs:123 (and WORDMARK at about lines 119-120). Problem: the Dock icon uses `include_bytes!("../assets/brand/app-icon.png")`, and the bundle icon is built from `assets/prod/black-macos-1024.png` (bundle-macos.sh:25). The two files are byte-identical today, and the wordmark copies match too, but nothing keeps them in sync. `assets/export-icons.py` rewrites the prod PNG on each run, so the Dock icon can go stale. Required fix: point `include_bytes!` at `../../../assets/prod/black-macos-1024.png` (valid from crates/app/src) and the wordmark at the prod SVG, then delete the duplicates under crates/app/assets/brand. If the copies must stay, add a check that they match and document the copy.

Verdict: PASS (no critical or high findings; four low items, with items 1 and 2 recommended before merge).

## Confirmed findings (raw)

### [low] Sidebar brand T mark is fixed px, does not scale with the interface font size
- `crates/app/src/components/sidebar/sidebar_chrome.rs:66` · dimension: correctness · upheld 2/2
- Evidence: sidebar_chrome.rs:66-67 `.w(px(8.71)).h(px(10.))`. Upstream SidebarBrandMark uses `h-[1cap]` on a 0.875rem-equivalent `text-sm` span (SidebarChrome.tsx:132-133), so the mark is a font-relative length. Turin's name text is rem-based (`TEXT_SM = text(0.875, 1.25)` in theme/tokens.rs:97). The interface font size is user-configurable and clamped 12..20 (theme/appearance_fonts.rs:126, 283). tokens.rs:4-7 states the convention that lengths are rems where upstream uses rems so they scale with the interface font size. At size_interface=20 the name renders at 17.5px while the mark stays 10px, so the brand slot no longer matches upstream proportions.
- Fix: Express the mark in rems so it matches the default 16px rem exactly and scales with the setting: h = rems(0.625) (10px at 16px), w = rems(0.625 * 48.8 / 56) (about 0.5446rem, i.e. 8.71px at 16px). Check the exact rems helper name in gpui_kit.

### [low] bundle-macos.sh resolves a relative output-dir argument against the repo root, not the caller's cwd
- `scripts/bundle-macos.sh:23` · dimension: correctness · upheld 2/2
- Evidence: Line 20 `cd "$root"` runs before line 23 `out_dir="${1:-$target_dir/bundle}"`, so $1 is interpreted relative to the repo root. Reproduced in a scratch copy: from cwd `<dir>` running `bash repo/scripts/bundle-macos.sh mybundle` created `repo/mybundle/Turin.app`, and nothing appeared under `<dir>`. README says 'Pass a directory as the first argument to write the bundle somewhere else', which a user will read as cwd-relative.
- Fix: Capture the caller's cwd before the cd, e.g. `if [ -n "${1:-}" ]; then out_arg="$(cd "$1" 2>/dev/null && pwd || echo "$PWD/$1")"; fi` or simply resolve `$1` with `realpath`/`$PWD/$1` before line 20, and use it as out_dir.

### [low] PORTING.md Deviations row for the sidebar brand is stale after this branch
- `PORTING.md:322` · dimension: correctness · upheld 2/2
- Evidence: PORTING.md:322 says the brand slot 'renders the text "Turin" in the slot's styling' and lists upstream as 'T3 wordmark + muted "Code"'. This branch adds the T mark before the name (sidebar_chrome.rs:57-69) and sets the mark size to 8.71x10px, which is not recorded. Ground rule 4 requires every place not ported faithfully to be logged in the Deviations table.
- Fix: Update the row to say the slot renders the upstream T glyph (`brand/wordmark.svg`, the T portion of T3Wordmark, `currentColor`, fixed 8.71x10px) followed by 'Turin' in foreground, and note that the mark is fixed px rather than `h-[1cap]`, or make the mark rem-based (see the sidebar finding) and update the row to match.

### [low] Dock icon and bundle icon are duplicated binaries that can drift apart
- `crates/app/src/assets.rs:123` · dimension: correctness · upheld 2/2
- Evidence: assets.rs:123 `APP_ICON_PNG: &[u8] = include_bytes!("../assets/brand/app-icon.png")`, while scripts/bundle-macos.sh:25 builds Turin.icns from `assets/prod/black-macos-1024.png`. `cmp` shows the two PNGs are byte-identical today. The wordmark is duplicated the same way (`crates/app/assets/brand/wordmark.svg` vs `assets/prod/wordmark.svg`, also identical). Nothing ties the copies together, so updating one icon source leaves the Dock icon and the bundle icon out of sync.
- Fix: Point include_bytes! at the single source of truth (e.g. `../../../assets/prod/black-macos-1024.png` relative to crates/app/src) and drop the duplicate under crates/app/assets/brand, or document the copy and add a check that they match.
