# Codex app-server stdio JSON-RPC

## Transport and launch

Spawn `codex app-server` with piped stdin/stdout, stderr drained separately, selected `CODEX_HOME`, cwd and extra CLI args from provider settings. One long-lived process may serve several threads (`upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts:1790-1830`; provider launch/environment `upstream/apps/server/src/provider/CodexProvider.ts:392-465`, `Drivers/CodexDriver.ts:1-35`). Protocol is newline-delimited JSON-RPC-style objects, **without** a `jsonrpc:"2.0"` field in upstream examples: client requests `{id:number|string,method:string,params?:object}`, server replies `{id,result?:any,error?:{code:number,message:string,data?:any}}`, notifications `{method,params?}`, and server-initiated requests also have `id`. The wrapper serializes one JSON value plus newline and routes by `id` (`upstream/packages/effect-codex-app-server/src/protocol.ts:38-115,143-220`; `_internal/shared.ts:1-30`). Example transcript: `upstream/apps/server/src/orchestration-v2/testkit/fixtures/tool_call_restricted_granular/codex_transcript.ndjson:9-79`.

After spawn send `initialize` with `{clientInfo:{name:"T3 Code",title:"T3 Code",version},capabilities:{experimentalApi:true,optOutNotificationMethods:["turn/diff/updated"]}}`, await reply, then notify `initialized` with no params. Codex rejects a second initialize; upstream serializes this handshake and accepts `-32600 Already initialized` if a prior response was lost (`CodexAdapterV2.ts:1791-1824`). The MCP-app capability extension is omitted because Turin cuts MCP apps. `experimentalApi` is required for experimental methods and cannot be corrected after initialization without respawning. Auth belongs to the installed CLI/CODEX_HOME. Preserve a user-set `CODEX_INTERNAL_ORIGINATOR_OVERRIDE`, otherwise set it to the client name to prevent the models cache race. Expand a leading `~` in `CODEX_HOME` (`CodexProvider.ts:392-452`).

## Method and message catalogue used upstream

The generated method→schema map is `upstream/packages/effect-codex-app-server/src/_generated/meta.gen.ts` and types are `schema.gen.ts`; use them for exact field types, optionality and enum spellings rather than hand-writing a parallel schema. Methods observed in `CodexAdapterV2.ts:~1750-7400` and `CodexProvider.ts`:

| Direction | Methods | Purpose |
|---|---|---|
| client request | `initialize`; `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/unarchive`, `thread/unsubscribe`, `thread/inject_items`, `thread/compact/start`; `turn/start`, `turn/steer`, `turn/interrupt` | Session and turn lifecycle. `turn/start` input is an array of typed text/image items plus model, cwd, approval/sandbox policy (`CodexAdapterV2.ts:6300-6320,6550-6775`). |
| client request | `thread/goal/get,set,clear`; `thread/backgroundTerminals/list,terminate`; `mcpServerStatus/list`; `mcpServer/resource/read`, `mcpServer/tool/call`; `feedback/upload` | Goal/background/MCP support (`CodexAdapterV2.ts:2350-2400,6340-6420`). |
| client notification | `initialized` | Finishes handshake. |
| server notification | `turn/started`, `turn/completed`, `turn/plan/updated`; `item/started`, `item/completed`, `item/agentMessage/delta`, `item/reasoning/textDelta`, `item/reasoning/summaryTextDelta`, `item/plan/delta`; `thread/tokenUsage/updated`, `thread/settings/updated`, `thread/goal/updated,cleared`; `account/rateLimits/updated`, `model/rerouted`, `error` | Streaming and state (`CodexAdapterV2.ts:4308-4817,6071-6180`). Upstream opts out of noisy `turn/diff/updated` (`:229`). |
| server request | `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput`, `mcpServer/elicitation/request`; legacy `execCommandApproval`, `applyPatchApproval` | Host replies with result on **same request ID**; wait for the owning turn before returning a protocol error (`CodexAdapterV2.ts:5111-5550`). |

Minimal wire example, with generated types defining all real fields:

