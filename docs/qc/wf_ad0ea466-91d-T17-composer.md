# QC wf_ad0ea466-91d — T17 composer (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 11 · refuted: 1

## Lead report

1. MEDIUM. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/chat_composer.rs:346. Plain (rich-text off) mode sets `literal_text: !rich_text`, which drops `@file` and `$skill` chips (rich_doc.rs:558-567). Fix: pass `literal_text` only for pending-answer editors (upstream ChatComposer.tsx:7594), and map the setting to styling only (`styling: rich_text`), as upstream ComposerPromptEditorTiptap.tsx:873-874 does. Keep chip atoms in plain mode. Add a plain-mode chip test and log any remaining difference in PORTING.md Deviations.

2. MEDIUM. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/chat_composer.rs:506. Alt+ArrowUp (edit latest queued run, caret at start) and Mod+Shift+Enter (steer next) are not wired. Upstream defaults are in shared/keybindings.ts:153-154 and handled in ChatView.tsx:8326-8345. The only queued-run paths are banner buttons (banners.rs:394, 407). Also `command: None` at chat_composer.rs:506 means keybinding-resolved composer commands never fire. Fix: add both keys with the same gating (caret at start for edit), or log them as a Deviation. Severity is contested between medium and low.

3. MEDIUM. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/prompt_editor.rs:1152, 1159-1160, 1178-1182. The code-block language trigger uses gap 6px, mono font, 11px text, and a 45% foreground chevron. Upstream (ComposerCodeBlockLanguagePicker.tsx, ui/button.tsx:39 xs) uses sans, 12px (text-xs), gap-1 (4px), font-medium, text-muted-foreground label, and an opacity-60 chevron. Fix: drop font_family(mono), use TEXT_XS, gap 4px, font weight 500, muted-foreground label color (hover foreground), and a muted chevron at 0.6 opacity. Keep h 1.5rem and the header padding.

4. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/list_continuation.rs:42, and /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/rich_doc.rs:373. The `\d` in both ordered-marker regexes is Unicode-aware in Rust, but ASCII-only in JS upstream. `٣. foo` continues as a list in Turin but not upstream. The rich_doc.rs site is the default path, so it matters more. Fix: `[0-9]` or `(?-u:\d)` in both.

5. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/chat_composer.rs:549 (and key.alt on the next line, plain_list_key around 536-552). Shift+Enter and Alt+Enter in plain mode skip list continuation. Upstream ChatComposer.tsx:4587-4594 has no shift/alt guard, and the documented behaviour is "Shift+Enter on a list item continues it". Fix: only bail out for ctrl/meta when a submission intent was produced, and let shift/alt reach list continuation. Add a test.

6. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/submission.rs:3-6 and :40. The length check ignores assistant-citation expansion (upstream composerSubmission.ts:14-17 uses Math.max of the two lengths). The comment says citations are "not ported", but timeline/assistant_citations.rs:323 already has `expand_assistant_citations_for_provider`. No Deviations row exists. Fix: either call that function in the length check, or add a Deviations row. Also fix the stale comment.

7. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/list_continuation.rs:85. The guard `number < (1u64 << 53) - 1` excludes 2^53-1. Upstream uses Number.isSafeInteger, which includes it. Fix: `number <= (1u64 << 53) - 1`.

8. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/menu.rs:580 and :584. Command rows are 32px (`min_h(rems(2.))`, `py_1`). Upstream desktop rows are 28px (autocomplete.tsx:128 `sm:min-h-7`, command.tsx:191 `py-1.5`). Fix: `min_h(rems(1.75))` and `py(rems(0.375))`, and also set `.line_height(TEXT_XS.line_height)` on the label and description. Without the line-height change the row will not reach 28px, because GPUI's default line height is 1.618x. Upstream CommandList also uses `p-2` (command.tsx:121) where Rust uses `p_1` (menu.rs:514-520).

9. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/menu.rs:~559-566 and :~618-622. The skill row is one string "/skill:{name}" in the foreground color. Upstream ComposerCommandMenu.tsx:211-217 dims the "/skill:" prefix with text-secondary-label. Fix: split into two spans, dimmed prefix and foreground name, inside the same label div.

10. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/prompt_editor.rs:1118 and :1267. Fenced code size is `size_prompt * 0.92` (12.88px). Upstream uses the code preference (index.css:1713-1714, DEFAULT_CODE_FONT_SIZE 13), and the 0.92 factor applies only to inline code. Fix: `px(theme.fonts.size_code)` at both sites. Chat markdown already does this (chat_markdown.rs:1291).

11. LOW. /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/chat_composer.rs:805-815. The dark frame has no inset top highlight. Upstream ComposerSurface.tsx:40 uses `dark:after:inset-shadow-2xs` with a 3% white highlight. Fix: in the dark branch add an inset BoxShadow with offset (0, 1px), blur 0, spread 0, white at 0.03 alpha, following the `shadow_dialog` pattern in theme/tokens.rs:232. The geometry in the original finding (offset 0, blur 1px) is wrong.

Deduplication: finding 4 has a second site (rich_doc.rs:373) that is merged into item 4. No other overlaps.

PASS (no critical or high findings; 3 medium, 8 low).

## Confirmed findings (raw)

### [medium] Plain (rich-text off) composer mode turns off mention and skill chips
- `crates/app/src/composer/views/chat_composer.rs:346` · dimension: logic-fidelity · upheld 2/2
- Evidence: chat_composer.rs:346 sets `literal_text: !rich_text`, so the plain setting maps to the verbatim path. rich_doc.rs:558-567 (`if options.literal_text`) then builds one plain paragraph per line with no inline atoms, so `@file` and `$skill` tokens are never chips. Upstream separates the two flags: ComposerPromptEditorTiptap.tsx:874 `const richText = !literalText && (richTextEnabled ?? false);` and the comment on line 873 says 'The setting toggles styling, not the engine'. Upstream sets `literalText` only for pending answers (ChatComposer.tsx:7594 `literalText={isLiteralPendingAnswer}`). Plain mode there only disables marks (`styling: richText`), so chips still build. Effect: with composerRichTextEnabled off, chips become raw text that is not atomic on delete and does not render as chips. The deviation is not logged in PORTING.md Deviations.
- Fix: Map the setting to `rich_text` only (styling off) and reserve `literal_text` for pending-answer editors, as upstream does. Keep chip atoms in plain mode. Add a test that plain mode keeps `@path` chips. Log any remaining plain-mode difference in Deviations.

### [low] Shift+Enter and Alt+Enter in plain mode skip list continuation
- `crates/app/src/composer/views/chat_composer.rs:549` · dimension: logic-fidelity · upheld 2/2
- Evidence: plain_list_key (chat_composer.rs:536) returns early on `|| key.shift` (line 549) and on alt, meta and ctrl. Upstream composer-list-continuation.ts:8-9 documents 'Shift+Enter on a list item continues it'. ChatComposer.tsx:4587-4594 runs `key === "Enter" ? listContinuationForEnter(...)` with no shiftKey check. composerSubmissionIntentForKey returns null for shift and alt (composer-logic.ts `event.shiftKey || event.altKey`), so those events reach the list branch. Effect: in plain mode, Shift+Enter on `- foo` inserts a bare newline instead of `- `, and Enter on an empty item does not exit the list.
- Fix: Only bail out for ctrl/meta when a submission intent was produced. Let Enter with shift or alt reach listContinuationForEnter, matching ChatComposer.tsx:4587. Add a plain-mode test for Shift+Enter.

