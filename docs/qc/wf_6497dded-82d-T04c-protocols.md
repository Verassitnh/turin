# QC wf_6497dded-82d — T04c protocol-doc application check

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 2 · refuted: 0

## Lead report

1. LOW · docs/protocols/cursor-acp.md:62 (and :64): the UNVERIFIED blockquote is stale. The create_plan fallback is verified in cursor-agent 2026.09.15-d2fe57e/2698.index.js. Problem: the rule is dropped, and line 64 ("the CLI's own fallbacks replace Turin's cards") has no referent, so the doc hides that plan approval is auto-accepted when Turin does not register the handler. Fix: replace the blockquote with the rule. On a create_plan error that matches the unsupported test used for ask_question (Unimplemented, "method not supported", "methodname(name=cursor/create_plan)", "-32601"), the CLI writes a local plan and returns success even if the write fails (planUri may be ""). For other errors it returns success only if the write produced a planUri, otherwise "Failed to create plan: client extension failed and local fallback artifact could not be created." Name the auto-accept explicitly in line 64.

2. LOW · docs/protocols/opencode.md:111 and :155: the UNVERIFIED note and the changelog line "UNVERIFIED note added for the remaining rows" are stale. Every listed row exists in the packed @opencode-ai/sdk 1.15.13 dist/v2/gen/sdk.gen.js: /global/health:174, /session:1854/1881, /session/{sessionID}:1927/1948/1973, /session/status:1906, /children:1999, /message:2065/2095, /fork:2166, /abort:2192, /summarize:2287, /prompt_async:2322, /command:2355. Fix: delete the blockquote and the changelog phrase, and cite these line numbers. Also note that the legacy /session/{sessionID}/permissions/{permissionID} (sdk.gen.js:1717) is in the v2 table too, since line 7 implies it is root-only.

3. LOW · docs/protocols/codex-app-server.md:95 and :114: the timeout note is stale. Upstream CodexProvider.ts:72-73 sets CODEX_CATALOG_REFRESH_TIMEOUT = "3 seconds" and CODEX_CATALOG_REFRESH_POLL = "100 millis". Line 339 decodes models_cache.json `fetched_at` as a String, parsed at :352. On timeout, upstream keeps the first model list and logs a warning (:380-389). Fix: replace the note with those facts, change ":114 (timeout value UNVERIFIED)" to "(3 s timeout, 100 ms poll)", and replace the "Read CodexProvider.ts:369-389" pointer with :72-73,339,352,380-389.

Rulings on the applier's judgment calls:
(a) Correct as written. CodexAdapterV2.ts:557-571 maps decline to { denied: { rejection: "User declined the request." } }. The legacy handlers return { decision: ... } (:5460-5462, :5530), matching schema.gen.ts:49171 (ExecCommandApprovalResponse = Struct({decision})). No change.
(b) Accurate. The 45 s silent-stream watchdog is in upstream v2/OpenCode2Client.ts:99 (SILENT_STREAM_TIMEOUT), and the doc says v1 has none. The "For v2," and "v1 auth is different" sentences are correct. The auto row (edit ask unless auto-accept-edits) matches v2/adapter.ts:509. No change.
(c) The notes are not a PORTING.md violation, but all three are now checkable and should be resolved, not shipped. Items 1-3 cover them.

Verdict: PASS (no critical/high; three low documentation-accuracy findings to fix).

## Confirmed findings (raw)

### [low] UNVERIFIED note on create_plan fallback is checkable, and the verified claim was dropped
- `docs/protocols/cursor-acp.md:` · dimension: application-check · upheld 2/2
- Evidence: The doc has: "> UNVERIFIED: the QC report does not re-verify the older claim that `create_plan` on -32601 writes a local plan and auto-accepts." The shipped bundle (~/.local/share/cursor-agent/versions/2026.09.15-d2fe57e/2698.index.js) confirms it. The create_plan catch block runs the same unsupported test as ask_question (Unimplemented, "method not supported", "methodname(name=cursor/create_plan)", "-32601"). It then calls `n=yield P()` and returns `createPlan(... {case:"success"} ..., planUri:n)` if the test matched or a local plan was written. Otherwise it returns the error "Failed to create plan: client extension failed and local fallback artifact could not be created." The old doc stated this fallback and the applied text no longer does. The tail "without them the CLI's own fallbacks replace Turin's cards" is left with no description of what the fallback does.
- Fix: Replace the UNVERIFIED blockquote with the verified rule. When create_plan gets an unsupported error (the same set as ask_question), the CLI writes a local plan file and reports success with that planUri. If the file cannot be written, it reports a failure.

### [low] UNVERIFIED notes on OpenCode v1 routes and Codex ChatGPT refresh are checkable and now stale
- `docs/protocols/opencode.md:` · dimension: application-check · upheld 2/2
- Evidence: Line 111 says the non-added v1 rows were "not re-checked against the `/v2` route table". The packed SDK (/tmp/oc-v1/package/dist/v2/gen/sdk.gen.js) already contains every one of them: /global/health:174, /session:1854/1881, /session/{sessionID}:1927-1973, /session/status:1906, /children:1999, /message:2065/2095, /fork:2166, /abort:2192, /summarize:2287, /prompt_async:2322, /command:2355, /permission/{requestID}/reply:1687, /question/{requestID}/reply:1616, and /provider, /agent, /skill, /command, /mcp. Likewise, codex-app-server.md:95 says the timeout value and the name of the fetch-time field are unknown. Upstream CodexProvider.ts:72-73,339 gives `CODEX_CATALOG_REFRESH_TIMEOUT = "3 seconds"`, a poll of "100 millis", and the file field `fetched_at` (a string parsed as a DateTime). If the timeout expires, upstream keeps the first model list and logs a warning.
- Fix: Drop the OpenCode UNVERIFIED blockquote. Cite the line numbers above, and note that `/session/{sessionID}/permissions/{permissionID}` also exists in the /v2 table as a legacy route next to `/permission/{requestID}/reply`. In codex-app-server.md, replace the UNVERIFIED note with: poll every 100 ms, time out after 3 s, read `fetched_at` from models_cache.json, and on timeout keep the first catalog.
