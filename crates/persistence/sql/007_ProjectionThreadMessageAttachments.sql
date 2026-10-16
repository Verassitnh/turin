-- Port of upstream/apps/server/src/persistence/Migrations/007_ProjectionThreadMessageAttachments.ts
ALTER TABLE projection_thread_messages
    ADD COLUMN attachments_json TEXT;
