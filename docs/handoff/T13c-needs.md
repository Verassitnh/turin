# T13c needs (from T13a, T13b, providers, others)

## From T13b (`pipeline`)
- `ProviderSessionSink` implementation over `EventSink`: `session_attached`, `session_updated`,
  `session_released` (status via `core::provider_session_manager::release_status_for`, runtime
  request settlement as `writeReleaseRecords`), `ingest_session_scoped_event` (via ingestor, T13c).
- `OrchestratorPort` implementation: done (`core/orchestrator.rs`).

## From T13a (`projection`)
- Read seam for detach: running provider turns of a thread+session (`ProviderSessionManager.detach`).
- Read seam for release roster cleanup: provider threads with `pendingBackgroundTasks` per session.
- `ProjectionStore` reads used by session release (runtime requests by session) for the sink.

## From providers (`provider-core` / adapters)
- `ProviderSessionRuntime::close()` or a documented guarantee that dropping the last
  `Arc<dyn ProviderSessionRuntime>` stops the provider process group. The manager relies on drop.
- Every adapter must return one event stream from `events()`; the manager consumes it once.
- `ProviderAdapterV2Error` must implement `Display` (used in release details).

## From T06-git
- Checkpoint capture/restore seam (`CheckpointService.ts`), not yet consumed.

## From other owners
- MCP credential service (`McpSessionRegistry`, `McpProviderSessions`) if MCP credentials per
  session are wanted; the manager omits them today.
- Agent access settings (`AgentScope`, browser/device tools) if the manager should clear agent
  scope on attach.

## Status update (T13c seams, resolved in-branch)
- `ProviderSessionSink` over `EventSink`: done (`core/session_sink.rs`).
- T13a read seams (running turns, pending background tasks, node stored, pending native inputs, plan): done
  in `core/projection_reads.rs` over `ProjectionStore::get_thread_records`, without editing `projection/**`.
- `ProviderAdapterV2Error: Display`: already present (`provider_adapter.rs`).
- `ProviderSessionRuntime::close()`: added in provider-core as an additive default (`None`). T08 to reconcile.
- `OrchestratorPort`: implemented (`core/orchestrator.rs`, with the deciders under `core/orchestrator/`).
