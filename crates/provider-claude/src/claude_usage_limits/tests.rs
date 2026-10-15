//! Port of upstream/apps/server/src/provider/claudeUsageLimits.test.ts.
use super::*;
use serde_json::json;

const CHECKED_AT: &str = "2026-07-18T10:00:00.000Z";

fn response(available: bool, rate_limits: Value) -> ClaudeUsageResponse {
    ClaudeUsageResponse {
        rate_limits_available: Some(available),
        rate_limits: Some(rate_limits),
    }
}

fn no_names() -> ClaudeScopedLimitNames {
    ClaudeScopedLimitNames::default()
}

fn event(value: Value) -> ClaudeRateLimitInfo {
    serde_json::from_value(value).expect("rate limit info decodes")
}

#[test]
fn maps_the_session_weekly_and_model_scoped_weekly_windows() {
    let (limits, names) = claude_usage_response_to_limits(
        &response(
            true,
            json!({
                "five_hour": {"utilization": 54, "resets_at": "2026-07-18T14:39:00Z"},
                "seven_day": {"utilization": 18.4, "resets_at": "2026-07-24T08:59:00+00:00"},
                "seven_day_opus": {"utilization": 3, "resets_at": null},
                // Newer CLIs add this on top of the typed keys.
                "model_scoped": [
                    {"display_name": "Fable", "utilization": 73, "resets_at": "2026-07-24T08:59:00Z"},
                    {"display_name": "Ghost", "utilization": null, "resets_at": null}
                ],
                "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null}
            }),
        ),
        CHECKED_AT,
    );

    assert_eq!(names.overage_included.as_deref(), Some("Fable"));
    assert_eq!(
        serde_json::to_value(&limits).unwrap(),
        json!({
            "checkedAt": CHECKED_AT,
            "windows": [
                {
                    "id": "five_hour",
                    "kind": "session",
                    "label": "Session",
                    "usedPercent": 54,
                    "windowDurationMins": 300,
                    "resetsAt": "2026-07-18T14:39:00.000Z"
                },
                {
                    "id": "seven_day",
                    "kind": "weekly",
                    "label": "Weekly",
                    "usedPercent": 18.4,
                    "windowDurationMins": 10080,
                    "resetsAt": "2026-07-24T08:59:00.000Z"
                },
                {
                    "id": "seven_day_fable",
                    "kind": "weekly",
                    "label": "Weekly · Fable",
                    "usedPercent": 73,
                    "windowDurationMins": 10080,
                    "resetsAt": "2026-07-24T08:59:00.000Z"
                }
            ]
        })
    );
}

#[test]
fn names_the_overage_included_bucket_only_from_a_scoped_entry_that_drew_a_row() {
    let (_, names) = claude_usage_response_to_limits(
        &response(
            true,
            json!({"model_scoped": [
                {"display_name": "Ghost", "utilization": null, "resets_at": null},
                {"display_name": "Fable", "utilization": 5, "resets_at": null}
            ]}),
        ),
        CHECKED_AT,
    );
    assert_eq!(names.overage_included.as_deref(), Some("Fable"));
}

#[test]
fn reports_api_key_and_bedrock_accounts_as_unsupported() {
    let (limits, _) = claude_usage_response_to_limits(
        &ClaudeUsageResponse {
            rate_limits_available: Some(false),
            rate_limits: Some(Value::Null),
        },
        CHECKED_AT,
    );
    assert_eq!(
        serde_json::to_value(&limits).unwrap(),
        json!({"checkedAt": CHECKED_AT, "windows": [], "unavailable": {"reason": "unsupported"}})
    );
}

#[test]
fn skips_a_window_the_endpoint_reports_without_a_utilization() {
    let (limits, _) = claude_usage_response_to_limits(
        &response(
            true,
            json!({
                "five_hour": {"utilization": null, "resets_at": null},
                "seven_day": {"utilization": 250, "resets_at": null}
            }),
        ),
        CHECKED_AT,
    );
    assert_eq!(
        serde_json::to_value(&limits.windows).unwrap(),
        json!([{
            "id": "seven_day",
            "kind": "weekly",
            "label": "Weekly",
            "usedPercent": 100,
            "windowDurationMins": 10080
        }])
    );
}

#[test]
fn scales_the_0_to_1_utilization_and_epoch_second_reset_onto_the_probes_window_id() {
    let update = claude_rate_limit_event_to_update(
        &event(json!({
            "status": "allowed_warning",
            "rateLimitType": "seven_day",
            "utilization": 0.85,
            "resetsAt": 1_784_000_000
        })),
        &no_names(),
    )
    .expect("update");
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        json!({"windows": [{
            "id": "seven_day",
            "kind": "weekly",
            "label": "Weekly",
            "usedPercent": 85,
            "windowDurationMins": 10080,
            "resetsAt": "2026-07-14T03:33:20.000Z"
        }]})
    );
}

#[test]
fn lands_the_streamed_overage_included_bucket_on_the_row_the_probe_named() {
    let streamed = event(json!({
        "status": "allowed",
        "rateLimitType": "seven_day_overage_included",
        "utilization": 0.4
    }));
    // No probe has named the bucket yet: guessing would open a stray row.
    assert_eq!(
        claude_rate_limit_event_to_update(&streamed, &no_names()),
        None
    );

    let named = ClaudeScopedLimitNames {
        overage_included: Some("Fable".into()),
    };
    assert_eq!(
        serde_json::to_value(claude_rate_limit_event_to_update(&streamed, &named).unwrap())
            .unwrap(),
        json!({"windows": [{
            "id": "seven_day_fable",
            "kind": "weekly",
            "label": "Weekly · Fable",
            "usedPercent": 40,
            "windowDurationMins": 10080
        }]})
    );
}

#[test]
fn ignores_windows_the_page_does_not_render_and_events_without_a_utilization() {
    assert_eq!(
        claude_rate_limit_event_to_update(
            &event(
                json!({"status": "allowed", "rateLimitType": "seven_day_opus", "utilization": 0.1})
            ),
            &no_names(),
        ),
        None
    );
    assert_eq!(
        claude_rate_limit_event_to_update(
            &event(json!({"status": "rejected", "rateLimitType": "five_hour"})),
            &no_names(),
        ),
        None
    );
}

#[test]
fn record_keeps_the_scoped_names_for_the_event_mapper() {
    let names = SharedClaudeScopedLimitNames::default();
    record_claude_usage_response(
        &names,
        &response(
            true,
            json!({"model_scoped": [{"display_name": "Fable", "utilization": 5, "resets_at": null}]}),
        ),
        CHECKED_AT,
    );
    assert_eq!(names.lock().overage_included.as_deref(), Some("Fable"));
}
