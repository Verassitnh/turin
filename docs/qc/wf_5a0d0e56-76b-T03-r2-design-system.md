# QC wf_5a0d0e56-76b — T03 fix round re-QC

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 6 · refuted: 0

## Lead report

Fix list (all low severity; nothing critical or high)

1. LOW, crates/app/src/components/chat/composer_surface.rs:53-56 (the chevron). Upstream ComposerControl.tsx:89 strokes ComposerControlChevron at 2.25. The port uses the stock stroke-2 `IconName::ChevronDown`, and `Icon` has no stroke option. PORTING.md has no Deviations row for it.
   Fix: add `assets/brand/chevron-down.svg` with `stroke-width="2.25"` (same approach as send-arrow.svg) and use it here. If you don't do that, add a Deviations row for composer_surface.rs.

2. LOW, crates/app/src/components/right_panel_tabs.rs:55-59 (the hover colour). The surface icon has a fixed colour, so on an inactive tab's hover it stays `muted_foreground` while the label turns foreground. Upstream swaps the slot to a foreground X on `group-hover/tab` (panel-tab-close-button.tsx), and that behaviour is not logged.
   Fix: use `Icon::group_hover_color`, or an explicit tab group, so the glyph turns foreground on hover. Alternatively add a Deviations row saying the X swap is omitted because the close-surface action is cut. Don't add a dead X control.

3. LOW, crates/app/src/components/chat/chat_header.rs:41-43, with crates/app/src/components/workspace_breadcrumb.rs:82-83 (the project crumb). Upstream ChatHeader.tsx:318 passes `className="shrink"` for the project crumb, which overrides the item's base `shrink-0`. The port's `WorkspaceBreadcrumbItem::new(false)` always applies `flex_shrink_0()` and has no override. As a result:
   - The project crumb never shrinks.
   - The title is squeezed to about 24px, where upstream keeps a 40px minimum and ellipsizes the project name instead.
   - The port's current item also lacks the upstream title's `min-w-10 flex-1`.
   Fix: add a shrink option or style refinement to `WorkspaceBreadcrumbItem`. Set `flex_shrink(1.)` on the ChatHeader project crumb. Give the current item `min_w(rems(2.5))` and `flex_1`.

4. LOW, crates/app/src/theme/theme_palette.rs:233-400 (the test module). Two pure-logic upstream themePalette.test.ts cases are not ported or logged: "keeps the T3 Chat palette faithful and readable" (line 481) and "includes the dual-mode maintainer themes" (line 525). PORTING rule 5 requires porting pure-logic tests. A spot check found the palette data faithful, so this is a missing regression guard rather than a current bug.
   Fix: port both tests. Include the T3 Chat light and dark hex pins, the 4.5 and 7 contrast floors, the muted-contrast ranges (4.705 and 5.082), and the `sidebar_artwork` checks. Alternatively add a Deviations note.

5. LOW, crates/app/src/theme/appearance_contrast.rs:28-29 (`AppearanceContrast::new`). The u32 input with `.clamp(50, 200)` is unlogged and has no doc comment. The only PORTING.md row that touches contrast is the generic app/theme/mod.rs one at line 210.
   Correction to the original finding text: upstream has no Math.round or NaN fallback here. Those belong to `clampFontSize`, which is ported faithfully.
   The real deviation is that upstream's `AppearanceContrast` is `Schema.Int` with a 50..=200 range, validated at decode (contracts/settings.ts:107-112). `applyAppearanceContrast` only does min/max on the result. Turin takes u32 and clamps silently. Schema-valid inputs behave identically.
   Fix: add a Deviations row for appearance_contrast.rs, or a doc comment on `new`, saying this. Note that settings decode must reject out-of-range values before calling it.

6. LOW, optional (split verification vote, 1 of 2 upheld), crates/app/src/components/chat/composer_surface.rs:170 (the send tooltip). `Tooltip::new("Send")` differs from upstream's idle `submitLabel`/`submitTooltip`, which is "Submit message" (ComposerPrimaryActions.tsx:279-307, 358-361).
   The refuting verdict argues that this button is a logged placeholder frame, that the send button was not in the prior QC item 6 table, and that upstream's label is state-dependent. These points are fair, but the one-line fix is cheap.
   Fix: change it to `Tooltip::new("Submit message")`, or leave it for the composer task (T17) and add a note in the composer_surface.rs Deviations row.

Verdict: PASS (no critical or high findings; 5 low-severity fixes plus 1 optional low).

## Confirmed findings (raw)

### [low] Item 26: contrast clamp signature deviation still unlogged
- `crates/app/src/theme/appearance_contrast.rs:` · dimension: fix-verification · upheld 2/2
- Evidence: AppearanceContrast::new(contrast: u32) does contrast.clamp(MIN, MAX) as f32. The prior QC fix asked to "Log the u32 clamp signature, which drops upstream's rounding and NaN fallback". `git diff 7eb8468 -- PORTING.md` adds no row for it, and grep for contrast/u32/NaN in PORTING.md finds only the generic `app/theme/mod.rs` row. There is no code comment either.
- Fix: Add a Deviations row for appearance_contrast.rs (u32 input, no Math.round / non-finite fallback), or a doc comment on `new`.

