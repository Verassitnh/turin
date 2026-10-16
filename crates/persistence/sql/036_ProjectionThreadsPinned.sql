-- Port of upstream/apps/server/src/persistence/Migrations/036_ProjectionThreadsPinned.ts
ALTER TABLE projection_threads
      ADD COLUMN pinned_at TEXT;
