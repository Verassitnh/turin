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
| `app/ui/*`, `app/composer/views/chat_composer.rs` | `backdrop-filter` glass (`surface-glass`, `dropdown-glass`, composer) | translucent fill without blur; composer glass flattened over the canvas | GPUI has no backdrop blur, and paints box shadows under translucent fills |
| `app/ui/scroll_area.rs` | `scrollFade` masks, `topbar-scroll-fade` under the header | no fades | GPUI has no alpha masks |
| `app/components/app_sidebar_layout.rs` | `surface-grain`: a 256px fractal-noise SVG tile at 3.5% opacity (128px at 2.745% for T3 Chat) baked into the body, sidebar and inset backgrounds | flat backgrounds | GPUI has no repeating background images; a full-window noise texture costs a large GPU image for a ~1-level luminance dither. Revisit if the flat surfaces read differently side by side |
| `app/components/app_sidebar_layout.rs` | widths persisted in localStorage; panel width animation | in-memory widths; no animation (upstream default duration is 0) | persistence belongs to the settings task |
| `app/components/app_sidebar_layout.rs` | layout toggles: thread details, terminal, right panel, maximize | terminal toggle dropped; thread-details toggle is inert | terminal cut; details panel not ported yet |
| `app/components/right_panel_tabs.rs` | surfaces: browser, terminal, files, diff, PRs, device | Diff + Pull requests tabs only, placeholder body | cut list |
| `app/components/drag_region.rs` | `-webkit-app-region: drag`, with `button, input, textarea, select, a` exempted (`no-drag`) | a left press on the region that moves past 2px while held calls `start_window_move`; double-click on the region itself → `titlebar_double_click`. Interactive elements over a region call `drag_region::no_drag` on mouse-down (stop propagation) — ui `Button`/`Toggle`, sidebar icon buttons and panel tabs do; new interactive titlebar elements must too | Electron CSS has no GPUI equivalent; GPUI bubbles mouse events to every hovered hitbox |
| `app/components/sidebar.rs` | live thread list (sections, statuses, hover actions, search) | static placeholder rows with upstream geometry | data/behaviour land with the sidebar task |
| `app/components/sidebar/sidebar_chrome.rs`, `app/branding.rs`, `app/main.rs` | `SidebarBrandMark`: T3 wordmark + muted "Code"; `APP_BASE_NAME` "T3 Code" with a stage label; window title from branding | brand slot renders the text "Turin" in the slot's styling (`text-sm font-medium`, foreground, `h-7` at `--workspace-titlebar-content-left`); `APP_BASE_NAME = "Turin"`, no stage label; window title "Turin". `tracking-tight` is not reproduced | Turin is its own product (PM decision); GPUI has no letter-spacing |
| `app/components/thread_sidebar_width.rs`, `app/components/app_sidebar_layout.rs` | `SidebarBrandWidthProbe` measures the brand and raises the minimum via `resolveThreadSidebarMinimumWidth` | probe not ported; minimum fixed at `THREAD_SIDEBAR_MIN_WIDTH` (208px); the function is ported and tested | "Turin" at 14px fits well inside 208px; wire the probe when the interface font size becomes configurable (settings task) |
| `app/composer/views/chat_composer.rs`, `footer.rs`, `banners.rs` | `chat/ComposerSurface.tsx`, `chat/ChatComposer.tsx`, `chat/ComposerPrimaryActions.tsx` (replaces T03's static `composer_surface.rs`) | `ChatComposer` entity: frame, editor, footer, primary actions (labels as upstream), drawers. Glass flattened over the canvas; `ComposerSurface.Shell/Host/Main` clip-path context strip not ported (context chips cut) | GPUI has no clip-path/backdrop blur; the context strip only exists for cut context records |
| `app/composer/{rich_doc,editor_state,views/prompt_editor}.rs` | Tiptap/ProseMirror editor (`ComposerPromptEditorTiptap.tsx`, extensions, node views) | Native flat line model over the markdown source (blocks keep their exact prefixes), snapshot undo with kind grouping, own input rules/surround-on-type/list+fence commands, `InlineFlow` for text layout, `EntityInputHandler` for IME. Tabs inside code blocks are not expanded; lone-surrogate key events (a JS-only case) have no test | no DOM/ProseMirror in GPUI; the model reproduces the behaviours the upstream suites pin, ported 1:1 where they apply |
| `app/components/chat/markdown/inline_flow.rs` | T16 line-box engine | extended with `FlowHighlight`, `FlowCaret`, `FlowProbe`, `.selectable/.highlights/.caret/.probe` for the editor (selection paint, caret, hit-testing, vertical moves); chat markdown output is unchanged | the editor needs the same wrapping as the message markdown; one engine keeps them identical |
| `app/composer/{js_string,inline_tokens,search_ranking,dispatch}.rs` | `@t3tools/shared` helpers (T05a) and T14's `composer_dispatch` | copied into the composer module instead of depended on | the shared crates are not wired into `turin` yet; replace the copies with the dependency when they are |
| `app/composer/{view_model,mock_source}.rs` | `ChatComposerProps` callbacks fed by `ChatView`/stores | `ComposerViewModel` + `ComposerSource` trait behind a `ComposerStore` entity; `MockComposerSource` provides fixtures and local handling | seam for T14: build the view model from reducers, `store.publish(..)`, implement `ComposerSource` |
| `app/composer/draft_store.rs` | `composerDraftStore.ts` (zustand + localStorage) | in-memory per-thread drafts and stash; no persistence, draft-thread sessions, sticky model, terminal/preview/review/thread-context records, environment scoping of uploads | persistence belongs to the settings/storage task; the cut records have no producers |
| `app/composer/views/menu.rs`, `footer.rs` | command menu (`@`/`/`/`$`/`#`), thread and pull-request items insert `ContextRecord` references | thread and pull-request picks insert their label as plain text; no review-comment or thread-context records; the usage-limits banner body (`LimitWindows`, reset credits) is not rendered, only the title/description a source supplies | context references and usage windows belong to features outside the composer |
| `app/composer/views/chat_composer.rs` | `ComposerImageThumbnail`, expanded image dialog, `SnapShot*` frames, video previews, attachment upload progress | image thumbnails (64px) and file chips with remove; no expanded preview, snapshots, video or upload state; file bytes stay in the draft until send | preview/viewer surfaces are cut; uploads are the source's concern |
| `app/composer/views/banners.rs` | `QueuedRunsControl.tsx` drag-reorder, expand-on-hover notice stack, `NoticeDescription` details popover, `ComposerTasksBadge` | queue rows without drag reorder (edit/steer/remove only), notice stack expands on hover of the front banner, long descriptions ellipsize (no details popover), no tasks badge/drawer | needs GPUI drag sources and DOM overflow measurement; tasks progress has no view-model producer yet |
| `app/composer/views/banners.rs` | `ContextWindowMeter.tsx` hover popover | click-toggled popover, ring painted with a stroked path | GPUI has no hover-open popover with close delay |
| `app/composer/views/{model_picker_view,language_picker_view}.rs` | Base UI `Combobox`/`ComboboxSearchInput` | search is a single-line `PromptEditor` (plain mode); virtualized list replaced by a scroll list (model counts are small); list height estimated instead of measured | no combobox primitive; the editor already gives IME/selection for free |
| `app/composer/views/chrome.rs` | `useComposerMenuProps` floating layers | one open layer at a time rendered with `deferred(anchored(..))` over an outside-click backdrop; glass painted opaque | gpui-component's `Popover` needs a `Selectable` trigger and cannot host these keyboard-driven lists |
| `app/composer/{footer_layout,restingComposerControlsMeasurement}` | DOM measurement of resting controls (`useRestingComposerControlsLayout`, `observeResize`) | `footer_layout` rules are ported; the footer picks compact actions from the last painted width, with no hidden-block overflow menu or resting transition | no DOM `ResizeObserver`; the resting/collapse-on-scroll mode is a web-only affordance |
| `app/composer/views/chat_composer.rs` | composer mention chips drag (`composerMentionDrag.ts`) and `composerContextUndo.ts` | mention-drag rules ported as pure code but the chips are not draggable between editors; context undo skipped | chips drag needs a typed GPUI payload; context records are cut |
| `app/composer/views/chat_composer.rs` | `TURIN_COMPOSER_DEMO` (new) | debug-only scenario hook that preloads prompt/layers/fixtures so screenshots can be taken without synthetic input | the capture tooling has no accessibility permission |
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
| `contracts/*` (every `Schema.Number` → `f64`) | JS encodes whole-valued numbers without a fraction (`7`) | serde_json encodes the `f64` `7.0` as `7.0`; decode accepts both | Both are the same JSON number to every reader (upstream decodes `7.0` identically), so no on-disk or wire incompatibility; only byte-level diffs of re-encoded JSON, and `serde_json::Value` equality between `7` and `7.0`, differ |
| `app/components/chat/timeline/turn_item.rs` | Views read `OrchestrationV2TurnItem` / `OrchestrationV2ProjectedTurnItem` from contracts | UI-side mirror with upstream field names (ids/timestamps as strings, `Schema.Unknown` as `serde_json::Value`) | orchestration-v2 contracts not on this branch; T14 maps the real projection into it (or replaces it with a re-export) |
| `app/components/chat/timeline/types.rs` | `TimelineEntry` includes `html-render` / `mcp-app`; rows include `worktree-setup` | Those kinds dropped | HTML render, MCP apps and worktree setup are cut |
| `app/components/chat/timeline/messages_timeline_logic.rs` | `MessagesTimelineRowsInput.timelineEntries` is a JS array (shared by reference) | `Arc<Vec<TimelineEntry>>`; rows are `Arc<MessagesTimelineRow>` and unchanged rows are detected with `Arc::ptr_eq` | a recompute (every streamed chunk) must not deep-copy thousands of entries; `===` identity has no Rust equivalent otherwise |
| `app/components/chat/messages_timeline.rs` | LegendList with `maintainScrollAtEnd`, `maintainVisibleContentPosition`, disclosure scroll anchoring, minimap, find, citation toolbar | GPUI `ListState` (`FollowMode::Tail`), spliced over the changed row range; no minimap, find, citation toolbar or disclosure anchoring | GPUI list gives virtualization + tail following; the other features are separate tasks |
| `app/components/chat/messages_timeline.rs` | `WorkingTimer` / `AgentElapsed` write DOM text every second | the timeline entity notifies once a second while the thread works | GPUI re-renders the visible rows only; no per-node text writes |
| `app/components/chat/markdown/inline_flow.rs` | Browser inline layout (`white-space`, `overflow-wrap: anywhere`, `box-decoration-break: clone`, mixed font sizes) | Own inline-flow element: breaks after spaces and around CJK, splits overlong words at char boundaries, CSS line-box metrics with a strut, inline-code chrome per line fragment | `StyledText` is single-size and has no inline boxes. Not full UAX #14 line breaking (no breaks after hyphens/slashes, same as Chromium for `overflow-wrap: anywhere` words) |
| `app/components/chat/markdown/inline_flow.rs` | Native browser selection across the whole page; double-click selects a word; `ChatMarkdown.handleCopy` copies markdown for whole-message selections | Each text leaf is a gpui-base window-selection participant (drag across leaves and rows works, copy joins leaves with `\n`); no double-click word selection; plain-text copy | gpui-base exposes participant geometry but its word hit-testing is crate-private |
| `app/components/chat/markdown/markdown_pipeline.rs` | remark + rehype (`rehype-raw` + sanitize renders allowed HTML such as `<details>`, codex directives, skill chips, file-link chips, media) | markdown-rs mdast lowered to a block tree; raw HTML is literal text; codex directives, skill chips and images are not rendered (images show their alt text). File chips (`MarkdownFileLink`) render for path-shaped inline code and file links, with a generic file glyph instead of pierre icons and without the parent-folder disambiguation suffix; external links get the GitHub mark or a globe instead of the fetched favicon | markdown-rs is the Rust port of micromark/mdast, so GFM parsing matches; the rich components need asset/file services not ported yet |
| `app/components/chat/markdown/code_highlight.rs` | Shiki with `pierre-light` / `pierre-dark`, pierre file icons in the code block header, "Run in terminal", Mermaid | tree-sitter (gpui-component grammars, no SQL: its crate pins `cc ~1.2`, conflicting with rusqlite) with capture names mapped to the pierre token colors; header shows the language text; no run button or Mermaid | no TextMate engine in-process; terminal is cut |
| `app/components/chat/markdown/chat_markdown.rs` | Tables: auto table layout, collapse/expand cells toggle, copy as Markdown or CSV menu; streaming blocks fade in (600ms) | flex columns capped at 24rem, always expanded (the `wordWrap` default), copy as Markdown; no fade-in | GPUI has no table layout or opacity starting styles |
| `app/components/chat/timeline_work_rows.rs` | `V2ItemInspector` fetches withheld output and renders structured values; `live-tool-shine` / `live-activity-focus` animations; thread-read titles link to threads; subagent cards read live runtime and provider icons, hover cards | reduced inspector over the item's own payload (command + output, tool args/output, file change, searches); static labels; no thread-read links, hover cards or provider icons (bot glyph) | Turin items carry their output; continuous animations and those panels are out of scope here |
| `app/components/chat/timeline_message_rows.rs` | User row: video/preview-annotation/context chips, review comments, prompt-link copy, scheduled-task link, mask fade on collapsed text; assistant fork button | images from local `preview_path`, files as rows, gradient fade into the bubble; no fork button | composer contexts and provider capabilities are other tasks; GPUI has no masks |
| `app/components/chat/timeline_message_rows.rs` | `ProposedPlanCard` menu: copy, download, save to workspace (dialog) | copy button only | no file dialogs in Turin yet |
| `app/components/chat/composer_pending_panels.rs` | Approval extra options in a `Menu`; 1–9 keys select question options; drafts in the composer draft store | inline disclosure for extra options; no digit shortcuts; drafts in element state | the composer task owns focus and drafts |
| `app/components/chat/timestamp_format.rs` | `Intl.DateTimeFormat` in the host locale | en-US formats (`h:mm AM`, `M/D`) | no ICU in the app crate; matches the default macOS desktop look |
| `app/components/chat/*` hover reveals | `opacity` transitions (200ms) on hover | instant `group_hover` | GPUI style refinements do not animate |
| `hooks` project `.turin/` | Upstream never runs commands from repo files; `t3.json` scripts are import-only (`resolveProjectScripts` reads settings only). | Project hooks are inert until the user approves the **whole `.turin/` tree**: `~/.turin/trusted-hooks.json` maps canonical project root → SHA-256 over sorted relative paths + bytes of every file under `.turin/`; re-checked on reload and on every dispatch. Symlinks leaving `.turin/` are untrustable. Commands running repo files outside `.turin/` are allowed but reported as `unhashed_references` (best-effort text scan; those files are not covered). Pending/changed hooks surface as `HookUpdate::ProjectHooksPending`; `HookRunner::trust_project_hooks(reviewed_hash)` approves. Global `~/.turin/hooks.toml` is trusted. | Hot-reloaded repo-controlled shell execution would otherwise run on clone/pull/agent edit. Residual: scripts outside `.turin/` can change without re-approval (same limit as direnv); the UI warns. QC wf_e8a1a7c1-d53 item 1, wf_44e1924e-b5c item 1. |
| `hooks` environment | `fixPath` / `readEnvironmentFromLoginShell` / `mergePathEntries` (+ launchctl fallback) hydrate `process.env.PATH` once at server startup (`os-jank.ts`). | `turin-hooks` inherits the app env and does no per-hook hydration. **Tracked:** port that hydration into `turin-shared` and call it once at app startup (T05b/T22). | Until then a Finder-launched app gives hooks `/usr/bin:/bin` (`gh`, `npm`, `bun` → exit 127). QC item 5. |
| `hooks` thread launch | `ThreadLaunchService` creates the worktree and binds the thread. | A blocking `thread.created` hook prints `{"worktreePath","branch"}` as its last stdout line; `DispatchSummary.thread_workspace` carries it and the orchestrator binds the thread. | Hooks replace launch-into-worktree (return protocol above). QC item 7. |
| `hooks` process exit | `processRunner.ts` waits for stdout/stderr EOF and never kills the group on normal exit (a background job holding the pipes stalls the run until timeout). | The run ends when the shell exits plus a 250 ms drain; if anything still holds the pipes, the whole group is killed (including output-redirected jobs). Daemons must leave the group (`setsid`). Timeout/shutdown always kill the group, using the pgid recorded at spawn. | A hook must not hang `thread.created` for the full timeout because of a stray `cmd &`; the early kill is the safe failure direction. QC wf_44e1924e-b5c item 2. |
| `apps/server/src/persistence/*` timestamps | Agent-event `occurredAt` is `DateTimeUtc`. Project `occurredAt`, receipt `acceptedAt`, and provider `lastSeenAt` are `IsoDateTime`. | Agent-event `occurred_at` is `UtcDateTime` and is stored with its canonical `YYYY-MM-DDTHH:MM:SS.sssZ` display. Project, receipt, and provider `*_at` stay caller-supplied strings. SQL `strftime` expressions are unchanged. | Only the agent-event field is `DateTimeUtc` upstream. The other timestamps are `IsoDateTime` pass-through. |
