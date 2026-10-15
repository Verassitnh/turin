# OpenCode 2 HTTP and SSE protocol

## Versions and evidence

Upstream pins `@opencode/client@2.0.28` and also `@opencode-ai/sdk@1.15.13` (`upstream/pnpm-lock.yaml:4516-4519`). The **2.x adapter** uses `@opencode/client/effect`; the 1.x SDK is for the older provider path and must not supply 2.x endpoint/types (`upstream/packages/provider-opencode/src/server/v2/OpenCode2Client.ts:1-2`, `v2/adapter.ts:1-20`; older `server/adapter.ts`, `server/OpenCodeRuntime.ts`). This document uses the exact packed client’s generated route code `dist/chunks/service-probe-ecs5ftka.js:147-1040` and types `dist/promise/generated/types.d.ts`, cross-checked with the v2 adapter. The installed local CLI is `1.19.0`, so it cannot be used to validate the 2.x wire behavior live.

The packed `@opencode-ai/sdk@1.15.13` exports its own `createOpencodeClient` and `createOpencodeServer` (package `dist/index.d.ts`, `dist/server.d.ts`, `dist/v2/server.d.ts`); upstream’s older `OpenCodeRuntime.ts:5,675` uses its client. Its `dist/process.js:1-28` only handles child process stop/abort. It is a distinct generated API surface from `@opencode/client@2.0.28`; no 1.x route or event type should be inferred for the v2 adapter from that SDK.

## Transport, launch, and authentication

One `opencode serve --hostname=127.0.0.1 --port=PORT` process serves all sessions/directories for one provider instance. Upstream chooses a port, tracks the process group, reads the `server listening on URL` log, verifies readiness via authenticated `GET /api/info`, and holds a shared server until idle (`OpenCodeRuntime.ts:86-100,312-316,700-870`; `v2/OpenCode2Server.ts:1-17,145-205`). An external configured URL is used without spawning. The installed CLI confirms `serve` defaults to `127.0.0.1` and port `0` if not supplied. For local 2.x, upstream generates 32 random bytes encoded base64url and passes `OPENCODE_PASSWORD`; it removes inherited `OPENCODE_SERVER_PASSWORD` because the former wins (`OpenCode2Server.ts:47-67`). Send `Authorization: Basic base64(UTF8("opencode:" + password))` on **every** HTTP and SSE request (`OpenCode2Client.ts:16-18,151-185`). Avoid Latin-1 `btoa` for non-ASCII passwords. `/health` may return web UI HTML even on wrong version; use `/api/info` (`OpenCode2Server.ts:104-149`).

JSON HTTP endpoints send/receive camelCase-ish OpenCode schema fields, notably `sessionID`, `requestID`, `messageID`, and `inboxID` (capital `ID`). Generated client methods unwrap `data`; Rust must read actual HTTP status and response body. Route evidence is in the pinned client `service-probe-ecs5ftka.js:219-1039`.

| Method/path | Body/query / response; upstream use |
|---|---|
| `GET /api/info` | Authenticated `{version,…}` readiness (`OpenCode2Server.ts:103-149`). |
| `POST /api/session` | `{id?,parentID?,title?,agent?,model?,location?,metadata?,permissions?}` → `SessionInfo` with `id` (`v2/adapter.ts:3790-3835`). `GET /api/session/{sessionID}`, `GET /api/session/active` resume/reconcile. |
| `POST /api/session/{id}/prompt` | `{id:string,text:string,files?,agents?,skills?,metadata?,delivery?:"steer"|"queue",resume?}`; normal turn and steer (`adapter.ts:3708-3770,4095-4130`). The client-chosen id makes retries detectable. |
| `POST /api/session/{id}/command`, `/compact` | Slash command and compaction. Command lacks T3's prompt id, so adapter records prior message boundary (`adapter.ts:3719-3740`). |
| `POST /api/session/{id}/interrupt` | Stops current execution; terminal SSE `session.execution.interrupted` follows (`adapter.ts:4140-4175`). |
| `GET /api/session/{id}/permission`; `POST /api/session/{id}/permission/{requestID}/reply` | List pending and `{decision:"once"|"always"|"reject",message?}` → 204 (`service-probe-ecs5ftka.js:930-969`). |
| `GET /api/session/{id}/form`; `POST .../form/{formID}/reply`; `DELETE .../form/{formID}` | Tool question form lifecycle (`service-probe-ecs5ftka.js:550-590`; `adapter.ts:2910-2930,4280-4305`). |
| `GET /api/session/{id}/message` | Ordered/paged history for replay and reconnect (`adapter.ts:2760-2800,3260-3315`). |
| `POST /api/session/{id}/fork`, `/move`, `/agent`, `/model`; `PATCH /api/session/{id}` | Fork before boundary, change directory/agent/model, update title/permissions (`adapter.ts:3100-3130,3860-4025,4410-4470`). |
| `POST /api/session/{id}/revert/stage`, `/revert/commit`; `GET /api/session/{id}/inbox`, `DELETE /inbox/{inboxID}` | Rollback and queued steer cleanup (`adapter.ts:3200-3255`). |
| `GET /api/model`, `/api/skill`, `/api/command` | Model/skill/command inventory (`adapter.ts:~3030-3070,3720-3750`). |

## SSE event stream and message catalogue

