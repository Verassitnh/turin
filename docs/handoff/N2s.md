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
| `normalizeProjectPathFor{Dispatch,Comparison}` (`shared/path.ts`) | `project_path.rs` | 2 | local, same reason as above |
| `KeyedLock` (from `shared/KeyedLock.ts`) | `keyed_lock.rs` | 1 | |
| `AgentSessionImporter.ts` | `agent_session_importer.rs` | 1 | all 1 upstream case; seams in `project_ports.rs` |
| `AgentSessionScanner.ts`: model types, `recentThreads` trait | `agent_session_scanner.rs` | 0 | `AgentSessionScanner` trait, `AgentSessionRecentThread` outcomes |
| `AgentSessionScanner.ts`: transcript parser | `agent_session_scanner.rs` | 16 | `parse_agent_session_transcript`; all pure upstream cases |

Support: `ttl_cache.rs`, `testing.rs` (fakes and real-git adapters, `#[cfg(test)]` only), `project_ports.rs` (seams), `project_service.rs`.

Current state: `cargo test -p turin-projects` = 101 passing. `cargo clippy -p turin-projects --all-targets -- -D warnings` (with deps) is clean. `rustfmt --check` is clean on all files in `crates/projects/src`.

**Gate `scripts/check.sh -p turin-projects` does NOT exit 0 yet.** It stops at the first step, `cargo fmt --all --check`, which reports diffs only in `crates/git/` (`git_manager.rs`, `shared_git.rs`, `vcs_driver_registry.rs`). That code is on `main` (T06) and is not ours, so the PM must fix it or merge the fix. The build, test, and clippy steps run after fmt and were run separately: all pass.

## Seams the T22 wiring must implement

All in `project_ports.rs`, `project_service.rs`, and `agent_session_scanner.rs`:

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
- Importer (new): `ThreadRecordReader` (`getThreadRecords`, errors read as absent),
  `OrchestrationEventWriter` (one `write` batch), `ProviderRuntimeRows` (`list_runtime_payloads`,
  `upsert_imported` with onConflict ignore, `record_imported_transcript`), `ProviderThreadIds`
  (upstream `derive.providerThread`, pure). The importer also takes `ProjectServiceApi` and
  the `AgentSessionScanner` trait.

## Remaining (in order)

1. **`AgentSessionScanner.ts`, discovery half** (upstream lines 620 to 1494 plus the 54 FS cases in the
   3202-line test file). Still to port:
   - config-path seam: Claude and Codex home dirs, `ServerConfig` base and worktrees dirs, the
     `expandHomePath` rules, and `HostProcess`;
   - `CodexHomeLayout` seam (`provider/Drivers/CodexHomeLayout.ts`) and provider settings seams
     (`ClaudeSettings`, `CodexSettings` decode from `ServerSettings`), enabled-instance rules
     (`resolveProviderInstanceEnabled`), and the "one provider instance per shared home" rule;
   - the `scan()` candidate list and `recentThreads` stream: newest-first discovery with the
     budgets (`MAX_TRANSCRIPTS_PER_SOURCE` etc.), the 30-day window, identity-based
     `completedSources` checks, and the `Duplicate` rule;
   - git identity read from `.git/config` without spawning git (`parseOriginUrlFromGitConfig`,
     `normalizeGitRemoteUrl`, GitHub `owner/name`);
   - T3 worktree exclusion (`isT3ManagedWorktree`) with realpath checks;
   - `read_transcript_json` via `agent_session_json.rs` (`TranscriptJsonReader` and the
     record selector) for the bounded, streamed reads.
   Tests use temp-dir fixtures only. Do not read `~/.claude` or `~/.codex`.
   Log each slice in `PORTING.md`. The Deviations rows already cover the pure parser.
2. **`ProjectService.test.ts`, 4 cases not ported**: enrichment failure, invalidation on move,
   slow metadata, update-after-delete lock wait. See the Known gaps.
3. **`ProjectService.deletion.test.ts`, 4 cases** (thread-deletion cascade). Depends on the
   `ThreadDeletions` seam; use fakes, as `ProjectService` tests do.
4. `ProjectEnrichmentService.test.ts` logger-capture case and `ProjectFaviconResolver.test.ts`
   FileSystem-instrumented and `t3.json` cases stay open (Known gaps).

## Known gaps

- `ProjectService.test.ts`: 4 cases not ported (enrichment failure, invalidation on move, slow
  metadata, update-after-delete lock wait). `ProjectService.deletion.test.ts` (4 cases) is not ported.
- `ProjectEnrichmentService.test.ts`: logger-capture case not ported.
- `ProjectFaviconResolver.test.ts`: FileSystem-instrumented cases and the `t3.json` cases not ported.
- `ProjectService` rejection receipt encoding is unverified against upstream (see seams).
- `AgentSessionScanner`: only the parser and model are ported. The 54 filesystem cases are open.

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
- `rustfmt --edition 2024 <files>` formats just your files. `cargo fmt --all` also touches `crates/git`.
