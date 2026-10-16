# QC: S2b chat media UI (image dialog, citations, highlighter), Cursor grok-4.7

Reviewer and fixer: turin-QC-S2b-cursor-grok. Round 1 FAIL had 4 high and 2 medium findings:
1. The image stage was sized to the window, which clipped the close button.
2. Zoom didn't keep the point under the pointer, and non-PNG images got a 640x480 fit.
3. Escape and arrow-key panning were unreachable.
4. A stale highlight could overwrite the current one, and UTF-16 offsets dropped non-ASCII spans.
5. The quote card repeated the comment, and the chip metrics were off.
6. The comment field was unstyled.

Fix round, commit 25800580 (same reviewer, given edit rights): all six fixed and re-verified, including a `"éé"` UTF-16 test. The intrinsic image size is now read for PNG, JPEG, GIF, WebP and SVG. Screenshots recaptured; the PM checked citation-quote.png.
Gate: `scripts/check.sh -p turin` 2474 passed, 0 failed.

**Verdict: PASS**
