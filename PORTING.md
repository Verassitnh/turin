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
| `DateTimeUtc` / `DateTimeUtcFromString`, every `*_at` column | `UtcDateTime`: always encodes `YYYY-MM-DDTHH:MM:SS.sssZ`. Upstream compares these as text (`ORDER BY created_at`, `available_at <=`), so format drift misorders rows |
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
| `provider-codex` | `packages/effect-codex-app-server`, `orchestration-v2/Adapters/CodexAdapterV2.ts`, `provider/Codex*` |
| `provider-claude` | `orchestration-v2/Adapters/ClaudeAdapterV2.ts`, `provider/Claude*`; the TS Agent SDK is replaced by speaking the `claude` CLI stream-json control protocol directly (same protocol the SDK uses) |
| `provider-opencode` | `packages/provider-opencode` (HTTP+SSE to `opencode serve`) |
| `provider-cursor` | `packages/provider-cursor` |
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
`TURIN_PROJECT_ID`, `TURIN_THREAD_ID`, `TURIN_CWD`, …).

## Deviations

| File | Upstream | Turin | Why |
|---|---|---|---|
| `contracts/base_schemas.rs` | `TrimmedNonEmptyString` trims only on decode/encode | trims at construction | no untrimmed state can exist, so `" "` can never encode to `""` |
| `contracts/base_schemas.rs` | `UnknownUnionMember { tag, value }` | adds `raw: Value` | lets callers render/log unknown members without re-reading the row |
| `contracts/base_schemas.rs` | `DateTimeUtc` keeps ms precision via JS `Date` | `UtcDateTime` truncates to ms on construction/parse | `Eq`/`Ord` agree with the wire form |
| `contracts/base_schemas.rs` | `PositiveInt` is a refined number | newtype over `NonZeroU64` with `new(u64) -> Option<Self>` | 0 unrepresentable |
| `contracts/base_schemas.rs` | `isUnknownUnionMember(value)` | no function; match `ForwardCompatible::Unknown` | Rust enums make it a pattern |
| `contracts/base_schemas.rs` | `PortSchema` | omitted | only used by cut features |
| `contracts/base_schemas.rs` | cut-feature exports (AuthSessionId, RpcClientId, SecretRef, DpopFailureReason, Client* enums) | omitted | features cut (PORTING cut list) |
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
| `provider-core` `ProviderAdapter` optional methods | a missing property means the capability is absent | the method exists and returns `None` | Rust traits cannot omit a method per value; `None` is the absent property |
| `provider-core` `ProviderInstance` | `acpSessionManagement` | omitted | ACP registry is cut |
| `provider-core` `ProviderInstanceRegistry.streamChanges` | `Stream.fromPubSub` subscribes when the stream is polled | `stream_changes` subscribes when it is called | closes the fork-before-subscribe race; `subscribe_changes` is still the explicit handle |
| `provider-core` `deriveProviderInstanceConfigMap` | walks the process-wide `BUILT_IN_DRIVERS` list | takes the driver slots as an argument | built-in drivers live in the adapter crates, not this one |
| `provider-core` `ProviderHost.withSnapshot` | generic over the closure's error | the closure's value is returned as-is; only a settings read failure is `ServerSettingsError` | a generic error is not object-safe on the host callback |
| `provider-core` credential store | file store only, under `secretsDir` | file store is the default and keeps the `provider-auth-<sha256>` key; `KeychainSecretStore` is an extra macOS backend using that same account | upstream bytes have to keep opening; keychain is opt-in because `security` can prompt |
| `provider-core` `subagentProjection.ts` `Pick<OrchestrationV2ThreadProjection, …>` arguments | each helper takes the slices it reads; `From<&OrchestrationV2ThreadProjection>` covers the full projection | no structural `Pick` in Rust; explicit slices show what each helper reads |
| `provider-core` `ProviderInstanceRegistryHydration` settings watcher (`Stream.runForEach` over `subscribeChanges`) | each emission is reconciled in order | the watcher drains queued emissions and reconciles only the newest map | the final map is the same, and skipping the stale intermediate reconciles avoids redundant driver close/create churn |
| `provider-core` `ProviderInstanceRegistryHydration` initial settings read | read failure is logged and the registry starts empty | `new` returns `ServerSettingsError` | upstream's layer build fails on a settings read error; an empty registry would hide it |
| `server/ProviderHostLive.ts` `shouldRunBackgroundWork` (`BackgroundPolicy.shouldRunScopeWork`) | the desktop host takes an app-supplied `ShouldRunBackgroundWork` closure | `BackgroundPolicy` (leases, host power) is an app service, not provider-core; the host has no default because upstream's "no leases" answer is `false`, which only the app can judge |
| `provider-core` `ProviderHostLive` paths and credentials | built from `ServerConfig` and `ServerSecretStore` | `DesktopProviderHost::new(base_dir, cwd, …)` derives `userdata`, `secrets`, `attachments`, `caches` under the T3 home (non-dev `config.ts` layout); file secret store only | one process owns the config; the dev-stage `dev/` state directory is not part of Turin |
| `provider-core` `attachmentStore.ts` `inferImageExtension` registry fallback (`Mime.getExtension`) | a Rust table covers the explicit MIME map plus `image/x-icon` and `image/vnd.microsoft.icon` → `.ico` | no MIME registry crate in the workspace. Known gap: an image MIME outside that table whose registry extension is a safe one (for example an alias the registry maps to `.jpeg`) gets `.bin` in Turin where upstream keeps the extension. The fallback is `.bin` (not wrong, just less specific); a MIME registry crate closes it |
| `provider/ModelManifest.ts` `Effect.cached` disk load, scoped `forkIn` refresh, `Semaphore` | `OnceCell` for the disk load, `AsyncMutex` around `refresh`, a `JoinHandle` list aborted on drop | same single-load and one-refresh-at-a-time behavior; the scope is the service's own, so no Effect scope is needed |
| `provider/ModelManifest.ts` `enableProviderUpdateChecks` read | `enable_checks` closure supplied by the app | upstream reads `ServerSettingsService` inside the service; provider-core does not own settings. A settings read failure still falls through to fetching in both |
| `provider/ModelManifest.ts` `compatibility` (`ProviderCompatibilityPolicy`) | kept as `Vec<Value>`, not validated | the compatibility policy port is not in this crate yet. A malformed policy is accepted here where upstream rejects the whole manifest; fix when that port lands |
| `provider/ModelManifest.ts` `ModelCatalog` layer (`layerModelCatalog`, `layerTest`) | `resolve_provider_catalog` and `bundled_model_manifest` are plain functions | the ModelCatalog service and its test layer are not ported yet; adapters that need a catalog call these directly |
| `provider/ClaudeModelManifest.ts` `hasValidClaudeManifestAdapters` | lives in `model_manifest.rs`, validating `claudeAgent` adapter payloads with `TrimmedNonEmptyString` and `is_valid_semver` | upstream runs this inside the manifest schema, so it belongs with the manifest. Missing keys are accepted; `null` in a non-nullable key is rejected, as Effect's `optional` does |
| `provider/ModelManifest.ts` cache write (`writeFileStringAtomically`) | write a temp file, then `rename` | same atomic-replace contract on one filesystem |
| `provider/ModelManifest.ts` `currentModels`, profile keys | not checked for blank values | upstream does not check them either (`Schema.String`); only the trimmed-string fields are checked |
| `provider/EventNdjsonLogger.ts` `SynchronizedRef` store state | `parking_lot::Mutex` for short state sections, plus an async mutex that serializes flushes | a write does not wait behind an in-flight flush; records it appends are written by the next flush. Per-thread order holds, and upstream's wait is gone |
| `provider/EventNdjsonLogger.ts` `Effect.forkIn` batch timer and `Scope` | timers are `JoinHandle`s kept by the store and aborted on `close` | none beyond the mapping |
| `provider/EventNdjsonLogger.ts` `attribution` (`ResourceAttribution`) | omitted | resource telemetry is cut (PORTING cut list) |
| `provider/EventNdjsonLogger.ts` per-stream logger views (cached in a `Map`) | a view is built per `logger(stream)` call | views are stateless wrappers over the shared store; identity is not observable |
| `provider/EventNdjsonLogger.ts` `isProviderLogFile` / `enforceRetention` `mtimeMs` | integer milliseconds; ties broken by byte-wise path order | upstream compares fractional mtimes and uses `localeCompare`; the two only differ for sub-millisecond ties or non-ASCII names |
| `provider/EventNdjsonLogger.ts` `boundProviderEventForLogging` cycle guard (`ancestors` WeakSet) | omitted | `serde_json::Value` is a tree and cannot be cyclic |
| `shared/logging.ts` `makeAsyncRotatingFileSink` | `rotating_file_sink.rs` inside provider-core | the shared logging package is not ported; move it there when that happens. Prune parses the suffix as `u64`, where JS `Number()` also accepts forms like `" 3"`; this sink never creates such names |
| `provider/EventNdjsonLogger.test.ts` (19 cases) | 14 Rust cases cover the same behavior; the rest need Effect fibers, interruption, attribution or the OpenCode fixture | the batch-window timer path has no timing test (no sleeps); close-flush and max-buffered flush are covered |
| `maintenanceResolver.ts` `Config`/`readCommandLookupEnv` | `command_lookup_env()` reads `PATH`, `Path`, `path`, `PATHEXT` from the process env | `Config` is an Effect service; the four keys are the same |
| `maintenanceResolver.ts` `runHomebrew` (`ChildProcessSpawner`, `timeoutOption`, scoped kill) | `tokio::process` with `kill_on_drop`, the child in its own process group, and a guard that kills the group on timeout | upstream kills the direct child on scope exit; the group kill also reaps brew's children, which upstream can leave running |
| `maintenanceResolver.ts` `makeCachedProviderMaintenanceResolution` (`Cache.make` with single-flight lookup) | a `tokio::sync::Mutex` holds the cached value; a fresh read clears it and resolves | a concurrent read waits for the in-flight resolution instead of sharing it; the result is the same, and a cancelled resolution is not cached in either version |
| `maintenanceResolver.ts` `fetchNpmLatestVersion` (`HttpClient` with `timeoutOption`) | `reqwest` with `tokio::time::timeout` at 4 s | same timeout and error-to-`null` mapping |
| `maintenanceResolver.ts` `ProviderLatestVersions.cached(key, fetch)` | `cached(key, lookup, fresh)` takes the freshness flag as an argument | the Effect service's `invalidate` path is the `fresh` flag here |
| `ProviderLatestVersions` / cursor maintenance | cursor-agent is manual-only (no npm package, no native updater) | upstream's `provider-cursor/driver.ts` declares it the same way |
| `failure.ts` `boundedText` (UTF-16 cap, one unit reserved for `…`) | also caps the UTF-8 byte length at the same number | the contracts newtype rejects longer messages, so a cut that is only UTF-16 correct can fail to decode. The `…` is 3 bytes, so a capped string can end up below the UTF-16 cap |
| `failure.ts` `redactProviderFailureText` key pattern (`\b(?:access[_-]?token\|…\|token)\b`) | the word boundary before `token`/`secret` leaves `refresh_token=`, `client_secret=` and `id_token:` (and their JSON forms) unredacted | both the pair and JSON patterns take an optional `[a-z0-9_-]*` key prefix; `input_tokens=` is still left alone | upstream's gap leaks the compound credential keys providers use; PORTING rule 4 |
| `subagentProjection.ts` `subagentThreadTitle` clip (`slice(0, 69)`) | clips by UTF-16 code units (`take_utf16`) | JS `length`/`slice` count UTF-16 units; `char` slicing would cut emoji titles at a different place |
| `provider-core` `mcpToolPresentation.ts` `resolveT3McpToolDefinition` gate | `mcp_tool_presentation.rs` carries a hand-copied 84-key `T3_MCP_TOOLS` name inventory and ports only the name resolution, not the display half | `shared/t3McpToolPresentation` has no Rust owner yet; move it to `turin-shared` when the app needs the display names. Keep the list in sync with upstream until then |
