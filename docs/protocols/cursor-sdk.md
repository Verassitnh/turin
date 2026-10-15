# Cursor SDK background and ACP migration

## Version and key conclusion

Upstream pins `@cursor/sdk@1.0.35` (`upstream/pnpm-lock.yaml:2946`). I unpacked that exact npm tarball. **The upstream local provider does not spawn `cursor-agent` or speak a CLI stdin protocol.** It imports the JS SDK, calls `Agent.create`/`Agent.resume` and `agent.send`, and the SDK executes local agent logic **inside the Node server process**, using shell/sandbox subprocesses for tools. Cursor service calls use Connect RPC over HTTP(S), and the SDK persists local agents/runs/checkpoints in its own store. Evidence: `upstream/packages/provider-cursor/src/server/CursorAgentSdk.ts:310-392,400-555`, `CursorSdk.ts:8-20`, package `dist/esm/platform.d.ts:15-85`, `transport.d.ts:1-38`, `agent.d.ts:1-75`, and `store/*`. Its `cursor-agent` CLI is a separate frontend to related internals, not an interchangeable SDK wire endpoint.

Turin uses the supported `cursor-agent acp` bidirectional stdio server. See [cursor-acp.md](cursor-acp.md) for the primary Rust protocol. The SDK details here document the behavior lost or changed by that transport.
## Transport, auth, and process launch

SDK local path: `Agent.create(options)` creates local agent state; `Agent.resume(agentId, options)` reloads it; `agent.send(message, {onDelta,...})` returns a `Run` with `id`, `wait()`, `cancel()`, `stream()`, and status. `Agent.listRuns` helps recover an active persisted run after a crashed host (`CursorAgentSdk.ts:368-392,455-550`; package `stubs.d.ts:41-58`, `agent.d.ts:36-58`, `run.d.ts:1-100`). The SDK defaults to SQLite when native SQLite is present, otherwise JSONL; IDs not beginning `bc-` route to local store, while `bc-` routes to cloud API (`agent.d.ts:145-175`, `options.d.ts:165-190`). SDK auth uses explicit `apiKey`, `CURSOR_API_KEY`, or stored browser login (`agent.d.ts:161-174`); upstream supports both API key and `Cursor.auth.login()` (`auth.ts:108-205`, `status.ts:273-313`). Its RPC transport seam selects `@connectrpc/connect-node` HTTP/2 or HTTP/1.1 in Node and Connect Web fetch elsewhere (`transport.d.ts:1-38`); endpoint/service protobufs are bundled, not a local stdio contract.

CLI probe (installed `cursor-agent` 2026.09.15-d2fe57e): `cursor-agent acp --help` says “Start the Cursor Agent as an ACP (Agent Client Protocol) server.” ACP is bidirectional JSON-RPC with approval requests; see the primary spec. Separate `--print --output-format stream-json` accepts `--trust` and `--approve-mcps` for noninteractive workspace/MCP prompts. A print-mode probe without these stopped at Workspace Trust Required; that does not limit ACP.
Upstream creates with model `{id,params?}`, name, `mode:"agent"|"plan"`, optional API key, local cwd, `autoReview`, `settingSources:["project","user","team","mdm","plugins"]`, sandbox enable, retries, and optional MCP servers (`adapter.ts:290-340`). It sends text or `{text,images:[{data:base64,mimeType}]}` and per-turn model/mode; `/compress` is passed through (`adapter.ts:2110-2225`). The SDK loads no ambient settings if `settingSources` is omitted (`adapter.ts:290-299`). The SDK adapter reports no interactive approval request capability, rollback, or native forks (`adapter.ts:2490-2502,2570-2588`). Auto-review is a classifier decision in the SDK/backend, not a host prompt surface.

## Message catalogue and streaming

These are **SDK callback objects** from package `dist/esm/messages.d.ts:1-115`, not guaranteed CLI JSON lines. All carry `agent_id:string` and `run_id:string`:

| `type` | Fields | Meaning |
|---|---|---|
| `system` | `subtype?:"init"`, `model?:{id,params?}`, `tools?:string[]` | Run initialization. |
| `user` | `message:{role:"user",content:[{type:"text",text}]}` | Submitted user content. |
| `assistant` | `message:{role:"assistant",content:[text\|tool_use blocks]}` | Assistant content. |
| `tool_call` | `call_id`, `name`, `status:"running"|"completed"|"error"`, `args?`, `result?`, `truncated?` | Tool lifecycle. |
| `thinking` | `text`, `thinking_duration_ms?` | Reasoning. |
| `status` | `status:"CREATING"|"RUNNING"|"FINISHED"|"ERROR"|"CANCELLED"|"EXPIRED"`, `message?` | Run state. |
| `request`, `task`, `usage` | subtype fields `request_id`, `status/text`, or token usage | Ancillary stream events. |

