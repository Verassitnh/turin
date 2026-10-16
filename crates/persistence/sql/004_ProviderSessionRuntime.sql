-- Port of upstream/apps/server/src/persistence/Migrations/004_ProviderSessionRuntime.ts
CREATE TABLE IF NOT EXISTS provider_session_runtime (
      thread_id TEXT PRIMARY KEY,
      provider_name TEXT NOT NULL,
      adapter_key TEXT NOT NULL,
      runtime_mode TEXT NOT NULL DEFAULT 'full-access',
      status TEXT NOT NULL,
      last_seen_at TEXT NOT NULL,
      resume_cursor_json TEXT,
      runtime_payload_json TEXT
    );
CREATE INDEX IF NOT EXISTS idx_provider_session_runtime_status
    ON provider_session_runtime(status);
CREATE INDEX IF NOT EXISTS idx_provider_session_runtime_provider
    ON provider_session_runtime(provider_name);
