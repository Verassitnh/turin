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
| `optional`/`optionalKey(ForwardCompatibleArray(X))` | `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_array::optional")] Option<Vec<X>>` (same for `forward_compatible_union_array::optional`). `null` fails, as upstream |
| `hasUnknownUnionTag` | `has_unknown_union_tag` |
| `Schema.optional(X)` / `Schema.optionalKey(X)` | `Option<X>` + `#[serde(default, skip_serializing_if = "Option::is_none")]`. Explicit `null` decodes as absent (lenient, see Deviations). `settings.rs` is the exception: `optional_key` rejects `null` |
| `Schema.NullOr(X)` (required key) | `Option<X>`, no skip (encodes `null`). A missing key also decodes as `None` (lenient, see Deviations) |
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
| child processes | `tokio::process::Command` with `kill_on_drop(true)`; long-lived CLIs (providers, hooks) run in their own process group and the group is killed on timeout/shutdown. Hooks, on normal exit: after the shell exits plus a 250 ms drain, the group is killed only if something still holds the stdout/stderr pipes |
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

**Trust (QC wf_e8a1a7c1-d53, T21c).** Global `~/.turin/hooks.toml` is trusted.
Project hooks never run until the user approves them. Approval covers the
**whole `<project>/.turin/` tree** (`hooks.toml` plus scripts such as
`.turin/hooks/*.sh`, nothing skipped): `~/.turin/trusted-hooks.json` maps the
canonical project root → one SHA-256 over sorted relative paths + file bytes.
Any change to any file there (git pull, branch switch, an agent edit)
invalidates it; the hash is re-checked on every reload **and on every
dispatch**, dropping project hooks while keeping global ones. A symlink that
resolves outside `.turin/` makes the project hooks *untrustable* (inert, Approve
refused, reason surfaced). Commands that run repo files *outside* `.turin/`
(`./scripts/x.sh`) are allowed but not covered: they are listed in
`PendingProjectHooks::unhashed_references` so the UI warns, and the docs steer
authors to keep scripts in `.turin/hooks/` (and to use
`$TURIN_PROJECT_ROOT/.turin/…` under `cwd = "thread"`). Untrusted/changed hooks
surface as a pending status the UI shows with an "Approve" action.
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
| `contracts/base_schemas.rs` | `DateTimeUtcFromString` accepts anything `new Date(s)` parses, any year V8 holds | `UtcDateTime` parses V8's ISO forms only (incl. no-zone, date-only, `±HHMM`, minute precision) and rejects instants outside years 0000-9999; `from_datetime` clamps to that range | V8's legacy forms (`Jan 2 2024`, `2024/01/02`) never appear in persisted data; `+010000-…` would break the fixed-width text order |
| `contracts/base_schemas.rs` | `PositiveInt` is a refined number | newtype over `NonZeroU64` with `new(u64) -> Option<Self>` | 0 unrepresentable |
| `contracts/base_schemas.rs` | `isUnknownUnionMember(value)` | no function; match `ForwardCompatible::Unknown` | Rust enums make it a pattern |
| `contracts/base_schemas.rs` | `PortSchema` | omitted | only used by cut features |
| `contracts/base_schemas.rs` | cut-feature exports (AuthSessionId, RpcClientId, SecretRef, DpopFailureReason, Client* enums) | omitted | features cut (PORTING cut list) |
| `app/theme/theme_palettes.rs` | `packages/shared/src/themePalettes.ts` (shared package) | lives in the app crate | only the UI consumes palettes; `turin-shared` owner can move it if another crate needs it |
| `app/theme/theme_palette.rs` | `apps/web/src/themePalette.ts`: custom/environment theme library (storage, theme files, vivid editor), light/dark `halves`, live previews, `resolveDesktopTheme`, `applyThemePalette` CSS variables | resolution helpers only (aliases, `getThemeDefinition` over built-ins, `resolveThemeAppearance` without `halves`, …); `AppTheme::resolve` stands in for `applyThemePalette`; `ThemeDefinition` drops `collection`/`managed` | library, halves and previews belong to the Settings → Appearance task (T20) |
| `app/theme/mod.rs` | browser resolves `oklch()`/`color-mix()`/alpha tokens live from `index.css` | `AppTheme` resolves every token once (palette + contrast + fonts) and pushes a `ThemeConfig` into gpui-component | GPUI needs concrete colors; same values at default contrast |
| `app/theme/mod.rs` | no `data-theme-id` → stock `index.css` tokens (some alpha, e.g. dark `--border: white/6%`) | standard look uses `T3_CODE_{LIGHT,DARK}_THEME_COLORS` (upstream's flattened capture of those tokens) | opaque colors render identically and keep one code path for all themes |
| `app/theme/appearance_contrast.rs` | `AppearanceContrast` is `Schema.Int` in 50..=200, range-checked at decode (contracts/settings.ts:107-112); `applyAppearanceContrast` only applies min/max to the result | `AppearanceContrast::new(u32)` clamps silently to 50..=200; no rounding and no NaN fallback. Settings decode must reject out-of-range values before calling it | the schema check lives at the settings boundary; schema-valid inputs behave identically. The rounding and NaN fallback belong to `clampFontSize`, which is ported faithfully |
| `app/theme/appearance_fonts.rs` | CSS font stacks; canvas probing for availability/monospace; `ui-monospace` resolves to SF Mono | first installed family of the stack via `text_system().all_font_names()`; `-apple-system`/`system-ui` → `.SystemUIFont`, `ui-monospace` (and the hidden `SF Mono`/`SFMono-Regular`) → CoreText's `.AppleSystemUIFontMonospaced`; no custom code font → index.css `--font-mono` stack | GPUI takes one family per run; no canvas; SF Mono is a hidden system family that only resolves through the CoreText alias |
| `app/theme/appearance_fonts.rs` | `resolveTerminalFont*Preference`, `TYPOGRAPHY_ADVANCED_STORAGE_KEY`, installed-family listing, canvas/DOM probes; tests for the terminal helpers | not ported; `resolve_default_family_label` takes the generic-keyword resolver as an argument (upstream measures DOM probes) | terminal cut; GPUI has no DOM. Remaining upstream `it()` cases ported 1:1 |
| `app/ui/button.rs` | shadcn/Base UI `Button` (cva variant table) | own `RenderOnce` on GPUI divs; no `active:scale-[0.97]`, no `media-*` variants, no focus ring | gpui-component `Button` can't express upstream's size table; GPUI has no transforms; media viewers cut |
| `app/ui/tooltip.rs` | Base UI tooltip, `side="top"`, collision-aware | GPUI native tooltip (anchored at the pointer) with upstream's popup surface | no positioner API on GPUI tooltips yet |
| `app/ui/*`, `app/components/chat/composer_surface.rs` | `backdrop-filter` glass (`surface-glass`, `dropdown-glass`, composer) | translucent fill without blur; composer glass flattened over the canvas | GPUI has no backdrop blur, and paints box shadows under translucent fills |
| `app/ui/scroll_area.rs` | `scrollFade` masks, `topbar-scroll-fade` under the header | no fades | GPUI has no alpha masks |
| `app/components/app_sidebar_layout.rs` | `surface-grain`: a 256px fractal-noise SVG tile at 3.5% opacity (128px at 2.745% for T3 Chat) baked into the body, sidebar and inset backgrounds | flat backgrounds | GPUI has no repeating background images; a full-window noise texture costs a large GPU image for a ~1-level luminance dither. Revisit if the flat surfaces read differently side by side |
| `app/components/app_sidebar_layout.rs` | widths persisted in localStorage; panel width animation | in-memory widths; no animation (upstream default duration is 0) | persistence belongs to the settings task |
| `app/components/app_sidebar_layout.rs` | layout toggles: thread details, terminal, right panel, maximize | terminal toggle dropped; thread-details toggle is inert | terminal cut; details panel not ported yet |
| `app/components/right_panel_tabs.rs` | surfaces: browser, terminal, files, diff, PRs, device; on tab hover the surface icon swaps for an X (`ui/panel-tab-close-button.tsx`) | Diff + Pull requests tabs only, placeholder body; on tab hover the surface icon recolors to foreground, no X swap | cut list; the X closes a surface, and the close action is cut |
| `app/components/drag_region.rs` | `-webkit-app-region: drag`, with `button, input, textarea, select, a` exempted (`no-drag`) | a left press on the region that moves past 2px while held calls `start_window_move`; double-click on the region itself → `titlebar_double_click`. Interactive elements over a region call `drag_region::no_drag` on mouse-down (stop propagation) — ui `Button`/`Toggle`, sidebar icon buttons and panel tabs do; new interactive titlebar elements must too | Electron CSS has no GPUI equivalent; GPUI bubbles mouse events to every hovered hitbox |
| `app/components/sidebar.rs` | live thread list (sections, statuses, hover actions, search) | static placeholder rows with upstream geometry | data/behaviour land with the sidebar task |
| `app/components/sidebar/sidebar_chrome.rs`, `app/branding.rs`, `app/main.rs` | `SidebarBrandMark`: T3 wordmark + muted "Code"; `APP_BASE_NAME` "T3 Code" with a stage label; window title from branding | brand slot renders the text "Turin" in the slot's styling (`text-sm font-medium`, foreground, `h-7` at `--workspace-titlebar-content-left`); `APP_BASE_NAME = "Turin"`, no stage label; window title "Turin". `tracking-tight` is not reproduced | Turin is its own product (PM decision); GPUI has no letter-spacing |
| `app/components/thread_sidebar_width.rs`, `app/components/app_sidebar_layout.rs` | `SidebarBrandWidthProbe` measures the brand and raises the minimum via `resolveThreadSidebarMinimumWidth` | probe not ported; minimum fixed at `THREAD_SIDEBAR_MIN_WIDTH` (208px); the function is ported and tested | "Turin" at 14px fits well inside 208px; wire the probe when the interface font size becomes configurable (settings task) |
| `app/components/chat/composer_surface.rs` | `chat/ComposerSurface.tsx`, `chat/ChatComposer.tsx` (body, placeholder, footer) and `chat/ComposerPrimaryActions.tsx` (send) | one static frame in `composer_surface.rs` | placeholder until the composer task splits them back into `chat_composer.rs` / `composer_primary_actions.rs` with behaviour |
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
| `hooks` project `.turin/` | Upstream never runs commands from repo files; `t3.json` scripts are import-only (`resolveProjectScripts` reads settings only). | Project hooks are inert until the user approves the **whole `.turin/` tree**: `~/.turin/trusted-hooks.json` maps canonical project root → SHA-256 over sorted relative paths + bytes of every file under `.turin/`; re-checked on reload and on every dispatch. Symlinks leaving `.turin/` are untrustable. Commands running repo files outside `.turin/` are allowed but reported as `unhashed_references` (best-effort text scan; those files are not covered). Pending/changed hooks surface as `HookUpdate::ProjectHooksPending`; `HookRunner::trust_project_hooks(reviewed_hash)` approves. Global `~/.turin/hooks.toml` is trusted. | Hot-reloaded repo-controlled shell execution would otherwise run on clone/pull/agent edit. Residual: scripts outside `.turin/` can change without re-approval (same limit as direnv); the UI warns. QC wf_e8a1a7c1-d53 item 1, wf_44e1924e-b5c item 1. |
| `hooks` environment | `fixPath` / `readEnvironmentFromLoginShell` / `mergePathEntries` (+ launchctl fallback) hydrate `process.env.PATH` once at server startup (`os-jank.ts`). | `turin-hooks` inherits the app env and does no per-hook hydration. **Tracked:** port that hydration into `turin-shared` and call it once at app startup (T05b/T22). | Until then a Finder-launched app gives hooks `/usr/bin:/bin` (`gh`, `npm`, `bun` → exit 127). QC item 5. |
| `hooks` thread launch | `ThreadLaunchService` creates the worktree and binds the thread. | A blocking `thread.created` hook prints `{"worktreePath","branch"}` as its last stdout line; `DispatchSummary.thread_workspace` carries it and the orchestrator binds the thread. | Hooks replace launch-into-worktree (return protocol above). QC item 7. |
| `hooks` process exit | `processRunner.ts` waits for stdout/stderr EOF and never kills the group on normal exit (a background job holding the pipes stalls the run until timeout). | The run ends when the shell exits plus a 250 ms drain; if anything still holds the pipes, the whole group is killed (including output-redirected jobs). Daemons must leave the group (`setsid`). Timeout/shutdown always kill the group, using the pgid recorded at spawn. | A hook must not hang `thread.created` for the full timeout because of a stray `cmd &`; the early kill is the safe failure direction. QC wf_44e1924e-b5c item 2. |
| `apps/server/src/persistence/*` timestamps | Agent-event `occurredAt` is `DateTimeUtc`. Project `occurredAt`, receipt `acceptedAt`, and provider `lastSeenAt` are `IsoDateTime`. | Agent-event `occurred_at` is `UtcDateTime` and is stored with its canonical `YYYY-MM-DDTHH:MM:SS.sssZ` display. Project, receipt, and provider `*_at` stay caller-supplied strings. SQL `strftime` expressions are unchanged. | Only the agent-event field is `DateTimeUtc` upstream. The other timestamps are `IsoDateTime` pass-through. |
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
| `contracts/vcs.rs` `VcsProcessExitError::from_process_exit` | `NotFound` detail is "Merge request not found." for command `glab`, "Pull request not found." for `gh` or `az`, else "VCS resource not found." | only `gh` gets "Pull request not found."; `glab`/`az` fall through to "VCS resource not found." | GitLab and Azure DevOps hosts are cut |
| `git/git_vcs_driver_core.rs` | trace2 JSON monitor emits span events for every git command | not emitted | span events are observability; command results, env, and timeouts are unchanged |
| `git/shared_git.rs` | remote host detection covers GitHub, GitLab, Bitbucket, Azure, Forgejo, GitCafe | GitHub and GitHub Enterprise only; other hosts return none | host detection for the other providers belongs to source control, which this crate does not import |
| `git/detach_stack_frame.rs` | Effect cache workaround that detaches a stack frame | identity function | there is no Effect runtime to work around |
| `git/vcs_provisioning_service.rs` | repository init only in this service; thread worktrees are created by thread launch | `init_repository` only | Turin creates thread worktrees in hooks, so that path is not wired |
| `git/git_manager.rs` | commit text, setup scripts, pull-request link, and post-push refresh call text generation, project setup, and the orchestrator | traits with logging defaults; a provided commit message still commits; empty text falls back to "Update project files" | those services are not in this crate. Call sites stay, and they do not fail the git action |
| `git/git_manager.rs` | pull-request lookup re-reads remote identity and errors if it changes mid-lookup | one remote URL read; a missing URL when a remote is selected is still an error | the second read is a race check around the host CLI, which is behind `ChangeRequestHost` |
| `git/git_manager.rs` | toast labels use the host's change-request terminology | labels say "PR" | terminology comes from the source-control provider |
| `git/git_manager.rs` | local checkout of a pull request goes through the provider's `checkoutChangeRequest` | switches to the head branch the host reported; worktree mode still calls `create_worktree` | fetching the head is the provider's job; explicit and pull-request worktrees stay |
| crate check | `scripts/check.sh` clippy includes path dependencies | `cargo clippy -p turin-git --all-targets --no-deps -- -D warnings` | `turin-contracts` still has a dead `deserialize_vec` warning, and this task does not edit contracts |
| `project/AgentSessionJson.ts` | `stream-json` tokenizer and `Assembler`; selector walks the Effect schema AST | byte-level pushdown reader; `JsonSelectSchema` tree written by hand for the transcript record | Rust schemas are not introspectable. Keep/drop, last-wins duplicate keys, depth 128, and budget errors match |
| `project/AgentSessionJson.ts` | allocation budget charged per stream-json token (64 + text × 2) | charged per selected value and per key; unselected values cost nothing | the budget still bounds retained data. Charging unselected tokens would reject records upstream accepts |
| `project/AgentSessionJson.ts` | lone `\uD800`-range escapes survive as JS strings | replaced with U+FFFD | Rust strings cannot hold lone surrogates |
| `project/RepositoryIdentityResolver.ts` | Effect `Cache` coalesces concurrent misses for one key | `ttl_cache::TtlCache`, no single-flight; concurrent misses each run git | coalescing is a cost optimization only; results are equal |
| `project/RepositoryIdentityResolver.ts` | `ProcessRunner` service | `GitCommandRunner` trait, implemented for `turin_git::process::ProcessRunner` | injection point for scripted git output in tests |
| `project/RepositoryIdentityResolver.ts` | remote names sorted with `localeCompare` | byte-order sort | differs only for non-ASCII remote names |
| `project/RepositoryIdentityResolver.ts` | identity with an empty canonical key or remote URL still returned | returns `None` | `TrimmedNonEmptyString` cannot hold `""` |
| `project/ProjectFaviconResolver.ts` | `FileSystem` service; candidates probed with `forkScoped` and interrupted after a hit | `tokio::fs`; `FuturesOrdered`, dropping the rest after a hit | same ordering and error rules; runtime-specific cancellation |
| `project/ProjectFaviconResolver.ts` | `<link>` icon match uses lookaheads in one regex | tag regex, then `rel` and `href` regexes on the tag | the regex crate has no lookahead; the tested cases match |
| `project/ProjectFaviconResolver.ts` | `t3.json` `iconPath` via `T3ProjectFileLoader` | `ProjectIconPathSource` trait, none wired by default | the t3 project-file feature is on the cut list and its loader is not in N2's file list |
| `project/ProjectFaviconResolver.test.ts` | FileSystem-instrumented tests (peak concurrency, `Effect.never` ordering, injected stat/read failures), `t3.json` cases | not ported; failure cases use real `ELOOP` symlinks | there is no FileSystem seam; the behavior runs on the real filesystem |
| `project/ProjectFaviconResolver.ts` | `ProjectFaviconResolutionError.cause` is `Schema.Defect()` | closed `ProjectFaviconCause` enum, boxed | callers match on causes; boxing keeps `Result` under clippy's size limit |
| `project/ProjectEnrichmentService.ts` | repository-identity lookup can fail (defect → unresolved) | identity source returns `Option`; failure path removed | `RepositoryIdentityResolver.resolve` has no error channel upstream (git errors resolve to `null`) |
| `project/ProjectEnrichmentService.test.ts` | logger-capture test; defect-as-identity-failure cases | not ported | no log-capture seam; the defect path cannot occur |
| `project/ProjectEnrichmentService.ts` | `PubSub.sliding(256)` | `tokio::sync::broadcast` with capacity 256 | lagging subscribers receive `Lagged` instead of a silent drop; subscribers must handle it |
| `project/ProjectEnrichmentService.ts` | workers are scoped fibers | tokio tasks in a `JoinSet`, aborted on drop | ownership by the service, same lifetime |
| `project/ProjectService.ts` | orchestration collaborators (`ProjectStore`, `ProjectionStore`, `EventSink`, `ProjectCommands` planner, `ThreadCommandExecutor`, `LegacyV1ThreadImporter`, `ServerSettings`) | narrow traits in `project_ports.rs`: `ProjectStoreReads`, `ProjectEventSink`, `ProjectCommandPlanner`, `ThreadDeletions`, `ProjectSettingsOverrides`, `EventIdAllocator` | orchestration is not on this branch (T13a/b/c). The T22 wiring implements the traits; planning stays with the orchestrator |
| `project/ProjectService.ts` | `planProjectCommand` decides events and rejections inside `ProjectCommands.ts` | `ProjectCommandPlanner` trait; rejection receipts encoded by `ProjectCommandRejection::encode` as `{_tag, ...}` | the encoding is written from the service's decode side. T22 must check it against upstream's `encodeProjectCommandRejection` before any receipt is read from an existing database |
| `project/ProjectService.ts` | `deleteChildThread` takes the thread command lock, reads the projection, plans and commits `thread.delete` | the whole step sits behind `ThreadDeletions::delete_child_thread`; the service still runs the cascade, the non-empty check and the retry order | thread planning is orchestration's. The command id `{command}:delete-thread:{thread}` is built in the implementation |
| `project/ProjectService.ts` | `Effect.cachedInvalidateWithTTL` and fiber-based locking | `KeyedLock` (semaphore per key, released on guard drop) | same ordering: project lock outside workspace lock |
| `project/ProjectService.ts` | errors are tagged `Schema.TaggedError` values | `ProjectServiceError` enum and `ProjectOperationError` struct | same messages and tags |
| `project/ProjectService.test.ts` (4 cases) | enrichment failure, metadata invalidation on move, slow metadata, update waiting on a lock while its project is deleted | not ported | need the enrichment and lock timing harness; the first three need Effect `TestClock` timing and the last needs the real `ProjectCommands` planner |
| `project/ProjectService.deletion.test.ts` | full orchestration deletion cascade, V2 shell, legacy transcript import | not ported | depends on orchestration thread deletion; the cascade and retry rules are covered by `ProjectService` tests against a fake |
| `project/ManagedProjectFolders.ts` | `Crypto.randomUUIDv4`, `DateTime.now`, `FileSystem`, `ServerConfig.baseDir` | `uuid` v4, `chrono::Utc::now`, `tokio::fs` and `std::fs`, a `base_dir` constructor argument | runtime services replaced by their direct equivalents |
| `project/ManagedProjectFolders.ts` | `GitWorkflowService.isRepository`, `GitVcsDriver.readConfigValue`/`execute` | `RepositoryProbe` and `GitCli` traits; real-git adapters live in `testing.rs` for tests | the app wires the `turin-git` driver. Probe errors keep upstream's split: git missing (hides nothing) vs other failures (hide Scratch) |
| `project/ManagedProjectFolders.ts` | `Effect.ensuring`/`onExit` removes an unused folder on any exit | `FolderGuard` drops and removes the folder unless disarmed | Drop also runs on cancellation, same as upstream's interrupt path |
| `project/ManagedProjectFolders.ts` | `scratchRoot` via `cachedInvalidateWithTTL(infinity)` | `tokio::sync::OnceCell`; a cancelled probe leaves it unset | same caching and invalidation |
| `shared/path.ts` `newProjectFolderName` | in `packages/shared` | `project_folder_name.rs` in this crate | the Rust `shared` crate does not port `path.ts` yet. Move it there when it does |
| `shared/KeyedLock.ts` | Effect `Semaphore` per key with `withLock` | `keyed_lock.rs` (`KeyedLock::lock` returns a guard) | same first-in, first-out semantics and cleanup of idle keys; not reentrant, as upstream |
| `project/ManagedProjectFolders.test.ts` | runs the real `ProjectService`, `GitVcsDriver`, SQLite | runs against `FakeProjects` (in-memory, same conflict rules) and real `git` through `SystemGit` | the Rust service layer is wired in T22. Git behavior is real; storage is faked |
| `contracts/*` (kept modules except `settings.rs`) | `Schema.optional`/`optionalKey(X)` rejects an explicit `null`; `Schema.NullOr(X)` on a required key rejects a missing key | serde `Option<X>` accepts `null` on optional keys and an absent key on required NullOr keys; both decode to `None`. Examples: `git.rs` `is_remote`/`remote_name`, `VcsRef.worktree_path`, `CheckpointDiffInput.ignore_whitespace`, `initial_answer` | DB and persisted-row compatibility comes first: rows written by earlier builds or hand-edited JSON must still load. No valid T3 payload differs. `settings.rs` keeps the strict `optional_key` codec, tested for null rejection |
| `contracts/orchestration_v2.rs` `OrchestrationV2RawProviderEvent.payload` (`Schema.Unknown`) | key is required; an absent key fails with "Missing key" and re-encode never writes `null` | `#[serde(default)]`: an absent key decodes to `null`, and re-encode writes `"payload": null` | raw provider event rows stored without `payload` must still load. Only input upstream never writes is affected |
| `contracts/usage_limit_source_id.rs` `UsageLimitSourceId` | branded `Schema.String` (`usageLimitSourceId.ts`), accepts `""` | plain `Arc<str>` newtype with a private field; not in the `id!` list | `id!` trims and rejects blank input, which would reject keys upstream accepts. The slug and date brands (`SourceControlProviderKind`, `UsageDay`, `provider_slug!`, `context_id!`) are not ids and are not `id!` either |
| `contracts/pull_request.rs` `provider_requirement`, `pull_request_host_of` | `forgejo`, `gitlab`, `azure-devops` and `bitbucket` provider-requirement entries; Forgejo `locator.remoteUrl` read for the host | only `github` is kept; the Forgejo `remoteUrl` branch is omitted, so the host comes from `canonicalKey` or the provider kind | those source-control hosts are cut (cut list) |
| `project/AgentSessionImporter.ts` | Effect services: `ProjectService`, `Orchestrator`, `EventSink`, `ProviderSessionRuntime`, `IdAllocator`, `AgentSessionScanner` | narrow seams: `ThreadRecordReader`, `OrchestrationEventWriter`, `ProviderRuntimeRows`, `ProviderThreadIds` in `project_ports.rs`; `ProjectServiceApi` and the scanner trait `AgentSessionScanner` in `agent_session_scanner.rs`. Payloads are contract types | orchestration is not on this branch. The T22 wiring implements the seams |
| `project/AgentSessionImporter.ts` | `Effect.option(getThreadRecords)` and `Effect.catch` per thread | a failed thread read counts as absent. A per-thread failure logs a warning and counts as skipped | same outcome for failures. Upstream defects still abort the batch; Rust has no defect channel, so every per-thread error is a skip |
| `project/AgentSessionImporter.ts` `DateTime.makeUnsafe` | an invalid timestamp is a defect that aborts the batch | an invalid timestamp is a per-thread failure (skipped) | the scanner normalizes timestamps, so this only differs on corrupt input |
| `project/AgentSessionImporter.ts` `ProviderDriverKind.make` | unvalidated brand | `ProviderDriverKind::new` (validated slug) | the two drivers the scanner emits always pass |
| `project/AgentSessionImporter.ts` `AgentSessionScanError.cause` | the Effect error object | the error's message as a JSON string | contract `cause` is a JSON value; Rust errors have no structured JSON form |
| `shared/path.ts` `normalizeProjectPathForDispatch`, `normalizeProjectPathForComparison` | in `packages/shared` | `project_path.rs` in this crate | same as `project_folder_name.rs`: move both to the Rust `shared` crate when it ports `path.ts` |
| `project/AgentSessionImporter.test.ts` (1 case) | Effect test layers | ported with `FakeProjects`, a fake orchestrator that doubles as thread reader and event writer, a fake runtime table, and a fake scanner | Effect layers replaced by plain fakes; assertions unchanged |
| `project/AgentSessionScanner.ts` `TranscriptMessage.role` | decoded, never read; a non-string `role` fails the record | field not modeled; serde ignores it, so a non-string `role` keeps the record | the value is never read. Only a malformed record differs, and it is still parsed from its other fields |
| `project/AgentSessionScanner.ts` `normalizeTimestamp` via `DateTime.make` | accepts any string `Date` can parse | `UtcDateTime::from_str` (ISO-8601 with offset), else the fallback time | the CLIs write ISO-8601. Non-ISO values fall back to the transcript's last-active time |
| `project/AgentSessionScanner.ts` record decode | `Schema.decodeUnknownOption` on each line: `optional` keys reject explicit `null` | serde `Option` accepts `null` on those keys (the contracts rule above) | a `null` on a transcript key is read as absent. Upstream drops the whole record |
| `project/AgentSessionScanner.test.ts` pure parser cases (16) | `it.each`, Effect-free | ported in `agent_session_scanner.rs` `tests` | |
| `project/AgentSessionScanner.test.ts` filesystem cases (`scan`, `recentThreads`) | `it.layer` with `FileSystem`, `ServerConfig`, `ServerSettings`, `ProjectStoreV2`, `TestClock`; `it.effect.each` and `Layer.mock` | 51 Rust tests in `agent_session_discovery/tests.rs` (upstream's `each` variants run in one test). Temp-dir fixtures; `FsDouble` replaces the instrumented `FileSystem` overrides; `Fixture::now_ms` replaces `TestClock` | the real `~/.claude` and `~/.codex` are never read |
| `project/AgentSessionScanner.ts` discovery (`scan`, `recentThreads`, git identity, exclusions, budgets) | `Effect.fn` over `FileSystem`, `ServerConfig`, `ServerSettings`, `ProjectStoreV2`, `HostProcess`, `Semaphore`, `Stream` | `agent_session_discovery.rs`: `ScannerFileSystem` (tokio fs by default), `AgentSessionDiscoverySettings`, `ProjectStoreReads`, `AgentSessionDiscoveryHost`; `recent_threads` is an `unfold` stream that takes the import lock per transcript | the Rust parser module already holds the transcript parser, so discovery is a sibling module |
| `provider/Drivers/CodexHomeLayout.ts` (shared-home path only) | `resolveCodexHomeLayout` | `codex_home_layout.rs`: direct and auth-overlay modes, same shared/effective paths and continuation key | shadow-home materialization (`CodexShadowHome*`) belongs to the Codex provider, not this crate |
| `shared` `expandHomePath` (`provider-core/src/server/pathExpansion.ts`) | `expandHomePath` | `project_path::expand_home_path` | `turin-git` has a `pub(crate)` copy; no cross-crate export |
| `project/AgentSessionScanner.ts` `scan` realpath exclusion | `directoryIdentity` and `isExcludedProjectPath` compare the raw `baseDir` and `worktreesDir` spellings | same comparison; the app must pass canonical `base_dir` and `worktrees_dir` (the tests do) | on macOS `TMPDIR` sits under `/var`, a symlink to `/private/var`. A realpath of a sandbox then misses the raw base prefix, so the symlink-into-worktrees case passes only with canonical config dirs. Canonicalize at config load |
| `project/AgentSessionScanner.ts` `localeCompare` (transcript and candidate ties) | locale-aware compare | byte-order `cmp` | differs only for non-ASCII paths in ties |
| `project/AgentSessionScanner.ts` `ProviderInstanceId.make` / `ProviderDriverKind.make` | unvalidated brands | `ProviderInstanceId::new` and `ProviderDriverKind::new`; a settings key that fails validation is skipped, the built-in default instance is always valid | the brands are validated slugs in Rust |
| `project/AgentSessionScanner.ts` `DateTime.now` | `DateTime.now` / `TestClock` | `AgentSessionDiscovery::with_clock` (wall clock by default) | test seam, same comparisons |
| `project/AgentSessionScanner.ts` `TranscriptRecord.payload` / `record.cwd` | `cwd` on the record | the Rust `TranscriptRecord` now also keeps `cwd` (the parser ignored it) | the discovery reads it; the parser never reads it, so its output is unchanged |
| `project/AgentSessionScanner.ts` `readTranscript` decoder | `TextDecoder` streaming over byte chunks | UTF-8 bytes split at `\n` (never inside a multi-byte sequence) and a partial tail held across chunks | same text; the JSON reader still gets `&str` |
| `project/AgentSessionScanner.ts` `readCwd` | `Effect.scoped` open failure becomes `null`; `readAlloc` returning `Option.none` ends the scan with `readLastRecord` | the same branches: an `Err` read is `None`, `Ok(None)` is end-of-file | the Rust `read_alloc` returns `io::Result<Option<Vec<u8>>>`, which keeps the two cases apart |
