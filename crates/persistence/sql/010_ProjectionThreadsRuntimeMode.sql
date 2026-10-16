-- Port of upstream/apps/server/src/persistence/Migrations/010_ProjectionThreadsRuntimeMode.ts
ALTER TABLE projection_threads
    ADD COLUMN runtime_mode TEXT NOT NULL DEFAULT 'full-access';
UPDATE projection_threads
    SET runtime_mode = 'full-access'
    WHERE runtime_mode IS NULL;
