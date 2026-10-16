-- Port of upstream/apps/server/src/persistence/Migrations/043_ProjectionThreadsUnsettledAt.ts
ALTER TABLE projection_threads
      ADD COLUMN unsettled_at TEXT;
