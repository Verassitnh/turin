ALTER TABLE provider_session_runtime
      ADD COLUMN provider_instance_id TEXT;
CREATE INDEX IF NOT EXISTS idx_provider_session_runtime_instance
    ON provider_session_runtime(provider_instance_id);
