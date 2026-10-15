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
| `crates/persistence/src/t3_import` | The server migrates its own `statev2.sqlite` in place. `NodeSqlite.backup` only seeds v2 from a sibling v1 file. | `T3Import` opens the T3 file `SQLITE_OPEN_READ_ONLY`, copies it with `sqlite3_backup_step` into `/tmp` (busy and locked steps retry; a failed copy deletes the destination), refuses a V2 ledger id (55 and up) or name this build does not have, and a V2 gap the migrator would skip (as `assertV2MigrationsCanRun`). Ids below 55 are accepted, as upstream accepts them. It migrates the copy, and hard-links it to `~/.turin/statev2.sqlite` only when that file is absent. | Turin must not write the live T3 database. A snapshot of a migration-62 database matched a fresh Turin schema. Every event the v2 reader returns decoded. Unknown types are counted, not treated as decode errors. Legacy v1 thread rows stay in the table and are not part of that read. |
| `apps/server/src/persistence/initializeV2Database.ts` (backup step loop) | Busy and locked steps retry until `Done`. A restart from a writer that commits during the copy is not counted. | `crates/persistence/src/t3_import` bounds the copy: 100 restarts (a step that starts again at page 1 is detected from `progress().remaining` not falling), 25ms pause after each restart, and a 60s wall-clock deadline that `More` never resets. Over either limit it fails with the busy error and deletes the partial copy. | A writer committing between steps restarts the copy but still returns `More`. Without a bound a busy T3 database never finishes the copy. |
| `crates/persistence/src/t3_import` file modes | SQLite creates files at 0644 and `tempfile` / `mkdir` use the umask (0755 at 022). The T3 copy is not private. | Snapshot, work, and fresh-schema directories are 0700. The copy is created 0600 before SQLite writes to it. The linked `statev2.sqlite` is the same inode, so it is 0600. `Db::open` still creates Turin's own database with SQLite's default mode. | The copy holds the user's whole history. The import is a one-time adoption, so the imported file stays private. Turin's own database is unchanged from the rest of the app. |
| `crates/persistence/src/t3_import` cleanup | Upstream copies nothing, so it has no temporary database to leave behind. | Each import first removes `turin-t3-import-*` under `/tmp` and `.t3-import-*` under the Turin data dir when nothing in them has changed for an hour. Drop-only cleanup does not run on SIGKILL, abort, or power loss. | A killed import leaves a full copy of the user's T3 database behind. Age is judged from the newest change in the directory or its direct children, so a copy still being written is kept. |
| `crates/persistence/src/migrations.rs` `sql_statements` (add-column and catch-per-statement scripts) | Effect runs each migration script through SQLite, whose tokenizer skips `--` and `/* */` comments. | The splitter skips the same comments before it looks at quotes or `;`. A comment stays in the statement it precedes, and the add-column guard strips leading `--` and `/* */` comments. | Turin splits scripts itself to guard `ADD COLUMN`. An apostrophe or `;` in a comment used to swallow the next `ALTER` or split it into a bad chunk. No shipped script hits this yet. |
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
| `contracts/*` (kept modules except `settings.rs`) | `Schema.optional`/`optionalKey(X)` rejects an explicit `null`; `Schema.NullOr(X)` on a required key rejects a missing key | serde `Option<X>` accepts `null` on optional keys and an absent key on required NullOr keys; both decode to `None`. Examples: `git.rs` `is_remote`/`remote_name`, `VcsRef.worktree_path`, `CheckpointDiffInput.ignore_whitespace`, `initial_answer` | DB and persisted-row compatibility comes first: rows written by earlier builds or hand-edited JSON must still load. No valid T3 payload differs. `settings.rs` keeps the strict `optional_key` codec, tested for null rejection |
| `contracts/orchestration_v2.rs` `OrchestrationV2RawProviderEvent.payload` (`Schema.Unknown`) | key is required; an absent key fails with "Missing key" and re-encode never writes `null` | `#[serde(default)]`: an absent key decodes to `null`, and re-encode writes `"payload": null` | raw provider event rows stored without `payload` must still load. Only input upstream never writes is affected |
| `contracts/usage_limit_source_id.rs` `UsageLimitSourceId` | branded `Schema.String` (`usageLimitSourceId.ts`), accepts `""` | plain `Arc<str>` newtype with a private field; not in the `id!` list | `id!` trims and rejects blank input, which would reject keys upstream accepts. The slug and date brands (`SourceControlProviderKind`, `UsageDay`, `provider_slug!`, `context_id!`) are not ids and are not `id!` either |
| `contracts/pull_request.rs` `provider_requirement`, `pull_request_host_of` | `forgejo`, `gitlab`, `azure-devops` and `bitbucket` provider-requirement entries; Forgejo `locator.remoteUrl` read for the host | only `github` is kept; the Forgejo `remoteUrl` branch is omitted, so the host comes from `canonicalKey` or the provider kind | those source-control hosts are cut (cut list) |
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
| `packages/shared/src/devProxy.ts` | Path prefixes the Vite dev proxy forwards to the server, and the server's dev catch-all that 404s them | not ported (T05a) | Single-origin browser dev through Vite has no counterpart in a one-process GPUI app. Nothing has to agree with a web dev server. |
| `packages/shared/src/cliRelease.ts` | Archive names and checksum file for the `t3` CLI attached to GitHub Releases, plus the mirror base-URL override | not ported (T05a) | Turin ships its own `turin` binary through its own release pipeline. The upstream `t3` archive names and release repository do not apply. |
| `packages/shared/src/legacyCliLauncher.ts` | Node launcher script for the `t3` npm boot service, used by installs before 0.0.41 | not ported (T05a) | Turin has no npm `t3` package and no pre-0.0.41 installs to upgrade. |
| `packages/shared/src/thirdPartyLicenses.ts` | Schema and validator for the third-party license manifest of the bundled JS dependencies | not ported (T05a) | Turin does not bundle upstream's JS dependency set, so there is no manifest to read. Port with the release notices step. |
| `packages/shared/src/shell.ts` | Effect services: `HostProcess.Platform`/`Environment`, `Context.Reference` resolvers (`SpawnExecutableResolution`, `WindowsShellEnvironment`, `CommandAvailability`, `CommandResolutionCache`), `Cache`-backed PATH listings, Effect tracing. `fixPath` is not in this file; PATH hydration is `read_environment_from_login_shell` + `merge_path_entries` | Plain functions taking `Platform`, `ShellEnv` (`IndexMap`), a `SpawnResolver { id, resolve }`, a `CommandResolutionCache` (`global()` for the process-wide map), and `now: Instant`. `withPathDirectoryListings` is not ported: every candidate is stat'd directly. `readUserLoginShell` reads `$SHELL`. `access(X_OK)` is approximated by the execute bits. Tracing tests are not ported | No Effect context here. The listing cache only skips stat calls, so answers match. In login sessions `$SHELL` is the passwd shell. Execute bits approximate `access(2)` for the current user. |
| `packages/shared/src/githubMarkdownLinks.ts` | Regex with lookbehind and lookahead; `remark-parse` + `remark-gfm` | `github_markdown_links.rs`: a hand-written matcher that reproduces the regex's alternation order and backtracking; `markdown` crate GFM parse | `regex` has no lookaround. markdown-rs positions are byte offsets, and edits use the same offsets, so the output is unchanged. |
| `packages/shared/src/{markdownPipeline,codexMarkdownDirectives,markdownListIndentation,markdownGithubAlerts}.ts` | unified/remark/rehype plugins over mdast and hast. Codex directives add custom micromark constructs. The pipeline sanitizes HAST for the DOM | not ported (T05a) | The mdast transforms need a Turin markdown AST decision: the `markdown` crate's mdast has no `data.hProperties`. The sanitize/HAST stage is web-only. Reopen with the GPUI markdown renderer. |
| `packages/shared/src/atomicWrite.test.ts` "succeeds when the write lands but its temp directory cannot be removed" | Effect `FileSystem` layer with injected `remove` failure | not ported | `write_file_string_atomically` uses `std::fs` directly with no fault injection. The removal failure is ignored in code (`let _ =`); no test covers it. |
| `packages/shared/src/KeyedLock.test.ts` "runs the effect in the caller's scope" | Effect `Scope` finalizers | not ported | Rust has no scope to attach finalizers to. The closure runs in the caller's task. |
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
| `provider/ModelManifest.ts` `manifestUpdatedAtMs` and `usage.ts` timestamp `Date.parse` | the full V8 legacy parser | `js::date_parse_ms` takes ISO forms only: a date-only string (UTC), a date-time with or without an offset (no offset means local time, as V8 reads it), and a space in place of `T`. Other legacy forms such as `Oct 10 2026` or `+0200` offsets without a colon return `None`, which the manifest maps to 0 as upstream does for NaN | every timestamp the providers write is ISO; the rest of V8's grammar is not worth porting |
| `subagentProjection.ts` `subagentThreadTitle` clip (`slice(0, 69)`) | clips by UTF-16 code units (`take_utf16`) | JS `length`/`slice` count UTF-16 units; `char` slicing would cut emoji titles at a different place |
| `provider-core` `mcpToolPresentation.ts` `resolveT3McpToolDefinition` gate | `mcp_tool_presentation.rs` carries a hand-copied 84-key `T3_MCP_TOOLS` name inventory and ports only the name resolution, not the display half | `shared/t3McpToolPresentation` has no Rust owner yet; move it to `turin-shared` when the app needs the display names. Keep the list in sync with upstream until then |
