# QC: U3e chat view logic (+ U3c chat find), Cursor grok-4.7 review

Reviewer: turin-QC-U3e-cursor-grok (grok-4.7, cursor). Round 1: FAIL. Three high and three medium defects; the three U3c fixes were confirmed.
1. High: worktree snapshot selection ignored the newer held/recorded snapshot.
2. High: the timeline anchor was released by any earlier work row.
3. High: plan follow-up restore dropped the context arrays, and the caret collapse used the byte length.
4. Medium: whitespace-only terminal text was treated as expired.
5. Medium: the revert wait failed when the message left the projection.
6. Medium: three pure helpers were skipped (`resolveThreadMetadataUpdateForNextTurn`, `shouldWriteThreadErrorToCurrentServerThread`, `codexArtifactTemplatePromptToAppend`).

Fix round (the same reviewer, given edit rights): commit b3a34b0e. Each defect was re-verified against upstream, and the upstream test cases were added.
Gate: `scripts/check.sh -p turin` 3013 passed, 0 failed, clippy clean.

**Verdict: PASS**
