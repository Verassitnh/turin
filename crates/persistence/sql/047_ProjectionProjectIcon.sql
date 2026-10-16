-- Port of upstream/apps/server/src/persistence/Migrations/047_ProjectionProjectIcon.ts
ALTER TABLE projection_projects
      ADD COLUMN project_icon_json TEXT;