Subscribe to `GET /api/event` with `Accept: text/event-stream`, Basic auth, **before** sending a prompt. Server emits SSE frames with JSON in `data:`; the `event:` name is not the application discriminator. JSON has `{id:string,created:number,type:string,location?,durable?,data:object}`. Durable events include `{aggregateID:string,seq:number,version:number}`. The pinned union `V2Event` has many cases (`types.d.ts:3418`); parse `type` as an open string and retain unknown data. The v2 wrapper decodes each frame separately because the generated strict `event.subscribe` stream dies on a newer event (`OpenCode2Client.ts:65-151`). OpenCode writes `: heartbeat` comments every 10–15 seconds; upstream declares a stream silent after 45 seconds (`:97-106`).

| `type` | Key `data` fields / role |
|---|---|
| `session.execution.started` | `sessionID`; starts execution. `succeeded`, `failed` (`error:{type,message,…}`), `interrupted` (`reason:"user"|"shutdown"|"superseded"|"inactivity"`) are terminal (`types.d.ts:1471-1540`; `adapter.ts:2350-2370`). |
| `session.text.started/delta/ended`, `session.reasoning.started/delta/ended` | Delta: `{sessionID:string,assistantMessageID:string,ordinal:number,delta:string}`; ended carries final text. Adapter emits streamed text/reasoning (`types.d.ts:2115-2150`; `adapter.ts:2240-2260`). |
| `session.tool.input.started/delta/ended`, `session.tool.called/progress/success/failed` | Input delta: `{sessionID:string,assistantMessageID:string,id:string,delta:string}`. Called: `{sessionID,assistantMessageID,id,input:object,executed:boolean,state?}`. Success/failure carry content/metadata or structured error; subagent child `sessionID` may appear in metadata (`types.d.ts:1835-1890,2150-2180`; `adapter.ts:2260-2335`). |
| `session.step.started/streamed/ended/failed` | Ended: `{sessionID:string,assistantMessageID:string,finish:"stop"|"length"|"tool-calls"|"content-filter"|"error"|"unknown",cost:number,tokens:TokenUsageInfo,providerState?,snapshot?,files?}`. Adapter adds input plus cache reads/writes and output plus reasoning (`types.d.ts:1686-1725`; `adapter.ts:2330-2355`). |
| `permission.asked` | `{id,sessionID,action:string,resources:string[],save?,metadata?,source?,message?}` (`types.d.ts:974-985,2213-2240`). `permission.replied` has `{sessionID,requestID,reply:"once"|"always"|"reject"}`. |
| `form.created/replied/cancelled` | `data.form` or form ID; user input requests (`adapter.ts:2629-2649`). |
| `session.inbox.enqueued/delivered/cancelled/delivery.changed` | `sessionID,inboxID`; steer and background wake lifecycle (`types.d.ts:1417-1470,3304-3316`; `adapter.ts:2666-2696`). |
| `session.model.selected`, `session.message.content.updated`, `session.compaction.*` | Model switch, replayable content, compaction progress (`types.d.ts:1278-1295,3365-3384`; `adapter.ts:2653-2663,2315-2335`). |

Representative JSON event carried as one SSE `data:` line (illustrative IDs):

```text
: heartbeat

data: {"id":"evt_1","created":1700000000000,"type":"session.execution.started","durable":{"aggregateID":"ses_1","seq":1,"version":1},"data":{"sessionID":"ses_1"}}

data: {"id":"evt_2","created":1700000001000,"type":"permission.asked","data":{"id":"per_1","sessionID":"ses_1","action":"shell","resources":["pwd"]}}

```

## Turn, approval, interrupt, resume

```mermaid
sequenceDiagram
  participant H as Turin
  participant O as opencode serve
  H->>O: GET /api/info (Basic auth)
  H->>O: GET /api/event (SSE; keep open)
  H->>O: POST /api/session (or GET existing session)
  H->>O: POST /api/session/{id}/prompt
  O-->>H: session.execution.started; text/tool/step events
  O-->>H: permission.asked or form.created
  H->>O: POST permission/{requestID}/reply or form reply
  O-->>H: permission.replied; execution.succeeded/failed
  H->>O: POST /interrupt (if stopped)
  O-->>H: execution.interrupted
  H->>O: GET session/message + active + pending permission/form (on reconnect)
```

The adapter represents `acceptForSession` as session permission rules then replies `once`: OpenCode's `always` would persist a **project-wide** grant (`adapter.ts:4240-4280`). Decline uses `reject` with a message; without a message OpenCode may cancel the whole run (`:4280-4295`). Tool forms are a distinct response path. It tracks prompt IDs and inbox delivery so queued steers do not settle the wrong turn (`:240-270,2666-2696,4095-4130`). Resume reuses session ID; it may move directory, switch agent/model, and reapply MCP/config (`:3860-4025`).

## Error handling and Rust implementation notes

Use `tokio::process`, `reqwest` with UTF-8 Basic auth, `eventsource-stream` or an SSE decoder that preserves comments/heartbeats, `serde_json::Value` for open event variants, and `tokio::time` for the 45-second silence watchdog. Generated HTTP endpoints declare 400/401/404/409 and sometimes 500/503; map 401 to wrong password, 404 to missing session/request, 409 to active/prompt conflict, and keep response detail private if URL/password might leak (`OpenCode2Server.ts:104-149`). On stream loss, upstream reconnects, calls `/api/session/active`, backfills `/message`, relists pending permissions/forms, and fails sessions after bounded retries (`adapter.ts:2760-2995`). Preserve that sequence: SSE is volatile, not a durable replay subscription. Even an undecodable execution terminal must close the turn; upstream extracts only `type` and `data.sessionID` as fallback (`OpenCode2Client.ts:108-150`). After interrupt timeout, old tail events may still arrive; gate a new turn on its own execution start (`adapter.ts:2600-2625`). Use server `/api/info.version` to distinguish 2.x from 1.x; the installed binary's version can differ from either npm client pin.
