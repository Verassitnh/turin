# Claude Code stream JSON protocol

## Version and evidence

Upstream locks `@anthropic-ai/claude-agent-sdk@0.3.276` (`upstream/pnpm-lock.yaml:1951`). This document describes its shipped `sdk.mjs` and `sdk.d.ts`, unpacked from that exact npm tarball. The SDK implementation is minified: the CLI argument builder and NDJSON dispatcher are on `sdk.mjs:127-132`; the readable wire declarations are in `sdk.d.ts`. Upstream uses the SDK through `ClaudeAdapterV2.ts:735-905,1080-1180,8098-8385` and probes it through `ClaudeProvider.ts:220-253`. These are **CLI/SDK protocol** shapes, not Anthropic Messages API calls.

## Transport and process launch

Spawn the selected `claude` executable with piped stdin/stdout and separate stderr, `cwd` and environment inherited/overridden from the provider settings. SDK base arguments are `--output-format stream-json --verbose --input-format stream-json` (`sdk.mjs:127`). The SDK writes one UTF-8 JSON object plus `\n` per stdin record and reads one per stdout line (`sdk.mjs:127-132`). Do not parse stderr as protocol. Keep stdin open for a multi-turn query; closing it makes the process one-shot. The Rust host must own process cancellation and reap children.

Relevant SDK option → CLI flag mapping (`sdk.mjs:127`, `sdk.d.ts:1446-2350`): `model` → `--model VALUE`; `resume` → `--resume=UUID`; `sessionId` → `--session-id=UUID`; `resumeSessionAt` → `--resume-session-at=UUID`; `persistSession:false` → `--no-session-persistence`; `permissionMode` → `--permission-mode VALUE`; `allowDangerouslySkipPermissions` → `--allow-dangerously-skip-permissions`; `canUseTool` → `--permission-prompt-tool stdio`; alternatively `permissionPromptToolName` supplies a named MCP tool, and the SDK refuses both together. `tools` → `--tools LIST` (`[]` yields empty string); `allowedTools` / `disallowedTools` → `--allowedTools` / `--disallowedTools`; `mcpServers` with nonempty map → inline JSON `--mcp-config '{"mcpServers":…}'`; `strictMcpConfig` → `--strict-mcp-config`; `settingSources` → `--setting-sources=user,project,local`; `settings` → `--settings JSON`; `includePartialMessages` → `--include-partial-messages`; `additionalDirectories` → repeated `--add-dir`; `plugins` may use `--await-initialize` and then stdin `initialize.plugins`; `extraArgs` append raw flags. Query `systemPrompt`, in-process MCP, hooks, agents, and supported dialogs are conveyed by `initialize`, not only argv. Do not put secrets in argv if stdin delivery can be used.

Upstream’s normal query sets the Claude Code preset system prompt plus T3 instructions, model/effort, selected tools, `includePartialMessages: true`, `canUseTool`, user dialog, settings, cwd/attachment directory, and optional MCP servers; new thread uses `sessionId`, continuation `resume`, sometimes `resumeSessionAt` (`ClaudeAdapterV2.ts:1080-1180`). **For normal T3 sessions, it removes `mcpServers` before `query()` and registers them after spawn with `setMcpServers` over stdin**; the SDK default would put the Authorization header in `--mcp-config` argv (`ClaudeAdapterV2.ts:735-805,1220-1253`). A Rust port must preserve this security behavior and its registration timeout/failure path. It strips `permission-mode` and `dangerously-skip-permissions` from configured raw flags and maps them to SDK options, defaults thinking display to summarized, and may set `autoCompactWindow` in settings (`:1088-1153`). The capability probe instead sets `persistSession:false`, `settingSources`, hooks disabled, `allowedTools:[]`, empty MCP config, optionally strict MCP, and environment disabling connected MCP/IDE discovery (`ClaudeProvider.ts:220-253`). Managed MCP config is an exception to strict mode (`ClaudeCapabilitiesProbe.test.ts:63-75,310-330`).

## NDJSON message catalogue

