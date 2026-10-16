-- Port of upstream/apps/server/src/persistence/Migrations/035_ProjectionThreadTitleRegeneration.ts
ALTER TABLE projection_threads
      ADD COLUMN title_regeneration_request_id TEXT;
ALTER TABLE projection_threads
      ADD COLUMN title_regeneration_started_at TEXT;