### [medium] Queued-run keyboard shortcuts (Alt+ArrowUp edit, Mod+Shift+Enter steer) are not wired
- `crates/app/src/composer/views/chat_composer.rs:506` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream defaults: shared/keybindings.ts:153 `{ key: "mod+shift+enter", command: "thread.steerQueuedMessage" }` and :154 `{ key: "alt+arrowup", command: "thread.editQueuedMessage", when: "composerFocus" }`. Upstream handles both in ChatView.tsx:8334-8345 (editLatest / steerNext, guarded by isCaretAtStart). In Turin, the only queued-run edit path is the banner button (banners.rs:394 `source.edit_queued_run`). No Rust code calls a steer-next or edit-latest action, and the only KeyBinding registrations are in components/app_sidebar_layout.rs:56-57. Also, chat_composer.rs:506 hardcodes `command: None` for composer_submission_intent_for_key, so keybinding-resolved commands (composer.sendAlternate/sendBackground/sendAndNewThread) never fire. The keybindings table is not consulted by the composer, and no deviation is logged for it.
- Fix: Add composer key handling for alt-up (edit latest queued run, only when the caret is at the start) and mod-shift-enter (steer next queued run), gated as in ChatView.tsx:8334-8345. Either resolve composer commands from the keybindings table or log the hardcoded shortcuts as a deviation.

### [low] Assistant citations omitted from prompt-length validation without a Deviations entry
- `crates/app/src/composer/submission.rs:3` · dimension: logic-fidelity · upheld 2/2
- Evidence: submission.rs:3-6 says 'Assistant citations are not ported, so the provider input is the trimmed prompt'. Upstream composerSubmission.ts:14-17 uses `Math.max(normalizedPrompt.length, expandAssistantCitationsForProvider(normalizedPrompt).length)`. PORTING.md lists assistantCitations as a kept contract (line 136), and the Deviations table (lines 225-309) has no row for this. Rule 4 requires every place that is not ported faithfully to be logged. Effect: an oversized prompt whose citation expansion pushes it past 120,000 characters is not rejected.
- Fix: Add a Deviations row for the citation omission, with the upstream expression and the reason. Or port expandAssistantCitationsForProvider for the length check.

### [low] List marker regex accepts Unicode digits that upstream rejects
- `crates/app/src/composer/list_continuation.rs:42` · dimension: logic-fidelity · upheld 2/2
- Evidence: list_continuation.rs:42 uses `Regex::new(r"^(\d+)([.)])([ \t]+|\s*$)")`. In the Rust regex crate, `\d` is Unicode-aware by default. Upstream composer-list-continuation.ts uses the JS pattern `/^(\d+)([.)])((?:[ \t]+)|\s*$)/`, where `\d` is ASCII only. Effect: Enter on a line such as `٣. foo` (Arabic-Indic digit) continues the list with `\n٣. ` in Turin, while upstream returns null and inserts a plain newline.
- Fix: Use `(?-u:\d)` or `[0-9]` in the ordered-marker regex so it matches JS `\d`.

### [low] Ordered-marker increment excludes 2^53-1, unlike Number.isSafeInteger
- `crates/app/src/composer/list_continuation.rs:85` · dimension: logic-fidelity · upheld 2/2
- Evidence: list_continuation.rs:85 `Ok(number) if number < (1u64 << 53) - 1 =>`. Upstream composer-list-continuation.ts `nextOrderedMarkerText` uses `Number.isSafeInteger(number)`, which accepts 9007199254740991 (2^53-1). Effect: `9007199254740991. foo` with Enter yields `9007199254740992. ` upstream and keeps `9007199254740991. ` in Turin.
- Fix: Change the guard to `number <= (1u64 << 53) - 1`.

