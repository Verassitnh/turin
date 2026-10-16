-- Port of upstream/apps/server/src/persistence/Migrations/029_ProjectionThreadDetailOrderingIndexes.ts
CREATE INDEX IF NOT EXISTS idx_projection_thread_activities_thread_sequence_created_id
    ON projection_thread_activities(thread_id, sequence, created_at, activity_id);
CREATE INDEX IF NOT EXISTS idx_projection_thread_messages_thread_created_id
    ON projection_thread_messages(thread_id, created_at, message_id);
