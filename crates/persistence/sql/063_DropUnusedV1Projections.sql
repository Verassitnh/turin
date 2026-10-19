-- Port of upstream/apps/server/src/persistence/Migrations/063_DropUnusedV1Projections.ts
-- V2 databases start as a copy of the V1 database, and nothing reads these V1
-- projections after the copy. The V1 transcript import still reads threads,
-- messages and pull request links, so those stay. Builds before 0.1.0 read
-- `state.sqlite`, never this file, so dropping them does not affect a rollback.
DROP TABLE IF EXISTS projection_thread_activities;
DROP TABLE IF EXISTS projection_turns;
DROP TABLE IF EXISTS projection_thread_proposed_plans;
DROP TABLE IF EXISTS projection_pending_approvals;
DROP TABLE IF EXISTS projection_state;
DROP TABLE IF EXISTS checkpoint_diff_blobs;
