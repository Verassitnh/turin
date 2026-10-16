# QC: S5 app shell + macOS shell, fix round (Cursor grok-4.7)

Reviewer and fixer: turin-QC-S5-cursor-grok. All 10 findings closed (commits ae171a61..f071b634, plus a merge of main d0aca642):
1. Hold-to-quit: synthesized repeat, the arming deadline, and conceal via opacity.
2. Menu Quit always quits.
3. Bounds debounce is generation-gated.
4. Bounds I/O runs off the UI thread, with a minimum size and an atomic merge.
5. Reopen-closed-panel follows the upstream cases.
6. The confirm host renders Confirm and Cancel.
7. The confirm queue is FIFO.
8. Close Window is bound.
9. The dock badge shows a decimal count; the pending source is a documented stub.
10. confirmQuit is read from client settings and reloaded on file watch.

Gate: `scripts/check.sh -p turin` 3216 passed, clippy clean.

**Verdict: PASS**
