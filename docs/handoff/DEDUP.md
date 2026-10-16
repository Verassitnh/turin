# DEDUP-1: duplicate upstream ports

Branch `task/DEDUP-1`, from `main`. Scope: every upstream file should have one Rust port, in the crate that owns it.

Method: scanned `crates/` for `Port of …` headers (also `Port of \`…\``, and `Port of the helpers of …`). Grouped by upstream path. For each group, compared public items, test names, and public signatures across copies. Also grepped for copies without a header (`git_branch_names.rs`, `expand_home_path`).

Rules applied: owner crate per the brief; loser deleted and callers repointed; a copy was only deleted when its behaviour matched the winner. Where repointing would change behaviour, the copy stays and is listed under Deferred.

## Done

| Upstream | Deleted copy (lines) | Kept copy | Notes |
|---|---|---|---|
| `packages/shared/src/filePreview.ts` | `app/components/chat/timeline/file_preview.rs` (61) | `shared/src/file_preview.rs` | Its one test is covered by `recognizes_browser_and_image_preview_paths`. Callers in `work_log_presentation.rs`, `tool_presentation.rs` repointed. |
| `packages/shared/src/usageFormat.ts` | `app/components/chat/timeline/usage_format.rs` (165) | `shared/src/usage_format.rs` | `format_tokens_matches_upstream` exists in shared under the same name. |
| `packages/shared/src/orchestrationTiming.ts` (`formatDuration`) | `app/state/support/timing.rs` (66) | `shared/src/orchestration_timing.rs` | Same test name in shared. Shared computes in `i64`; the copy in `f64`. Same output for every finite duration below 2^53 ms. |
| `packages/shared/src/path.ts` (state helpers) | `app/state/support/path.rs` (239) | `shared/src/path.rs` | `file_basename` now returns `String` (was `&str`). 4 unique tests ported into shared. Callers in `app/state/**` repointed with `turin_shared::path::`. |
| `packages/shared/src/path.ts` (normalize helpers) | `projects/src/project_path.rs` (102 lines removed; file keeps `expand_home_path`) | `shared/src/path.rs` | 2 unique tests ported into shared. |
| `packages/shared/src/gitPatchPath.ts` | `git/src/git_patch_path.rs` (212) | `shared/src/git_patch_path.rs` | The git copy had no callers. 4 unique tests ported into shared. |
| `packages/shared/src/orchestrationV2ThreadError.ts` | `orchestration/src/projection/shared/thread_error.rs` (308) | `shared/src/orchestration_v2_thread_error.rs` | 6 unique tests and the `run`/`failure_item` helpers ported. Added `Default` to `ThreadErrorSummary` (the orchestration copy derived it). |
| `apps/server/src/orchestration-v2/ProjectCommands.ts` | `orchestration/src/projection/project_commands.rs` (627) | `orchestration/src/pipeline/project_commands.rs` | Pipeline copy is the one the orchestrator calls. Projection copy had no callers. Its 1 unique test (rejection receipt round-trip) moved to `pipeline/project_commands/tests.rs`. |

Totals: 7 whole files deleted (1,678 lines). `project_path.rs` loses 102 lines. Tests were moved, not dropped. The branch diff against `d31b9de` is +325 / -1807 lines, including the moved tests.

Commits (grouped, not one per dedup; the copies share files, so per-dedup commits would need hunk staging):
- `1501008` refactor(shared): take in path, git patch path and thread-error tests
- `3fdac39` refactor(app): use turin-shared for file preview, usage, timing and path helpers
- `ca5bb2c` refactor(git): delete unused git_patch_path copy
- `2fa9321` refactor(orchestration): use turin-shared thread-error helpers
- `97da9ba` refactor(projects): use turin-shared path normalization
- `4753cbf` refactor(orchestration): drop duplicate project_commands projection module

## Tried and reverted (behaviour differs)

These were deduped, then reverted after tests showed live behaviour changes.

