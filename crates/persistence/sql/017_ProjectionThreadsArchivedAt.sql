-- Port of upstream/apps/server/src/persistence/Migrations/017_ProjectionThreadsArchivedAt.ts
ALTER TABLE projection_threads
    ADD COLUMN archived_at TEXT;