```jsonl
{"id":1,"method":"initialize","params":{"clientInfo":{"name":"T3 Code","title":"T3 Code","version":"0.1.0"},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":["turn/diff/updated"]}}}
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

Use `tokio::process`, one serialized stdin writer, an unbounded line reader matching upstream, `serde_json::Value` fallback for unknown methods/items, and a pending `HashMap<id, oneshot::Sender<_>>`. Server requests must be serviced concurrently while a turn request is pending or approvals deadlock. Keep a separate table for client requests and server request handlers. Match IDs as string **or** number; preserve `null` versus omitted fields from generated schemas. On EOF/nonzero exit fail every pending request; on any undecodable or unroutable line terminate the session and fail all pending requests; on JSON-RPC error use code/message/data, especially `-32600 Already initialized` (`protocol.ts:~170-360`, `_internal/shared.ts:1-30`). An approval can come from a subagent and needs routing to the owning root turn (`CodexAdapterV2.ts:4110-4200`). A turn ends on `turn/completed`, not merely the `turn/start` response. `thread/resume` restores a thread in a new app-server process; retain native thread ID and handle archive/missing-thread errors.

## Methods and fields beyond generated types

The pinned schema omits some calls the adapter uses. Hand-type `thread/backgroundTerminals/list {threadId,cursor?} -> {data:[{processId,...}],nextCursor}` and `thread/backgroundTerminals/terminate {threadId,processId} -> {terminated}`. Decode `thread/read`'s `historyMode` from the raw response. `thread/resume.excludeTurns` **is** in the schema. Add `thread/turns/list`, `thread/revert`, `thread/reverted`, `model/list`, `skills/list`, `account/read`, and `account/rateLimits/read` to the method registry (`CodexAdapterV2.ts:209-226,1030-1047,2355-2400,7324-7410`; `CodexProvider.ts:328,495-559`).

Extend generated `turn/start` types with `collaborationMode:{mode:"plan"|"default",settings:{model,reasoning_effort,developer_instructions?}}`; plan turns require mode `plan`. Upstream also assigns `additionalContext`, mostly for cut T3 MCP/MCP-app features. Always send `summary:"detailed"` and `approvalsReviewer` (it otherwise sticks across resume). thread/start, thread/resume and thread/fork params include a `config` request parameter (`CodexAdapterV2.ts:662-668,802-805,1322-1341`):

```text
config:{"tools.update_plan.enabled":true,"mcp_optional_startup_grace_ms":10000, mcp_servers?}
```

Plus `cwd` and `model` when known. `mcp_servers` is present only when MCP servers are configured. This is **not** an environment variable: upstream's `CODEX_THREAD_CONFIG` is a TS constant spread into the request params by `codexThreadRuntimeParams`.

| runtimeMode | approvalPolicy | approvalsReviewer | sandboxPolicy (turn/start) |
|---|---|---|---|
| approval-required | untrusted | user | `{type:"readOnly"}` |
| auto-accept-edits | on-request | user | `{type:"workspaceWrite"}` |
| auto | on-request | auto_review | `{type:"workspaceWrite"}` |
| full-access | never | user | `{type:"dangerFullAccess"}` |

This table is from `codexRuntimeModeTurnDefaults` (`CodexAdapterV2.ts:679-717`); explicit runtime policy overrides still apply. `sandboxPolicy` is sent on `turn/start` as an object, not a bare string. `turn/steer` must include `expectedTurnId`.

Rollback (`CodexThreadRevert.ts:6-44`, `CodexAdapterV2.ts:1007-1050,7324-7410`): raw `thread/read {includeTurns:false}` to inspect `historyMode`; reject anything other than `paginated`. Compute `loaded` as `thread.status.type !== "notLoaded"`. If not loaded, first `thread/resume {excludeTurns:true,...}`. Page turns with exactly these params, with a repeated-cursor guard:

```text
thread/turns/list {threadId, cursor:null|nextCursor, limit:min(remaining,100), sortDirection:"desc", itemsView:"summary"}
```

`beforeTurnId` is the id of the Nth-newest turn, where N is the number of turns to drop. That turn and everything after it are removed. If the thread has fewer than N turns, it is the oldest turn found.

Then `thread/revert {threadId,beforeTurnId}` (or `thread/read` when no revert is needed). Use the `thread` from the `thread/revert` (or `thread/read`) response as the result. `thread/reverted {threadId}` is a later informational notification that upstream ignores. Do not block on it: the no-revert branch never emits it, so awaiting it hangs.

Provider probing starts with `account/read`. Return early only when there is **no** `account` AND `requiresOpenaiAuth` (`CodexProvider.ts:495-503`). Logged-in ChatGPT accounts also report `requiresOpenaiAuth:true`; a literal early return on that flag gives them no models, skills or rate limits. Otherwise run `skills/list {cwds}`, paginated `model/list`, and `account/rateLimits/read null` concurrently, with a degrading timeout on rate limits (`CodexProvider.ts:495-559`).

For `account.type==="chatgpt"`, upstream calls `requestCodexChatGptModels({client, codexHome, spawnedAtMs})` (`CodexProvider.ts:369-389`, `:509-511`) instead of paginated `model/list`. It runs `model/list`, polls `<codexHome>/models_cache.json` until its `fetched_at` is at or after spawn time, then lists again.

Cache-refresh timing (`CodexProvider.ts:72-73,339,352,380-389`): poll every **100 ms** (`CODEX_CATALOG_REFRESH_POLL = "100 millis"`) and time out after **3 s** (`CODEX_CATALOG_REFRESH_TIMEOUT = "3 seconds"`). `models_cache.json` `fetched_at` is decoded as a String (`:339`) and parsed as a DateTime (`:352`). On timeout, upstream keeps the first model list and logs a warning (`:380-389`).

The version is parsed from `initialize.userAgent` with `/\/([^\s]+)/`.

## Server requests and ordering

| Request | Response on same ID |
|---|---|
| `item/commandExecution/requestApproval`, `item/fileChange/requestApproval` | `{decision:"accept"|"acceptForSession"|"decline"|"cancel"}`; UI acceptAlways becomes acceptForSession. |
| `item/permissions/requestApproval` | `{permissions,scope:"turn"|"session"}`; deny uses `{permissions:{},scope:"turn"}`. |
| Legacy `execCommandApproval`, `applyPatchApproval` | Wrapped as `{decision:<ReviewDecision>}`: `{decision:"approved"}`, `{decision:"approved_for_session"}`, `{decision:"abort"}`, or decline `{decision:{denied:{rejection:"User declined the request."}}}`. Route by conversationId/callId. |

Approvals can precede `turn/started`: `awaitActiveTurn` yields up to 1000 times before giving up. Background-terminal item events can follow `turn/completed` and resolve against `settledTurns`. Cache per-thread session allowances by runtimeMode and request kind (`CodexAdapterV2.ts:1892-1919,2110-2136,5120-5125`). On an archived resume error matching the case-insensitive regex `(?i)\bsession \S+ is archived\b|\bcodex unarchive\b` (`:6618`), call `thread/unarchive` and retry (`:6612-6629`).

The protocol reader terminates on **every** undecodable or unroutable line and fails all pending requests. It has no upstream line bound; any added bound must exceed multi-MB payloads and be logged as a deviation. Server request handlers run concurrently up to 32 active; excess receive `-32001` overloaded. Notifications run inline in arrival order (`effect-codex-app-server/src/protocol.ts:18,303-440`). Generate Rust types from the pinned OpenAI commit `687a119f0fcaace47e1f1abcc77cec6c813fd6da`; the crates.io `codex-app-server-protocol` package is a third-party fork.

## Changelog (QC wf_9dc37bfd-861)

- Item 5 (MEDIUM): rollback uses the `thread` from the `thread/revert` or `thread/read` response; `thread/reverted` is informational and not awaited; `beforeTurnId` defined as the Nth-newest turn (oldest found if fewer); exact `thread/turns/list` params; `loaded` derived from `thread.status.type`.
- Item 6 (MEDIUM): provider early return is "no account AND requiresOpenaiAuth"; ChatGPT account path via `requestCodexChatGptModels` with `models_cache.json` polling (3 s timeout, 100 ms poll); version parsed from `initialize.userAgent`.
- Item 7 (MEDIUM): `config` request parameter (not an env var) on thread/start, resume and fork; sandboxPolicy as `{type:...}` objects on turn/start; legacy approval replies wrapped in `{decision}` (decline shown as the ReviewDecision value); archived-resume regex is case-insensitive.
- QC wf_6497dded-82d item 3 (LOW): UNVERIFIED timeout note replaced with 100 ms poll, 3 s timeout, `fetched_at` String/DateTime parse, and keep-first-catalog on timeout; citations `CodexProvider.ts:72-73,339,352,380-389`.
