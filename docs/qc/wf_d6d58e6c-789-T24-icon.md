# QC wf_d6d58e6c-789 — T24 app icon

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 7 · refuted: 1

## Lead report

1. MEDIUM, assets/README.md:75-81. The documented actool command uses `--app-icon AppIcon`, but the project is `app-icon.icon`. actool exits 0 and writes nothing: Res/ stays empty and partial.plist is `<dict/>`. Lines 80-81 then say it writes `AppIcon.icns` and to set CFBundleIconName/CFBundleIconFile to `AppIcon`. A bundle built from these steps ships with no icon and no error.
   Fix:
   - Change the flag to `--app-icon app-icon`.
   - Say the command writes `Assets.car` + `app-icon.icns`.
   - Set CFBundleIconName/CFBundleIconFile to `app-icon`, or merge in the generated partial plist. Renaming the project to AppIcon.icon also works.
   - Add a check that the output dir is non-empty, because actool exits 0 on this failure.
   - Using an absolute path (`"$PWD/assets/prod/app-icon.icon"`) is optional. One verifier saw the relative path work from the repo root.

2. LOW (needs user decision), assets/prod/app-icon.icon/Assets/text.svg:2. The T is upstream's T path enlarged 1.25x (x' = 1.25x + 14.086375, y' = 1.25y - 16.25). Cap height goes from 56 to 70 units and stem width from 12.96 to 16.2. compare-t3-vs-turin.png shows it visibly taller and heavier than upstream's T. README.md:24-26 justifies this on taste. That conflicts with the user's literal requirement: "exactly like the t3 code icon except with a T instead of a T3".
   Fix: either get explicit user sign-off on the 1.25x, or use upstream's T at 1.0, only translated to centre it (e.g. `matrix(1 0 0 1 15.11 0)`), and regenerate all exports. Either way, record the decision in the PORTING.md Deviations table or the README.

3. LOW, raster colour format. This merges two findings.
   - assets/export-icons.py:75-85 (`ictool_png`) and :95 (ICO dir entry): ictool 27.0 output is written unchanged, as 16-bit Display P3. This affects black-ios-1024, black-universal-1024, apple-touch-180, favicon-32x32 and the ICO entries for 24-256. The 16px entry is 8-bit sRGB. The ICONDIRENTRY bpp is hard-coded to 32 over these 64bpp PNG payloads. Upstream's files are all 8-bit sRGB, and its ICO is 82 KB versus Turin's 154 KB.
   - assets/export-icons.py:289: the synthesized black-macos-1024.png is saved without a profile (`profile: <nil>`), though upstream's is sRGB-tagged. That untagged image also feeds turin-black.icns.

   Fix:
   - Normalise every rendition to 8-bit RGBA sRGB before writing or packing the ICO: do an ICC P3-to-sRGB transform, quantise to 8 bits, and embed an sRGB chunk.
   - For the macOS PNG, pass `icc_profile` (or an sRGB chunk) from the upstream source into `mac.save`.
   - Regenerate all PNG, ICO and ICNS outputs and the compare sheet.

4. LOW, assets/README.md:66-68 together with assets/export-icons.py:264-294. The documented manual native-macOS path doesn't work. `main()` takes no arguments, so there is no icns-only step. It always overwrites prod/black-macos-1024.png (line 290) before building turin-black.icns (line 292) and the compare sheet (line 294). Following "whole script, then restore the PNG" leaves the icns built from the derived PNG. A committed native PNG would also break the README:31-32 claim that `git status` detects stale exports.
   Fix: add a flag such as `--keep-macos-png` that skips the derivation and builds the icns and compare sheet from the existing PNG. Upstream never overwrites its macOS PNG. Update README:66-68 to describe the flag.

5. LOW, assets/export-icons.py:4 and README:30-32. The README says output is deterministic, but that depends on unpinned tools:
   - `dependencies = ["numpy", "pillow", "scipy"]` has no versions, no lockfile and no exclude-newer. Pillow's zlib and FreeType change the bytes of the macOS PNG and the compare sheet.
   - The compare sheet also uses the system Helvetica.ttc.
   - The ictool build (27.0, bundle 129) isn't recorded.

   Fix: pin the dependencies (or add `uv lock --script` or `exclude-newer`). Record in the README the ictool and Pillow versions that produced the committed exports. Qualify the byte-identity claim as "on the recorded toolchain".

6. LOW, assets/export-icons.py:64-72 (`find_ictool`). It checks only two hard-coded /Applications paths and never probes the version. It ignores ICON_COMPOSER_TOOL, DEVELOPER_DIR, `xcode-select -p`, ~/Applications and other Xcode*.app installs. README:30 says "Xcode 26+ (or Icon Composer 2+)", but nothing enforces it. Upstream does all of this in scripts/export-brand-icons.ts:410-470. As a result, Xcode at a non-default path makes the script exit, and an Icon Composer 1.x install fails with a raw ictool error.
   Fix:
   - Look in ICON_COMPOSER_TOOL, DEVELOPER_DIR, `xcode-select -p` and ~/Applications first, then other Xcode*.app installs.
   - Run `ictool --version` and require short-bundle-version >= 2, with a clear error message.

