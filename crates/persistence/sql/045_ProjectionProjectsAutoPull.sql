-- Port of upstream/apps/server/src/persistence/Migrations/045_ProjectionProjectsAutoPull.ts
ALTER TABLE projection_projects
      ADD COLUMN auto_pull INTEGER NOT NULL DEFAULT 0;
