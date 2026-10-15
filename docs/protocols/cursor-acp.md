# Cursor Agent ACP over stdio — Turin transport

## Decision and evidence

Turin runs `cursor-agent acp` (installed probe: 2026.09.15-d2fe57e) as a bidirectional, newline-delimited ACP JSON-RPC server. Port `upstream/packages/effect-acp` and the core of `AcpAdapterV2`, adding a Cursor flavor; omit the ACP registry and Grok-specific branches. Upstream ec1313c8 has no Cursor ACP flavor, so this is a recorded transport deviation in PORTING.md. `cursor-agent acp --help` documents the server. Live `initialize {protocolVersion:1}` returned protocolVersion 1, `loadSession:true`, HTTP/SSE MCP, image prompts, and `sessionCapabilities.list:{}`; audio and embedded context were false. Authentication methods advertised only `cursor_login`. A non-destructive `session/new` succeeded in the QC probe and returned modes `agent`, `plan`, `ask`, model config options, and an `available_commands_update`. No paid prompt was sent.

| Method | Status and use |
|---|---|
| `initialize` | Send `clientInfo`, `protocolVersion:1`, `clientCapabilities:{_meta:{parameterizedModelPicker:true}}`; receive capabilities and auth methods. Keep client fs and terminal capabilities false. |
| `authenticate {methodId:"cursor_login"}` | Use existing Cursor login / `cursor-agent login`. Do not assume API-key ACP auth from this advertisement. |
| `session/new {cwd,mcpServers:[]}` | New session; returns sessionId, modes, models and configOptions. One process per Turin thread. |
| `session/load {sessionId,cwd,mcpServers:[]}`; `session/list` | Resume persisted session or enumerate sessions; `loadSession:true` and `list:{}` are advertised. |
| `session/prompt {sessionId,prompt:[...]}` | User turn. Text and image prompt content are supported. Stream `session/update`; terminal response has stopReason. |
| `session/cancel {sessionId}` | Cancel active prompt. |
| `session/set_mode`, `session/set_model`, `session/set_config_option` | Change mode/model; use config options for parameterized model selection. |
| `session/fork`, `session/resume`, `session/close` | Not supported; fork returned -32601 Method not found. Load is the resume operation. |
| `cursor/list_available_models` | Cursor extension returns model and per-model parameter matrix; use for catalog. |

Typical sequence: spawn; initialize; authenticate when needed; new or load; set mode/model/config; prompt; consume `session/update` text, tool-call status, plan, command and config updates; answer client-directed requests; await terminal prompt result. Route `session/request_permission` to Turin approval cards and return the selected ACP outcome. This gains interactive approval that the SDK adapter rejects (`provider-cursor/src/server/adapter.ts:2499`). Use upstream `AcpAdapterV2Flavor.registerExtensions` and `extractPermissionQuestion` seams for Cursor-specific events. Preserve open update variants.

## Cursor extension methods

The CLI bundle sends client-directed requests `cursor/ask_question` and `cursor/create_plan`. The former includes questions/choices and waits for `{outcome:"answered",answers:...}`; map it to a Turin user-input card. The latter carries a proposed plan and waits for `{outcome:"accepted",planUri?:string}`; map it to plan approval. `cursor/update_todos`, `cursor/task`, and `cursor/generate_image` arrive as extension notifications: update plan/task/tool cards and image attachment state from their payloads, retaining unknown fields. If `ask_question` gets -32601, Cursor falls back to one `session/request_permission` per question and loses multi-select. If `create_plan` gets -32601, it writes a local plan and auto-accepts; therefore both request handlers are required for faithful UI behavior (QC live bundle analysis, `2698.index.js`/`8096.index.js`). The older ACP mock has only `cursor/ask_question` and `cursor/list_available_models` stubs (`upstream/apps/server/scripts/acp-mock-agent.ts:2141,2401`), not a complete flavor.

## Model state, auth, and lifecycle

Set `clientCapabilities._meta.parameterizedModelPicker:true` at initialize. Without it, `session/set_model` and `session/set_config_option {configId:"model"}` accept only exact listed variant strings; bare model IDs and invented `[context=...,reasoning=...]` suffixes return -32602. Query `cursor/list_available_models` for valid parameter names/options, then map T3 ModelSelection options to individual `set_config_option` calls. The CLI synchronizes model changes to **all sessions in its process**, so launch a separate ACP process for each Turin thread. Model changes persist to `~/.cursor/cli-config.json` (`model`, `selectedModel`, `modelSelectionHistory`) and `~/.cursor/acp-config.json` (`selectedModelVariantId`), changing the global Cursor default. This is a user-visible deviation from SDK per-send `{id,params}` model selection.

The CLI's `--api-key` and `CURSOR_API_KEY` are accepted syntactically before `acp`; both produced a successful initialize in an isolated-home probe. With a deliberately invalid key, `session/new` returned -32603 “Failed to initialize session services” in both cases. Thus usable API-key authentication over ACP is **unverified**; do not advertise it as supported. Use `cursor_login`, the only method ACP advertises. Keep upstream keychain and usage-limit authentication separate. `--trust` and `--approve-mcps` matter for noninteractive `--print` probes; a QC ACP `session/new` worked in an untrusted temporary directory without them. Do not infer that `--sandbox` or `--force` print flags configure ACP policy without a probe.

Spawn the process in its own process group (`process_group(0)` on Unix), drain stderr, and kill the group on shutdown or timeout. Do not rely on stdin EOF: after `session/new`, QC observed that ACP stayed alive beyond 10 seconds. The first session may launch a detached shared `index.js worker-server` daemon reparented to PID 1; Turin does not own or kill that daemon. Preserve session ID for `session/load` after process restart.

## Differences from the upstream SDK route

The upstream `@cursor/sdk@1.0.35` runs a local JS executor in the Node process. ACP loses `Agent.listRuns`/`cancelRun` active-run recovery, `Run.wait()` usage/durationMs detail, `autoReview`, sandbox enable and priming, `settingSources`, `enableAgentRetries`, SDK local store compatibility, `{id,params}` per-send model selection, and SDK `onDelta` shell/step granularity. API-key auth remains unverified for ACP. ACP gains interactive `session/request_permission`, session listing/load, and explicit image and HTTP/SSE MCP capability negotiation. Keep the non-agent `usageLimits.ts` HTTPS flow and keychain auth. Text generation can use a disposable ACP session in ask mode. These are capability deviations, not wire-compatible replacements.
