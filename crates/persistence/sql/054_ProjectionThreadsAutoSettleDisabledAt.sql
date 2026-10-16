-- Port of upstream/apps/server/src/persistence/Migrations/054_ProjectionThreadsAutoSettleDisabledAt.ts
ALTER TABLE projection_threads
      ADD COLUMN auto_settle_disabled_at TEXT;
