# QC wf_b85e3821-0f4 — N5s editor open-in (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 4 · refuted: 1

## Lead report

Fix list (all low severity, no critical or high):

1. LOW, crates/launcher/src/host.rs:98. Detached spawn uses only `.process_group(0)`, so the child stays in the server's session and keeps its controlling terminal. Upstream `detached: true` also starts a new session. Fix: either add a `pre_exec` `setsid` (this conflicts with `process_group(0)` and needs unsafe code), or add a Deviations note in PORTING.md (line ~220) that says "own process group, same session". Verifiers disagreed on whether this is a defect or only a documentation gap. No observable behaviour difference was found for GUI or `open`-launched editors.

2. LOW, crates/launcher/src/host.rs:151. PATH lookup applies only `entry.trim()`. Upstream shell.ts:108 and :630 apply `stripWrappingQuotes`, which removes wrapping `"` (shell.ts:366-368) and is not platform-gated. A PATH entry like `"/opt/x/bin"` is missed. Fix: strip wrapping double quotes after trim, or log the omission in the PORTING.md isCommandAvailable Deviations row (~line 225). Both verifiers upheld this.

3. LOW, crates/launcher/src/open_in_picker_shortcut.rs:25-41. `favorite_editor_shortcut` takes a caller-computed `matches_shortcut: bool` and has no `repeat` input. Upstream keybindings.ts:347-356 rejects `event.repeat === true`, and keybindings.test.ts:724-731 covers it. No repeat guard or keybinding matcher exists in the Rust tree, so this is latent. Fix: add a `repeat: bool` parameter that returns Ignore when true, add a test for a repeated keydown, and note the requirement in the Deviations row (~line 234). Both verifiers upheld this.

4. LOW, crates/launcher/src/external_launcher.rs:179-181 (launch_browser), :273 (file-manager reveal), :326-340 (editor args). User-controlled values are passed as bare argv elements with no leading-dash check and no `--`. Option injection is possible (example: `open -z` is parsed as an option), but there is no shell injection because the code uses `Command::new` with an args vector. Upstream externalLauncher.ts has the same shape, so this is an inherited faithful-port gap, not a regression. Verifiers split. One upheld it at low. The other refuted the browser half: `launch_browser` has no Rust callers, and upstream passes server-generated URLs only. Fix (optional hardening): reject values that start with `-`, or insert `--` where the target CLI honours it, and log the deviation in PORTING.md. Any per-editor change needs upstream verification first.

Verdict: PASS (no critical or high findings; four low items, two of them need only documentation or latent-gap fixes).

## Confirmed findings (raw)

### [low] Detached launch does not start a new session as upstream detached:true does
- `crates/launcher/src/host.rs:` · dimension: logic-fidelity · upheld 1/2
- Evidence: host.rs:98 `.process_group(0)` only. Upstream externalLauncher.ts DETACHED_IGNORE_STDIO_OPTIONS sets `detached: true`, which Node documents as making the child leader of a new process group AND session on POSIX. Rust child stays in the server's session and keeps its controlling terminal. The PORTING.md Deviations row says 'own process group' and does not mention the session difference.
- Fix: Call setsid in a pre_exec hook (unsafe libc::setsid) for spawn_detached, or log the session difference in the Deviations table.

### [low] PATH lookup skips upstream stripWrappingQuotes on PATH entries
- `crates/launcher/src/host.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: host.rs:151 `let entry = entry.trim();`. Upstream shell.ts:108 and :630 use `stripWrappingQuotes(pathEntry.trim())`, which removes leading and trailing double quotes (shell.ts:366-368). A PATH entry written as "/opt/x/bin" is searched literally by Rust and never matches. The Deviations row for isCommandAvailable does not list this.
- Fix: Strip wrapping double quotes after trim (value.trim().trim_matches('"')-style, matching /^"+|"+$/) before joining, or add it to the Deviations table.

### [low] Favorite-editor shortcut loses upstream's event.repeat guard
- `crates/launcher/src/open_in_picker_shortcut.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: open_in_picker_shortcut.rs:25 `favorite_editor_shortcut(enabled, matches_shortcut, ...)` takes a caller-computed bool. Upstream OpenInPickerShortcut.ts uses isOpenFavoriteEditorShortcut (upstream/apps/web/src/keybindings.ts:347-356), which returns false when `event.repeat === true`. No repeat check or keybinding matcher exists in any crate (grep finds only the 'editor.openFavorite' id at crates/contracts/src/keybindings.rs:122). The Deviations row says key matching lives in keybindings, but that code is not there. When the UI wires this, holding the key would relaunch the editor on every auto-repeat.
- Fix: Carry the repeat flag into the decision (ignore when repeat is true) or port isOpenFavoriteEditorShortcut including the repeat guard, and add a test for a repeated keydown.

### [low] User-controlled cwd/target reaches spawned CLIs as an unguarded argv element (leading '-' parsed as option, no '--', no absolute-path check)
- `crates/launcher/src/external_launcher.rs:` · dimension: safety · upheld 1/2
- Evidence: launch_browser (line 179-181): `let args = vec![target.to_owned()]; self.host.spawn_detached("open", &args)` with no check on target. resolve_editor_launch file-manager path (line 273): `args: vec![cwd.to_owned()]`. resolve_command_editor_args DirectPath (line 326): `EditorLaunchStyle::DirectPath => vec![target.to_owned()]`; Goto/LineColumn (lines 330-340) push target or parsed path as a bare positional after options. LaunchEditorInput.cwd is only TrimmedNonEmptyString (crates/contracts/src/editor.rs:244), so a value such as `--install-extension=/x.vsix` or `-n` is emitted as its own argv element with nothing rejecting it or inserting `--`. Not shell injection (Command::new + args, no shell), and upstream externalLauncher.ts:172/416/435 has the same shape, so this is a port-faithfulness gap rather than a regression, but the brief asks for argv safety and the launcher spawns user-controlled values.
- Fix: Before building argv, reject (or fix up) any cwd/target that does not start with '/' for paths, or that starts with '-'. Where the CLI honours it, insert '--' before the positional path (for example `open -- <path>`, `cursor --classic -- <path>`) after verifying each binary accepts it. Record the deviation in PORTING.md if upstream behaviour is kept for some editors.