Verdict: PASS (no critical/high). Item 2 still needs explicit user sign-off before merge.

## Confirmed findings (raw)

### [low] Raster exports are 16-bit/channel Display P3, but upstream's are 8-bit sRGB. ICO entries mix bit depths while the directory claims 32bpp
- `assets/export-icons.py:82` · dimension: visual-fidelity · upheld 2/2
- Evidence: sips output. Turin black-ios-1024.png, black-universal-1024.png, turin-black-web-apple-touch-180.png and turin-black-web-favicon-32x32.png are 'bitsPerSample: 16 profile: Display P3'. Turin's 16x16 is 8-bit sRGB. All of upstream's counterparts (black-ios-1024.png, t3-black-web-apple-touch-180.png, t3-black-web-favicon-32x32.png and the rest) are 'bitsPerSample: 8 profile: sRGB IEC61966-2.1'. Parsing turin-black-windows.ico and turin-black-web-favicon.ico shows the 16px entry is 8-bit with an sRGB chunk. The 24, 32, 48, 64, 128 and 256 entries are 'bitdepth 16 ctype 6' with iCCP and cICP chunks, yet every ICONDIRENTRY written at export-icons.py:95 declares bpp=32. All seven entries in upstream's t3-black-windows.ico are 8-bit with an sRGB chunk. The ICOs are 153839 bytes against upstream's 82338. Cause: the local ictool is Icon Composer 27.0 (bundle 129). Re-rendering upstream's own .icon with it also gives 16-bit P3, and its pixels differ from upstream's committed PNGs by only 0.21 levels on average. So the drift is the toolchain, not the design, but the written files no longer match upstream's format. A 64bpp PNG payload under a 32bpp ICO header is also a known compatibility risk for Windows shell icon loaders.
- Fix: In ictool_png (or right after it), normalise every rendition to 8-bit RGBA sRGB before writing or packing into the ICO: convert P3 to sRGB with an ICC transform, quantise to 8 bits per channel, and embed the sRGB chunk. Then regenerate all PNG and ICO outputs. Alternatively, record the format change in the PORTING.md Deviations table.

### [low] The lone T is enlarged 1.25x, so it is 25% taller and heavier than upstream's T. Literal reading of 'exactly like the T3 icon with a T' not met, and nothing logged
- `assets/prod/app-icon.icon/Assets/text.svg:2` · dimension: visual-fidelity · upheld 2/2
- Evidence: Checked numerically. Turin path 'M55.9 100V43.2H33.5V30H94.5V43.2H72.1V100H55.9Z' equals upstream's T subpath 'M33.4509 93V47.56H15.5309V37H64.3309V47.56H46.4109V93H33.4509Z' under x' = 1.25x + 14.086375, y' = 1.25y - 16.25. All 7 vertices match exactly (for example 33.4509*1.25 + 14.086375 = 55.9). It is centred horizontally on x=64, with vertical centre at 65 (upstream mark centre: x 62.7, y 65.5). Because of the scale, cap height grows from 56 to 70 units and stem width from 12.96 to 16.2 units. At 1024 that is 138px instead of 110px, heavier than any stroke in upstream's mark, which the compare-t3-vs-turin.png sheet shows. README.md:24-25 calls 1.25 an aesthetic choice ('at 1.0 the lone T looks lost'). The user asked for 'exactly like the t3 code icon except with a T instead of a T3', and PORTING.md has no Deviations row for the scale change, the synthesized macOS PNG, or the dropped dev/nightly variants (rule 4: record any place you could not port faithfully).
- Fix: Get explicit user sign-off on the 1.25x enlargement, or use upstream's T at 1.0, translated only (for example, matrix(1 0 0 1 15.11 0) centres it at x=64), and regenerate. Either way, add Deviations rows for the icon scale and the synthesized black-macos-1024.png.

### [low] The synthesized black-macos-1024.png has no colour profile, but upstream's is sRGB-tagged
- `assets/export-icons.py:289` · dimension: visual-fidelity · upheld 2/2
- Evidence: sips -g profile gives 'profile: <nil>' for assets/prod/black-macos-1024.png and 'profile: sRGB IEC61966-2.1' for upstream/assets/prod/black-macos-1024.png. The pixels come from upstream's sRGB-tagged PNG, but `mac.save(buf, "PNG", optimize=True)` drops the sRGB/iCCP chunk because PIL does not carry it over unless it is passed explicitly. Its format therefore differs from both its upstream counterpart and the other Turin exports. It also feeds turin-black.icns.
- Fix: Pass the source's profile through, for example mac.save(buf, 'PNG', optimize=True, icc_profile=Image.open(upstream_png).info.get('icc_profile')), or write an sRGB chunk. Then regenerate the macOS PNG and the icns.