1. **`change_request_url`** (app timeline copy 412 lines, app `state/support` copy). The app copy keeps Forgejo port authority (`forge.example:3000`). Shared's `parse_change_request_url` drops it (`authority: None`, Forgejo cut). Two app tests failed (`a_forgejo_port_gives_one_key_on_every_path`, `preserves_forgejo_ports_in_a_multi_link_mutation`). Reverted. **PM decision:** drop Forgejo authority from the app (matches the cut list), or keep it and extend shared.
2. **`git/src/shared_git.rs`** (1030) vs `shared/src/git.rs` (1087). The two diverge on cut-host behaviour: `detect_source_control_provider_from_git_remote_url("https://gitlab.com/…")` is `None` in the git copy (GitHub-only) and `Some` in shared. Azure SSH normalizes differently (`ssh.dev.azure.com/v3/…` vs `dev.azure.com/…/_git/…`). The git copy's tests assert the GitHub-only behaviour. **PM decision:** which behaviour is correct. Once decided, the loser goes and callers repoint (`git_manager.rs`, `vcs_status_broadcaster.rs`, `driver.rs`, `projects/{agent_session_discovery,repository_identity_resolver}.rs`, `review/git_actions_control_logic.rs`).
3. **`text-generation/src/git_branch_names.rs`** (copy of the `shared_git` sanitizers, "Copied … Dedupe when it lands"). Its `EDGE_SEPARATORS` uses Rust `\s`; shared uses the JS whitespace class. They differ on U+0085 and U+FEFF in branch names. Shared is the more faithful one, but switching changes output for those characters. Reverted until the PM confirms.

## Deferred (both copies stay)

Each entry names the other copy and why it is not deduped yet.

**Different API or item shape (repointing needs call-site rework):**
- `packages/shared/src/assistantCitations.ts`: `app/components/chat/timeline/assistant_citations.rs` (1414) vs `shared/src/assistant_citations.rs` (910). Both cover all 9 exports. The app copy is larger and has WHATWG URL parsing tests; the owner rule says shared. Needs a decision on moving the app copy's logic into shared (adds `url` parsing there).
- `packages/shared/src/toolActivity.ts`: `app/.../tool_activity.rs` (979) vs `shared/src/tool_activity.rs` (899). `collect_tool_file_paths`, `merge_tool_activity_data` and friends take `Option<&Value>` in the app and `&Value` in shared.
- `packages/shared/src/toolOutput.ts`: `app/.../tool_output.rs` (776) vs `shared/src/tool_output.rs` (750). Signature differences (`Option<&Value>`, `ToolOutputImage<'a>`); the app copy also has the provider-send image helpers.
- `packages/shared/src/orchestrationTiming.ts`: `app/.../timeline/orchestration_timing.rs` (403) vs `shared/src/orchestration_timing.rs` (306). The app copy takes `f64` where shared takes `i64`, and `is_orchestration_v2_work_active` uses timeline `TurnItemStatus`.
- `packages/shared/src/markdownLinks.ts`: `app/components/chat/markdown/markdown_links.rs` (339) vs `shared/src/markdown_links.rs` (665). Different return types for `parse_markdown_file_link` (`String` vs `FilePathPosition`). The app's `file_link_label` has no shared counterpart.
- `packages/shared/src/orchestrationV2Timeline.ts`: `app/.../timeline/orchestration_v2_timeline.rs` (328), `app/state/support/timeline_visibility.rs` (153), `orchestration/src/projection/shared/timeline.rs` (368), `shared/src/orchestration_v2_timeline.rs` (304). Shared uses owned mirror structs. Orchestration uses borrowed generics. The app state copy uses contract types.
- `packages/shared/src/orchestrationV2PendingBackgroundWork.ts`: `app/state/support/background_work.rs` (906), `orchestration/src/projection/shared/pending_background_work.rs` (1054), `shared/src/orchestration_v2_pending_background_work.rs` (1065). Three different type families. Also blocks `agent_awareness`.
- `packages/shared/src/agentAwareness.ts`: `orchestration/src/projection/shared/agent_awareness.rs` (355) vs `shared/src/agent_awareness.rs` (317). The orchestration copy has `project_thread_awareness_v2` and the deep link, which shared lacks; its `background_work_holds_completion` takes the contract task type. Needs the pending-background-work decision first.
- `packages/shared/src/threadPullRequests.ts`: `app/state/support/thread_pull_requests.rs` (730), `orchestration/src/projection/shared/thread_pull_requests.rs` (347) vs `shared/src/thread_pull_requests.rs` (1232). Different function shapes: the app and orchestration copies take the thread object; shared takes link lists.
- `packages/shared/src/model.ts` (app `state/support/model.rs` vs `shared/src/model.rs`): `SelectableModelOption` is a trait in the app and a struct in shared, and `resolve_selectable_model` returns differently.
- `packages/client-runtime/src/state/subagentRuntime.ts`: `app/state/subagent_runtime.rs` (896) vs `app/components/chat/timeline/subagent_runtime.rs` (10, one function). The stub works on timeline `TurnItem`; the state copy works on contract `OrchestrationV2TurnItem`.
- `packages/client-runtime/src/state/turnItemPresentation.ts`: `app/state/turn_item_presentation.rs` (155) vs `app/components/chat/timeline/turn_item_presentation.rs` (25). Same cross-type issue. The timeline copy's caller is `session_logic.rs`.
- `packages/shared/src/orchestrationV2ThreadError.ts` (app state copy, 161 lines, no tests): takes `Arc<…>` slices. Shared takes plain slices. Deferred with the Arc adapters.
- `app/state/helpers/shared_support.rs` (558): a bundle of `packages/shared` helpers (markdown, file URL, media mime, semver, prompt-link types). Shared has the same functions under other module names with other signatures (`FileUrlTarget`, `MarkdownLinkSearchAndHash`).

