# Cursor Agent ACP over stdio — Turin transport

## Decision and evidence

Turin runs `cursor-agent acp` (installed probe: 2026.09.15-d2fe57e) as a bidirectional, newline-delimited ACP JSON-RPC server. Port `upstream/packages/effect-acp` and the core of `AcpAdapterV2`, adding a Cursor flavor; omit the ACP registry and Grok-specific branches. Upstream ec1313c8 has no Cursor ACP flavor, so this is a recorded transport deviation in PORTING.md. `cursor-agent acp --help` documents the server. Live `initialize {protocolVersion:1}` returned protocolVersion 1, `loadSession:true`, HTTP/SSE MCP, image prompts, and `sessionCapabilities.list:{}`; audio and embedded context were false. Authentication methods advertised only `cursor_login`. A non-destructive `session/new` succeeded in the QC probe and returned modes `agent`, `plan`, `ask`, model config options, and an `available_commands_update`. No paid prompt was sent.

| Method | Status and use |
|---|---|
| `initialize` | Send `clientInfo`, `protocolVersion:1`, `clientCapabilities:{_meta:{parameterizedModelPicker:true}}`; receive capabilities and auth methods. Keep client fs and terminal capabilities false. |
| `authenticate {methodId:"cursor_login"}` | Explicit user sign-in only (see Authentication flow below). Opens a browser and blocks; never sent on the normal path. Not the API-key path: keys are process-start credentials. |
| `session/new {cwd,mcpServers:[]}` | New session; returns sessionId, modes, models and configOptions. One process per Turin thread. |
| `session/load {sessionId,cwd,mcpServers:[]}`; `session/list` | Resume persisted session or enumerate sessions; `loadSession:true` and `list:{}` are advertised. |
| `session/prompt {sessionId,prompt:[...]}` | User turn. Text and image prompt content are supported. Stream `session/update`; terminal response has stopReason. |
| `session/cancel {sessionId}` | Cancel active prompt. |
| `session/set_mode`, `session/set_model`, `session/set_config_option` | Change mode/model; use config options for parameterized model selection. |
| `session/fork`, `session/resume`, `session/close` | Not supported; fork returned -32601 Method not found; resume and close also return -32601. Load is the resume operation. |
| `cursor/list_available_models` | Cursor extension (client→agent) returns model and per-model parameter matrix; use for catalog. Shape in Cursor extension methods below. |

Typical sequence: spawn; initialize; new or load (no `authenticate` on the normal path; map `authRequired` to "not signed in"); set mode/model/config; prompt; consume `session/update` text, tool-call status, plan, command and config updates; answer client-directed requests; await terminal prompt result. Route `session/request_permission` to Turin approval cards and return the selected ACP outcome, subject to the Cursor permission rules below. This gains interactive approval that the SDK adapter rejects (`provider-cursor/src/server/adapter.ts:2499`). Use upstream `AcpAdapterV2Flavor.registerExtensions` and `extractPermissionQuestion` seams for Cursor-specific events. Preserve open update variants.

## Cursor extension methods

The CLI bundle (`2698.index.js`, with ACP SDK 0.14.1 in `8096.index.js`) sends its extension calls through `extMethod`, which is a JSON-RPC **request** and carries an `id`. Register every handler below with `handleExtRequest`. A handler registered with `handleExtNotification` never fires: upstream effect-acp routes requests and notifications to separate tables and answers an unknown request with methodNotFound (`packages/effect-acp/src/agent.ts:297-313`). The CLI then logs the -32601 and the data is lost. None of these payloads carry a sessionId, so route them by process (one ACP process per Turin thread).

### `cursor/ask_question` (request)

Params:

```text
{toolCallId, title?, questions:[{id, prompt, options:[{id,label}], allowMultiple:boolean}]}
```

Result, one of:

```text
{outcome:{outcome:"answered", answers:[{questionId, selectedOptionIds:string[]}]}}
{outcome:{outcome:"skipped", reason?}}
{outcome:{outcome:"cancelled"}}
```

Map it to a Turin user-input card. The CLI reads `e.outcome.outcome`, so a flat reply such as `{outcome:"answered",answers:...}` is read as "User cancelled".

Fallback: the CLI falls back when ask_question gets any "unsupported" error (gRPC Unimplemented, "method not supported", or -32601), not only -32601. It sends one `session/request_permission` per single-select question that has options. The toolCallId is `<toolCallId>_q<i>`. Each option becomes `{optionId:id, name:label, kind:"allow_once"}`, plus `{optionId:"__ask_question_skip__", name:"Skip", kind:"reject_once"}`. Multi-select and option-less questions are dropped. If no question qualifies, the CLI rejects with "Client does not support ask_question extension". In ACP subagent mode, ask_question is always rejected.

### `cursor/create_plan` (request)

Params:

```text
{toolCallId, name?, overview?, plan:string, todos:[{id,content,status:"pending"|"in_progress"|"completed"|"cancelled"}], isProject?, phases?:[{name,todos}]}
```

Result, one of:

```text
{outcome:{outcome:"accepted", planUri?}}
{outcome:{outcome:"rejected", reason?}}
```

