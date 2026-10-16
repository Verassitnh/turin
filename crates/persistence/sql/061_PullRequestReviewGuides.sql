-- Port of upstream/apps/server/src/persistence/Migrations/061_PullRequestReviewGuides.ts
CREATE TABLE IF NOT EXISTS pull_request_review_guides (
      host TEXT NOT NULL,
      repository TEXT NOT NULL,
      number INTEGER NOT NULL,
      guide_json TEXT NOT NULL,
      PRIMARY KEY (host, repository, number)
    ) WITHOUT ROWID;
