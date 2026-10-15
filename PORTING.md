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
`TURIN_PROJECT_ID`, `TURIN_THREAD_ID`, `TURIN_CWD`, …).

## Deviations

| File | Upstream | Turin | Why |
|---|---|---|---|
| `provider-cursor` | runs `@cursor/sdk` local executor in-process (Node) | `cursor-agent acp` subprocess over ACP | Rust can't host the JS SDK and it has no stable wire protocol; ACP is Cursor's supported external protocol. Gains interactive approvals; loses SDK-only features (autoReview, run recovery via listRuns, per-send `{id,params}` model selection). QC wf_42618e43-71a |
| `contracts/base_schemas.rs` | `TrimmedNonEmptyString` trims only on decode/encode | trims at construction | no untrimmed state can exist, so `" "` can never encode to `""` |
| `contracts/base_schemas.rs` | `UnknownUnionMember { tag, value }` | adds `raw: Value` | lets callers render/log unknown members without re-reading the row |
| `contracts/base_schemas.rs` | `DateTimeUtc` keeps ms precision via JS `Date` | `UtcDateTime` truncates to ms on construction/parse | `Eq`/`Ord` agree with the wire form |
| `contracts/base_schemas.rs` | `PositiveInt` is a refined number | newtype over `NonZeroU64` with `new(u64) -> Option<Self>` | 0 unrepresentable |
| `contracts/base_schemas.rs` | `isUnknownUnionMember(value)` | no function; match `ForwardCompatible::Unknown` | Rust enums make it a pattern |
| `contracts/base_schemas.rs` | `PortSchema` | omitted | only used by cut features |
| `contracts/base_schemas.rs` | cut-feature exports (AuthSessionId, RpcClientId, SecretRef, DpopFailureReason, Client* enums) | omitted | features cut (PORTING cut list) |
| `app/theme/theme_palettes.rs` | `packages/shared/src/themePalettes.ts` (shared package) | lives in the app crate | only the UI consumes palettes; `turin-shared` owner can move it if another crate needs it |
| `app/theme/theme_palette.rs` | `apps/web/src/themePalette.ts`: custom/environment theme library (storage, theme files, vivid editor), light/dark `halves`, live previews, `resolveDesktopTheme`, `applyThemePalette` CSS variables | resolution helpers only (aliases, `getThemeDefinition` over built-ins, `resolveThemeAppearance` without `halves`, …); `AppTheme::resolve` stands in for `applyThemePalette`; `ThemeDefinition` drops `collection`/`managed` | library, halves and previews belong to the Settings → Appearance task (T20) |
| `app/theme/mod.rs` | browser resolves `oklch()`/`color-mix()`/alpha tokens live from `index.css` | `AppTheme` resolves every token once (palette + contrast + fonts) and pushes a `ThemeConfig` into gpui-component | GPUI needs concrete colors; same values at default contrast |
| `app/theme/mod.rs` | no `data-theme-id` → stock `index.css` tokens (some alpha, e.g. dark `--border: white/6%`) | standard look uses `T3_CODE_{LIGHT,DARK}_THEME_COLORS` (upstream's flattened capture of those tokens) | opaque colors render identically and keep one code path for all themes |
| `app/theme/appearance_fonts.rs` | CSS font stacks; canvas probing for availability/monospace; `ui-monospace` resolves to SF Mono | first installed family of the stack via `text_system().all_font_names()`; `-apple-system`/`system-ui` → `.SystemUIFont`, `ui-monospace` (and the hidden `SF Mono`/`SFMono-Regular`) → CoreText's `.AppleSystemUIFontMonospaced`; no custom code font → index.css `--font-mono` stack | GPUI takes one family per run; no canvas; SF Mono is a hidden system family that only resolves through the CoreText alias |
| `app/theme/appearance_fonts.rs` | `resolveTerminalFont*Preference`, `TYPOGRAPHY_ADVANCED_STORAGE_KEY`, installed-family listing, canvas/DOM probes; tests for the terminal helpers | not ported; `resolve_default_family_label` takes the generic-keyword resolver as an argument (upstream measures DOM probes) | terminal cut; GPUI has no DOM. Remaining upstream `it()` cases ported 1:1 |
| `app/ui/button.rs` | shadcn/Base UI `Button` (cva variant table) | own `RenderOnce` on GPUI divs; no `active:scale-[0.97]`, no `media-*` variants, no focus ring | gpui-component `Button` can't express upstream's size table; GPUI has no transforms; media viewers cut |
| `app/ui/tooltip.rs` | Base UI tooltip, `side="top"`, collision-aware | GPUI native tooltip (anchored at the pointer) with upstream's popup surface | no positioner API on GPUI tooltips yet |
| `app/ui/*`, `app/components/chat/composer_surface.rs` | `backdrop-filter` glass (`surface-glass`, `dropdown-glass`, composer) | translucent fill without blur; composer glass flattened over the canvas | GPUI has no backdrop blur, and paints box shadows under translucent fills |
| `app/ui/scroll_area.rs` | `scrollFade` masks, `topbar-scroll-fade` under the header | no fades | GPUI has no alpha masks |
| `app/components/app_sidebar_layout.rs` | `surface-grain`: a 256px fractal-noise SVG tile at 3.5% opacity (128px at 2.745% for T3 Chat) baked into the body, sidebar and inset backgrounds | flat backgrounds | GPUI has no repeating background images; a full-window noise texture costs a large GPU image for a ~1-level luminance dither. Revisit if the flat surfaces read differently side by side |
| `app/components/app_sidebar_layout.rs` | widths persisted in localStorage; panel width animation | in-memory widths; no animation (upstream default duration is 0) | persistence belongs to the settings task |
| `app/components/app_sidebar_layout.rs` | layout toggles: thread details, terminal, right panel, maximize | terminal toggle dropped; thread-details toggle is inert | terminal cut; details panel not ported yet |
| `app/components/right_panel_tabs.rs` | surfaces: browser, terminal, files, diff, PRs, device | Diff + Pull requests tabs only, placeholder body | cut list |
| `app/components/drag_region.rs` | `-webkit-app-region: drag`, with `button, input, textarea, select, a` exempted (`no-drag`) | a left press on the region that moves past 2px while held calls `start_window_move`; double-click on the region itself → `titlebar_double_click`. Interactive elements over a region call `drag_region::no_drag` on mouse-down (stop propagation) — ui `Button`/`Toggle`, sidebar icon buttons and panel tabs do; new interactive titlebar elements must too | Electron CSS has no GPUI equivalent; GPUI bubbles mouse events to every hovered hitbox |
| `app/components/sidebar.rs` | live thread list (sections, statuses, hover actions, search) | static placeholder rows with upstream geometry | data/behaviour land with the sidebar task |
| `app/components/sidebar/sidebar_chrome.rs`, `app/branding.rs`, `app/main.rs` | `SidebarBrandMark`: T3 wordmark + muted "Code"; `APP_BASE_NAME` "T3 Code" with a stage label; window title from branding | brand slot renders the text "Turin" in the slot's styling (`text-sm font-medium`, foreground, `h-7` at `--workspace-titlebar-content-left`); `APP_BASE_NAME = "Turin"`, no stage label; window title "Turin". `tracking-tight` is not reproduced | Turin is its own product (PM decision); GPUI has no letter-spacing |
| `app/components/thread_sidebar_width.rs`, `app/components/app_sidebar_layout.rs` | `SidebarBrandWidthProbe` measures the brand and raises the minimum via `resolveThreadSidebarMinimumWidth` | probe not ported; minimum fixed at `THREAD_SIDEBAR_MIN_WIDTH` (208px); the function is ported and tested | "Turin" at 14px fits well inside 208px; wire the probe when the interface font size becomes configurable (settings task) |
| `app/components/chat/composer_surface.rs` | `chat/ComposerSurface.tsx`, `chat/ChatComposer.tsx` (body, placeholder, footer) and `chat/ComposerPrimaryActions.tsx` (send) | one static frame in `composer_surface.rs` | placeholder until the composer task splits them back into `chat_composer.rs` / `composer_primary_actions.rs` with behaviour |
| `apps/server/src/persistence/reconcileV2PreviewMigration.ts` | Rewrites preview databases that recorded `OrchestrationV2` as migration 53 or 54 so those files keep their ledger and still migrate forward. | Not called. A preview file whose recorded id `>= 55` has a different name, or whose later id would skip a V2 migration, fails closed in `assert_v2_migrations_can_run`. | Fresh databases and released-main files already use the current ids. Rewriting 53/54 would hide a schema this build does not contain. |
| `apps/server/src/persistence/OrchestrationEventStore.ts` | Decodes each payload with the orchestration-v2 contract schema. | Payloads stay `serde_json::Value`. Invalid JSON fails. Unknown event types fail a strict read and are skipped when `skip_unknown_event_types` is set. A known type must be a non-empty JSON object. | `turin-contracts` event schemas are not in this crate. Empty-object rejection stands in for "schema rejected a known payload" until those types land. Non-empty but schema-invalid JSON is accepted. |
| `apps/server/src/persistence/Errors.ts` | Decode failures name Effect schema issue tags and do not copy the rejected value. | Decode failures carry the serde error string. | There is no Effect schema layer to map. SQL failures still use `SQLITE({code}) {errstr}` and do not include the query text. |
| `packages/shared/src/nodeSqliteClient.ts` | Reader and checkpoint workers are separate Node worker threads. The checkpoint worker uses `wal_checkpoint(NOOP)` and stays off when SQLite is older than 3.51. | One process: a `SQLITE_OPEN_READ_ONLY` connection for reads, and a `std` thread for checkpoints, joined on `Db` drop. Same pragmas, 1000-frame passive threshold, 100ms idle flush, and writer autocheckpoint of 10000. The worker is not started when `sqlite_version()` is below 3.51. `:memory:` has no reader and no checkpoint thread. | A second connection cannot see an in-memory database. Bundled SQLite may be older than 3.51; leaving the worker off is the upstream behavior, and the writer keeps the default autocheckpoint. |
| `apps/server/src/persistence/initializeV2Database.ts` | After the snapshot copy, orchestration lazily imports V1 transcripts. | Copies the committed V1 snapshot (WAL included, uncommitted rows excluded) and stops. | Transcript import depends on the orchestration importer, which is not this crate. |
| `apps/server/src/persistence/Migrations/009_ProviderSessionRuntimeMode.ts` | Intentional `Effect.asVoid` no-op, still recorded. | Empty script, id still inserted. | Same ledger outcome. |
| `apps/server/src/persistence/Migrations/006_*.ts` | Binds `DEFAULT_RUNTIME_MODE` (`full-access`) as a parameter. | The literal `'full-access'` is in the SQL file. | The extracted script has no parameter slot. The written value is the same. |
| `apps/server/src/persistence/Migrations/055_OrchestrationV2.ts` | `ApplicationEventSource` runs between thread-launch workflows and effect-cancellation, including a procedural copy. | The generated SQL has `-- TURIN_APPLICATION_EVENT_SOURCE --`. The runner executes the copy in Rust at that point. | Concatenating the TypeScript module would run its static receipt insert before the events it depends on exist. |
| `apps/server/src/persistence/*` timestamps | `*_at` values are UTC text `YYYY-MM-DDTHH:MM:SS.sssZ`. | Caller-supplied strings are stored as given. Values this crate generates use `chrono` `to_rfc3339_opts(SecondsFormat::Millis, true)`. SQL `strftime` expressions are unchanged. | `turin_contracts::UtcDateTime` is not on this branch yet. |
| `client-runtime/src/sourceControlClients.ts` | Registers GitHub, GitLab, GitCafe, Forgejo, Bitbucket and Azure DevOps client definitions; `readGitHubSettings` decodes `settings.sourceControlHosts.github` | Registers GitHub only. The definition type, registry and GitHub definition live in `state/helpers/source_control_clients.rs`. `read_github_settings` and `GitHubSettings` are omitted; they exist as `turin_source_control::github::definition` (T07), which the app does not depend on yet. The registry's cut-host test cases are replaced by "no shipped host claims them" | PORTING cut list (every host but GitHub); no cross-crate dependency yet |
| `client-runtime/src/providerSkills.ts`, `providerUpdateRequiredModels.ts`, `promptLinks.ts`, `handoff.ts` | Read `ServerProvider` from `@t3tools/contracts` | Read a field-for-field subset in `state/helpers/provider_types.rs` (same names and optionality; text fields are `String`) | `turin_provider_core::server_provider::ServerProvider` (T08) is not an app dependency yet; swap the import then |
| `client-runtime/src/handoff.ts` | `resolvePendingHandoff(projection, ...)` reads only `projection.runs` | `resolve_pending_handoff(runs: Option<&[OrchestrationV2Run]>, ...)` | Building a full thread projection adds nothing; callers pass `projection.runs` |
| `client-runtime/src/mediaSource.ts` | `MediaSourceResource` is `Extract<AssetResource, "media-file">` | Local `MediaSourceResource { thread_id, path }` | `assets` contract is on the cut list |
| `client-runtime/src/delayedStatus.ts` | Calls `setTimeout`/`clearTimeout` | Same state machine over a `Timers` trait the GPUI side implements | No ambient timer in Rust; tests use a fake clock |
| `client-runtime/src/reviewGuideTour.ts` | Takes `@pierre/diffs` `FileDiffMetadata` from `parsePatchFiles` | Local `FileDiffMetadata`/`Hunk`/`HunkContent` (the fields the tour reads); tests parse their patch with a small test-only parser | `@pierre/diffs` has no Rust port; the diff view (T16) decides the real parser |
| `client-runtime/src/diffCount.ts`, `threadPullRequestCompatibility.ts`, `packages/shared/src/{semver,threadPullRequests,changeRequestUrl}.ts` | `Intl.NumberFormat()` in the viewer's locale; `localeCompare` fallbacks; `ThreadPullRequestKey`-shaped inputs and commands from the RPC contracts | `format_diff_count` groups with commas (en-US); semver fallback compares bytes; the compat module uses plain structs for the link/unlink/meta-update inputs. `parse_change_request_url` keeps every upstream host shape because stored links carry them | No locale or ICU data; `thread.pull-request.*` commands are not in `turin-contracts` |
| `client-runtime/src/*` using `packages/shared` | Imports `path`, `markdownLinks`, `fileLinks`, `filePreview`, `video`, `model`, `semver`, `changeRequestUrl`, `threadPullRequests`, `inlineSkills` | Private copies of the functions used, in `state/helpers/shared_support.rs`, each tagged with its upstream file | `turin-shared` (T05a) is not an app dependency yet; replace with `use` once it is |
| `client-runtime/src/state/{runtime,shell,threads,environmentStreams,snapshots,*Http}.ts` → `app/state/{events,store,client_state}.rs` | RPC subscriptions (`subscribeShell`/`subscribeThread`), HTTP snapshots, sync replay, relay, query/cache persistence and atom plumbing feed the reducers | The orchestrator feeds `ClientStateSender` (`Shell`, `Thread`, `*Feed`, `ThreadHistoryPage`, …) and drains `ClientStateRequest`s; one reducer task folds batches into `ClientStateCore` and publishes `Arc<ClientState>` through a `watch` to the GPUI `ClientStore` entity. Closed threads stay for `THREAD_SNAPSHOT_IDLE_TTL` (5 min, `threadRetention.ts`) | One process: no transport, auth, budgets or caches to survive reconnects |
| `client-runtime/src/state/*` (reducers, selectors) | `===` identity on immutable objects; reducers return the same object when nothing changed | `Arc<Vec<Arc<T>>>` with `Arc::ptr_eq`; reducers return `Option` (`None` = unchanged); selectors are memoized structs (`ThreadDetailSelectors`, `ShellViewBuilder`) | Rust has no reference identity on values; keeps views from re-deriving untouched rows |
| `app/state/support/{turn_item,domain_event}.rs` | Reads `item.type`, `item.payload.*`, `event.type` directly | `TurnItemExt` / `DomainEventExt` accessor traits | Contract enums are struct-like variants with no shared accessors |
| `client-runtime/src/state/operations/*`, `threadCommands.ts` | Commands go through `createEnvironmentRpcCommand` with client-side fallbacks for older servers (resolved command context, legacy first-message launch) | `CommandPort` async trait + `EnvironmentCommands`; the orchestrator resolves command context; a first-message launch is `thread.create` then `message.dispatch`. Optional timestamps (`settledAt`, …) are omitted when `None` where upstream sent `null` | Same-version orchestrator, no legacy servers; contracts skip `None` on serialize |
| `client-runtime/src/state/models.ts` (`ShellSource`) | Rows carry a `source` reference to the shell object | `SourceRef<T>` (`Arc` newtype with pointer equality) in `state/models.rs`; T15's sidebar model types kept as-is with `source` added; `ProjectIconColor` is the contracts enum (same variants as T15's local copy) | Shared row types with the sidebar seam |
| `client-runtime/src/state/projects.ts` | `findProjectByPath` reads `project.cwd` | `find_project_by_path` takes a `cwd_of` closure | Works over both contract rows and view models |
| `client-runtime/src/state/threadExecution.ts` | `formatModelSelectionEffort` | Not ported | Needs `getProviderOptionDescriptors` / `getProviderOptionCurrentLabel` from `shared/model` and the provider capability types, which no branch has ported yet; port with the composer's model picker |
| `client-runtime/src/state/threadLifecycle.ts`, `threadCommands.ts` (`optimistic.wrap`) | Pending updates are closures in a per-environment atom; `wrap` subscribes to the source atom to drop confirmed entries; every apply returns a new thread object | `OptimisticThreadUpdate` data enum + `OptimisticThreads` held by `ClientStateCore` (`ClientStateEvent::Optimistic{Begin,Confirm,Discard}`); `reconcile` runs after every shell batch; the overlay is applied at publish, so `ClientState.shell.snapshot` and `shell_view` are the visible snapshot; `with_optimistic` / `ThreadLifecycleCommands` play `wrap` (a dropped future discards like upstream's `finally`); an apply that changes nothing keeps the row's `Arc` | Events must be `Clone + PartialEq`; one environment; identity keeps untouched rows from rebuilding |
| `client-runtime/src/state/filesystem.ts` | `resolveFilesystemReadAccess` (auth scopes, connection phase), `canPreloadBrowsePath` (connection phase), RPC browse atoms | Only `get_filesystem_browse_path`, `filter_filesystem_browse_entries`, `BrowseNavigationCoordinator` (`state/helpers/filesystem.rs`) | No sessions or remote connection in one process; access is always granted |
| `client-runtime/src/state/attachments.ts` | Signed-URL upload cycle (`runAttachmentUploadCycle`, `verifyPersistedAttachmentUpload`, `deletePendingAttachmentUpload`) | Only the limit/label helpers (`state/helpers/attachments.rs`) | The composer passes bytes in-process (`CommandPort::persist_chat_attachments`); there is no upload URL |
| `client-runtime/src/state/{fileMetadata,assets,sharedSettings,sourceControl,review,serverConfigProjection,server,outdatedServerUpdate,commandPermissions}.ts`, `projectCommands.test.ts` | Per-RPC-session metadata batching cache; asset URLs over HTTP; cross-environment shared settings; RPC atoms for source control/review; `ServerConfig` stream projection, self-update and welcome flows; auth scope checks | Not ported | One process and one environment: metadata is a local `stat`, assets are local files (`assets` contract cut), settings are not shared across environments, the `server` contract is cut, no self-update or auth. `projectCommands.test.ts` only covers the metadata cache and the `openScratch` RPC round trip |
| `client-runtime/src/state/{threadSort,threadInbox,threadSettled}.ts` (+ `threadSnoozed`/`customSnooze` tests) | Live in client-runtime state | Ported by T15 under `app/components/sidebar/runtime/` (tests included) | Owned by the sidebar task; not duplicated here |
| `client-runtime/src/state/usage.ts` | `refreshUsageLimits` keeps `limitsRefreshAfter`/`limitsRefreshes` in module-level maps and reads `Date.now()`; `refreshUsage` drives atoms (`usageSummary`, `refreshUsageRates`, presentation atom) | `UsageLimitsRefresher` in `state/helpers/usage.rs`: the maps and an injected clock live on the value; the check runs on a spawned tokio task (like a JS promise) and callers join a `Shared` future. `refreshUsage` is not ported | module state is untestable in Rust; the atom registry and RPC commands are cut (single process). The Cursor Keychain predicates are ported unchanged |
| `client-runtime/src/state/usageAccess.ts` | Imports `sessionGrantsScope`, `AuthDiagnosticsReadScope` from the auth contracts | Local `SessionGrantInput`, `session_grants_scope` and the legacy-parent table in `state/helpers/usage_access.rs` | `auth` contract is on the cut list; `resolve_usage_access` keeps upstream's decision table and strings |
| `client-runtime/src/state/usageProgress.ts` | Structural `UsageProgressEnvironment`, returns `Set`/object literals | `UsageProgressEnvironment<'a>` borrows its `UsageSummary`; phases are an enum; `usage_progress` takes `refreshing` and the label fn as plain arguments | Avoids cloning summaries per render |
| `client-runtime/src/state/vcs.ts`, `vcsRefInvalidation.ts` | Atom families for refs/status/commands, `PartitionedSemaphore`, `EnvironmentCacheStore`, `makeCachedVcsRefsChanges` (connection-generation stream with retry) | `VcsRefsCache` (per-environment revision state + persistence lock) and a `VcsRefsStore` trait in `vcs_ref_invalidation.rs`; `commit_vcs_refs_refresh`, `load_cached_vcs_refs`, `can_use_vcs_refs_cache`, `vcs_refs_retry_delay` in `vcs.rs`. The stream, atoms, RPC commands and `applyGitStatusStreamEvent` wiring are not ported. `invalidate_cached` logs store failures with `tracing::warn!` | The streams are transport and the atom registry is cut; the generation-checked commit and invalidation ordering are the load-bearing parts and keep their tests (all but the stream/command cases of `vcs.test.ts`) |
| `client-runtime/src/state/git.ts`, `vcsCommandScheduler.ts` | Two RPC atoms (`pullRequestResolution`, `preparePullRequestThread`); a serial-per-`[environment, cwd]` atom command scheduler | Not ported | Only transport. Callers serialize per target themselves |
| `client-runtime/src/state/vcsAction.ts` | Atom family of action states; `createRuntimeCommand` with the scheduler, `RpcPermissionGuard`, `Effect.ensuring(invalidateCachedVcsRefs)`; `Cause.hasInterruptsOnly` clears state on interrupt | `VcsActionManager` owns a `Mutex<HashMap>` of states; `run_stacked_action` takes the progress stream factory and an `invalidate` future from the caller. Dropping a `track`/`run_stacked_action` future clears the state it owns (drop guard) but does not run the async refs invalidation; there is no per-target command queue. `consume_vcs_action_progress`'s `onProgress` is a plain callback; error `message` fields come from `Display` | Rust has no interrupts, only cancellation by drop; invalidation needs an async store call and cannot run in `Drop` |
| `client-runtime/src/state/gitActions.ts` | Plain objects with string-literal unions | Enums plus `&'static str` labels/hints | Same strings; no upstream test existed, so the Rust tests were written against the upstream branch structure |
| `client-runtime/src/state/pullRequests.ts` | Atom families and RPC commands (`createPullRequestEnvironmentAtoms`, linked summary/stack families, `writableQueryFamily`, `updateCached`), query persistence, HTTP diff loader | Pure pieces only (`state/helpers/pull_requests.rs`): `pull_request_{detail,summary}_to_vcs_status`, the command ordering keys, `prepare_pull_request_action` (quick-merge checks, reads injected as closures), `apply_set_labels`, `plan_request_reviewers` (+ `apply_to_{candidates,detail,activity}`) and the query timing constants. Tests ported from `pullRequests.test.ts` cover quick merge (closed/draft/permission/stack/method), label and reviewer cache patches, per-host diff file keys | The rest is transport/atoms; a cache layer applies the patches to whatever it holds. `missing_identities` is the signal upstream turns into a refresh |
| `client-runtime/src/state/pullRequestRouting.ts` | Routes reads and writes across connected environments by verified GitHub account (identity probes, alternates, invalidation fan-out) | Not ported except `matches_reference`, `routed_read_key`, `is_local_base_url`, `rejected_before_dispatch` (over a `RoutedWriteFailure` enum) and a bounded `RoutedReads` map | One process, one environment: there is nothing to route between. The routing scenarios in `pullRequests.test.ts` are skipped with it |
| `client-runtime/src/state/shellPullRequests.ts` | `loadPullRequests` is an `Effect` stored on the snapshot; yields with `Effect.sleep("1 millis")` | `DeferredShellSnapshot::load_pull_requests` (async, same 400-link batches and 1ms `tokio::time::sleep` yields); `detach_pull_requests` takes `&mut serde_json::Value` | Same behavior over serde values |
| `client-runtime/src/state/shellCommands.ts`, `orchestration.ts`, `projectEntities.ts` | RPC atom families: `openInEditor` command; orchestration atoms over the shell snapshot; per-environment project atoms (`environmentProjectsAtom`, `projectsAtom`, `projectConfigsReadyAtom`) | Not ported as atoms. The project rows, scratch flag (`is_scratch_project`) and project ordering are in `state/projects.rs` and `state/project_grouping.rs`; `openInEditor` is not in the state layer (owned by the editor-open task, N5s) | One environment, so there is no per-environment index to keep; the atoms only fan out over environments |
| `client-runtime/src/state/threadHistoryController.ts` | Effect service with a handler registry (`ThreadHistoryController`, `ThreadHistoryRegistration`) that routes "load earlier" to the mounted thread view | `ClientStateRequest::LoadEarlier` drained by the orchestrator; history pages arrive as `ClientStateSender::ThreadHistoryPage` and fold through `thread_history_merge.rs` | One consumer per thread; no handler registry needed |
| `client-runtime/src/state/composerPathSearch.ts` | Types for the `@` path search state; the search runs as an RPC atom | Types only (`state/composer_path_search.rs`); the composer runs the search in-process | Shapes kept for the composer seam; no logic to port |
