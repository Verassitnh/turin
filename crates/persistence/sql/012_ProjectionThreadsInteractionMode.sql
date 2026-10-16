-- Port of upstream/apps/server/src/persistence/Migrations/012_ProjectionThreadsInteractionMode.ts
ALTER TABLE projection_threads
    ADD COLUMN interaction_mode TEXT NOT NULL DEFAULT 'default';