### [low] Command menu rows are 32px tall; upstream desktop rows are 28px
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/menu.rs:580` · dimension: look-and-feel · upheld 2/2
- Evidence: menu.rs:580 `.min_h(rems(2.))` and menu.rs:584 `.py_1()` (4px). Upstream ui/autocomplete.tsx:128 AutocompleteItem base is `min-h-8 ... py-1 ... sm:min-h-7`, and ui/command.tsx:191 CommandItem overrides to `py-1.5`. At desktop width (sm) the row is max(28px, 12px + 16px text-xs line) = 28px. Rust gives 32px. Screenshot docs/screenshots/T17/dark-slash.png: row pitch is about 63px at 2x (about 31.5pt), where upstream would be about 56px at 2x.
- Fix: Use `.min_h(rems(1.75))` for the desktop command row (ui/autocomplete.tsx `sm:min-h-7`) and `.py(rems(0.375))` (py-1.5) at menu.rs:584, keeping px_2 and gap_2.

### [low] Skill rows do not dim the '/skill:' prefix
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/menu.rs:564` · dimension: look-and-feel · upheld 2/2
- Evidence: menu.rs:562-568 builds one label `format!("/skill:{}", ...)` rendered as a single run in the row's foreground color (menu.rs:~618-622). Upstream ComposerCommandMenu.tsx:211-217 renders `<span className="text-secondary-label">/skill:</span>` followed by the display name. Screenshot dark-slash.png shows '/skill:Code Review' and '/skill:Unslop' in uniform white.
- Fix: Split the label into a secondary_label span for '/skill:' and a foreground span for the display name inside the existing label div (keep max_w 45%, font-medium, text-xs).

### [medium] Code block language trigger uses mono 11px with gap 6px; upstream is sans 12px with gap 4px
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/prompt_editor.rs:1152` · dimension: look-and-feel · upheld 2/2
- Evidence: prompt_editor.rs:1152 `.gap(env.rem * 0.375)` (6px), :1159 `.font_family(env.mono.clone())`, :1160 `.text_size(env.rem * 0.6875)` (11px). Upstream ComposerCodeBlockNodeView.tsx:46-52 passes the language picker as `title`, so ChatMarkdown.tsx:781-789 skips its `font-mono text-2xs` span (that span is only the title fallback) and the header has no font-family. The trigger is ui/button.tsx Button size `xs` = `gap-1 ... text-xs sm:h-6 sm:text-xs` (ui/button.tsx:39), so it is 12px, 4px gap, inherited sans. Its chevron is `opacity-60` on currentColor, not foreground at 45% (prompt_editor.rs:~1178-1182).
- Fix: Drop font_family(mono) and use TEXT_XS (12px) for the trigger label, gap_1 (4px), and set the chevron to muted_foreground at 0.6 opacity; keep h 1.5rem and the header padding as they are.

### [low] Code block font size is 12.88px; upstream prompt code is 13px
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/prompt_editor.rs:1118` · dimension: look-and-feel · upheld 2/2
- Evidence: prompt_editor.rs:1118 and :1267 `let code_size = px(theme.fonts.size_prompt) * 0.92;` gives 14 * 0.92 = 12.88px. Upstream sets `--font-size-code` from the code preference (appearanceFonts.ts:121), with DEFAULT_CODE_FONT_SIZE = 13 (packages/contracts/src/settings.ts:151), and index.css:1713-1714 applies it to `.composer-tiptap pre code`. Rust already has `theme.fonts.size_code` (theme/appearance_fonts.rs:111, default 13).
- Fix: Use `px(theme.fonts.size_code as f32)` for code_size at both sites instead of size_prompt * 0.92.

### [low] Dark composer frame is missing the 1px inset highlight
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T17-composer/crates/app/src/composer/views/chat_composer.rs:806` · dimension: look-and-feel · upheld 2/2
- Evidence: chat_composer.rs:806-815 paints `.border_1().border_color(outline)` with `shadow(if dark { Vec::new() } else { shadow_composer(false) })` and nothing for dark. Upstream ComposerSurface.tsx (Host/Main outlineClasses) has `dark:after:inset-shadow-2xs dark:after:inset-shadow-(color:--chat-composer-highlight)`, with `dark:[--chat-composer-highlight:rgb(255_255_255/3%)]` on the Shell (ComposerSurface.tsx:~line 12). The composer's inner top highlight is absent in dark mode.
- Fix: Add an inset 1px BoxShadow with color white at 3% alpha to the dark frame, e.g. `tokens::BoxShadow { inset: true, offset (0,0), blur 1px, color with_alpha(WHITE, 0.03) }`, the same way shadow_dialog does its inset highlight.