Every emitted conversation frame carries a top-level `type`; most have `uuid` and `session_id`. Model content uses Anthropic content blocks (`text`, `thinking`, `tool_use`, `tool_result`) nested in `message`. See the complete open union `SDKMessage` (`sdk.d.ts:5019`) and `SDKAssistantMessage`, `SDKUserMessage`, `SDKResultMessage`, `SDKSystemMessage` (`:3399-3478,5410-5567,5865-5941`). Implement an unknown variant.

| Direction/type | Required and salient fields | Meaning |
|---|---|---|
| stdin `user` | `message:{role:"user",content:string\|block[]}`, `parent_tool_use_id:null`, `uuid?:string`, `priority?:"now"\|"next"\|"later"` | Submit prompt; CLI also emits user frames for tool results. Upstream sets UUID for prompt echo and may set priority (`ClaudeAdapterV2.ts:1558-1595`). |
| stdout `system/init` | `session_id:string`, `tools:string[]`, `model:string`, `mcp_servers`, `capabilities?:string[]` | Session identity and available capabilities. |
| stdout `stream_event` | `event` Anthropic streaming event, `parent_tool_use_id`, `uuid`, `session_id` | Partial text/thinking/tool JSON when partials enabled (`sdk.d.ts:5164`). |
| stdout `assistant` | `message:{id,model,role:"assistant",content:block[],usage,…}`, `parent_tool_use_id:string\|null`, `uuid`, `session_id` | Completed *block*, not necessarily whole turn; consecutive frames may share message id (`sdk.d.ts:3399-3410`). |
| stdout `result` | `subtype:"success"\|error subtype`, `is_error`, `num_turns`, `usage`, `modelUsage`, `total_cost_usd`, `permission_denials`, `session_id` | Turn terminal. Cost/model usage are cumulative across turns in one process; use latest value, do not sum (`sdk.d.ts:5410-5460`). |
| stdout `system/*` | subtype-specific (`task_started`, `task_progress`, `task_notification`, `tool_progress`, `status`, `api_retry`, etc.) | Background/subagent lifecycle; may arrive after `result` (`sdk.d.ts:5019,5567-5650`). |

Example lines (minimal illustrative valid shapes; real CLI adds fields):

```jsonl
{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","sdkMcpServers":[],"supportedDialogKinds":["resume_return"]}}
{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"commands":[],"models":[]}}}
{"type":"user","message":{"role":"user","content":"Inspect this file"},"parent_tool_use_id":null,"uuid":"prompt-1"}
{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"pwd"},"tool_use_id":"toolu_1"}}
{"type":"control_response","response":{"subtype":"success","request_id":"ask-1","response":{"behavior":"allow","updatedInput":{"command":"pwd"}}}}
{"type":"control_request","request_id":"stop-1","request":{"subtype":"interrupt"}}
```

## Bidirectional control protocol

Envelope: `{type:"control_request",request_id:string,request:{subtype:string,…}}`; reply: `{type:"control_response",response:{subtype:"success"|"error",request_id:string,response?:object,error?:string}}`. IDs match; both sides may initiate. A `control_cancel_request` cancels an in-flight request without its own reply. Unknown IDs and subtypes need tolerant handling (`sdk.d.ts:3669-3676,4680-4748`). SDK’s dispatcher maintains pending requests and abort controllers (`sdk.mjs:128-132`).

