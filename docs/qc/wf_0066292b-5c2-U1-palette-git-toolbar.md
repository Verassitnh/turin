# QC wf_0066292b-5c2 — U1 command palette, git actions, branch toolbar

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 8 · refuted: 1

## Lead report

1. [medium] crates/app/src/components/command_palette.rs:1151 (move_highlight), with render at crates/app/src/components/command_palette/command_palette_results.rs:185. Problem: on open no row is highlighted (begin_session at command_palette.rs:351 sets highlighted=None), so the first ArrowDown selects row 1. Upstream auto-highlights row 1 on open (CommandPalette.tsx:3514, :3554-3556), so ArrowDown should move to row 2. Enter already uses effective_highlight() (command_palette.rs:1134/1299), so the two disagree. Fix: use effective_highlight() as the current position in move_highlight, and as the `active` test in render_item, when auto_highlights_first_row() is true and highlight_cleared is false. Keep the cleared-state rule: after a query change or push/pop, the first ArrowDown lands on row 1 (CommandPalette.tsx:3212-3223).

2. [medium] crates/app/src/components/branch_toolbar/composer_control.rs:144; also crates/app/src/components/branch_toolbar/env_selectors.rs:73 and crates/app/src/components/branch_toolbar.rs:503. Problem: `.h(rems(1.75))` (28px) applies at all sizes. Upstream ComposerControl.tsx:21 (xs) is `h-7 ... sm:h-6`, BranchToolbarEnvironmentSelector.tsx:63 and the BranchToolbar.tsx:270 locked-context span are `h-7 ... sm:h-6`. Turin's minimum window is 840px, so desktop always gets 24px. Fix: use rems(1.5) at desktop for the xs trigger, the environment indicator, and the locked-context row. Keep TEXT_XS. Sm variant stays at 1.75rem.

3. [low] crates/app/src/components/git_actions_control/commit_dialog.rs:501. Problem: `.w(px(560.))`. Upstream GitActionsControl.tsx:1940 is a bare DialogPopup, which is max-w-lg (512px). Fix: w(px(512.)) or rems(32.). Not logged in Deviations.

4. [low] crates/app/src/components/git_actions_control/publish_dialog.rs:768. Problem: `.w(px(560.))`. Upstream GitActionsControl.tsx:648 uses WizardPopup, max-w-xl (576px, w-full capped). Fix: rems(36.) (576px), or max_w(rems(36.)) with full width to match the cap. Not logged.

5. [low] crates/app/src/components/git_actions_control/default_branch_dialog.rs:120. Problem: `.w(px(560.))`. Upstream GitActionsControl.tsx:2121 is `className="max-w-xl"` (576px). Fix: rems(36.) (or max_w with full width). Not logged.

6. [low] crates/app/src/components/git_actions_control/publish_dialog.rs:262. Problem: the single provider card is a fixed `.w(rems(14.))` (224px). Upstream GitActionsControl.tsx:679 is a grid-cols-2, so each card is about half the content width (about 264px). The one-card cut is logged (PORTING.md:773), but the width change is not. Fix: give the card a half-width cell (w_1_2 of the content width) or equivalent, or log the deviation.

7. [low] crates/app/src/components/git_actions_control/view.rs:273. Problem: `.min_w(rems(12.))`. Upstream ui/menu.tsx:57 is `min-w-[min(10rem,calc(100vw-2rem))]`. The panel variant should also size to the anchor (GitActionsControl.tsx:1896-1902, `w-(--anchor-width)`), which Rust does not do. The earlier citation of 1880-1885 was wrong. Fix: min_w(rems(10.)), and size the panel to the split group's width. Not logged.

8. [low] crates/app/src/ui/command.rs:163-180 (CommandShortcut); also crates/app/src/ui/menu.rs:31-41 (MenuShortcut). Problem: upstream uses `tracking-widest` (ui/command.tsx:208, ui/menu.tsx:255). GPUI has no letter-spacing API, so the gap is platform-caused, but PORTING.md rule 4 requires a Deviations row and none exists. Fix: add a Deviations row for CommandShortcut/MenuShortcut tracking-widest. No code change needed.

Verdict: PASS (no critical/high; 2 medium, 6 low).

## Confirmed findings (raw)

### [medium] Palette shows no highlighted row on open, so the first ArrowDown selects row 1 instead of row 2
- `crates/app/src/components/command_palette.rs:1151` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust move_highlight matches on self.highlighted (None on open after begin_session at command_palette.rs:351) and treats None as 'no current row': `(None, _, d) if d > 0 => 0`. Rendering uses the same field: command_palette_results.rs:185 `let active = enabled && self.highlighted.as_deref() == Some(item.value.as_str());`, so no row is highlighted on open (docs/screenshots/U1/dark-palette-root.png shows no highlighted row). Upstream CommandPalette.tsx:3514 sets `autoHighlight={autoHighlightsFirstRow ? "always" : false}` and :3554-3556 stores the auto-highlighted first row into highlightedItemValue unless highlightClearedRef is set. On open upstream therefore highlights row 1, and ArrowDown (no pre-handler, since highlightClearedRef is false, CommandPalette.tsx:3212-3223) moves to row 2. Rust Enter already uses effective_highlight() (command_palette.rs:1134) but ArrowDown and row rendering do not.
- Fix: Use effective_highlight() as the current position in move_highlight and as the `active` test in render_item when auto_highlights_first_row() is true and highlight_cleared is false. Keep the cleared-state rule: after a query change or push/pop view, the first ArrowDown lands on row 1 (CommandPalette.tsx:3212-3223).