**Same file ported twice in one crate, both partial:**
- `apps/web/src/timestampFormat.ts`: `app/src/timestamp_format.rs` (185, covers 4/17 exports) vs `app/src/components/chat/timestamp_format.rs` (177, covers 6/17). Neither is complete. Merging them would be hand-blending; the right fix is one module with the missing upstream functions ported.

**Expand-home-path copies (`pathExpansion.ts` is owned by provider-core):**
- `provider-core/src/path_expansion.rs` (owner), `projects/src/project_path.rs` (still holds it), `review/src/worktrees_directory.rs:43`, `git/src/workspace/workspace_paths.rs:210`. Repointing needs `turin-provider-core` deps added to projects, review and git. Left for the PM.

**Persistence (cut feature):**
- `apps/server/src/persistence/initializeV2Database.ts`: `persistence/src/initialize_v2_database.rs` (121, the port) vs `persistence/src/t3_import/mod.rs` (137, legacy v1 importer). `t3_import` is cut; not deleted (PM decides).

**Complementary splits, not duplicates by content** (one upstream file split across two modules with no shared items; PM to confirm they should stay split):
- `apps/web/src/components/Sidebar`: `app/components/sidebar.rs` (709) + `sidebar/sidebar_sections.rs` (486).
- `apps/web/src/components/chat/MessagesTimeline`: `app/components/chat/messages_timeline.rs` (410) + `timeline_message_rows.rs` (762).
- `apps/web/src/index.css`: `app/theme/tokens.rs` (297) + `app/theme/mod.rs` (430).
- `apps/server/src/project/AgentSessionScanner`: `projects/src/agent_session_discovery.rs` (1858, discovery impl) + `projects/src/agent_session_scanner.rs` (1060, trait and model types). The discovery header says the model lives in the scanner file. The scanner header still says "transcript scan follows in later slices". Merging the two is a restructure.