Any other value counts as cancelled. Map it to plan approval. If a plan is accepted without `planUri`, the CLI writes a local plan file and uses its path. Before `create_plan`, the CLI sends a standard ACP `plan` session/update with entries `{content, priority, status}`.

`create_plan` fallback (verified in cursor-agent 2026.09.15-d2fe57e, `2698.index.js`): on a `create_plan` error that matches the unsupported test used for ask_question (Unimplemented, "method not supported", "methodname(name=cursor/create_plan)", or -32601), the CLI writes a local plan and returns success even if the write fails (`planUri` may be `""`). For other errors it returns success only if the write produced a `planUri`; otherwise it returns "Failed to create plan: client extension failed and local fallback artifact could not be created."

Both request handlers are required for faithful UI behavior. If Turin does not register the `create_plan` handler, the CLI **auto-accepts** the plan through its local-file fallback, so no plan approval card is shown.

### `cursor/update_todos`, `cursor/task`, `cursor/generate_image` (requests, fire-and-forget)

Register all three with `handleExtRequest` and reply `{}`; the CLI ignores the result. They are sent once on `toolCallCompleted`, after the standard `tool_call_update`. Update plan/task/tool cards and image attachment state from their payloads, retaining unknown fields. Payloads:

```text
update_todos: {toolCallId, todos:[{id,content,status}], merge:boolean}
task: {toolCallId, description, prompt, subagentType:"computer_use"|"explore"|"video_review"|"browser_use"|"shell"|"vm_setup_helper"|"unspecified"|{custom:string}, model?, agentId?, durationMs?}
generate_image: {toolCallId, description, filePath?, referenceImagePaths?}
```

### `cursor/list_available_models` (client→agent request)

Params: `{}`. Result:

```text
{models:[{value, name, configOptions:[{id, name, description, category:"thought_level"|"model_config", type:"select", currentValue, options:[{value,name}]}]}]}
```

Each config option `id` is the configId to pass to `session/set_config_option`. The model selector's configId is `"model"`. The probed result matches this shape.

The older ACP mock has only `cursor/ask_question` and `cursor/list_available_models` stubs (`upstream/apps/server/scripts/acp-mock-agent.ts:2141,2401`), not a complete flavor.

## Model state, auth, and lifecycle

Set `clientCapabilities._meta.parameterizedModelPicker:true` at initialize. Without it, `session/set_model` and `session/set_config_option {configId:"model"}` accept only exact listed variant strings; bare model IDs and invented `[context=...,reasoning=...]` suffixes return -32602. Query `cursor/list_available_models` for valid parameter names/options, then map T3 ModelSelection options to individual `set_config_option` calls. The CLI synchronizes model changes to **all sessions in its process**, so launch a separate ACP process for each Turin thread. Model changes persist to `~/.cursor/cli-config.json` (`model`, `selectedModel`, `modelSelectionHistory`), changing the global Cursor default. This is a user-visible deviation from SDK per-send `{id,params}` model selection.

Parameterized mode also touches `~/.cursor/acp-config.json` (`selectedModelVariantId`), but not the way the setter suggests. `set_config_option {configId:"model"}` **clears** that key rather than writing it. `session/new` and `session/load` (via `initializeSessionState`) migrate a stored variant into the global cli-config model, then clear the key. This happens once, and only if a variants-mode client left the key set. Probed: initialize with `parameterizedModelPicker:true`, then `session/new`, `list_available_models` and an invalid `set_model`, changed `acp-config.json` from `{"selectedModelVariantId":"claude-opus-5-5[context=300k,effort=medium,fast=false]"}` to `{}`. Opening or loading a session in parameterized mode therefore rewrites the global model.

Deviation mitigation: snapshot and restore the cli-config model and `acp-config.json` around `session/new`, `session/load` and every model setter, not only `set_config_option`. Record it in the Cursor Deviations row.

### Credentials

ACP accepts `--api-key`/`CURSOR_API_KEY` and `--auth-token`/`CURSOR_AUTH_TOKEN` as process-start credentials, before `acp`. Each marks the process authenticated (`isAuthenticated` is computed at run start from the key, the auth token, or a stored login), so no `authenticate` call is needed. The API key is exchanged lazily: `loginWithApiKey` runs on first service use, and only when no stored token exists or the stored token expires within 300s. The exchanged tokens are then written to the user's **global** Cursor credential store, replacing the browser login. An auth token is persisted the same way.

A valid, non-expiring stored login takes precedence. The process then runs as that account and the key is ignored. With multiple instances that is a wrong-account risk.

Probed: an invalid key plus a stored login still completes `session/new` (sessionId returned). An invalid key with no login fails `session/new` with -32603 "Failed to initialize session services". Baseline with no key and no `authenticate` call also succeeded. Valid-key-only behaviour is **not live-tested** (no valid key available), so keep API-key auth unadvertised.

Mitigation to evaluate: isolate the credential store per spawned process, e.g. `AGENT_CLI_CREDENTIAL_STORE=memory` (untested) or a per-instance HOME. Pass an API key only when the user has configured one. Log the credential-store side effect in PORTING Deviations.

