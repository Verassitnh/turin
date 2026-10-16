-- Port of upstream/apps/server/src/persistence/Migrations/023_ProjectionThreadShellSummary.ts
ALTER TABLE projection_threads
    ADD COLUMN latest_user_message_at TEXT;
ALTER TABLE projection_threads
    ADD COLUMN pending_approval_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projection_threads
    ADD COLUMN pending_user_input_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projection_threads
    ADD COLUMN has_actionable_proposed_plan INTEGER NOT NULL DEFAULT 0;
