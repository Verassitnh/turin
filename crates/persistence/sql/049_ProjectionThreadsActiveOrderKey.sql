-- Port of upstream/apps/server/src/persistence/Migrations/049_ProjectionThreadsActiveOrderKey.ts
ALTER TABLE projection_threads
      ADD COLUMN active_order_key TEXT;
