# QC (Cursor review, grok-4.7) — U3c chat find, embeddedScripts, markdown-clipboard

Verdict: **FAIL** (1 high, 1 medium, 1 low). Fix round: U3cfix (grok-4.7).

1. [high] markdown_clipboard.rs ~365/373 + markdown_pipeline.rs push_image ~611: image alt copied twice; `![alt](src)` appended after the paragraph instead of in place; image inside a link flushed outside it. Upstream markdown-clipboard.ts:237 (IMG branch). Fix: emit `![alt](src)` in place of the alt span, keep link.
2. [medium] thread_find_state.rs live_refresh ~396 reads raw local_selection; upstream useThreadFind.ts:131/202 uses rebaseLocalSelection, so after local→server step the response re-anchors upstream but refetches here. Fix: use rebased selection.
3. [low] embedded_scripts.rs ~718 assignment-name regex uses Unicode \w; upstream ASCII. Fix: (?-u:\w).

Verified OK: other modules match upstream on ported branches; UTF-8 offsets in embedded_scripts documented and no caller expects UTF-16; DOM cuts logged.
