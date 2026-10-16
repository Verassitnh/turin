# QC (Cursor dual review) — S4 provider update UI, prompt links, search, open-in

Reviewers: grok-4.7 (A), composer-2.5 (B). Verdict: **FAIL** (1 critical, 6 high, ~9 medium, 2 low). Fix round: S4c (composer-2.5).

# S4 reviewer B (composer-2.5) — FAIL
1. [high] copy_prompt_link.rs:21 uses chars().count(); upstream uses JS UTF-16 length (copyPromptLink.ts:30, shared promptLinks.ts:134). Fix: turin_shared::js_string::utf16_len.
2. [high] thread_link_prompt.rs:35-37 always passes "" as existing draft to place_linked_prompt_decision; upstream ThreadLinkPrompt.tsx:39-40 reads live composer draft ("Replace draft?"). Fix: take existing_draft arg / let host decide.
3. [high] open_in_picker.rs:38-45 maps every non-JetBrains editor to PROVIDER_CURSOR icon; upstream per-editor icons (OpenInPicker.tsx:85-195, Icons.tsx). Fix: per-EditorId asset map.
4. [medium] thread_link_prompt.rs:25-27 gate uses contains("prompt="); upstream URLSearchParams.has("prompt") (ThreadLinkPrompt.tsx:27). Fix: parse query keys.
5. [medium] project_content_search_dialog.rs:39-54 groups with HashMap (unstable order); upstream Map first-seen order. Fix: IndexMap.
6. [medium] project_content_search_dialog.rs:98-114 ignores is_pending/error/can_read_files; upstream ProjectContentSearchDialog.tsx:236-260 shows Searching…/error/access/empty states. Fix: branch render.
7. [medium] highlighted_search_line.rs:174-178 sync syntax highlighting on UI thread per line (up to 100); upstream async. Fix: precompute/cache off-thread.
Also noted (logged deviation): JetBrains SVGs are placeholders, not upstream artwork — upstream inlines them in JetBrainsIcons.tsx, so port the real artwork.

# S4 reviewer A (grok-4.7) — FAIL
- [critical] highlighted_search_line.rs UTF-16 ranges sliced as bytes → panic on non-ASCII.
- [high] uncolored syntax gaps dropped; cache key ignores match ranges; primary toast passes &[] one-click + shows toast with None key; ?prompt not stripped on place/error.
- [medium] sidebar pill tones/icon; env toast one-click arg; search dialog W/ab, file count, cwd footer, line numbers, invented access message; new-thread link copy.
- [low] env rows TEXT_XS + success check; Mac/Windows regex in open_in_picker.

## Re-QC (grok-4.7) — FAIL → fix round
All previous findings verified fixed with tests (UTF-16 ranges, gaps, cache key, toasts, ?prompt strip + draft, utf16 length, per-editor icons, query key, IndexMap order, dialog states/controls/line numbers, pill tones, env toast, link copy, env rows, Mac/Win regex, real JetBrains artwork).
New: [high] off-thread syntax highlight never repaints (no view notify after cache fill; upstream Suspense re-render). [low] extra "Preparing your draft." description on opening state.
