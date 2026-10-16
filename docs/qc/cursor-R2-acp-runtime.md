# QC (Cursor, grok-4.7) — R2 ACP session runtime + cursor accounts — FAIL → reviewer fixing
- [high] prompt cancel: prompt not forked/recorded; interrupt never aborts; wait-for-prompt deadlocks on dispatch semaphore; no 15s timeout/retire; session/cancel errors swallowed (AcpSessionRuntime.ts:2465-2508, 2715-2773).
- [high] containment: macOS make() fails "unsupported" instead of POSIX ledger; Linux no ledger/cgroup lease; ESRCH treated as error (1649-1756).
- [medium] stderr tail + on_stderr never wired; exit errors lack sanitized stderr (1449-1468, 1780-1798).
- [medium] failed start() leaves `starting`, metadata and load gate set (2426-2430, 2149).
- [medium] session/resume called when not advertised; upstream -32601 (2325-2341).
- [medium] CursorKeychain notify_waiters race → false timeout (CursorKeychain.ts:109-112).
- [low] symlinked skill-package test missing; shown-tool eviction not oldest-first.
- Also: spawn env/extend_env ignored; cursor cancel_turn reports success on failed cancel.