**Not on this branch:**
- N1 `shared_keybindings` vs `turin_shared::keybindings`: no `shared_keybindings` on this branch. Nothing to do here.
- N5 `editor.rs`: `crates/launcher/src/editor.rs` and `contracts/src/editor.rs` exist, but they are not the N5 copy. Left alone.

**Copy without a header, found by grep:** `text-generation/src/git_branch_names.rs` (see Tried and reverted, item 3).

## Cut-feature ports still present (not deleted; PM decides)

Checked against PORTING.md's cut list and `docs/inventory/upstream-coverage.md`. Matched on upstream path and grep, so the list is a best effort; the PM should confirm each.

- **Legacy v1 importer:** `persistence/src/t3_import/` (mod, adopt, plan, snapshot, tests) and `persistence/examples/t3_import_check.rs`.
- **File tree side panel:** `app/src/diff/file_tree.rs`, `app/src/diff/diff_file_tree_logic.rs`, `app/src/components/chat/changed_files_tree.rs`. The diff file tree may count as the file tree panel; confirm.
- **Browser / preview:** `app/src/hooks/use_preview_panel_inline_size.rs`. `shared/src/file_preview.rs` (from `filePreview.ts`) includes browser-preview classification; `shared/src/delimited_preview.rs` is preview code of unclear status.
- **Non-GitHub source-control hosts (GitLab, Bitbucket, Azure, Forgejo, GitCafe)** still in kept code: `shared/src/source_control.rs`, `shared/src/git.rs`, `shared/src/change_request_url.rs`, `shared/src/thread_pull_requests.rs`, `app/src/state/helpers/source_control_clients.rs`, `app/src/state/support/thread_pull_requests.rs`, `app/src/state/helpers/thread_pull_request_compatibility.rs`, `projects/src/repository_identity_resolver.rs`, `git/src/link_created_pull_request.rs`, `persistence/src/legacy_pull_request_key.rs`, `contracts/src/pull_request.rs`.
- **Orchestrator MCP server / HTML render / MCP apps** inside kept tool-output code: `app/.../timeline/t3_mcp_tool_presentation.rs`, `t3_tool_summary.rs`, `tool_output.rs`; `shared/src/tool_output.rs` (`html_render_from_tool_item`, `mcp_app_from_tool_item`); `provider-core/src/mcp_tool_presentation.rs`, `orchestration_instructions.rs`.
- **Auth / sessions:** `shared/src/codex_auth_handoff.rs`, `shared/src/provider_auth_return_url.rs`, `provider-core/src/auth.rs`, `provider-cursor/src/sign_in.rs`. Provider sign-in may be kept; confirm.
- **Tailscale / host classification:** `shared/src/host_classification.rs`.
- **ACP registry (effect-acp):** `crates/acp/` ports `packages/effect-acp`. Confirm whether the ACP protocol is kept for OpenCode and Cursor.
- **Cut providers in contracts:** `contracts/src/settings.rs`, `contracts/src/model.rs`, `contracts/src/usage.rs` mention Antigravity, Grok, and Muse. `DEDUP` notes Antigravity settings are omitted. Confirm the remaining Grok and Muse fields.

## Gates

- `scripts/check.sh -p turin-shared -p turin-git -p turin-projects -p turin-review -p turin-text-generation -p turin-orchestration -p turin`: **green** (gate5, after reverting the three items above). Run before the final `project_commands` commit.
- `scripts/check.sh -p turin-orchestration` after the `project_commands` commit: **green** (gate6, `GATE_EXIT=0`).
- `cargo check --workspace`: see the final report.
- Gate was run once for the whole touched set, not after each dedup. The copies share files, and each gate takes about 10 to 20 minutes.

## Not touched

- `TASKS.md` (PM board).
- `PORTING.md` (its DEDUP section still says the app does not depend on `turin-shared`; the app does, so that section is stale).
- Nothing pushed.
