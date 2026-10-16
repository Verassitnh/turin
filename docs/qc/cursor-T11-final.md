# QC: T11 OpenCode adapter, final verify (Cursor grok-4.7)

Verifier and fixer: turin-verify-T11d-cursor-grok. The 2 high, 3 medium and flaky-test findings from cursor-T11-verify.md were fixed in commits dcf1bdd..1694c52 (late wake replay order, interrupting unattached background children, settling open tools and subagents at turn end, reconnect wake discard, late background permission, event-wait failure, orphan process group).
Three sequential runs of `cargo test -p turin-provider-opencode --lib -- --test-threads=1`: 191/191 each time. Gate `scripts/check.sh -p turin-provider-opencode` green.

**Verdict: PASS**
