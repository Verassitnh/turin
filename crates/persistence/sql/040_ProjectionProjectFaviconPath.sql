-- Port of upstream/apps/server/src/persistence/Migrations/040_ProjectionProjectFaviconPath.ts
ALTER TABLE projection_projects
      ADD COLUMN favicon_path TEXT;