### [medium] README actool command exits 0 but produces nothing: --app-icon must be the .icon basename (app-icon), and the relative path fails
- `assets/README.md:75` · dimension: reproducibility · upheld 2/2
- Evidence: I ran the documented command verbatim from the repo root (DEVELOPER_DIR=Xcode, actool 27.0) into /tmp. The exit code was 0, the Res/ dir was empty and partial.plist was `<dict/>`. Running it with an absolute path and `--app-icon AppIcon` gave the same empty result. Running it with an absolute path and `--app-icon app-icon` wrote Res/Assets.car and Res/app-icon.icns, and the partial plist had CFBundleIconFile => "app-icon" and CFBundleIconName => "app-icon". Running it with the relative `assets/prod/app-icon.icon` and `--app-icon app-icon` failed with "Failed to create a catalog from the Icon Studio document: /private/tmp/t24/assets/prod/app-icon.icon" (the relative path is resolved against a different cwd). So README lines 80-81 ("This writes `Assets.car` and `AppIcon.icns`. Set `CFBundleIconName = AppIcon` and `CFBundleIconFile = AppIcon`") are wrong too: a bundle built from these instructions would silently ship with no icon.
- Fix: Change the command to use an absolute path (e.g. "$PWD/assets/prod/app-icon.icon") and `--app-icon app-icon`. Say that it writes `Assets.car` + `app-icon.icns`, and set CFBundleIconName/CFBundleIconFile to `app-icon`, or merge in the generated partial plist. Another option is to rename the project to AppIcon.icon. Add a check that fails when the output dir is empty, because actool exits 0 in that case.

### [low] The manual native macOS export steps don't work: the script has no icns-only step and always overwrites the PNG before building the icns
- `assets/README.md:66` · dimension: reproducibility · upheld 2/2
- Evidence: README:66-68 says to export natively to prod/black-macos-1024.png "then re-run only the icns step (or the whole script, then restore the PNG)". export-icons.py:264-292 `main()` takes no arguments, so there is no icns-only mode. It also always runs `(PROD / OUTPUTS["macos"]).write_bytes(...)` (line 290) and only then `build_icns(PROD / OUTPUTS["macos"], ...)` (line 292). Following "whole script, then restore the PNG" therefore leaves turin-black.icns (and compare-t3-vs-turin.png, line 294) built from the derived PNG, not the native one. It also conflicts with README:31-32 ("`git status assets/` after a run shows whether the committed exports are stale"): once a native PNG is committed, every later run overwrites it and shows it as modified.
- Fix: Add a flag (e.g. `--keep-macos-png`, or skip the derivation when the PNG is newer/native) that leaves black-macos-1024.png untouched and builds the icns and compare sheet from it. Upstream's script already leaves the macOS PNGs unchanged. Then update README:66-68 to describe that flag.

### [low] The determinism claim depends on unpinned Python deps and an unrecorded ictool version
- `assets/export-icons.py:4` · dimension: reproducibility · upheld 2/2
- Evidence: The PEP 723 header lists `dependencies = ["numpy", "pillow", "scipy"]` with no versions, no `uv lock --script` lockfile and no `exclude-newer`. black-macos-1024.png is encoded by Pillow (`mac.save(buf, "PNG", optimize=True)`, line 289) using Pillow's bundled zlib (zlib-ng 1.3.1 in the resolved wheel), and compare-t3-vs-turin.png uses Pillow's FreeType (2.13.3) for its text. Both bytes change when the Pillow wheel's zlib or FreeType changes. The ictool renders also depend on the Icon Composer build (this run: short-bundle-version 27.0, bundle 129), which the README doesn't record (README:30-32 says only "Xcode 26+" and "Its output is deterministic"). Today's regeneration in a temp copy was byte-identical for all 10 outputs, but that only holds on this exact toolchain.
- Fix: Pin versions in the script header (or commit a `uv lock --script` lockfile / add `exclude-newer`). Record the ictool version that produced the committed exports in the README, and say that byte-identity is only expected with that toolchain.

### [low] ictool discovery skips the version check and DEVELOPER_DIR/xcode-select that upstream uses
- `assets/export-icons.py:64` · dimension: reproducibility · upheld 2/2
- Evidence: `find_ictool` returns the first existing path out of two hard-coded paths, without probing `--version`. Upstream's scripts/export-brand-icons.ts:412-470 honours ICON_COMPOSER_TOOL, DEVELOPER_DIR, the xcode-select directory and ~/Applications, and rejects tools older than Icon Composer 2 ("version 2 or newer is required for design generation"). README:30-31 claims a requirement of "Xcode 26+ (or Icon Composer 2+)", but the script never enforces it. A standalone Icon Composer 1.x at /Applications/Icon Composer.app would be used, and Xcode at a non-default path (e.g. Xcode-beta.app selected via xcode-select) would make the script exit.
- Fix: Probe `ictool --version`, require short-bundle-version >= 2 (or the equivalent check), and look in DEVELOPER_DIR / `xcode-select -p` / ICON_COMPOSER_TOOL first, like upstream does.