`SendOptions.onDelta` receives `InteractionUpdate` (`agent.d.ts:36-58`; large generated union at `vendor/cursor-sdk-shared/delta-types.d.ts:52202-52220`): the exact discriminators `text-delta`, `thinking-delta`, `thinking-completed`, `tool-call-started`, `partial-tool-call`, `tool-call-completed`, `shell-output-delta`, `step-completed`, `turn-ended`; `token-delta` is ignored. Upstream handles these to render text, reasoning, shell, tools, plans, and subagents (`adapter.ts:~1800-2000`); callback updates may arrive **before** `send()` returns, so it buffers until the run ID/context exists (`CursorAgentSdk.ts:410-463`, `adapter.ts:2206-2225`). `Run.wait()` returns `{id,status:"finished"|"error"|"cancelled",result?,error?,model?,durationMs?,usage?}` (`run.d.ts:36-70`). The SDK's *local persisted notifier* envelope is `{schemaVersion:1,type:"sdk_message"|"result"|"done",agentId,runId,…}` (`messages.d.ts:91-115`); it is not CLI stdout.

Illustrative SDK event, not evidence for CLI formatting:

```json
{"type":"tool_call","agent_id":"local-agent-id","run_id":"run-id","call_id":"call-id","name":"shell","status":"running","args":{"command":"pwd"}}
```

## Turn sequence

```mermaid
sequenceDiagram
  participant H as Turin or JS sidecar
  participant S as Cursor SDK local executor
  participant A as Cursor service
  H->>S: Agent.create(options) or Agent.resume(agentId, options)
  H->>S: agent.send(message, onDelta)
  S->>A: authenticated Connect RPC model/agent calls
  S-->>H: onDelta InteractionUpdate (may precede send return)
  S->>S: execute local tools; auto-review/sandbox policy
  S-->>H: Run handle, then wait() result
  H->>S: run.cancel() on Stop
  H->>S: Agent.resume(agentId) after host restart
  H->>S: Agent.listRuns(agentId) to recover abandoned run
```

There is no upstream SDK tool approval request/response step: policy is set through sandbox and auto-review, and `adapter.ts:2499` rejects interactive approvals. Turin ACP uses a per-thread persistent process and answers `session/request_permission`. Upstream Cursor steering remains unsupported (`adapter.ts:2445-2451`); `Run.steer` and `idempotencyKey` are not port targets.

## SDK-only recovery and Rust boundary

Keep auth, Connect HTTP status, local store, run status, and tool errors distinct in the SDK background model. Upstream handles an active-run conflict by finding and cancelling the persisted running run (`CursorAgentSdk.ts:392-490`) and primes a sandboxed executor before any unsandboxed run (`:313-350`). ACP has neither behavior. The Rust transport and process rules are in [cursor-acp.md](cursor-acp.md).

## Non-agent SDK surfaces retained

`usageLimits.ts` calls `DashboardService/GetCurrentPeriodUsage` on `api2.cursor.sh` and `get-filtered-usage-events`. Token lookup order is `CURSOR_AUTH_TOKEN`, keychain `cursor-access-token`/`cursor-user`, then `auth.json` locations. `CursorSdkCatalog.ts` calls `Cursor.me` and `Cursor.models.list` for account/catalog; their Connect wire details are not documented here. Under ACP, query `cursor/list_available_models` for the model parameter catalog while retaining the usage-limit and keychain flows. The SDK callback adapter handles tool kinds `shell`, `edit`, `read`, `write`, `delete`, `ls`, `grep`, `glob`, `mcp`, `task`, plus special `createPlan` and `updateTodos` paths (`adapter.ts:1820-1930`; replay fixtures `apps/server/src/orchestration-v2/testkit/fixtures/*/cursor_transcript.ndjson`).

`Agent.create`, `Agent.resume`, and `Agent.listRuns` are declared in package `stubs.d.ts:41-58`, whereas `SendOptions` and its `onDelta` are in `agent.d.ts:36-58`. Upstream's `respondToRuntimeRequest` rejects interactive approvals on the SDK route; ACP supplies a separate permission channel. SDK `Run.steer` and `idempotencyKey` exist in the package but upstream Cursor steering is unsupported, so they are background only.
