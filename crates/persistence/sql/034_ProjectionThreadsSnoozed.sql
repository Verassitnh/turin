-- Port of upstream/apps/server/src/persistence/Migrations/034_ProjectionThreadsSnoozed.ts
ALTER TABLE projection_threads
      ADD COLUMN snoozed_until TEXT;
ALTER TABLE projection_threads
      ADD COLUMN snoozed_at TEXT;