### [low] Item 26: two pure-logic upstream themePalette tests not ported or logged
- `crates/app/src/theme/theme_palette.rs:` · dimension: fix-verification · upheld 2/2
- Evidence: Upstream themePalette.test.ts has "keeps the T3 Chat palette faithful and readable" (line 481) and "includes the dual-mode maintainer themes" (line 525). Both use only built-in data, getThemeDefinition, getThemeModes, getThemeColorsForMode and themeAllowsSidebarArtwork, and all of those are ported. The test module (grep for fn names; 47 tests pass) has no equivalent (grep for faithful / maintainer / #fdf7fd / 5.082 / 4.705 finds only the alias comment). The PORTING.md row for theme_palette.rs does not say these were skipped. PORTING rule 5 says to port pure-logic tests, and the prior QC said to port the applicable cases one-to-one.
- Fix: Port the two tests (the hex pins for T3 Chat light and dark, the 4.5 / 7 contrast floors, the muted-contrast ranges 4.705 / 5.082, and sidebar_artwork), or add a Deviations note.

### [low] Send button tooltip reads 'Send'; upstream's idle label is 'Submit message'
- `crates/app/src/components/chat/composer_surface.rs:170` · dimension: look-and-feel · upheld 1/2
- Evidence: Port: `.tooltip(|window, cx| Tooltip::new("Send").build(window, cx))`. Upstream ComposerPrimaryActions.tsx:279-289 sets `submitLabel = ... : "Submit message"` for an idle thread. :301-307 sets `submitTooltip = submitStatus ?? (... : submitLabel)`. :358-361 renders `<TooltipPopup>{submitTooltip}</TooltipPopup>` around the send button, inside a span trigger, so it also shows while disabled. Item 6 of this round asked for tooltip strings copied verbatim from upstream, and this round edited this same send button (item 13).
- Fix: Use Tooltip::new("Submit message") until the composer task wires the full submitLabel/submitStatus logic.

### [low] Composer chevron uses stroke 2 instead of upstream's 2.25, and this is not logged in Deviations
- `crates/app/src/components/chat/composer_surface.rs:52` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ComposerControl.tsx ComposerControlChevron: `<ChevronDownIcon ... className="size-3.5 text-icon-muted" strokeWidth={2.25} />`. The port's comment says "Upstream strokes it at 2.25; the lucide asset is the stock 2" and it renders `Icon::new(IconName::ChevronDown)`, the stock stroke-2 lucide SVG. PORTING.md rule 4 requires anything that cannot be ported faithfully to be logged in the Deviations table, and the table has no row for this. The prior QC fix for item 12 had suggested adding a stroke-2.25 chevron asset.
- Fix: Add a chevron-down SVG with stroke-width="2.25" under assets/brand (like send-arrow.svg) and use it here, or add a Deviations row for composer_surface.rs.

### [low] Header project crumb never shrinks; upstream overrides shrink-0 with 'shrink'
- `crates/app/src/components/chat/chat_header.rs:43` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ChatHeader.tsx:318 has `<WorkspaceBreadcrumbItem className="shrink">` for the project crumb. tailwind-merge resolves the item's base `shrink-0` to `shrink`, so the project crumb and the title both shrink when space is tight. The port uses `WorkspaceBreadcrumbItem::new(false)`, which always applies `.flex_shrink_0()` (workspace_breadcrumb.rs:82), and has no way to override it. In dark-right-panel.png only the title truncates ("Port design system and app..."). Upstream also gives up width from the project crumb (min-w-0 truncating text), so the project name ellipsizes first in narrow layouts.
- Fix: Let WorkspaceBreadcrumbItem accept a style refinement (impl Styled) or a `shrink(bool)` option, and set flex_shrink(1.) on the ChatHeader project crumb.

### [low] Hovered right-panel tab keeps its muted surface icon; upstream swaps it for a foreground X
- `crates/app/src/components/right_panel_tabs.rs:55` · dimension: look-and-feel · upheld 2/2
- Evidence: Port: the icon is `Icon::new(icon).size(rems(0.75)).color(if active { theme.foreground } else { theme.muted_foreground })`. The colour is fixed, and the tab's hover only changes the bg and text colour. Upstream ui/panel-tab-close-button.tsx: the surface icon span is `group-hover/tab:hidden` and `<X className="hidden size-3 group-hover/tab:block" />` appears instead. Both use currentColor, so on an inactive tab's hover (`hover:text-foreground`, RightPanelTabs.tsx:1222) the 16px slot shows a foreground X. The port shows the FileDiff/Link2 glyph still in muted_foreground next to a label that has turned foreground.
- Fix: In surface_tab, give the 16px slot a group and swap the surface icon for IconName::X (size 0.75rem, currentColor) on group hover. At minimum, recolour the icon to foreground on hover, matching upstream's currentColor inheritance.
