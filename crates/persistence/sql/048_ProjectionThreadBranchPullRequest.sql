-- Port of upstream/apps/server/src/persistence/Migrations/048_ProjectionThreadBranchPullRequest.ts
ALTER TABLE projection_threads
      ADD COLUMN branch_pull_request_json TEXT;
