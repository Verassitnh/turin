ALTER TABLE projection_thread_activities
    ADD COLUMN sequence INTEGER;
CREATE INDEX IF NOT EXISTS idx_projection_thread_activities_thread_sequence
    ON projection_thread_activities(thread_id, sequence);
