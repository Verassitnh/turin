-- Port of upstream/apps/server/src/persistence/Migrations/015_ProjectionTurnsSourceProposedPlan.ts
ALTER TABLE projection_turns
    ADD COLUMN source_proposed_plan_thread_id TEXT;
ALTER TABLE projection_turns
    ADD COLUMN source_proposed_plan_id TEXT;