### [medium] Branch toolbar xs control and environment indicator are 28px tall on desktop; upstream is 24px at sm+
- `crates/app/src/components/branch_toolbar/composer_control.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: Rust composer_control.rs:144 `.h(rems(1.75))` applies to every size, including Xs (the branch/run-context triggers). Upstream components/chat/ComposerControl.tsx:21 for size xs is `h-7 gap-1 px-1.75 ... text-sm ... sm:h-6 sm:text-xs`, so at sm+ (desktop) the height is 1.5rem (24px). Same mismatch in the environment indicator: crates/app/src/components/branch_toolbar/env_selectors.rs:73 `.h(rems(1.75))` vs upstream components/BranchToolbarEnvironmentSelector.tsx:63 `h-7 ... sm:h-6`, and the locked-context span in BranchToolbar.tsx (`h-7 min-w-0 max-w-[48%] ... sm:h-6`).
- Fix: For the xs variant (and the environment indicator) use h(rems(1.5)) on desktop, i.e. 1.75rem only for the non-xs size. Keep the xs text at TEXT_XS, which already matches sm:text-xs.

### [low] Commit changes dialog is 560px wide; upstream DialogPopup default is max-w-lg (512px)
- `crates/app/src/components/git_actions_control/commit_dialog.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: commit_dialog.rs:501 `dialog.title(COMMIT_DIALOG_TITLE).w(px(560.))`. Upstream GitActionsControl.tsx:1940 renders `<DialogPopup>` with no className, and ui/dialog.tsx:81-88 sets `variant === "default" && "max-h-full max-w-lg"`, i.e. 32rem = 512px.
- Fix: Use w(px(512.)) (or rems(32.)) for the commit dialog, matching max-w-lg.

### [low] Publish repository dialog is 560px wide; upstream WizardPopup default is max-w-xl (576px)
- `crates/app/src/components/git_actions_control/publish_dialog.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: publish_dialog.rs:768 `.w(px(560.))`. Upstream GitActionsControl.tsx:648 uses `<WizardPopup>`, and ui/wizard.tsx:18 sets `size === "wide" ? "max-w-3xl" : "max-w-xl"`, so the default is 36rem = 576px.
- Fix: Use w(px(576.)) (rems(36.)) for the publish dialog.

### [low] Default-branch confirm dialog is 560px wide; upstream uses max-w-xl (576px)
- `crates/app/src/components/git_actions_control/default_branch_dialog.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: default_branch_dialog.rs:120 `dialog.title(title.clone()).w(px(560.))`. Upstream GitActionsControl.tsx:2118 `<DialogPopup className="max-w-xl">`, which is 576px.
- Fix: Use w(px(576.)) (rems(36.)) for this dialog.

### [low] Publish provider card is a fixed 14rem instead of half of a 2-column grid
- `crates/app/src/components/git_actions_control/publish_dialog.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: publish_dialog.rs:262 `.w(rems(14.))` on the single provider card (224px). Upstream GitActionsControl.tsx:679 lays the provider cards out as `className="grid grid-cols-2"`, so each card is half the content width (about 256px at max-w-xl with p-6), not a fixed 224px.
- Fix: Lay the cards out in a two-column grid (or w_1_2 of the content width) instead of a fixed 14rem width.

### [low] Git actions menu popup min-width is 12rem; upstream MenuPopup minimum is 10rem
- `crates/app/src/components/git_actions_control/view.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: view.rs:273 `.min_w(rems(12.))` on the git menu container. Upstream ui/menu.tsx:50-56 sets `min-w-[min(10rem,calc(100vw-2rem))]`, and GitActionsControl.tsx:1880-1885 sizes the header popup `w-full` and the panel popup `w-(--anchor-width)`, so the minimum should be 10rem and the width should follow the anchor, not a fixed 12rem.
- Fix: Use min_w(rems(10.)) and size the panel variant to the anchor width instead of a hard 12rem minimum.

### [low] Palette shortcut label drops tracking-widest with no Deviations entry
- `crates/app/src/ui/command.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ui/command.tsx CommandShortcut uses `ms-auto font-medium font-sans text-secondary-label text-xs tracking-widest`. The Rust CommandShortcut (command.rs:163-180) sets font, weight, size and color but no letter spacing. GPUI in the registry (gpui-base/gpui-pre 0.3.9 / 0.7.3) exposes no letter_spacing setter, so this is a platform gap, but PORTING.md's Deviations table has no row for it (same omission in ui/menu.rs MenuShortcut).
- Fix: Add a Deviations row for CommandShortcut/MenuShortcut tracking-widest (GPUI has no letter-spacing API), or emulate it. Nothing else in the palette chrome was found to differ: the dialog width (36rem), max height (26.25rem), input height (2.125rem at sm+), row min-height and padding, group heading, footer and kbd chip classes all match upstream.
