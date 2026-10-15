# N2s handoff: server project services (`crates/projects`)

Branch `task/N2s-project-services`. Worktree `turin-wt/N2s-project-services`.
Crate `turin-projects`. Upstream `apps/server/src/project/*`.
Deviations are logged in `PORTING.md` (rows for each file below).

## Done (commits on the branch)

| Upstream file | Rust module | Tests | Notes |
|---|---|---|---|
| `AgentSessionJson.ts` | `agent_session_json.rs` | 6 | byte-level pushdown reader; schema selection via hand-written `JsonSelectSchema` |
| `RepositoryIdentityResolver.ts` | `repository_identity_resolver.rs` | 10 | `GitCommandRunner` trait; 6 tests use real git |
| `ProjectFaviconResolver.ts` | `project_favicon_resolver.rs` | 23 | `t3.json` iconPath is an injected `ProjectIconPathSource` (none by default) |
| `ProjectEnrichmentService.ts` | `project_enrichment_service.rs` | 9 | identity source returns `Option` (no error channel upstream) |
| `ProjectService.ts` | `project_service.rs` | 13 | surface, errors, trait `ProjectServiceApi`, implementation |
| `ProjectMutation.ts` | `project_mutation.rs` | 1 | pure mapping |
| `ManagedProjectFolders.ts` | `managed_project_folders.rs` | 16 | 14 upstream cases + 2 helpers; real git; fake project service |
| `ProjectFolderName` (from `shared/path.ts`) | `project_folder_name.rs` | 2 | local until the Rust `shared` crate ports `path.ts` |
| `KeyedLock` (from `shared/KeyedLock.ts`) | `keyed_lock.rs` | 1 | |

Support: `ttl_cache.rs` (replaces Effect `Cache`, with a shared `TestClock`), `testing.rs`
(fakes and real-git adapters, `#[cfg(test)]` only), `project_ports.rs` (seams), `project_service.rs`.

Current state: `cargo test -p turin-projects` = 82 passing (`project_ports` adds one test for the rejection encoding). Clippy with `--no-deps -D warnings` is clean.
`scripts/check.sh -p turin-projects` fails only on the workspace fmt step (`crates/git`, from T06, not ours).

## Seams the T22 wiring must implement

All in `project_ports.rs` and `project_service.rs`:

- `ProjectStoreReads`: project rows (get, find by root, list, shells). `ProjectRow` is the read model.
- `ProjectEventSink`: `commit_project_command` and `commit_rejected_project_command`. Each is one
  transaction (event, row, receipt). A reused command id returns the stored receipt.
- `ProjectCommandPlanner`: the upstream `planProjectCommand`. It returns an opaque event payload
  or a `ProjectCommandRejection`. Those rejection JSON shapes need checking against upstream
  `encodeProjectCommandRejection` before any existing receipt is read.
- `ThreadDeletions`: `project_thread_ids` and `delete_child_thread`. The second holds the thread lock,
  does the thread-delete plan and commit, and builds the command id `{cmd}:delete-thread:{thread}`.
- `ProjectSettingsOverrides`: clears a deleted project's override.
- `EventIdAllocator`: upstream `idAllocator.allocate.event`.
- `RepositoryProbe`, `GitCli` (ManagedProjectFolders): the app wraps `turin-git`'s
  `GitWorkflowService.is_repository` and the `GitVcsDriver` execute and read-config methods.
- `RepositoryIdentitySource`, `FaviconSource`, `ProjectIconPathSource`: already implemented
  for our resolvers, except the icon source, which the app may leave `None`.

## Remaining (in order)

1. **`AgentSessionImporter.ts`** (409 lines, test 147). Blocked on nothing new, but it needs:
   - the scanner outcome types (see step 2), defined first as a model module;
   - a `ThreadRecordsReader` seam for `orchestrator.getThreadRecords` (returns project id and
     history origin, or none);
   - an `EventWriter` seam for `eventSink.write(Vec<OrchestrationV2DomainEvent>)`; the event and
     payload types are in `turin_contracts::orchestration_v2`;
   - a `ProviderRuntimeRows` seam for list, upsert (onConflict ignore), recordImportedTranscript;
   - `ProviderThreadIds::derive_provider_thread(driver, native_id)`.
   The importer uses `ProjectServiceApi::get_by_id`. Port the 147-line test with fakes.
2. **`AgentSessionScanner.ts`** (1494 lines, test 3202). Do it in slices. First the model types
   (`AgentSessionThread`, `AgentSessionThreadMessage`, the `recentThreads` outcome union:
   `Skipped`, `AlreadyImported`, `Duplicate`, found-with-thread). Then the config-path seam
   (home dirs for Claude, Codex, Cursor, OpenCode), a `CodexHomeLayout` seam, and a provider
   settings seam (`ClaudeSettings`, `CodexSettings` decoders). Then the JSON transcript reading,
   which uses `agent_session_json.rs`. The test file runs against temp-dir fixtures and never
   writes to real transcript dirs.
3. Log the importer and scanner rows in `PORTING.md` as each slice lands.

## Known gaps

- `ProjectService.test.ts`: 4 cases not ported (enrichment failure, invalidation on move, slow
  metadata, update-after-delete lock wait). `ProjectService.deletion.test.ts` (4 cases) is not ported.
- `ProjectEnrichmentService.test.ts`: logger-capture case not ported.
- `ProjectFaviconResolver.test.ts`: FileSystem-instrumented cases and the `t3.json` cases not ported.
- `ProjectService` rejection receipt encoding is unverified against upstream (see seams).

## Working rules that bit this session

- Disk: the volume was full mid-session (`No space left on device`). Use
  `CARGO_INCREMENTAL=0` for builds. Clear only this crate with `cargo clean -p turin-projects`.
  Never `rm -rf target`. The `turin-targets` tree is about 114 GB across worktrees; leave other
  worktrees alone.
- The Edit tool decodes `\uXXXX` escapes in parameter text. Patch escape-literal test inputs with
  Python and `chr(92)`, then check the bytes with `od -c`.
- Python `re.sub` treats `\n` in the replacement as a real newline. Use a lambda or a plain
  `str.replace` for code edits.
- Commit per slice with the trailer `Model: claude-haiku-5-5 (claudeAgent)`.
