-- Port of upstream/apps/server/src/persistence/Migrations/018_ProjectionThreadsArchivedAtIndex.ts
CREATE INDEX IF NOT EXISTS idx_projection_threads_project_archived_at
    ON projection_threads(project_id, archived_at);
