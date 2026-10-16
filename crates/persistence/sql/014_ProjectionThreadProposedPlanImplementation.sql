-- Port of upstream/apps/server/src/persistence/Migrations/014_ProjectionThreadProposedPlanImplementation.ts
ALTER TABLE projection_thread_proposed_plans
    ADD COLUMN implemented_at TEXT;
ALTER TABLE projection_thread_proposed_plans
    ADD COLUMN implementation_thread_id TEXT;
