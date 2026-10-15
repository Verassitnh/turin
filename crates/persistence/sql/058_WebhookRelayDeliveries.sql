CREATE TABLE IF NOT EXISTS scheduled_task_webhook_relay_deliveries (
      relay_delivery_id TEXT PRIMARY KEY,
      task_id TEXT NOT NULL,
      seen_at TEXT NOT NULL
    );
CREATE INDEX IF NOT EXISTS idx_scheduled_task_webhook_relay_deliveries_seen
    ON scheduled_task_webhook_relay_deliveries(seen_at);
