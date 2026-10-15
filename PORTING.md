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
| `orchestration-v2/ThreadLiveEventCoalescer.ts` | Charges a serialized wire budget and fails a socket stream on overflow. | In-process unbounded feed keeps the 50 ms window and 512 pending-update flush, without wire budget accounting. | GPUI reads the feed in-process; bounded snapshot transport and wire budgets are cut. |
| `orchestration-v2/assistantStreaming.ts` | Filters a typed provider adapter event. | Filters the same JSON event shape as `serde_json::Value`. | T08's provider adapter event type is still being finished on the stage branch; the filter can be narrowed to that type after merge. |
| `orchestration-v2/ProjectionStore.ts` (current T13a slice) | Full canonical read builds inherited fork history, shell summaries, filtered records, and windows. | The first read API loads all normalized rows and local visible items. Fork inheritance, shell summaries, filters, and windows are not implemented yet. | The initial T13a module establishes the transactional writer/read seam while these read algorithms are being ported. |
| `contracts/vcs.rs` `VcsProcessExitError::from_process_exit` | `NotFound` detail is "Merge request not found." for command `glab`, "Pull request not found." for `gh` or `az`, else "VCS resource not found." | only `gh` gets "Pull request not found."; `glab`/`az` fall through to "VCS resource not found." | GitLab and Azure DevOps hosts are cut |
| `provider-core` `ProviderAdapter` optional methods | a missing property means the capability is absent | the method exists and returns `None` | Rust traits cannot omit a method per value; `None` is the absent property |
| `provider-core` `ProviderInstance` | `acpSessionManagement` | omitted | ACP registry is cut |
| `provider-core` `ProviderInstanceRegistry.streamChanges` | `Stream.fromPubSub` subscribes when the stream is polled | `stream_changes` subscribes when it is called | closes the fork-before-subscribe race; `subscribe_changes` is still the explicit handle |
| `provider-core` `deriveProviderInstanceConfigMap` | walks the process-wide `BUILT_IN_DRIVERS` list | takes the driver slots as an argument | built-in drivers live in the adapter crates, not this one |
| `provider-core` `ProviderHost.withSnapshot` | generic over the closure's error | the closure's value is returned as-is; only a settings read failure is `ServerSettingsError` | a generic error is not object-safe on the host callback |
| `provider-core` credential store | file store only, under `secretsDir` | file store is the default and keeps the `provider-auth-<sha256>` key; `KeychainSecretStore` is an extra macOS backend using that same account | upstream bytes have to keep opening; keychain is opt-in because `security` can prompt |
| `orchestration` `pipeline` (Orchestrator) | `apps/server/src/orchestration-v2/Orchestrator.ts` decides and commits commands | Cut from this crate; the pipeline reaches it only through `OrchestratorPort` (`orchestrator_port.rs`), which T13c implements | the decider and its I/O wiring belong to the orchestrator port, not the write pipeline |
| `orchestration` `pipeline` (ThreadLaunchService, legacy V1 importer) | `ThreadLaunchService` and the V1 state importer | Cut. `thread.created` hooks run inside `ThreadManagementService::dispatch` and bind the reported worktree and branch | Launch is a hook on the dispatch chokepoint now, so a second launch path would be redundant |
| `orchestration` `pipeline/thread_management_service.rs` (settleAfterRun fork) | `settleAfterRun` is `Effect.forkIn(layerScope)`, interrupted when the layer closes | The service owns a `JoinSet` of settle waits. Each task holds `Arc<ThreadManagementCore>`, not the service, so dropping the service aborts them. The service derefs to the core | A task holding the service would keep the `JoinSet` owner alive and never be aborted. Waits run up to 24 h, so a dropped service could otherwise dispatch `thread.settle` later |
| `orchestration` `pipeline` (ThreadManagementService reads) | read pass-throughs and legacy importer hooks | Dropped. Reads go through `OrchestratorPort` directly; only the project-scoped `get_project_thread*` / `list_project_threads` remain | Read pass-throughs duplicated the port. The importer is cut |
| `orchestration` `pipeline/resource_cleanup_service.rs` (terminal and preview) | Live layer calls `terminals.close({threadId, deleteHistory: true})` and `previews.close({threadId})` | `cleanup_terminals` and `cleanup_previews` return `Ok(())`. Attachment cleanup is real | Terminal / PTY and preview are cut, so there is nothing to close. The `terminal.cleanup` and `preview.cleanup` effects from `thread_deletion.rs` should complete through these no-ops. No user data is lost |
| `orchestration` `pipeline/event_sink.rs` (write interrupt) | An Effect transaction is interrupted with its caller, so a write dropped mid-transaction rolls back and commits nothing | `spawn_writer` runs each writer's transaction and its publish as one spawned task, holding the publish lane. A caller dropped mid-write does not stop the commit, and the publish still runs | `Db::write` cannot stop a blocking SQLite commit once it starts. Publishing from the caller alone left committed events unpublished until restart or the 30 s claimer poll |
| `orchestration` `pipeline/event_sink.rs` | calls the persistence layer's `appendProjectEvent` | Duplicates the private `append_project_event` and `infer_actor_kind` from persistence | Persistence keeps that function private. Ask the persistence owner to expose `append_project_event_in`, then delete the copy |
| `orchestration` `pipeline/event_sink.rs` (ProjectionApplier) | projection is applied inside the Effect layer graph | `ProjectionApplier` trait (`normalize_turn_item`, `apply_event`, `apply_project_event`, `schema_version`). T13a implements it with `ProjectionStore::apply_in_tx` and `turn_item_position_store::allocate_in_tx` | Keeps the write side independent of the read-side crate. This is the T13a seam |
| `orchestration` `pipeline/dispatch_mode_limit.rs` | dispatch-mode limit read from ambient Effect context | explicit `Option<&DispatchModeLimit>` argument | No ambient context in Rust. Callers must pass the limit they hold |
| `orchestration` `pipeline/event_sink.rs` (publish lane) | publish lane is taken at the end of the transaction | Taken before the transaction | SQLite has one writer, so the order observed by subscribers is identical |
| `orchestration` `pipeline/mod.rs` | no lint override | `#![allow(clippy::result_large_err)]` | `PersistenceError` is about 128 bytes and every store call returns it. Boxing it would leak into the persistence API this crate does not own |
| `orchestration` `pipeline/thread_title_regeneration_service.rs` | `ThreadTitleRegenerationService` Effect service with `Effect.retry` (exponential 2 s) and `catchCause` | Borrowing struct `new(threads, projects, settings, text)`. Retries are an explicit loop: 3 attempts for initial titles, 1 for regenerations, 2 s then 4 s backoff. Cancellation is dropping the future. `with_retry_base` overrides the 2 s base for tests | Matches `ThreadLifecycleService`'s borrowing pattern. Effect's `retry` and interrupt semantics map onto a loop and future drop |
| `orchestration` `pipeline/thread_title_regeneration_service.rs` (decider tests) | `ThreadTitleRegenerationService.test.ts` arms and clears the marker through the real decider | Not ported. Those cases belong to the orchestrator decider (T13c). The service tests use `MockOrchestrator` with a projection-mirroring dispatch function | The decider is not in this crate |
| `orchestration` `pipeline/thread_title_regeneration_service.rs` (project settings) | `resolveProjectSettings(...).settings.textGenerationModelSelection` from `@t3tools/shared/projectSettings` | Project override when its instance is enabled for the project, otherwise the environment value. The `fallbackTextGenerationModelSelection` swap for a disabled instance is not ported, so a disabled selection passes through | The full resolver and `fallbackTextGenerationModelSelection` are not in Rust yet |
| `orchestration` `pipeline/thread_title_regeneration_service.rs` (blank title) | the generated title is dispatched as-is; a blank value fails schema decode and the command errors | A blank generated title completes without a title | A blank title cannot be a `TrimmedNonEmptyString`. Treating it as "no title" keeps the marker from sticking |
| `orchestration` `pipeline/command_policy.rs` | `CommandPolicyV2` Effect `Context.Service` with `Layer.succeed` | Pure functions (`ensure_*`, `decide_*`, `resolve_message_dispatch_intent`) taking a `CapabilityCheck` | No service container in Rust. The decisions are pure, so the service wrapper adds nothing |
| `orchestration` `pipeline/command_policy.rs` (test fixtures) | Cursor and Grok capability constants from `@t3tools/provider-cursor/testing` and `@t3tools/provider-grok/testing` | Test fixtures derived from the Codex set (Grok: no active steering, no native fork; Cursor: no native fork) | The Cursor and Grok capability modules are not in Rust. Revisit when those drivers' capability constants are ported |
| `packages/shared/src/searchRanking.ts`, `semver.ts` (`localeCompare`) | ICU collation of the runtime's default locale | `js_string::locale_compare`: hand-rolled 3-level approximation of ICU root collation (whitespace < punctuation < digits < letters; case-insensitive base, then accents, then lowercase before uppercase; non-ASCII letters beyond Latin accents fall back to code point order) | No ICU collator is vendored; the call sites only compare ids/versions/tie-breakers where ASCII ordering decides |
| `packages/shared/src/String.ts` `truncate` | `slice(0, maxLength)` in UTF-16 units | same, but a cut inside a surrogate pair drops the whole pair | a Rust `&str` cannot hold a lone surrogate. Also takes `max_length` explicitly (upstream default 50) |
| `packages/shared/src/semver.ts` | `Number.parseInt` floats | `i64`; digit runs overflowing `i64` fail to parse | past JS's 2^53 anyway |
| `packages/shared/src/fileLinks.ts` | line/column are JS numbers | `u64`, absurdly long digit runs saturate | no precision loss concerns for real positions |
| `packages/shared/src/composerTrigger.ts` | `isWhitespaceChar(char: string)` gets one UTF-16 unit | gets a `char`; a lone surrogate is passed as U+FFFD | lone surrogates cannot be a `char` |
| `packages/shared/src/searchRanking.ts` | `trimLeadingPattern` is a JS `RegExp` (flags decide global replace) | `regex::Regex`, first match replaced only | callers pass non-global patterns |
| `packages/shared/src/dateTime.ts` | `Date.parse` + Effect `DateTime` | civil-day arithmetic written by hand, TimeClip range check, `-000000` year rejected | stays exact without chrono's ±262k-year limit |
| `packages/shared/src/gitPatchPath.ts` | unescaped astral char in a body that also holds an escape becomes two lone surrogates → U+FFFD | the real character survives | JS-string artifact, not behaviour worth keeping |
| `packages/shared/src/cliArgs.ts` | `flags` is a plain object | `IndexMap<String, Option<String>>` | keeps insertion order; `indexmap` added to workspace deps |
| `packages/shared/src/{Net,HostProcess,nodeRuntime,devHome,httpReadiness,mcpResponsePayloads,observability,t3McpToolPresentation,themePalettes,themePreview,schemaYaml,codexAuthCallback}.ts` | contract-free files | not ported (T05a) | terminal-only / Node host / remote / device hosts / telemetry / orchestrator-MCP / mobile+web theming / build scripts; `codexAuthCallback` needs `codexAuthHandoff` (contracts, T05b) |
| `packages/shared/src/shell.ts` | Effect services: `HostProcess.Platform`/`Environment`, `Context.Reference` resolvers (`SpawnExecutableResolution`, `WindowsShellEnvironment`, `CommandAvailability`, `CommandResolutionCache`), `Cache`-backed PATH listings, Effect tracing. `fixPath` is not in this file; PATH hydration is `read_environment_from_login_shell` + `merge_path_entries` | Plain functions taking `Platform`, `ShellEnv` (`IndexMap`), a `SpawnResolver { id, resolve }`, a `CommandResolutionCache` (`global()` for the process-wide map), and `now: Instant`. `withPathDirectoryListings` is not ported: every candidate is stat'd directly. `readUserLoginShell` reads `$SHELL`. `access(X_OK)` is approximated by the execute bits. Tracing tests are not ported | No Effect context here. The listing cache only skips stat calls, so answers match. In login sessions `$SHELL` is the passwd shell. Execute bits approximate `access(2)` for the current user. |
| `packages/shared/src/githubMarkdownLinks.ts` | Regex with lookbehind and lookahead; `remark-parse` + `remark-gfm` | `github_markdown_links.rs`: a hand-written matcher that reproduces the regex's alternation order and backtracking; `markdown` crate GFM parse | `regex` has no lookaround. markdown-rs positions are byte offsets, and edits use the same offsets, so the output is unchanged. |
| `packages/shared/src/{markdownPipeline,codexMarkdownDirectives,markdownListIndentation,markdownGithubAlerts}.ts` | unified/remark/rehype plugins over mdast and hast. Codex directives add custom micromark constructs. The pipeline sanitizes HAST for the DOM | not ported (T05a) | The mdast transforms need a Turin markdown AST decision: the `markdown` crate's mdast has no `data.hProperties`. The sanitize/HAST stage is web-only. Reopen with the GPUI markdown renderer. |
| `packages/shared/src/atomicWrite.test.ts` "succeeds when the write lands but its temp directory cannot be removed" | Effect `FileSystem` layer with injected `remove` failure | not ported | `write_file_string_atomically` uses `std::fs` directly with no fault injection. The removal failure is ignored in code (`let _ =`); no test covers it. |
| `packages/shared/src/KeyedLock.test.ts` "runs the effect in the caller's scope" | Effect `Scope` finalizers | not ported | Rust has no scope to attach finalizers to. The closure runs in the caller's task. |
| `orchestration` `pipeline/thread_settlement_service.rs` (ServerActivation gate) | `forkParked` waits for `ServerActivation` before the sweep loops run | `start()` runs the loops immediately; the gate is not ported | No `ServerActivation` service in Rust yet. The caller (T13c) starts the reactor after activation |
| `orchestration` `pipeline/thread_settlement_service.rs` (concurrency) | `Effect.forEach` with `concurrency: 8` over candidates and groups | Sequential, in candidate and group order | Same decisions and dispatch order for every case in the upstream suite. Parallel lookups would only change latency |
| `orchestration` `pipeline/thread_settlement_service.rs` (crypto) | `Crypto.randomUUIDv4` service | `uuid::Uuid::new_v4()` directly | Only the command id suffix is random. No `Crypto` service exists in Rust |
| `orchestration` `pipeline/thread_settlement_service.rs` (clock) | `DateTime.now` / `TestClock` | `SettlementDeps::now_ms` closure (epoch ms) | Tests pin the clock with a closure; production passes the system clock |
| `orchestration` `pipeline/thread_settlement_service.rs` (GitManager) | `GitManager.branchPullRequest` / `invalidateStatus` | `SettlementGit` trait; `BranchPullRequest` carries only the fields a decision reads | `GitBranchPullRequest` is not ported. The git owner (T06/T07) maps the real type to this subset |
| `orchestration` `pipeline/thread_settlement_service.rs` (PullRequestService) | `summary(...)` returns `PullRequestSummary` and `subscribeMerges` | `SettlementPullRequests` returns `SettlementPullRequest` (state plus terminal timestamps) and a `PullRequestMergeEvent` stream | Settlement reads only the terminal fields. `PullRequestMergeEvent` has no Rust type yet |
| `orchestration` `pipeline/thread_settlement_service.rs` (TerminalManager) | `TerminalManager.closeIdle` | `SettlementTerminals::close_idle` | `TerminalManager` is not ported (cut from the write side) |
| `orchestration` `pipeline/thread_settlement_service.rs` (ProjectSetupScriptRunner) | `runForThread({trigger: "settle", observeCompletion})` returns a status and an optional completion | `SettlementScripts::run_settle_script` returns `NoScript` or `Started { completion }`; a started completion is spawned detached | The runner is not ported. `observeCompletion` is always `{}` here, so it is dropped |
| `orchestration` `pipeline/thread_settlement_service.rs` (ProjectStore / ProjectionStore reads) | `ProjectStoreV2.listShells`, `ProjectionStoreV2.getSettlementCandidates`, `getThread` | `SettlementStore` trait | T13a's store and T13c's adapter implement it. Keeps the reactor testable without SQLite |
| `orchestration` `pipeline/thread_settlement_service.rs` (settings subscribe) | `settingsService.subscribeChanges` stream | `SettlementSettingsSource::subscribe_changes` | Same stream shape, behind a trait |
| `orchestration` `pipeline/thread_settlement_service.rs` (Orchestrator) | `Orchestrator.dispatch` and `streamDomainEvents` | `SettlementOrchestrator`, blanket-implemented for every `OrchestratorPort` | The reactor depends on the two calls it uses, not the whole port |
| `orchestration` `pipeline/thread_settlement_service.rs` (periodic sweep) | `Schedule.spaced("1 minute")` in a forked fiber | `tokio::spawn` loop (enqueue, drain, sleep 60 s); `Drop` aborts the tasks | Equivalent. Effect's scope maps to the owned `JoinHandle`s |
| `orchestration` `pipeline/thread_settlement_service.rs` (resolveProjectSettings) | `resolveProjectSettings(...)` for the sidebar keys, plus file-backed and fallback resolution | `resolve_settlement_settings`: override where set, otherwise environment. A disabled instance is not swapped for a fallback | Same gap as the title-regeneration row: the full resolver and `fallbackTextGenerationModelSelection` are not in Rust yet |
| `orchestration` `pipeline/thread_settlement_service.rs` (test harness) | `ThreadSettlementService.test.ts` drives the Effect layer graph with `TestClock` and `Deferred` gates | `service_tests.rs` drives the same 15 reactor cases through one fake implementing every narrow trait; `Latch` and channels replace `Deferred` and `Queue` | Every reactor case from the suite runs. The `ServerActivation` gate and the layer wiring are not covered (see the first row) |
| `crates/shared/src/thread_pull_requests.rs` (clippy) | not in upstream | two lint-only rewrites (`is_none_or`, collapsed `if`) to clear `-D warnings` | Behavior unchanged. The file came in with the T05b merge; the owner can take these or revert |
