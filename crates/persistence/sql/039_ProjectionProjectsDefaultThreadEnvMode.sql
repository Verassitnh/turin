-- Port of upstream/apps/server/src/persistence/Migrations/039_ProjectionProjectsDefaultThreadEnvMode.ts
ALTER TABLE projection_projects
      ADD COLUMN default_thread_env_mode TEXT;
