-- Port of upstream/apps/server/src/persistence/Migrations/052_ProjectionThreadTitleState.ts
ALTER TABLE projection_threads ADD COLUMN title_state_json TEXT;
