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
| `app/components/sidebar.rs` | `Sidebar.tsx` (v2) by default; `LegacySidebar.tsx` (project → threads tree) when Settings → General → Legacy features → `legacySidebarEnabled` | v2 only: one sectioned list across projects with the project scope menu; the legacy project tree is not ported | the legacy tree is an opt-in fallback upstream is retiring; v2 is the default look. Revisit if users ask for the tree |
| `app/components/sidebar.rs` and `sidebar/*` | one 5.6k-line `Sidebar.tsx` component | split by concern (`sidebar_list`, `sidebar_thread_row`, `sidebar_actions`, `sidebar_gestures`, `sidebar_search`, `sidebar_sections`) around one `Sidebar` entity | GPUI views are entities with methods, not nested hooks; each file's header names the upstream pieces it holds |
| `app/components/sidebar/view_model.rs`, `mock_source.rs` | reads atoms/stores directly (`useThreadShells`, `useProjects`, server configs, composer drafts, `useThreadSearch`) and calls `useThreadActions` | one typed `SidebarSnapshot` in a `SidebarStore` entity plus a `SidebarBackend` command trait; an in-memory mock fills both until T14 | explicit seam for the client view-model task |
| `app/components/sidebar/runtime/*` | `packages/client-runtime/src/state/{models,threadSort,threadSettled,threadInbox}.ts` | ported under the sidebar until T14 moves them to `crates/app/src/state`; V1 shapes (`session`, `latestTurn`) dropped, so `hasQueuedTurnStart` no longer clears on a V1 `session.status === "error"` | V1 importer is cut; T14 owns the folder |
| `app/components/sidebar/runtime/collate.rs` | `String#localeCompare` (ICU root collation) for id/title tiebreaks | punctuation < digits < letters, case-insensitive then lowercase-first | equivalent for ids, branches and project titles; no ICU dependency |
| `app/components/sidebar/runtime/models.rs` | `ThreadPullRequestLink` with snapshots and stack chains; `resolveThreadCurrentPullRequest` walks chains | link narrowed to number/title/state/draft/dismissed; current link = the only open link, else the newest | stacks and PR snapshots are the PR task's (T19) |
| `app/components/sidebar/runtime/models.rs` | `latestRunDiff` returns `null` (shells carry no checkpoint summary) | `latest_run_diff` seam field; the mock fills it so the row's diff slot renders | upstream keeps the slot as a seam too; T14 fills it when the shell projection grows diffs |
| `app/components/sidebar/sidebar_logic.rs` | React hooks (`useSidebarRowSubscriptionLease`, `useRetainedValue`, `useThreadJumpHintVisibility`), dnd-kit `animateSidebarLayoutChanges`, DOM helpers (`shouldClearThreadSelectionOnMouseDown`, `isSidebarNestedLinkClick`), `resolveSidebarStageBadgeLabel`, `resolveThreadRowClassName` | not ported; `createThreadJumpHintVisibilityController` is a timer-free state machine; `normalizeSearchText` skips NFKD mark stripping | no subscriptions/DOM/dnd-kit in GPUI; the stage badge is branding (cut); row class names are legacy-sidebar only; no unicode-normalization dependency |
| `app/components/sidebar/sidebar_drag.rs`, `sidebar_gestures.rs` | dnd-kit `DndContext` measures DOM rects, applies CSS transforms, auto-scrolls; dragging out of the list hands threads to the composer as context (`threadContextDrag`) | pure ports of the collision detection and sorting projection over rects predicted from the known row heights; transforms applied as relative offsets; the lifted card paints in an overlay; no auto-scroll; no context drag; window-level pointer/Escape listeners stand in for the sensor's document listeners | GPUI has no DnD kit; composer context drops land with the composer task |
| `app/components/sidebar/sidebar_motion.rs`, `sidebar_list.rs` | `createSidebarListMotion` animates DOM nodes with WAAPI (FLIP from `offsetTop`) and clones removed nodes | the same model over predicted row positions, sampled per frame with a 150ms `ease-out`; exit fades re-render the removed row from last frame's data | no WAAPI/DOM cloning; positions are deterministic from the item list |
| `app/components/sidebar/sidebar_thread_row.rs` | per-row unsent-composer-draft marker, terminal status button, files dropped on rows, `ProviderInstanceIcon` brand glyphs, `EnvironmentMachineIcon`, branch-mismatch warning (VCS status query), `content-visibility` | not ported: drafts/terminal/files belong to composer/terminal tasks (terminal cut); provider stack uses a neutral `Bot` glyph until provider assets land; one machine, so no environment glyphs; no VCS status in the sidebar yet | cut list / later tasks |
| `app/components/sidebar/sidebar_thread_row.rs`, `sidebar_list.rs`, `sidebar_search.rs` | `group-hover` CSS, `grayscale` filter, focus-visible rings, row keyboard activation (Enter/Space, ContextMenu key), `role`/`aria-*` | hover tracked per row on the view; grayscale approximated by desaturating the project tint; rows are not focusable (keyboard reaches threads through ⌘1…9 and ⌘⇧[ / ]) | GPUI has no CSS filters or ARIA; row focus would need per-row focus handles (follow-up) |
| `app/components/sidebar/thread_hover_card.rs` | `TooltipPopup side="right" align="start"`, 150ms delay | GPUI's pointer-anchored tooltip with the glass card | no positioner API on GPUI tooltips (as `ui/tooltip.rs`) |
| `app/components/sidebar/sidebar_actions.rs` | desktop native context menus (`api.contextMenu.show`), `api.dialogs.confirm`, toast manager with stacked thread toasts | gpui-component `PopupMenu` (right-click), alert dialogs, notifications | no Electron bridge |
| `app/components/sidebar/custom_snooze_dialog.rs` | calendar popover for the date, native time input, number stepper, Select for the unit | typed `YYYY-MM-DD` / `HH:MM` fields, plain amount field, segmented unit toggles | no calendar/number-field/select primitives yet; validation is the ported `resolveCustomSnooze` |
| `app/components/sidebar/sidebar.rs` | keybindings resolved from the user's keybindings file (`resolveShortcutCommand`, `shortcutLabelForCommand`); jump hints follow the exact jump modifier set | upstream defaults bound as GPUI actions in the shell context (⌘1…9, ⌘⇧[ / ], ⌘⇧S, ⌘⇧P, ⌘N, ⌘⇧O, ⌘⇧N); hints show while ⌘ alone is held | the keybindings task (T20) owns custom bindings |
| `app/components/sidebar/sidebar_actions.rs` | new thread / add project / project settings / settings / usage / PR page navigate via the router and command palette | emitted as `SidebarEvent`s; the shell shows a "not available yet" notice for screens later tasks own | those screens are T17/T19/T20 |
| `app/components/project_favicon.rs`, `app/project_identity.rs` | favicon images from the workspace (`projectFaviconUrlAtom`), dynamic lucide project icons, NFKC name normalization | generated monogram (or configured monogram/emoji); a lucide icon falls back to `FolderCode` in the project color; names trimmed only | no favicon serving or dynamic icon catalog yet |
| `app/ui/sidebar.rs` | `SidebarProvider` context (`useSidebar`), mobile `Sheet`, `ui/sidebarState.ts`, cookie persistence, `icon`/`floating`/`inset` variants, `SidebarMenuSub*` | the owner passes `open` explicitly; desktop offcanvas only | GPUI has no React context; mobile is cut; no desktop caller uses the rest |
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