| Subtype / direction | Payload and response | Use |
|---|---|---|
| `initialize` host→CLI | `hooks?`, `sdkMcpServers?:string[]`, `sdkMcpServerConfigs?`, `agents?`, `systemPrompt?`, `appendSystemPrompt?`, `supportedDialogKinds?`, `plugins?`; response contains commands, models, MCP state and pending requests (`sdk.d.ts:4218-4330`) | Handshake before work; repeat for reconnect. SDK `initializationResult()` consumes it; upstream capability probe reads commands/models (`ClaudeProvider.ts:~390-445`). |
| `can_use_tool` CLI→host | `tool_name:string`, `input:object`, `tool_use_id:string`, optional suggestions/reason/agent_id; success payload is permission result `{behavior:"allow",updatedInput?:object,updatedPermissions?:array}` or `{behavior:"deny",message:string,toolUseID?:string}` (`sdk.d.ts:4484-4535,2405-2420`) | Upstream routes approval/user input through `canUseToolEffect` (`ClaudeAdapterV2.ts:8098-8328`). Deny if owning turn vanished. |
| `hook_callback` CLI→host | `callback_id`, `input`, `tool_use_id?`; response hook output (`sdk.d.ts:4870-4890`) | Only if hooks registered; upstream primarily relies on user settings/hooks. |
| `interrupt` host→CLI | no required payload; receipt may contain `still_queued`, and `cancelled` with `cancel_queued:true` (`sdk.d.ts:4342-4362`) | Abort active turn; queued sends can survive. Feature-detect capabilities. |
| `set_permission_mode`, `set_model`, `apply_flag_settings`, `stop_task` host→CLI | `mode:string`, `model?:string|null`, settings object, or `task_id:string`; ack (`sdk.d.ts:3644-3650,4780-4807`) | Upstream live query exposes all four (`ClaudeAdapterV2.ts:863-930`). |
| `mcp_message` both directions | `server_name:string`, `message:JSON-RPC object`; response `mcp_response?` (`sdk.d.ts:4439-4450`) | In-process MCP server bridge; full MCP initialize/tools/list/tools/call must be supported. `mcp_set_servers`, `mcp_status`, `mcp_reconnect` are related controls. |
| `request_user_dialog` CLI→host | `dialog_kind:string`, `payload:object`, `tool_use_id?`; result dialog-specific | Upstream supports `resume_return` and maps it into an AskUserQuestion approval (`ClaudeAdapterV2.ts:8330-8385`). |

The SDK additionally exposes `rewindFiles`, `forkSession`, `getMcpStatus`, `setMcpServers`, `setMaxThinkingTokens`, `supportedModels`, and `supportedCommands`. Upstream’s live runner uses prompt offer/stream, interrupt, model/mode/flag changes, stop task, MCP registration, session fork, and a subagent launch lookup through CLI session storage (`ClaudeAdapterV2.ts:408-492,735-940`). Preserve the full control union from `sdk.d.ts:4693` as an open enum, implementing the used requests first.

## Turn sequence

```mermaid
sequenceDiagram
  participant H as Turin
  participant C as claude CLI
  H->>C: spawn stream-json, open stdin
  H->>C: control_request initialize
  C-->>H: control_response initialize; system/init
  H->>C: user prompt (NDJSON)
  C-->>H: stream_event / assistant tool_use
  C->>H: control_request can_use_tool
  H-->>C: control_response allow or deny
  C-->>H: user tool_result; assistant; result
  H->>C: control_request interrupt (if stopped)
  C-->>H: control_response receipt; result
  H->>C: new process --resume=SESSION (after restart)
```

## Errors and Rust implementation notes

Use `tokio::process`, `tokio_util::codec::LinesCodec` or bounded `BufReader::lines`, `serde_json::Value` for unknown fields, `uuid` for local request IDs, and `tokio::sync::oneshot` keyed by request ID. Serialize all stdin writes through one task to avoid interleaved lines. Bound line/frame size explicitly, but allow large MCP and image payloads. Treat EOF before `result`, nonzero exit, malformed JSON, `control_response.error`, and `result.is_error` as distinct failures. Keep stderr for diagnostics with secret redaction. Use `system/init.capabilities` for newer controls rather than inferring support from npm version. `can_use_tool` can originate in a subagent; use `agent_id`/tool use ID to route. `result` ends a turn, not the process or background work; root frames can race with wake/subagent frames, so preserve prompt UUID echo and parent tool IDs (`ClaudeAdapterV2.ts:~7900-8095`). Resume uses persisted CLI session ID; start fresh if resume is rejected. The SDK may be newer than the installed CLI: handshake and capability gating are mandatory.
