-- Port of upstream/apps/server/src/persistence/Migrations/028_ProjectionThreadSessionInstanceId.ts
ALTER TABLE projection_thread_sessions
      ADD COLUMN provider_instance_id TEXT;
CREATE INDEX IF NOT EXISTS idx_projection_thread_sessions_instance
    ON projection_thread_sessions(provider_instance_id);
