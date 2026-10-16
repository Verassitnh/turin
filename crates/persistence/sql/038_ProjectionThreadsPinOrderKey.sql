-- Port of upstream/apps/server/src/persistence/Migrations/038_ProjectionThreadsPinOrderKey.ts
ALTER TABLE projection_threads
      ADD COLUMN pin_order_key TEXT;
