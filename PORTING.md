# Turin porting guide

Turin is a Rust/GPUI port of [T3 Code](https://github.com/pingdotgg/t3code):
**t3 fork but without all the bullshit.**

Upstream reference: `pingdotgg/t3code@ec1313c8f8333292506f05a355bb72d308a455d8`,
checked out at `./upstream` (symlink, gitignored).

## Ground rules

1. **Translate, don't redesign.** Upstream logic is the spec. Keep its control
   flow, state machines, names, constants, edge-case handling and comments that
   explain *why*. If upstream does something odd, assume it's load-bearing until
   proven otherwise.
2. **One upstream file → one Rust module** where practical, snake_cased
   (`ProviderTurnStartService.ts` → `provider_turn_start_service.rs`). Every
   module starts with `//! Port of upstream/<path>` so it can be diffed later.
3. **Cut at the seams, not in the middle.** Drop whole features from the cut
   list below. Inside kept code, keep small features even if unused; they can
   be trimmed later.
4. **Fix bugs and perf issues you find**, and record each one in the
   *Deviations* table at the bottom (file, what upstream did, what Turin does,
   why). Same for any place you could not port faithfully.
5. **Port tests for pure logic** (`*.logic.test.ts`, reducers, parsers,
   projections) as `#[cfg(test)]` modules. Skip tests that only exercise the
   Effect runtime, RPC transport, or cut features.

## No client/server split

Upstream is a server (`apps/server`) plus clients that talk to it over a
WebSocket Effect-RPC protocol (`packages/contracts/src/rpc.ts`,
`packages/client-runtime`). Turin is one process: the GPUI app owns the
orchestrator directly. Consequences:

- RPC handlers in `ws.ts` / `*/http.ts` become plain `async fn` methods on the
  owning service. Skip auth, scopes, DPoP, permissions, wire projection,
  bounded snapshot transport, and stream budgets.
- `packages/client-runtime` state (`state/*`) is the UI's view model. Port the
  reducers (they're the read model the UI expects), feed them in-process
  events instead of WS frames.
- Persisted JSON (SQLite rows, event payloads, settings files) keeps the
  upstream shape so a T3 database can be imported.

## Effect → Rust mapping

Revised after QC `wf_6ce02f2f-38c` (docs/qc/). Helpers live in
`turin_contracts::base_schemas`; use them, never hand-roll equivalents.

### Schemas (wire/DB shape must match upstream byte for byte)

| Effect Schema | Rust |
|---|---|
| `Schema.Struct` | `#[derive(Serialize, Deserialize)] #[serde(rename_all = "camelCase")]` struct |
| `Schema.Literals([...])` | enum, exact `#[serde(rename = "...")]` per variant |
| tagged unions (`_tag` / `type` discriminator) | `#[serde(tag = "...")]` enum |
| `ForwardCompatibleUnion(members, tag)` | `ForwardCompatible<T>` + `impl TaggedUnion for T` (+ a test calling `assert_known_tags`). **Never** `#[serde(other)]` or `untagged` catch-alls: they swallow broken known payloads and re-encode unknowns, both of which upstream forbids |
| `ForwardCompatibleUnionArray` | `#[serde(with = "forward_compatible_union_array")] Vec<T>` |
| `ForwardCompatibleArray` | `#[serde(with = "forward_compatible_array")] Vec<T>` |
| `optional`/`optionalKey(ForwardCompatibleArray(X))` | `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_array::optional")] Option<Vec<X>>` (same for `forward_compatible_union_array::optional`; landing in T00c) |
| `hasUnknownUnionTag` | `has_unknown_union_tag` |
| `Schema.optional(X)` / `Schema.optionalKey(X)` | `Option<X>` + `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `Schema.NullOr(X)` (required key) | `Option<X>`, no skip (encodes `null`) |
| `optional`/`optionalKey(NullOr(X))` (absent ≠ null) | `Option<Option<X>>` + `#[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]`. absent = unchanged/inherit, `null` = clear. Plain `Option<Option<_>>` without `with` silently collapses null |
| `X.pipe(withDecodingDefault(() => v))` | non-`Option` field + `#[serde(default = "default_<field>")]` returning upstream's exact `v`; **no** `skip_serializing_if` (upstream always encodes the key). Bare `#[serde(default)]` only when `v == Default::default()`. Applies to struct/record/array defaults too |
| `ForwardCompatibleOptional(X)` | `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional")] Option<X>` |
| `ForwardCompatibleOptional(NullOr(X))` | `Option<Option<X>>` + `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional_nullable")]` |
| `ForwardCompatibleNullable(X)` | `#[serde(default, with = "forward_compatible_nullable")] Option<X>` (encodes `null`) |
| `OmittedWhenNull(X)` | `#[serde(default, skip_serializing_if = "Option::is_none", with = "omitted_when_null")] Option<X>` |
| `TrimmedString` / `TrimmedNonEmptyString` | same-named types; trimming uses `js_trim` (JS `trim()` semantics, not Rust `str::trim`) |
| branded ids (`ThreadId`, …) | declare in the `id!` list in base_schemas.rs (opaque `Arc<str>` newtype with `new`/`random`/`as_str`). Never hand-write id newtypes |
| `Schema.Number` | `f64` (fractional values exist, e.g. `costUsd`) |
| `Schema.Int` / `NonNegativeInt` | `i64` / `u64` |
| `PositiveInt` | `PositiveInt` (validating newtype, rejects 0) |
| `Schema.Record(K, V)` | `IndexMap<K, V>` or `serde_json::Map` (keeps key order; never `HashMap` for persisted data) |
| `IsoDateTime` (`Schema.String`) | `String`, passed through untouched |
| `DateTimeUtc` / `DateTimeUtcFromString` | `UtcDateTime`: always encodes `YYYY-MM-DDTHH:MM:SS.sssZ`. Upstream compares these as text (`ORDER BY created_at`), so format drift misorders rows. `IsoDateTime` and `*_at` columns whose upstream schema is `IsoDateTime` stay `String` |
| `Schema.TaggedError` | `thiserror` enum variant, same name |

### Services and effects

| Effect | Rust |
|---|---|
| `Effect.gen` / `Effect<A, E, R>` | `async fn … -> Result<A, E>` |
| `Context.Service` + `Layer` | plain struct; deps passed to `new()` as `Arc<…>`. Trait only when upstream has multiple implementations (provider adapters, VCS drivers, source-control drivers) |
| `Ref` / `SynchronizedRef` | `parking_lot::Mutex` (sync sections) / `tokio::sync::Mutex` (only if held across `.await`) |
| `Deferred` | `tokio::sync::oneshot` (single) / `watch` (many readers) |
| `Semaphore.make(n)` + `withPermits` | `Arc<tokio::sync::Semaphore>`; `acquire_owned` when the permit outlives the scope |
| `Effect.forEach/all(…, {concurrency: n})` | `stream::iter(..).map(f).buffered(n)` (ordered) / `buffer_unordered(n)` if upstream discards order; `"unbounded"` → `try_join_all`; no option → sequential loop |
| `PubSub.unbounded` | per-subscriber `mpsc::unbounded_channel` fan-out (publishing never waits), or `broadcast` **with** an explicit `Lagged` handler that replays from the store |
| `PubSub.sliding(n)` | `broadcast::channel(n)` |
| `Queue.unbounded` / `.bounded(n)` / `.dropping(n)` / `.sliding(n)` | `mpsc::unbounded_channel` / `mpsc::channel(n)` / `try_send` on bounded / `VecDeque` + `Notify` |
| `Stream` | `futures::Stream` |
| `Fiber` / `forkScoped` / `forkDaemon` | task owned by a `JoinSet` or `tokio_util::task::AbortOnDropHandle` tied to its owner, plus `CancellationToken` for cooperative stop. **Never drop a bare `JoinHandle`** (it detaches, unlike Effect's scoped interruption) |
| `Effect.timeout` | `tokio::time::timeout`; on timeout the underlying work must actually stop (abort task / kill process) |
| child processes | `tokio::process::Command` with `kill_on_drop(true)`; long-lived CLIs (providers, hooks) run in their own process group and the group is killed on timeout/shutdown |
| `Schedule` / retries | explicit loop with the same delays and caps |
| `@effect/sql` SQLite | `turin_persistence::Db`: one writer connection (upstream: single writer behind `Semaphore(1)`) + a read-only reader connection, upstream PRAGMAs verbatim. `db.write(|tx| …)` runs a whole transaction in one blocking call; stores take `&Transaction`/`&Connection` so several compose in one transaction (upstream `sql.withTransaction`). Async work (publishing) happens after commit |

**Rules:** never do a bounded `send().await` or any `.await` while holding a
DB transaction or a sync lock.

**Async-first (user rule).** Everything that can be async is async: all IO
(processes, network, SQLite via `Db`, filesystem scans, git) runs on tokio or
gpui's background executor, never on the UI thread; independent work runs
concurrently (`join!`/`buffer_unordered` per the table above) instead of in
sequence unless upstream's ordering is semantically required. UI renders from
cached view models and never awaits.

Async runtime: tokio for all backend work. The GPUI app spawns a multi-thread
tokio runtime at startup and bridges results into entities via channels.

## Module map

| Turin crate | Upstream source |
|---|---|
| `contracts` | `packages/contracts/src` (kept files only, see below) |
| `shared` | `packages/shared/src` (kept files only) |
| `persistence` | `apps/server/src/persistence` (SQLite, migrations, event store) |
| `git` | `apps/server/src/{git,vcs,checkpointing}`, `apps/server/src/workspace` |
| `source-control` | `packages/source-control-{core,github}`, `apps/server/src/{sourceControl,pullRequest}` |
| `provider-core` | `packages/provider-core`, `apps/server/src/provider` (registry, catalog, auth for kept providers) |
| `codex-app-server` | `packages/effect-codex-app-server` (generated protocol types + stdio JSON-RPC client) |
| `provider-codex` | `orchestration-v2/Adapters/CodexAdapterV2.ts`, `provider/Codex*` |
| `claude-protocol` | replaces `@anthropic-ai/claude-agent-sdk` (pinned version): `claude` CLI launch flags, stream-json frames, control protocol, SDK-local helpers (transcript fork etc.) |
| `acp` | `packages/effect-acp` (Agent Client Protocol JSON-RPC client) |
| `provider-claude` | `orchestration-v2/Adapters/ClaudeAdapterV2.ts`, `provider/Claude*`; the TS Agent SDK is replaced by speaking the `claude` CLI stream-json control protocol directly (same protocol the SDK uses) |
| `provider-opencode` | `packages/provider-opencode`: version probe + both generations (v1 `server/adapter.ts`+`OpenCodeRuntime.ts` for 1.x, which is what users have installed today; v2 `server/v2/*` for ≥2) |
| `provider-cursor` | `packages/provider-cursor` minus the in-process `@cursor/sdk` runtime; transport is `cursor-agent acp` via `acp` + upstream `AcpAdapterV2` (new Cursor flavor, see Deviations) |
| `orchestration` | `apps/server/src/orchestration-v2` (minus wire/transport files), `apps/server/src/project` |
| `hooks` | **new**, replaces thread launch / worktree creation (see below) |
| `app` | `apps/web/src` → GPUI, `packages/client-runtime/src/state` reducers. Binary `turin`. |

### Contracts: keep

`baseSchemas chatAttachment checkpointDiff composerContext composerContextClipboard
assistantCitations editor git keybindings model modelSelection orchestrationDispatch
orchestrationProject orchestrationV2 project provider providerInstance providerPolicy
providerRuntime providerSetup providerUsageLimits pullRequest pullRequestReviewGuide
review settings sourceControl threadPullRequest threadSearch threadTitle usage
usageLimitSourceId vcs filesystem environment(local fields only) agentSessions`

### Contracts: cut

`auth relay relayClient remoteAccess environmentHttp rpc clientRpcPermissions ipc
device preview previewAutomation browserImport browserProfile desktopBrowser
desktopBootstrap desktopAppActivation terminal mcpApps orchestratorMcp worktreeMcp
worktreeSetup threadMetadataMcp scheduledTask secretRequest resourceTelemetry
background acpRegistry t3ProjectFile projectClone server(keep only settings-ish
config types referenced by kept code) applicationEvent assets`

When a kept type references a cut type, drop that field and note it in
Deviations.

## Cut list (whole features)

- T3 Connect, relay, cloud, remote access, pairing, auth/sessions, DPoP, Tailscale, SSH, WSL
- Mobile app, marketing site, Electron shell, auto-updates, telemetry, resource telemetry
- Every source-control host except GitHub (GitLab, Bitbucket, Azure DevOps, Forgejo, GitCafe)
- Every provider except Codex, Claude, OpenCode, Cursor (no ACP registry, Grok, Pi, Muse, Antigravity)
- Terminal / PTY, browser / preview / devices, HTML render, MCP apps, orchestrator MCP server
- File tree side panel, scheduled tasks, legacy v1 importer, project clone
- **Thread launch into new worktree / new thread spawning** → replaced by hooks

## Kept features (must match upstream look and feel)

- Sidebar: projects → threads, status indicators, snooze, drag, search, hover cards
- Thread view: messages timeline, markdown, tool cards, approvals, pending user input,
  plan follow-up, context window meter, usage limits, wrapping behaviour
- Composer: rich prompt editor, mentions, model picker, attachments, stash, queue/steer
- Diff: changed files tree, turn diffs, full thread diff, checkpoints and rollback
- Git: status, branches, commit/push flows (`GitWorkflowService`), checkpoints
- GitHub PRs: list + filters, detail (summary, timeline, code, checks, reviews, guide),
  thread↔PR linking, PR stacks, watch/sync reactors, review guides, files viewed
- Settings and keybindings with upstream defaults

## Hooks (new)

Replaces `ThreadLaunchService` / worktree setup. User-defined shell commands
that run on orchestrator events. Config in `~/.turin/hooks.toml` and
per-project `.turin/hooks.toml`:

```toml
[[hook]]
event = "thread.created"        # thread.created | turn.started | turn.completed
                                # | turn.failed | approval.requested | pr.linked
                                # | pr.checks.completed | git.committed
command = "./scripts/setup.sh"
cwd = "project"                 # project | thread (thread's workspace path)
blocking = false                # true: wait before continuing (thread.created only)
timeout_secs = 300
```

Hooks get event JSON on stdin and `TURIN_*` env vars (`TURIN_EVENT`,
`TURIN_PROJECT_ID`, `TURIN_THREAD_ID`, `TURIN_CWD`, …); inherited `TURIN_*`
vars are scrubbed first.

**Trust (QC wf_e8a1a7c1-d53).** Global `~/.turin/hooks.toml` is trusted.
Project `.turin/hooks.toml` never runs until the user approves it: approval is
stored outside the repo (`~/.turin/trusted-hooks.json`: canonical project root
→ SHA-256 of the exact file bytes). Any change (git pull, branch switch, an
agent editing the file) invalidates it; hot reload re-checks the hash and drops
untrusted project hooks while keeping global ones. Untrusted/changed project
hooks surface as a pending status the UI shows with an "Approve" action.
Rationale: upstream never auto-runs repo-supplied commands (`t3.json` scripts
are import-only).

**Return protocol.** A blocking `thread.created` hook may print, as its last
stdout line, a JSON object `{"worktreePath": "...", "branch": "..."}` (both
optional). The orchestrator then binds the new thread to that workspace, which
is how hooks replace upstream's launch-into-worktree flow. Malformed or absent
output = no change.

**Environment.** Hooks inherit the app's environment; PATH hydration from the
login shell (upstream `fixPath` / `readEnvironmentFromLoginShell`) happens once
at app startup (T05b/T22), not per hook.

## Deviations

| File | Upstream | Turin | Why |
|---|---|---|---|
| `assets/prod/black-macos-1024.png` | exported from Icon Composer GUI with the "macOS pre-Tahoe" preset | synthesized from upstream's macOS PNG (T3 glyph erased, T redrawn with upstream's gradient/edges); validated against an `actool` render. `--keep-macos-png` keeps a GUI export instead | Icon Composer's CLI has no pre-Tahoe preset |
| `assets/{dev,nightly}` | blueprint/nightly icon variants | not produced | single prod build; variants aren't trivial (grid, annotations) |
| `provider-cursor` (credentials) | SDK uses explicit apiKey / CURSOR_API_KEY / stored login per Agent instance | `cursor-agent acp` reads stored login with precedence over env/flags at process start and may write the global credential store during `authenticate` | CLI-owned auth; Turin only triggers `authenticate` explicitly and never with `authenticateOnAuthRequired`. docs/protocols/cursor-acp.md "Credentials" |
| `provider-cursor` (permission outcomes) | SDK path rejects interactive approvals (adapter.ts:2499) | ACP `session/request_permission` is answered: allow-once / labelled option; allow-always maps to Cursor's global allowlist; never launches with `--force`/`--yolo` | ACP exposes approvals; gaining them matches T3's approval UX for other providers |
| `provider-cursor` (acp-config) | n/a | snapshot/restore of `~/.cursor/acp-config.json` around per-thread model changes, scoped to keys Turin touches | limits ACP's global side effect (see model-selection row) |
| `provider-opencode` (v1 SSE) | v1 SDK reconnects 3s→30s, no watchdog/backfill | Turin applies the v2-style 45s raw-byte watchdog + manual reconnect + backfill to v1 too | one transport policy; avoids silent stalls on 1.x. docs/protocols/opencode.md v1 SSE |
| `provider-cursor` (ACP model selection) | SDK sends `{id, params}` per send; no global side effects | ACP `set_config_option`/`set_model` with `parameterizedModelPicker`; Cursor CLI persists the choice to `~/.cursor/cli-config.json` + `acp-config.json` and syncs all sessions in the process | ACP is the only stable transport. Mitigation: one `cursor-agent acp` process per thread; snapshot/restore user config if the side effect proves visible. QC wf_42618e43-71a item 14 |
| `provider-cursor` | runs `@cursor/sdk` local executor in-process (Node) | `cursor-agent acp` subprocess over ACP | Rust can't host the JS SDK and it has no stable wire protocol; ACP is Cursor's supported external protocol. Gains interactive approvals; loses SDK-only features (autoReview, run recovery via listRuns, per-send `{id,params}` model selection). QC wf_42618e43-71a |
| `contracts/base_schemas.rs` | `TrimmedNonEmptyString` trims only on decode/encode | trims at construction | no untrimmed state can exist, so `" "` can never encode to `""` |
| `contracts/base_schemas.rs` | `UnknownUnionMember { tag, value }` | adds `raw: Value` | lets callers render/log unknown members without re-reading the row |
| `contracts/base_schemas.rs` | `DateTimeUtc` keeps ms precision via JS `Date` | `UtcDateTime` truncates to ms on construction/parse | `Eq`/`Ord` agree with the wire form |
| `contracts/base_schemas.rs` | `PositiveInt` is a refined number | newtype over `NonZeroU64` with `new(u64) -> Option<Self>` | 0 unrepresentable |
| `contracts/base_schemas.rs` | `isUnknownUnionMember(value)` | no function; match `ForwardCompatible::Unknown` | Rust enums make it a pattern |
| `contracts/base_schemas.rs` | `PortSchema` | omitted | only used by cut features |
| `contracts/base_schemas.rs` | cut-feature exports (AuthSessionId, RpcClientId, SecretRef, DpopFailureReason, Client* enums) | omitted | features cut (PORTING cut list) |
| `apps/server/src/persistence/reconcileV2PreviewMigration.ts` | Rewrites preview databases that recorded `OrchestrationV2` as migration 53 or 54 so those files keep their ledger and still migrate forward. | Not called. A preview file whose recorded id `>= 55` has a different name, or whose later id would skip a V2 migration, fails closed in `assert_v2_migrations_can_run`. | Fresh databases and released-main files already use the current ids. Rewriting 53/54 would hide a schema this build does not contain. |
| `apps/server/src/persistence/OrchestrationEventStore.ts` | Decodes each payload with the orchestration-v2 contract schema. | Payloads stay `serde_json::Value`. Invalid JSON fails. Unknown event types fail a strict read and are skipped when `skip_unknown_event_types` is set. A known type must be a non-empty JSON object. | `turin-contracts` event schemas are not in this crate. Empty-object rejection stands in for "schema rejected a known payload" until those types land. Non-empty but schema-invalid JSON is accepted. |
| `apps/server/src/persistence/Errors.ts` | Decode failures name Effect schema issue tags and do not copy the rejected value. | Decode failures carry the serde error string. | There is no Effect schema layer to map. SQL failures still use `SQLITE({code}) {errstr}` and do not include the query text. |
| `packages/shared/src/nodeSqliteClient.ts` | Reader and checkpoint workers are separate Node worker threads. The checkpoint worker uses `wal_checkpoint(NOOP)` and stays off when SQLite is older than 3.51. | One process: a `SQLITE_OPEN_READ_ONLY` connection for reads, and a `std` thread for checkpoints, joined on `Db` drop. Same pragmas, 1000-frame passive threshold, 100ms one-shot idle flush, and writer autocheckpoint of 10000. The side connection is opened before that pragma is raised. The worker is not started when `sqlite_version()` is below 3.51. `:memory:` has no reader and no checkpoint thread. | A second connection cannot see an in-memory database. Bundled SQLite is 3.53.2 (`rusqlite` 0.40 / `libsqlite3-sys` 0.38), so file databases start the worker. Below 3.51 the writer keeps the default autocheckpoint. |
| `packages/shared/src/Sqlite.ts` | `PRAGMA optimize` on the upstream SQLite build does not create `sqlite_stat4`. | The same `PRAGMA optimize = 0x10012` runs. Bundled SQLite is compiled with `SQLITE_ENABLE_STAT4`, so optimize creates `sqlite_stat4` as well as `sqlite_stat1`. | Rebuilding SQLite without STAT4 would fork the bundled amalgamation. The extra stats table is unused by queries. |
| `apps/server/src/persistence/initializeV2Database.ts` | After the snapshot copy, orchestration lazily imports V1 transcripts. | Copies the committed V1 snapshot (WAL included, uncommitted rows excluded) and stops. | Transcript import depends on the orchestration importer, which is not this crate. |
| `apps/server/src/persistence/Migrations/009_ProviderSessionRuntimeMode.ts` | Intentional `Effect.asVoid` no-op, still recorded. | Empty script, id still inserted. | Same ledger outcome. |
| `apps/server/src/persistence/Migrations/006_*.ts` | Binds `DEFAULT_RUNTIME_MODE` (`full-access`) as a parameter. | The literal `'full-access'` is in the SQL file. | The extracted script has no parameter slot. The written value is the same. |
| `apps/server/src/persistence/Migrations/055_OrchestrationV2.ts` | `ApplicationEventSource` runs between thread-launch workflows and effect-cancellation, including a procedural copy. | The generated SQL has `-- TURIN_APPLICATION_EVENT_SOURCE --`. The runner executes the copy in Rust at that point. | Concatenating the TypeScript module would run its static receipt insert before the events it depends on exist. |
| `apps/server/src/persistence/*` timestamps | `*_at` values are UTC text `YYYY-MM-DDTHH:MM:SS.sssZ`. | Caller-supplied strings are stored as given. Values this crate generates use `chrono` `to_rfc3339_opts(SecondsFormat::Millis, true)`. SQL `strftime` expressions are unchanged. | `turin_contracts::UtcDateTime` is not on this branch yet. |
| `hooks` project `.turin/hooks.toml` | Upstream never runs commands from repo files; `t3.json` scripts are import-only (`resolveProjectScripts` reads settings only). | Project hooks are inert until the user approves the file: `~/.turin/trusted-hooks.json` maps canonical project root → SHA-256 of the exact bytes; checked on every reload. Pending/changed hooks surface as `HookUpdate::ProjectHooksPending`; `HookRunner::trust_project_hooks(reviewed_hash)` approves. Global `~/.turin/hooks.toml` is trusted. | New hot-reloaded repo-controlled shell execution would otherwise run on clone/pull/agent edit. QC wf_e8a1a7c1-d53 item 1. |
| `hooks` environment | `fixPath` / `readEnvironmentFromLoginShell` / `mergePathEntries` (+ launchctl fallback) hydrate `process.env.PATH` once at server startup (`os-jank.ts`). | `turin-hooks` inherits the app env and does no per-hook hydration. **Tracked:** port that hydration into `turin-shared` and call it once at app startup (T05b/T22). | Until then a Finder-launched app gives hooks `/usr/bin:/bin` (`gh`, `npm`, `bun` → exit 127). QC item 5. |
| `hooks` thread launch | `ThreadLaunchService` creates the worktree and binds the thread. | A blocking `thread.created` hook prints `{"worktreePath","branch"}` as its last stdout line; `DispatchSummary.thread_workspace` carries it and the orchestrator binds the thread. | Hooks replace launch-into-worktree (return protocol above). QC item 7. |
| `contracts/orchestration_v2.rs` | `OrchestrationV2RawProviderEvent.jsonRpcId`: `Schema.Number` | `serde_json::Number` (not `f64`) | an `f64` re-encodes integer id `5` as `5.0`; JSON-RPC ids are matched textually by providers |
| `contracts/orchestration_v2.rs` | `ORCHESTRATION_V2_WS_METHODS`, `OrchestrationV2RpcSchemas` | omitted | WS/RPC plumbing; Turin is one process (payload structs/errors are kept) |
| `contracts/orchestration_v2.rs` | `*Json` schema variants (`DateTimeUtcFromString`) | type aliases of the runtime types | `UtcDateTime` already decodes/encodes the same ISO strings; the Json variants add no other differences |
| `contracts/orchestration_v2.rs` | `OrchestrationV2UnknownThreadStreamEvent` is a fourth union arm | `OrchestrationV2ThreadStreamItem::UnknownEvent` variant with hand-written codec | decode-only arm needs the "known type must decode in full" filter; encoding it is an error |
| `contracts/settings.rs` `ClientSettings`/`ClientSettingsPatch` | `browserDefaultViewport` (`PreviewViewportSetting`), `DEFAULT_BROWSER_VIEWPORT` | field and constant omitted | `preview.ts` is cut; the key decodes away and is not written back on save |
| `contracts/settings.rs` `ClientSettings`/`ClientSettingsPatch` | `browserDefaultZoomFactor` (`PreviewZoomFactor`) | field omitted | `preview.ts` is cut |
| `contracts/settings.rs` `ClientSettings`/`ClientSettingsPatch` | `browserDefaultAppearance` (`PreviewAppearancePreference`) | field omitted | `preview.ts` is cut |
| `contracts/settings.rs` `ClientSettings`/`ClientSettingsPatch` | `browserProfiles` (`BrowserProfile`) | field omitted | `browserProfile.ts` is cut |
| `contracts/settings.rs` `ClientSettings`/`ClientSettingsPatch` | `browserDefaultProfileId` (`BrowserProfileId`, `DEFAULT_BROWSER_PROFILE_ID`) | field omitted | `browserProfile.ts` is cut |
| `contracts/settings.rs` `ServerSettings`/`ServerSettingsPatch` | `deviceHosts` (`SshDeviceHostConfigs`) | field omitted | `device.ts` is cut |
| `contracts/settings.rs` | `AntigravitySettings`, `AntigravityAuthMethod`, `ANTIGRAVITY_AUTH_METHODS` | omitted | Antigravity provider is cut |
| `contracts/settings.rs` | `requiredScopesForServerSettingsPatch`, `requiredScopesForProjectMutation` | omitted | auth scopes (`auth.ts`) are cut |
| `contracts/settings.rs` | `Schema.optionalKey(X)` rejects `null`; plain serde `Option<T>` would accept it as absent | file-local `optional_key` serde module on every non-nullable optional key | upstream tests require `{"wordWrap": null}` to fail; belongs in `base_schemas` if other modules want it |
| `contracts/settings.rs` | `Schema.Number` fields encode via `JSON.stringify` (`2`, never `2.0`) | `js_number` helpers write whole numbers as integers | keeps re-saved settings files byte-identical to the TS server's |
| `contracts/settings.rs` | `Schema.DurationFromMillis` accepts any number, including negative and fractional | `DurationFromMillis = u64` | a negative or fractional background interval is nonsense; the app never writes one |
| `contracts/settings.rs` | `UnifiedSettings = Omit<ServerSettings, …> & ClientSettings` flat object | `UnifiedSettings { server, client }` | avoids duplicating ~110 fields; never a wire shape |
| `contracts/settings.rs` | `resolveProjectProviderInstanceEnabled(settings, …)` takes `Pick<ServerSettings, …>` | takes the two maps (`providerInstances`, `projectSettingsOverrides`) | no structural subtyping in Rust |
| `contracts/settings.rs` | `Schema.annotateKey` / `makeProviderSettingsSchema` / `secretSettingsFields` on provider settings | `codex_settings_form_fields()`, `claude_settings_form_fields()`, `*_SETTINGS_FORM_ORDER`, `*_SECRET_SETTINGS_FIELDS` data tables; `decode_binary_path_setting` is the shared helper behind `makeBinaryPathSetting` | no schema reflection in Rust; the settings UI reads the tables instead |
| `apps/server/src/persistence/*` timestamps | Agent-event `occurredAt` is `DateTimeUtc`. Project `occurredAt`, receipt `acceptedAt`, and provider `lastSeenAt` are `IsoDateTime`. | Agent-event `occurred_at` is `UtcDateTime` and is stored with its canonical `YYYY-MM-DDTHH:MM:SS.sssZ` display. Project, receipt, and provider `*_at` stay caller-supplied strings. SQL `strftime` expressions are unchanged. | Only the agent-event field is `DateTimeUtc` upstream. The other timestamps are `IsoDateTime` pass-through. |
| `codex-app-server/src/generated/types.generated.rs` | Effect importer emits concrete field types for 2,328 protocol schemas | generated transparent serde `Value` wrappers with `TryFrom<Value>` validation against the pinned JSON Schema; method and namespace metadata are generated separately | `typify` 0.6.2 panicked in `merge.rs` on the aggregate pinned schema; wrappers preserve exact JSON including absent versus null fields while runtime validation enforces schema constraints at the client boundary |
| `codex-app-server/src/schema.rs` | Effect codecs may transform decoded objects and supply decoding defaults | JSON Schema validation accepts/rejects input without transforming it; valid input retains its JSON field shape through serde | avoids inventing transformations that the Codex JSON Schema does not specify; callers see the original wire object |
| `codex-app-server/src/errors.rs` | Effect `SchemaIssue` trees provide issue kinds, counts, and path depth, with opaque `cause` defects | Rust errors retain operation, method, request ID, JSON-RPC code/message/data, and a textual JSON Schema validation detail | `jsonschema` reports a different issue tree; exact Effect diagnostic metadata cannot be derived from it |
| `codex-app-server/src/replay.rs` | replay errors are tagged Effect errors containing scenario, cursor, label, and frame details; extra outbound frames can be observed until the scope closes | Rust replay errors expose cursor and relevant frame details, and the driver checks for extra frames for 10 ms after completion | no Effect scope/sink lifecycle in the Rust duplex driver; the bounded final check avoids leaving the replay task alive after a successful transcript |
| `codex-app-server/src/schema.rs` | Effect schema errors can embed the rejected value in `cause` only | `jsonschema` failures are reported with `ValidationError::masked`, so the instance text is replaced before it reaches an error string | the default `Display` includes the rejected value, which would copy secrets into logs and request errors |
| `codex-app-server/src/protocol.rs` | a failed stdout sink surfaces as an Effect defect | write failures are `Transport { operation: "write-output-stream" }` and terminate the session | upstream only names `read-input-stream` and `read-process-exit-status` |
| `codex-app-server/src/client.rs` | closing the Effect scope kills the child | `shutdown` sends `SIGTERM` to the process group, waits 2 seconds, then `SIGKILL`; `Drop` sends `SIGKILL`. `kill_on_drop` still applies | there is no Effect scope; the grace period lets the child exit before the group is killed |
| `codex-app-server/src/extensions.rs` | `revertCodexThread` lives in `apps/server` and returns the `thread/revert` or `thread/read` response directly | the same paging and response selection lives next to the protocol client. `thread/reverted` is not awaited | the wire rule is part of this crate so a later provider port cannot block on a notification the `thread/read` branch never sends |
| `crates/claude-protocol` launch env | Sets `process.env.CLAUDE_AGENT_SDK_VERSION` and injects an OTEL `traceparent` into the child env. | Child env only. `CLAUDE_CODE_ENTRYPOINT` and `CLAUDE_AGENT_SDK_VERSION` are set when absent. `NODE_OPTIONS` is always removed. `DEBUG` follows `Pe()` on `DEBUG_CLAUDE_AGENT_SDK`. The parent process env is not mutated and no trace context is injected. | Rust must not leak SDK bookkeeping into the host process. There is no OTEL context to forward. |
| `crates/claude-protocol` MCP | In-process `type: "sdk"` MCP servers are hosted inside the Node SDK. | Names and JSON configs are copied onto the initialize control request. No in-process server is started. | The crate speaks the CLI protocol. Hosting a JS MCP server is not possible here. Stdio, SSE, and HTTP configs still go over stdin via `set_mcp_servers`, which is how upstream keeps `Authorization` out of argv. |
| `crates/claude-protocol` stdout | The SDK reads newline-delimited JSON with no explicit byte cap. | `LinesCodec` rejects a line longer than 32MiB and the session emits `SessionEvent::Error`. | A hostile or corrupt CLI must not be able to grow memory without bound. Recorded fixtures and image payloads fit under the cap. |
| `crates/claude-protocol` malformed stdout | The SDK logs a bad JSON line and keeps reading. | The line becomes `SessionEvent::Error` and reading continues. A known type with a broken payload still stops the reader. | The host can see the bad line. Swallowing a broken known frame would hide a protocol bug. |
| `crates/claude-protocol` stderr | Stderr is streamed, and the last 2048 characters are attached to the process-exit error. | Stderr chunks are streamed as `SessionEvent::Stderr`. Exit errors do not include that tail. | The host already observed the chunks. Duplicating a truncated tail on exit would not match what the host saw. |
| `crates/claude-protocol` request ids | `Math.random().toString(36).substring(2, 15)`. | `uuid` v4 strings. | The QC Rust note for this protocol specifies uuid. Correlation only requires uniqueness. |
| `crates/claude-protocol` system prompt | QC prose said an omitted prompt is `""`. Shipped SDK 0.3.276 wraps a string, so the initialize body sends `systemPrompt: [""]`. | `[""]`, matching the shipped SDK. A preset still sends only `appendSystemPrompt`. | Source order is the pinned SDK, then upstream, then the QC doc. |
| `crates/claude-protocol` transcripts | `getSessionMessages` runs `UGe`, which synthesizes `queued_command` attachments into user messages. `CLAUDE_CODE_PROJECT_DIR_NAME` can override the project key when `CLAUDE_CONFIG_DIR` is set. | Compact-boundary rewrites, the main-chain walk, sibling `message.id` insertion, and fork parent rewiring are ported. Queued-command synthesis and the project-dir override are not. | Those two paths depend on further CLI-internal helpers and are not what the orchestration adapter reads on the turn path. |
| `crates/claude-protocol` fork | The provider refuses `forkSession` during an active turn. The SDK itself does not. | This crate forks the transcript file either way. | Active-turn refusal is orchestration policy and stays in the provider. |
| `crates/claude-protocol` shutdown | The SDK closes stdin, waits, sends SIGTERM to the child, then SIGKILL after 5s. | Same 2s then SIGTERM then 5s SIGKILL sequence, but signals go to the process group. `kill_on_drop` sends an immediate SIGKILL to the group. | Required by the Turin process rules so a dropped session cannot leave grandchildren behind. |
| `packages/shared/src/semver.ts` | `parseSemver` / `compareSemverVersions` live in `@t3tools/shared`. | Copied into `turin-provider-opencode` (`semver.rs`), including `parseGenericCliVersion` from `snapshotProbe.ts`. | `turin-shared` is still a stub. The adapter crate should switch to the shared copy when it lands. |
| `packages/provider-opencode` HTTP bodies | Session, message, model, permission, and form documents are SDK types. | Those documents are `serde_json::Value` inside the exact envelopes (`{data}`, `{data,cursor}`, `{location,data}`, raw `/api/info`). | `turin-contracts` has no OpenCode wire types yet. The adapter task reconciles the fields it reads. |
| `packages/provider-opencode` `@opencode-ai/sdk` | Client methods return `{ data: body }`. | Methods return the JSON body. v1 has no `{data}` envelope on the wire. | The wrapper is the SDK, not the server. |
| `packages/provider-opencode/src/server/v2/OpenCode2Client.ts` | `durable` on an event is an optional cursor object `{aggregateID,seq,version}`. `server.connected` may omit `created`. | Decoded that way. `created` and `durable` are optional. Unknown fields stay on `data` / `durable` as `Value`. | QC shorthand called `durable` a boolean. The 2.0.28 client types do not. |
| `packages/provider-opencode` v1 `event.subscribe` | The 1.x SDK does not time out a quiet socket. | Both generations use a 45s watchdog on the raw byte stream, before SSE parsing, so `: heartbeat` comments reset it. | The task requires the watchdog on each generation. Reconnect stays manual. |
| `packages/provider-opencode` JSON calls | Call sites pass their own abort timeouts. Health and `/api/info` use 5s. | The reqwest client has no request timeout. Health, `/api/info`, and the version probe set their own. Other JSON calls are untimed here. | A client timeout would cut the SSE body and race the adapter's per-call deadlines. |
| `packages/shared/src/shell.ts` `resolveSpawnCommand` | Windows resolves the binary through the shell. | The binary is spawned directly. Process groups are Unix-only; `pid <= 1` is not signaled. | This crate targets the Unix server path. There is no Windows checkout in the worktree. |
| `packages/provider-opencode/src/server/v2/adapter.ts` `sessionRules` | Appends MCP allow/deny rules after the path allows. | `session_permission_rules` stops after the path allows. | MCP servers are not part of this transport crate. |
| `packages/provider-opencode/src/server/OpenCodeServerLedger.ts` | Records the process group under `{stateDir}/opencode-servers/{pgid}.json` and reaps it on the next start when the owner is gone. | `OpenCodeServerLedger` does the same on macOS (`ps`) and Linux (`/proc`). `make` does not reap; `start` reaps in the background. `shutdown` and `Drop` signal the group, then delete the entry. A SIGKILL of this process leaves the file. | Windows is not recorded. |
| `packages/provider-opencode/src/server/OpenCodeRuntime.ts` startup readers | After the listening line, stdout/stderr keep being drained and discarded. | Same. Startup capture is capped at 64k Rust chars (upstream slices a JS string). | Stopping the readers fills OpenCode's stdio buffers and stalls the server. |
| `packages/effect-acp` | Generates Effect Schema from pinned `schema-v2.0.0-alpha.3` and `schema-v1.21.0`. The Zed `agent-client-protocol` crate is a possible substitute. | Vendors those four JSON documents under `crates/acp/schema/` and checks the unions the upstream tests cover. Wire payloads stay `serde_json::Value`. | The Zed crate does not carry both pinned generations, the v1/v2 compat layer, or extension registration. A Rust twin of the generated Effect schema would not change the wire. |
| `packages/effect-acp/src/protocol.ts` | Extension handlers run inline on the reader. | Inbound requests are answered on a side task, one at a time per connection, so an outbound `session/prompt` can stay open while `session/request_permission` or a `cursor/*` request is handled. | The reader has to keep consuming frames while a client call is pending. |
| `packages/effect-acp` | Generic ext-request and ext-notification hooks only. | Typed structs for `cursor/ask_question`, `cursor/create_plan`, `cursor/update_todos`, `cursor/task`, and `cursor/generate_image`. All five are requests. `ask_question` and `create_plan` results nest `outcome`. The other three reply `{}`. None of the params carry `sessionId`. | QC `wf_9dc37bfd-861` items 1 and 4. A flat `{outcome:"answered"}` is what the CLI treats as cancel. |
| `packages/effect-acp` | Does not spawn. `layerChildProcess` wraps an existing child. | `spawn` starts the child with `kill_on_drop` and `process_group(0)`, and signals that group on drop. | ACP does not exit on stdin EOF after `session/new`. Killing only the direct child leaves the group. |
| `contracts/vcs.rs` `VcsProcessExitError::from_process_exit` | `NotFound` detail is "Merge request not found." for command `glab`, "Pull request not found." for `gh` or `az`, else "VCS resource not found." | only `gh` gets "Pull request not found."; `glab`/`az` fall through to "VCS resource not found." | GitLab and Azure DevOps hosts are cut |
| `runtime/config.rs` | `ServerConfig` Effect service: OTLP, tracing, Tailscale, desktop fds, dev auth, static dir | only paths, mode, cwd, base dir, dev URL kept | observability, tailscale and desktop bootstrap are cut; `ensureServerDirectories` attachment sweep omitted until `attachmentStore` lands |
| `runtime/config.rs` | base dir `~/.t3`, env `T3CODE_HOME` | `~/.turin`, env `TURIN_HOME` | Turin owns its data dir. The `<base>/userdata/…` layout is kept, so a copied `~/.t3` reads unchanged |
| `runtime/support/{symlink,atomic_write,lenient_json}.rs` | `@t3tools/shared` helpers | copies of T05a's `turin-shared` modules | T05a is not merged to this branch. Dedupe into `turin-shared` once it lands |
| `runtime/shared_keybindings.rs` | `packages/shared/src/keybindings.ts` event matchers (`matchesKeybindingShortcut`, `resolveEventKeys`, …) | omitted | browser event handling belongs to the UI port |
| `runtime/shared_keybindings.rs` | shortcut key `space` is the `" "` key, same as upstream | parsed shortcuts and resolved rules carry a runtime `ShortcutKey` (any non-empty string) and `ResolvedKeybinding`, not contracts' `KeybindingValue` | contracts `KeybindingValue` trims and requires length ≥ 1, so `" "` cannot be held there. Contracts are untouched. Resolved rules that leave the runtime for the wire need a contracts change |
| `runtime/keybindings.rs` | `Keybindings` Effect service: Cache, PubSub, Semaphore, Scope, Deferred readiness | plain struct behind `Arc`; `parking_lot` locks; per-subscriber unbounded channels; `start()` spawns the reload task and must run inside a tokio runtime | PORTING Effect→Rust table; readiness is implied by `start()` returning |
| `runtime/keybindings.rs` | `ServerConfigIssue`, `ServerUpsertKeybindingInput`, `ServerRemoveKeybindingInput` from `contracts/server.ts` | defined in the runtime module | `server.ts` is not a kept contract; the shapes are kept byte-for-byte |
| `runtime/keybindings.rs` | `fs.watch(dir)` + `Stream.debounce(100ms)` | `notify` via `support::file_watch`, debounced by `next_debounced` | PORTING watcher rule: async, coalesced |
| `runtime/keybindings.rs` | `encodeWhenAst` and the encode side of `ResolvedKeybindingFromConfig` | omitted | no server-path caller reads resolved rules back to disk |
| `runtime/environment/server_environment.rs` | `ServerEnvironmentMachine` (`detectServerEnvironmentMachineKind`) | `platform.machine` is always absent | `ServerEnvironmentMachine.ts` is cut (T3 Connect / remote) |
| `runtime/environment/server_environment.rs` | capability flags for cut features: `serverSelfUpdate`, `serverInstallation`, `serverSelfUpdateProgress`, `desktopAppUpdate`, `agentActivityPublishing`, `serverBrowser` | left unset | self-update, desktop shell, cloud publish and the browser feature are cut |
| `runtime/environment/server_environment.rs` | `getDescriptor` re-reads the cloud publish opt-in per request | descriptor is built once at load | the only per-request input was cloud publish, which is cut |
| `runtime/environment/server_environment.rs` | `serverVersion` from `package.json` | `CARGO_PKG_VERSION` of `turin-runtime` | Turin has no `package.json`; the app version is set by the UI port if it needs to differ |
| `runtime/environment/server_environment.rs` | `makeTempFileScoped` temp file in `stateDir` | temp file named `.environment-id-<uuid>` in `stateDir`, removed after publish | same publish semantics: a hard link never replaces an existing file |
| `shared/server_settings.rs` | `deepMerge` treats arrays as objects (Effect `Predicate.isObject`) | arrays are replaced, never merged | an index-keyed merge would turn an array patch into an object that cannot decode. No upstream test relies on the merge |
| `shared/server_settings.rs` | `parsePersistedServerObservabilitySettings` and the `observability` URL fields | omitted | OTLP observability is cut |
| `shared/server_settings.rs` | `readCustomModelEntries` (name, capability decoding) | slugs only | only `fallbackTextGenerationModelSelection` reads custom models, and it needs slugs |
| `shared/server_settings.rs` | `isProviderAvailable` / `isProviderTextGenerationCapable` on `ServerProvider` | `ProviderStatus` trait | the provider snapshot lives in `provider-core`; `turin-shared` cannot depend on it |
| `shared/server_settings.rs` | `applyServerSettingsPatch` on typed objects | runs on `serde_json::Value` objects, converting at the edges | keeps upstream's generic `deepMerge` and null-clears rules instead of hand-written per-field merges |
| `runtime/settings_secrets.rs` | `sourceControlHostSecrets.ts`: secret fields of all six host definitions | GitHub `tokens` only | the other hosts are cut. The T07 `host_secrets.rs` owns the full table once it lands here |
| `runtime/settings_secrets.rs` | provider config secret fields from each driver's settings schema | contracts `CODEX_/CLAUDE_SECRET_SETTINGS_FIELDS` constants, none for cursor and opencode | the contracts constants are the Rust form of `secretSettingsFields(...)` |
| `runtime/environment/server_environment_label.rs` | direct OS calls | `HostProbe` trait, with `SystemHost` as the real implementation | makes the fallback order testable without the machine |
| `runtime/keybindings.rs` | `Keybindings::start` runs startup sync and watcher attach | same, plus `KeybindingsWatcher` drop guard | Rust needs an owner for the watcher task |
| `runtime/server_settings/` `apps/server/src/serverSettings.ts` (ported; its 51 test cases are pending) | legacy `providers`, `bitbucket`, `github` migrations and the project-fold run as JSON steps in `legacy.rs` | the persistence reads that feed them are `persistence_reader.rs` (SQL verbatim) | step 1: legacy migrations belong to the app-service side, not `turin-shared`. Secret copy and remove run through `provider-core`'s store |
| `runtime/server_settings/service.rs` | `Effect` service: `Cache`, `Semaphore`, `PubSub`, `Scope`, `Deferred` readiness | plain struct with a tokio mutex for writes, a parking_lot cache, and per-subscriber channels. `start()` returns a `SettingsWatcher` guard and must run inside a tokio runtime | PORTING Effect→Rust mapping |
| `runtime/server_settings/service.rs` | symlinked settings file: watch the link target again whenever the link changes | the link target is watched once, at `start()` | simpler; a repointed link needs a restart |
| `runtime/server_settings/service.rs` | `ServerSettingsError` is 152 bytes | `#![allow(clippy::result_large_err)]` on the module | the contracts wire type is kept, not boxed |
| `runtime/server_settings/service.rs` | `subscribeChanges`, `streamChanges` as `Stream` | `SettingsChanges::recv()` materializes each change on receive | the same values; `Stream` combinators are not needed here |
| `runtime/server_settings/service.rs` | `encodeServerSettingsJson` on the sparse object | sparse JSON is written from the stripped `Value` | upstream's encode would fill decoding defaults back in; the stripped form is the file contents |
| `runtime/server_settings/service.rs` | `migrated !== loaded` object identity | value comparison of the JSON before and after the migration chain | same write decision |
| `provider-core/credential_store.rs` (T08) | `ServerSecretStore.create` and `getOrCreateRandom` | `SecretStore::exclusive_create` and `get_or_create_random` as additive trait methods, with defaults on get and set. File and memory stores override `exclusive_create` atomically | step 3 of N1. T08 owns the trait; please review the defaults |
| `runtime/` `apps/server/src/storageCleanup.ts` (not ported) | cleanup service and pure policy helpers | not ported | the service needs orchestrator, git manager, terminal manager and workspace lease. Of the helper types, only `OrchestrationV2ThreadShell` is reachable. `GitBranchPullRequest`, `threadHasQueuedTurnStart` and `resolveWorktreeCleanup` are not. Deferred to T22 wiring |
| `provider-core` `ProviderAdapter` optional methods | a missing property means the capability is absent | the method exists and returns `None` | Rust traits cannot omit a method per value; `None` is the absent property |
| `provider-core` `ProviderInstance` | `acpSessionManagement` | omitted | ACP registry is cut |
| `provider-core` `ProviderInstanceRegistry.streamChanges` | `Stream.fromPubSub` subscribes when the stream is polled | `stream_changes` subscribes when it is called | closes the fork-before-subscribe race; `subscribe_changes` is still the explicit handle |
| `provider-core` `deriveProviderInstanceConfigMap` | walks the process-wide `BUILT_IN_DRIVERS` list | takes the driver slots as an argument | built-in drivers live in the adapter crates, not this one |
| `provider-core` `ProviderHost.withSnapshot` | generic over the closure's error | the closure's value is returned as-is; only a settings read failure is `ServerSettingsError` | a generic error is not object-safe on the host callback |
| `provider-core` credential store | file store only, under `secretsDir` | file store is the default and keeps the `provider-auth-<sha256>` key; `KeychainSecretStore` is an extra macOS backend using that same account | upstream bytes have to keep opening; keychain is opt-in because `security` can prompt |