Use `cursor_login`, the only method ACP advertises, for explicit sign-in. Keep upstream keychain and usage-limit authentication separate. `--trust` and `--approve-mcps` matter for noninteractive `--print` probes; a QC ACP `session/new` worked in an untrusted temporary directory without them. Never pass `--force` or `--yolo` to `acp`: a force option swaps in a different approver (2698.index.js `{force:E=!1}=T`).

### Authentication flow

With no stored login, `authenticate {methodId:"cursor_login"}` runs `startLogin()`, opens a browser at the login URL, and `waitForResult` blocks until success or timeout. The poll (`/auth/poll`) can run up to 150 times with backoff to 10s, so the request can stay pending for about 20 minutes. Any methodId other than `cursor_login` gets invalidParams.

Cursor's method has no `type`, and upstream `AcpSessionRuntime.ts:2258-2300` and `2372-2378` auto-authenticate on authRequired by default. So the Cursor flavour must not use that default:

- Set `authenticateOnAuthRequired:false` for the Cursor flavour and for status probes.
- Map `authRequired` from `session/new`/`load`/`list` to "not signed in, run cursor-agent login". The CLI error text is "Authentication required. Please run 'cursor-agent login' first, then call authenticate() with methodId 'cursor_login'."
- Send `authenticate` only as an explicit user sign-in action, or right after a confirmed external login. `isAuthenticated` is captured at spawn. Once a login exists, `session/new`/`load`/`list` work without `authenticate`, and the call does not open a browser.

Probed result shapes:

```text
session/list -> {sessions:[{sessionId, cwd, title?, updatedAt}]}
```

### Permission outcomes (Cursor flavour)

Cursor's `allow-always` option calls `persistAllowlistEntry`, which appends `Shell(cmd)`, `Write(path)` or `Mcp(provider:tool)` to the global `~/.cursor/cli-config.json` `permissions.allow`. The upstream ACP core maps `acceptForSession` to `allow_always` (`provider-acp` adapter.ts:1074-1085), so a T3 "allow for session" click would become permanent and global. Choose one:

- Answer `allow-once` in the Cursor flavour, and keep a host-side per-thread session allowance (as Codex `sessionApprovals` does).
- Or keep the native option, labelled "Always allow (saved to Cursor config)".

Either choice is a PORTING Deviations entry. The user's own `permissions.allow` and `approvalMode` decide which tools Cursor asks about, so T3 runtimeMode cannot force an approval for allowlisted commands.

Spawn the process in its own process group (`process_group(0)` on Unix), drain stderr, and kill the group on shutdown or timeout. Do not rely on stdin EOF: after `session/new`, QC observed that ACP stayed alive beyond 10 seconds. The first session may launch a detached shared `index.js worker-server` daemon reparented to PID 1; Turin does not own or kill that daemon. Preserve session ID for `session/load` after process restart.

## Differences from the upstream SDK route

The upstream `@cursor/sdk@1.0.35` runs a local JS executor in the Node process. ACP loses `Agent.listRuns`/`cancelRun` active-run recovery, `Run.wait()` usage/durationMs detail, `autoReview`, sandbox enable and priming, `settingSources`, `enableAgentRetries`, SDK local store compatibility, `{id,params}` per-send model selection, and SDK `onDelta` shell/step granularity. ACP accepts API-key and auth-token credentials at process start, but valid-key behaviour is not live-tested and the credential store side effect applies (see Credentials). ACP gains interactive `session/request_permission`, session listing/load, and explicit image and HTTP/SSE MCP capability negotiation. Keep the non-agent `usageLimits.ts` HTTPS flow and keychain auth. Text generation can use a disposable ACP session in ask mode. These are capability deviations, not wire-compatible replacements.

## Changelog (QC wf_9dc37bfd-861)

- Item 1 (HIGH): ask_question/create_plan request params and result shapes (`outcome.outcome`), fallback rule and per-question permission mapping, planUri behaviour, plan session/update.
- Item 4 (MEDIUM): update_todos, task, generate_image as `handleExtRequest` replying `{}`; payloads; `cursor/list_available_models` result.
- Item 9 (MEDIUM): API-key and auth-token process-start auth (replaces "unverified"); stored-login precedence; global credential-store side effect; session/list shape; resume/close -32601.
- Item 10 (MEDIUM): `authenticate` is an explicit sign-in only; `authenticateOnAuthRequired:false`; authRequired mapping; methodId check.
- Item 11 (MEDIUM): `allow-always` persists to global Cursor allowlist; Permission outcomes subsection; never `--force`/`--yolo`.
- Item 15 (LOW): parameterized mode clears (does not write) `acp-config.json selectedModelVariantId`; session/new and session/load migrate it; snapshot/restore must wrap session/new, session/load and setters.
- PORTING.md Deviations rows needed (not edited here, per brief): Cursor credential-store side effect; Cursor permission outcome choice; Cursor acp-config snapshot/restore.
- QC wf_6497dded-82d item 1 (LOW): UNVERIFIED create_plan note replaced with the verified fallback rule; auto-accept named explicitly.
