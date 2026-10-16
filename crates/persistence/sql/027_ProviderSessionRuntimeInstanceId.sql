-- Port of upstream/apps/server/src/persistence/Migrations/027_ProviderSessionRuntimeInstanceId.ts
-- Adds the nullable provider_instance_id routing column.
-- The column is nullable and not backfilled: existing rows have only the
-- driver name in provider_name, and which configured instance owned a
-- historical session is ambiguous once a driver has more than one instance.
ALTER TABLE provider_session_runtime
      ADD COLUMN provider_instance_id TEXT;
CREATE INDEX IF NOT EXISTS idx_provider_session_runtime_instance
    ON provider_session_runtime(provider_instance_id);
