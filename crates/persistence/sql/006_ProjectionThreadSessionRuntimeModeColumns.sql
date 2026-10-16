-- Port of upstream/apps/server/src/persistence/Migrations/006_ProjectionThreadSessionRuntimeModeColumns.ts
ALTER TABLE projection_thread_sessions
      ADD COLUMN runtime_mode TEXT NOT NULL DEFAULT 'full-access';
UPDATE projection_thread_sessions
    SET runtime_mode = 'full-access'
    WHERE runtime_mode IS NULL;
