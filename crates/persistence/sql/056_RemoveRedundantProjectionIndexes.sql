-- Port of upstream/apps/server/src/persistence/Migrations/056_RemoveRedundantProjectionIndexes.ts
DROP INDEX IF EXISTS idx_projection_threads_project_id;
DROP INDEX IF EXISTS idx_projection_thread_messages_thread_created;
DROP INDEX IF EXISTS idx_projection_thread_activities_thread_sequence;
DROP INDEX IF EXISTS idx_projection_turns_thread_requested;
