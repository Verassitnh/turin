# Codex app-server stdio JSON-RPC

## Transport and launch

Spawn `codex app-server` with piped stdin/stdout, stderr drained separately, selected `CODEX_HOME`, cwd and extra CLI args from provider settings. One long-lived process may serve several threads (`upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts:1790-1830`; provider launch/environment `upstream/apps/server/src/provider/CodexProvider.ts:392-465`, `Drivers/CodexDriver.ts:1-35`). Protocol is newline-delimited JSON-RPC-style objects, **without** a `jsonrpc:"2.0"` field in upstream examples: client requests `{id:number|string,method:string,params?:object}`, server replies `{id,result?:any,error?:{code:number,message:string,data?:any}}`, notifications `{method,params?}`, and server-initiated requests also have `id`. The wrapper serializes one JSON value plus newline and routes by `id` (`upstream/packages/effect-codex-app-server/src/protocol.ts:38-115,143-220`; `_internal/shared.ts:1-30`). Example transcript: `upstream/apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/codex_transcript.ndjson:9-79`.

After spawn send `initialize` with `{clientInfo,capabilities}`, await reply, then notify `initialized` with no params. Codex rejects a second initialize; upstream serializes this handshake and accepts `-32600 Already initialized` if a prior response was lost (`CodexAdapterV2.ts:1791-1824`). Auth belongs to the installed Codex CLI/CODEX_HOME, not an app-server HTTP header. A `codex` installed on this research host failed to start because its npm launcher referenced a missing vendored binary; no live schema command was verified here.

## Method and message catalogue used upstream

The complete generated method→schema map is `upstream/packages/effect-codex-app-server/src/_generated/meta.gen.ts` and types are `schema.gen.ts`; use them for exact field types, optionality and enum spellings rather than hand-writing a parallel schema. Methods observed in `CodexAdapterV2.ts:~1750-7400` and `CodexProvider.ts`:

| Direction | Methods | Purpose |
|---|---|---|
| client request | `initialize`; `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/unarchive`, `thread/unsubscribe`, `thread/inject_items`, `thread/compact/start`; `turn/start`, `turn/steer`, `turn/interrupt` | Session and turn lifecycle. `turn/start` input is an array of typed text/image items plus model, cwd, approval/sandbox policy (`CodexAdapterV2.ts:6300-6320,6550-6775`). |
| client request | `thread/goal/get,set,clear`; `thread/backgroundTerminals/list,terminate`; `mcpServerStatus/list`; `mcpServer/resource/read`, `mcpServer/tool/call`; `feedback/upload` | Goal/background/MCP support (`CodexAdapterV2.ts:2350-2400,6340-6420`). |
| client notification | `initialized` | Finishes handshake. |
| server notification | `turn/started`, `turn/completed`, `turn/plan/updated`; `item/started`, `item/completed`, `item/agentMessage/delta`, `item/reasoning/textDelta`, `item/reasoning/summaryTextDelta`, `item/plan/delta`; `thread/tokenUsage/updated`, `thread/settings/updated`, `thread/goal/updated,cleared`; `account/rateLimits/updated`, `model/rerouted`, `error` | Streaming and state (`CodexAdapterV2.ts:4308-4817,6071-6180`). Upstream opts out of noisy `turn/diff/updated` (`:229`). |
| server request | `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput`, `mcpServer/elicitation/request`; legacy `execCommandApproval`, `applyPatchApproval` | Host replies with result on **same request ID**; deny/error if no owning turn (`CodexAdapterV2.ts:5111-5550`). |

Minimal wire example, with generated types defining all real fields:

```jsonl
{"id":1,"method":"initialize","params":{"clientInfo":{"name":"turin","version":"0.1.0"},"capabilities":{}}}
{"id":1,"result":{}}
{"method":"initialized"}
{"id":2,"method":"thread/start","params":{"cwd":"/workspace"}}
{"id":2,"result":{"thread":{"id":"thread-id"}}}
{"method":"turn/started","params":{"threadId":"thread-id","turn":{"id":"turn-id"}}}
{"id":99,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-id","turnId":"turn-id","itemId":"item-id"}}
{"id":99,"result":{"decision":"accept"}}
```

The last examples are **illustrative envelopes**; actual method payloads must be generated from the pinned schema and can require more fields.

## Turn sequence

```mermaid
sequenceDiagram
  participant H as Turin
  participant C as codex app-server
  H->>C: spawn; initialize request
  C-->>H: initialize response
  H->>C: initialized notification
  H->>C: thread/start or thread/resume; turn/start
  C-->>H: turn/started; item deltas/started/completed
  C->>H: item/.../requestApproval (id)
  H-->>C: result or error with same id
  C-->>H: turn/completed
  H->>C: turn/interrupt on Stop
  H->>C: thread/resume after process restart
```

## Generated schema and Rust implementation notes

Codex's source tree has `codex-rs/app-server-protocol/schema/json/{v1,v2}` plus TypeScript method declarations. Upstream's `scripts/generate.ts:15-20,620-625,705-725` downloads a **pinned upstream Codex commit** (`687a119f0fcaace47e1f1abcc77cec6c813fd6da`) and generates `_generated/{schema,meta,namespaces}.gen.ts`. This is concrete codegen input for Rust: vendor that pinned JSON schema directory, use `typify`/`schemars`-compatible generation or targeted `serde` types, and verify against `meta.gen.ts` method mappings. Recent Codex builds expose an app-server schema generation command, commonly `codex app-server generate-json-schema --out DIR`; verify syntax against the **actual installed binary** before making it a build step. The existing pinned source schema is sufficient if the local command is absent.

Use `tokio::process`, one serialized stdin writer, bounded UTF-8 line reader, `serde_json::Value` fallback for unknown methods/items, and a pending `HashMap<id, oneshot::Sender<_>>`. Server requests must be serviced concurrently while a turn request is pending or approvals deadlock. Keep a separate table for client requests and server request handlers. Match IDs as string **or** number; preserve `null` versus omitted fields from generated schemas. On EOF/nonzero exit fail every pending request; on JSON parse failure log safely and continue or terminate according to severity; on JSON-RPC error use code/message/data, especially `-32600 Already initialized` (`protocol.ts:~170-360`, `_internal/shared.ts:1-30`). An approval can come from a subagent and needs routing to the owning root turn (`CodexAdapterV2.ts:4110-4200`). A turn ends on `turn/completed`, not merely the `turn/start` response. `thread/resume` restores a thread in a new app-server process; retain native thread ID and handle archive/missing-thread errors.
