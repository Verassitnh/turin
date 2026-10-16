-- Port of upstream/apps/server/src/persistence/Migrations/042_ProjectionThreadLinkedPullRequest.ts
ALTER TABLE projection_threads
      ADD COLUMN linked_pull_request_json TEXT;
