-- Port of upstream/apps/server/src/persistence/Migrations/051_ProjectionThreadMessageContext.ts
ALTER TABLE projection_thread_messages
      ADD COLUMN context_json TEXT;
