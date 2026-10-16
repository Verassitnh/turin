-- Port of upstream/apps/server/src/persistence/Migrations/011_OrchestrationThreadCreatedRuntimeMode.ts
UPDATE orchestration_events
    SET payload_json = json_set(payload_json, '$.runtimeMode', 'full-access')
    WHERE event_type = 'thread.created'
      AND json_type(payload_json, '$.runtimeMode') IS NULL;
