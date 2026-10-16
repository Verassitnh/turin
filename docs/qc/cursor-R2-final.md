# QC: R2 ACP runtime + Cursor accounts, fix round (Cursor grok-4.7)

Reviewer and fixer: turin-QC-R2-cursor-grok. All 8 findings from cursor-R2-acp-runtime.md were closed in commit 4c49c3c:
- prompt cancel and serialization permits;
- POSIX/cgroup process containment, including ESRCH;
- bounded stderr tail and hook kill;
- start_once reset on every error;
- capability-gated `session/resume`;
- keychain wait race (the test uses ImmediateBackend; no real `security` call);
- symlinked skill package boundary;
- bounded insertion-ordered shown tool-call ids.

Gate: `scripts/check.sh -p turin-acp -p turin-provider-cursor` green. Tests: turin-acp 145, fd_stability 1, provider-cursor 96 (+1 ignored live test).

**Verdict: PASS**
