-- Port of upstream/apps/server/src/persistence/Migrations/055_OrchestrationV2.ts
CREATE TABLE orchestration_v2_events (
      sequence INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id TEXT NOT NULL UNIQUE,
      command_id TEXT,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT,
      provider TEXT,
      raw_event_id TEXT,
      event_type TEXT NOT NULL,
      occurred_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_events_command_idx ON orchestration_v2_events(command_id, sequence);
CREATE INDEX orchestration_v2_events_thread_sequence_idx ON orchestration_v2_events(thread_id, sequence);
CREATE INDEX orchestration_v2_events_thread_type_sequence_idx ON orchestration_v2_events(thread_id, event_type, sequence);
CREATE INDEX orchestration_v2_events_run_sequence_idx ON orchestration_v2_events(run_id, sequence);
CREATE INDEX orchestration_v2_events_node_sequence_idx ON orchestration_v2_events(node_id, sequence);
CREATE INDEX orchestration_v2_events_raw_event_idx ON orchestration_v2_events(raw_event_id);
CREATE TABLE orchestration_v2_command_receipts (
      command_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      command_type TEXT NOT NULL,
      accepted_at TEXT NOT NULL,
      result_sequence INTEGER NOT NULL,
      status TEXT NOT NULL,
      error TEXT
    );
CREATE INDEX orchestration_v2_command_receipts_thread_sequence_idx ON orchestration_v2_command_receipts(thread_id, result_sequence);
CREATE TABLE orchestration_v2_projection_threads (
      thread_id TEXT PRIMARY KEY,
      project_id TEXT NOT NULL,
      title TEXT NOT NULL,
      default_provider TEXT NOT NULL,
      runtime_mode TEXT NOT NULL,
      interaction_mode TEXT NOT NULL,
      active_provider_thread_id TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      archived_at TEXT,
      deleted_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_threads_project_updated_idx ON orchestration_v2_projection_threads(project_id, updated_at);
CREATE TABLE orchestration_v2_projection_runs (
      run_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      ordinal INTEGER NOT NULL,
      provider TEXT NOT NULL,
      provider_thread_id TEXT,
      status TEXT NOT NULL,
      requested_at TEXT NOT NULL,
      completed_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE UNIQUE INDEX orchestration_v2_projection_runs_thread_ordinal_idx ON orchestration_v2_projection_runs(thread_id, ordinal);
CREATE INDEX orchestration_v2_projection_runs_provider_thread_idx ON orchestration_v2_projection_runs(provider_thread_id);
CREATE INDEX orchestration_v2_projection_runs_thread_status_idx ON orchestration_v2_projection_runs(thread_id, status);
CREATE TABLE orchestration_v2_projection_run_attempts (
      attempt_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT NOT NULL,
      attempt_ordinal INTEGER NOT NULL,
      root_node_id TEXT NOT NULL,
      provider TEXT NOT NULL,
      provider_thread_id TEXT NOT NULL,
      provider_turn_id TEXT,
      status TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE UNIQUE INDEX orchestration_v2_projection_run_attempts_run_ordinal_idx ON orchestration_v2_projection_run_attempts(run_id, attempt_ordinal);
CREATE INDEX orchestration_v2_projection_run_attempts_thread_idx ON orchestration_v2_projection_run_attempts(thread_id, run_id);
CREATE TABLE orchestration_v2_projection_nodes (
      node_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      parent_node_id TEXT,
      root_node_id TEXT NOT NULL,
      kind TEXT NOT NULL,
      status TEXT NOT NULL,
      provider_thread_id TEXT,
      provider_turn_id TEXT,
      runtime_request_id TEXT,
      checkpoint_scope_id TEXT,
      started_at TEXT,
      completed_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_nodes_thread_run_idx ON orchestration_v2_projection_nodes(thread_id, run_id);
CREATE INDEX orchestration_v2_projection_nodes_parent_idx ON orchestration_v2_projection_nodes(parent_node_id);
CREATE INDEX orchestration_v2_projection_nodes_provider_turn_idx ON orchestration_v2_projection_nodes(provider_turn_id);
CREATE TABLE orchestration_v2_projection_provider_sessions (
      provider_session_id TEXT PRIMARY KEY,
      thread_id TEXT,
      provider TEXT NOT NULL,
      status TEXT NOT NULL,
      model TEXT,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_provider_sessions_thread_idx ON orchestration_v2_projection_provider_sessions(thread_id);
CREATE INDEX orchestration_v2_projection_provider_sessions_provider_status_idx ON orchestration_v2_projection_provider_sessions(provider, status);
CREATE TABLE orchestration_v2_projection_provider_threads (
      provider_thread_id TEXT PRIMARY KEY,
      thread_id TEXT,
      owner_node_id TEXT,
      provider TEXT NOT NULL,
      provider_session_id TEXT,
      status TEXT NOT NULL,
      first_run_ordinal INTEGER,
      last_run_ordinal INTEGER,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_provider_threads_thread_idx ON orchestration_v2_projection_provider_threads(thread_id);
CREATE INDEX orchestration_v2_projection_provider_threads_session_idx ON orchestration_v2_projection_provider_threads(provider_session_id);
CREATE INDEX orchestration_v2_projection_provider_threads_owner_idx ON orchestration_v2_projection_provider_threads(owner_node_id);
CREATE TABLE orchestration_v2_projection_provider_turns (
      provider_turn_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      provider_thread_id TEXT NOT NULL,
      node_id TEXT NOT NULL,
      run_attempt_id TEXT,
      ordinal INTEGER NOT NULL,
      status TEXT NOT NULL,
      started_at TEXT,
      completed_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_provider_turns_thread_idx ON orchestration_v2_projection_provider_turns(thread_id);
CREATE UNIQUE INDEX orchestration_v2_projection_provider_turns_thread_ordinal_idx ON orchestration_v2_projection_provider_turns(provider_thread_id, ordinal);
CREATE TABLE orchestration_v2_projection_runtime_requests (
      runtime_request_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      node_id TEXT NOT NULL,
      provider_turn_id TEXT,
      kind TEXT NOT NULL,
      status TEXT NOT NULL,
      created_at TEXT NOT NULL,
      resolved_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_runtime_requests_thread_status_idx ON orchestration_v2_projection_runtime_requests(thread_id, status);
CREATE INDEX orchestration_v2_projection_runtime_requests_provider_turn_idx ON orchestration_v2_projection_runtime_requests(provider_turn_id);
CREATE TABLE orchestration_v2_projection_messages (
      message_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT,
      role TEXT NOT NULL,
      streaming INTEGER NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_messages_thread_created_idx ON orchestration_v2_projection_messages(thread_id, created_at, message_id);
CREATE INDEX orchestration_v2_projection_messages_run_idx ON orchestration_v2_projection_messages(run_id);
CREATE INDEX orchestration_v2_projection_messages_node_idx ON orchestration_v2_projection_messages(node_id);
CREATE TABLE orchestration_v2_projection_plans (
      plan_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT NOT NULL,
      kind TEXT NOT NULL,
      status TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_plans_thread_idx ON orchestration_v2_projection_plans(thread_id);
CREATE INDEX orchestration_v2_projection_plans_run_idx ON orchestration_v2_projection_plans(run_id);
CREATE TABLE orchestration_v2_projection_turn_items (
      turn_item_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT,
      provider_thread_id TEXT,
      provider_turn_id TEXT,
      parent_item_id TEXT,
      ordinal INTEGER NOT NULL,
      type TEXT NOT NULL,
      status TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_turn_items_thread_ordinal_idx ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id);
CREATE INDEX orchestration_v2_projection_turn_items_run_ordinal_idx ON orchestration_v2_projection_turn_items(run_id, ordinal);
CREATE INDEX orchestration_v2_projection_turn_items_node_ordinal_idx ON orchestration_v2_projection_turn_items(node_id, ordinal);
CREATE INDEX orchestration_v2_projection_turn_items_provider_turn_idx ON orchestration_v2_projection_turn_items(provider_turn_id);
CREATE TABLE orchestration_v2_projection_checkpoint_scopes (
      scope_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT NOT NULL,
      parent_scope_id TEXT,
      provider_thread_id TEXT,
      kind TEXT NOT NULL,
      ordinal_within_parent INTEGER NOT NULL,
      advances_app_run_count INTEGER NOT NULL,
      created_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_checkpoint_scopes_thread_idx ON orchestration_v2_projection_checkpoint_scopes(thread_id);
CREATE INDEX orchestration_v2_projection_checkpoint_scopes_parent_idx ON orchestration_v2_projection_checkpoint_scopes(parent_scope_id);
CREATE TABLE orchestration_v2_projection_checkpoints (
      checkpoint_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      scope_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT NOT NULL,
      parent_checkpoint_id TEXT,
      ordinal_within_scope INTEGER NOT NULL,
      app_run_ordinal INTEGER,
      status TEXT NOT NULL,
      captured_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE UNIQUE INDEX orchestration_v2_projection_checkpoints_scope_ordinal_idx ON orchestration_v2_projection_checkpoints(scope_id, ordinal_within_scope);
CREATE INDEX orchestration_v2_projection_checkpoints_thread_idx ON orchestration_v2_projection_checkpoints(thread_id);
CREATE INDEX orchestration_v2_projection_checkpoints_parent_idx ON orchestration_v2_projection_checkpoints(parent_checkpoint_id);
CREATE TABLE orchestration_v2_projection_context_handoffs (
      context_handoff_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      target_run_id TEXT NOT NULL,
      to_provider_thread_id TEXT NOT NULL,
      strategy TEXT NOT NULL,
      status TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_context_handoffs_thread_idx ON orchestration_v2_projection_context_handoffs(thread_id);
CREATE INDEX orchestration_v2_projection_context_handoffs_target_run_idx ON orchestration_v2_projection_context_handoffs(target_run_id);
CREATE TABLE orchestration_v2_projection_context_transfers (
      context_transfer_id TEXT PRIMARY KEY,
      source_thread_id TEXT NOT NULL,
      target_thread_id TEXT NOT NULL,
      target_run_id TEXT,
      type TEXT NOT NULL,
      status TEXT NOT NULL,
      source_provider TEXT,
      target_provider TEXT,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_context_transfers_source_thread_idx ON orchestration_v2_projection_context_transfers(source_thread_id);
CREATE INDEX orchestration_v2_projection_context_transfers_target_thread_idx ON orchestration_v2_projection_context_transfers(target_thread_id, status);
CREATE INDEX orchestration_v2_projection_context_transfers_target_run_idx ON orchestration_v2_projection_context_transfers(target_run_id);
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/Subagents.ts
CREATE TABLE orchestration_v2_projection_subagents (
      subagent_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      parent_node_id TEXT NOT NULL,
      provider TEXT NOT NULL,
      provider_thread_id TEXT,
      child_thread_id TEXT,
      origin TEXT NOT NULL,
      status TEXT NOT NULL,
      started_at TEXT,
      completed_at TEXT,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_subagents_thread_idx ON orchestration_v2_projection_subagents(thread_id, started_at, subagent_id);
CREATE INDEX orchestration_v2_projection_subagents_parent_node_idx ON orchestration_v2_projection_subagents(parent_node_id);
CREATE INDEX orchestration_v2_projection_subagents_provider_thread_idx ON orchestration_v2_projection_subagents(provider_thread_id);
CREATE INDEX orchestration_v2_projection_subagents_child_thread_idx ON orchestration_v2_projection_subagents(child_thread_id);
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/Foundation.ts
ALTER TABLE orchestration_v2_events ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_events ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_events
    SET
      driver = COALESCE(json_extract(payload_json, '$.driver'), provider),
      provider_instance_id = COALESCE(
        json_extract(payload_json, '$.providerInstanceId'),
        provider
      );
CREATE INDEX orchestration_v2_events_instance_sequence_idx ON orchestration_v2_events(provider_instance_id, sequence);
ALTER TABLE orchestration_v2_projection_threads ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_projection_threads
    SET provider_instance_id = COALESCE(
      json_extract(payload_json, '$.providerInstanceId'),
      default_provider
    );
ALTER TABLE orchestration_v2_projection_runs ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_projection_runs
    SET provider_instance_id = COALESCE(
      json_extract(payload_json, '$.providerInstanceId'),
      provider
    );
ALTER TABLE orchestration_v2_projection_run_attempts ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_projection_run_attempts
    SET provider_instance_id = COALESCE(
      json_extract(payload_json, '$.providerInstanceId'),
      provider
    );
ALTER TABLE orchestration_v2_projection_provider_sessions ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_projection_provider_sessions ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_projection_provider_sessions
    SET
      driver = COALESCE(json_extract(payload_json, '$.driver'), provider),
      provider_instance_id = COALESCE(
        json_extract(payload_json, '$.providerInstanceId'),
        provider
      );
CREATE INDEX orchestration_v2_projection_provider_sessions_instance_status_idx ON orchestration_v2_projection_provider_sessions(provider_instance_id, status);
ALTER TABLE orchestration_v2_projection_provider_threads ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_projection_provider_threads ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_projection_provider_threads
    SET
      driver = COALESCE(json_extract(payload_json, '$.driver'), provider),
      provider_instance_id = COALESCE(
        json_extract(payload_json, '$.providerInstanceId'),
        provider
      );
CREATE INDEX orchestration_v2_projection_provider_threads_instance_status_idx ON orchestration_v2_projection_provider_threads(provider_instance_id, status);
ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN provider_instance_id TEXT;
UPDATE orchestration_v2_projection_subagents
    SET
      driver = json_extract(payload_json, '$.driver'),
      provider_instance_id = COALESCE(
        json_extract(payload_json, '$.providerInstanceId'),
        provider
      );
ALTER TABLE orchestration_v2_projection_context_transfers ADD COLUMN source_provider_instance_id TEXT;
ALTER TABLE orchestration_v2_projection_context_transfers ADD COLUMN target_provider_instance_id TEXT;
UPDATE orchestration_v2_projection_context_transfers
    SET
      source_provider_instance_id = COALESCE(
        json_extract(payload_json, '$.sourceProviderInstanceId'),
        source_provider
      ),
      target_provider_instance_id = COALESCE(
        json_extract(payload_json, '$.targetProviderInstanceId'),
        target_provider
      );
CREATE TABLE orchestration_v2_effect_outbox (
      effect_id TEXT PRIMARY KEY,
      command_id TEXT NOT NULL,
      thread_id TEXT NOT NULL,
      effect_type TEXT NOT NULL,
      payload_json TEXT NOT NULL,
      status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
      attempt_count INTEGER NOT NULL DEFAULT 0,
      available_at TEXT NOT NULL,
      lease_owner TEXT,
      lease_expires_at TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      completed_at TEXT,
      last_error TEXT
    );
CREATE INDEX orchestration_v2_effect_outbox_claim_idx ON orchestration_v2_effect_outbox(status, available_at, lease_expires_at, created_at);
CREATE INDEX orchestration_v2_effect_outbox_command_idx ON orchestration_v2_effect_outbox(command_id, effect_id);
CREATE TABLE orchestration_v2_turn_item_positions (
      thread_id TEXT NOT NULL,
      turn_item_id TEXT NOT NULL,
      ordinal INTEGER NOT NULL,
      PRIMARY KEY (thread_id, turn_item_id),
      UNIQUE (thread_id, ordinal)
    );
INSERT INTO orchestration_v2_turn_item_positions (
      thread_id,
      turn_item_id,
      ordinal
    )
    SELECT
      item.thread_id,
      item.turn_item_id,
      COALESCE(run.ordinal, 0) * 1000000 + ROW_NUMBER() OVER (
        PARTITION BY item.thread_id, COALESCE(run.ordinal, 0)
        ORDER BY item.ordinal, item.turn_item_id
      )
    FROM orchestration_v2_projection_turn_items AS item
    LEFT JOIN orchestration_v2_projection_runs AS run
      ON run.thread_id = item.thread_id
      AND run.run_id = item.run_id;
CREATE TABLE orchestration_v2_projection_metadata (
      projection_name TEXT PRIMARY KEY,
      schema_version INTEGER NOT NULL,
      last_sequence INTEGER NOT NULL,
      updated_at TEXT NOT NULL
    );
INSERT INTO orchestration_v2_projection_metadata (
      projection_name,
      schema_version,
      last_sequence,
      updated_at
    )
    VALUES (
      'thread-projections',
      1,
      COALESCE((SELECT MAX(sequence) FROM orchestration_v2_events), 0),
      strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    );
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ProviderSessionBindings.ts
CREATE TABLE orchestration_v2_projection_provider_session_bindings (
      provider_session_id TEXT NOT NULL,
      thread_id TEXT NOT NULL,
      PRIMARY KEY (provider_session_id, thread_id)
    );
INSERT OR IGNORE INTO orchestration_v2_projection_provider_session_bindings (
      provider_session_id,
      thread_id
    )
    SELECT provider_session_id, thread_id
    FROM orchestration_v2_projection_provider_sessions
    WHERE thread_id IS NOT NULL;
CREATE INDEX orchestration_v2_projection_provider_session_bindings_thread_idx
    ON orchestration_v2_projection_provider_session_bindings(thread_id);
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ThreadLaunchWorkflows.ts
CREATE TABLE IF NOT EXISTS orchestration_v2_thread_launch_workflows (
      command_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      project_id TEXT NOT NULL,
      status TEXT NOT NULL,
      title TEXT NOT NULL,
      worktree_path TEXT,
      branch TEXT,
      setup_committed INTEGER NOT NULL DEFAULT 0,
      thread_committed INTEGER NOT NULL DEFAULT 0,
      message_committed INTEGER NOT NULL DEFAULT 0,
      last_error TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ApplicationEventSource.ts
-- TURIN_APPLICATION_EVENT_SOURCE --
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/EffectCancellation.ts
CREATE TABLE orchestration_v2_effect_outbox_next (
      effect_id TEXT PRIMARY KEY,
      command_id TEXT NOT NULL,
      thread_id TEXT NOT NULL,
      effect_type TEXT NOT NULL,
      payload_json TEXT NOT NULL,
      status TEXT NOT NULL CHECK (
        status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled')
      ),
      attempt_count INTEGER NOT NULL DEFAULT 0,
      available_at TEXT NOT NULL,
      lease_owner TEXT,
      lease_expires_at TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      completed_at TEXT,
      last_error TEXT
    );
INSERT INTO orchestration_v2_effect_outbox_next (
      effect_id,
      command_id,
      thread_id,
      effect_type,
      payload_json,
      status,
      attempt_count,
      available_at,
      lease_owner,
      lease_expires_at,
      created_at,
      updated_at,
      completed_at,
      last_error
    )
    SELECT
      effect_id,
      command_id,
      thread_id,
      effect_type,
      payload_json,
      status,
      attempt_count,
      available_at,
      lease_owner,
      lease_expires_at,
      created_at,
      updated_at,
      completed_at,
      last_error
    FROM orchestration_v2_effect_outbox;
DROP TABLE orchestration_v2_effect_outbox;
ALTER TABLE orchestration_v2_effect_outbox_next
    RENAME TO orchestration_v2_effect_outbox;
CREATE INDEX orchestration_v2_effect_outbox_claim_idx
    ON orchestration_v2_effect_outbox(status, available_at, lease_expires_at, created_at);
CREATE INDEX orchestration_v2_effect_outbox_command_idx
    ON orchestration_v2_effect_outbox(command_id, effect_id);
CREATE INDEX orchestration_v2_effect_outbox_thread_status_idx
    ON orchestration_v2_effect_outbox(thread_id, status, effect_type);
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ScheduledTasks.ts
CREATE TABLE IF NOT EXISTS scheduled_tasks (
      task_id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      prompt TEXT NOT NULL,
      enabled INTEGER NOT NULL,
      schedule_json TEXT NOT NULL,
      project_id TEXT NOT NULL,
      thread_id TEXT,
      workspace_strategy_json TEXT NOT NULL,
      model_selection_json TEXT NOT NULL,
      runtime_mode TEXT NOT NULL,
      interaction_mode TEXT NOT NULL,
      created_by TEXT NOT NULL,
      creation_source TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      next_run_at TEXT,
      last_run_at TEXT,
      last_run_status TEXT NOT NULL,
      last_run_error TEXT,
      run_count INTEGER NOT NULL
    );
CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
    ON scheduled_tasks(enabled, next_run_at)
    WHERE enabled = 1 AND next_run_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_project
    ON scheduled_tasks(project_id, updated_at);
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/LegacyV1ImportState.ts
CREATE TABLE orchestration_v2_legacy_imports (
      thread_id TEXT PRIMARY KEY,
      source_updated_at TEXT NOT NULL,
      shell_imported_at TEXT NOT NULL,
      transcript_imported_at TEXT,
      imported_message_count INTEGER NOT NULL DEFAULT 0,
      last_error TEXT
    );
CREATE INDEX orchestration_v2_legacy_imports_pending_transcript_idx
    ON orchestration_v2_legacy_imports(transcript_imported_at, shell_imported_at, thread_id);
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ApplicationEventSequenceIndexes.ts
CREATE INDEX idx_orchestration_events_application_high_water
    ON orchestration_events(sequence)
    WHERE aggregate_kind = 'project'
      OR (application_event_version = 2 AND aggregate_kind = 'thread');
CREATE INDEX idx_orchestration_events_agent_stream_sequence
    ON orchestration_events(stream_id, sequence)
    WHERE application_event_version = 2 AND aggregate_kind = 'thread';
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/RecoveryIndexes.ts
CREATE INDEX orchestration_events_v2_created_threads_idx
    ON orchestration_events(stream_id)
    WHERE application_event_version = 2
      AND aggregate_kind = 'thread'
      AND event_type = 'thread.created';
CREATE INDEX orchestration_v2_projection_runs_recovery_idx
    ON orchestration_v2_projection_runs(status, thread_id)
    WHERE status IN ('queued', 'preparing', 'starting', 'running', 'waiting');
CREATE INDEX orchestration_v2_projection_requests_recovery_idx
    ON orchestration_v2_projection_runtime_requests(thread_id)
    WHERE status = 'pending';
CREATE INDEX orchestration_v2_projection_turn_items_recovery_idx
    ON orchestration_v2_projection_turn_items(thread_id)
    WHERE type IN ('command_execution', 'dynamic_tool', 'subagent')
      AND status IN ('pending', 'running', 'waiting');
-- Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ShellIndexes.ts
CREATE INDEX orchestration_v2_projection_turn_items_thread_run_idx
    ON orchestration_v2_projection_turn_items(thread_id, run_id);
CREATE INDEX orchestration_v2_projection_turn_items_shell_pending_idx
    ON orchestration_v2_projection_turn_items(thread_id, run_id)
    WHERE type IN ('command_execution', 'dynamic_tool', 'subagent')
      AND status NOT IN ('completed', 'interrupted', 'failed', 'cancelled');
CREATE INDEX orchestration_v2_projection_messages_latest_user_idx
    ON orchestration_v2_projection_messages(thread_id, updated_at DESC, message_id DESC)
    WHERE role = 'user';
