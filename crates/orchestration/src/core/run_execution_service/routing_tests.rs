//! Upstream `RunExecutionService.test.ts` routing cases (lines 70-569):
//! `finalProviderThreadStatus`, `routeProviderEvent`, and the inherited
//! background selection. Upstream threads an immutable state through
//! `routeProviderEvent`; the Rust function updates it in place, so `route`
//! clones the state first and returns the state after, as upstream's tuple does.

use serde_json::{Value, json};
use turin_contracts::{
    ProviderThreadId, RunAttemptId, RunId, ThreadId, TurnItemId,
    orchestration_v2::{
        OrchestrationV2ProviderThreadDisposition, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2Run, OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
    },
};
use turin_provider_core::provider_adapter::ProviderAdapterV2Event;

use super::*;
use crate::projection::test_support::{message, run, thread, turn_item};

const NOW: &str = "2026-10-03T12:00:00.000Z";

fn route(
    event: &ProviderAdapterV2Event,
    identity: &ProviderEventRouteIdentity,
    state: &ProviderEventRoutingState,
) -> (bool, ProviderEventRoutingState) {
    let mut after = state.clone();
    let accepted = route_provider_event(event, identity, &mut after);
    (accepted, after)
}

fn event(value: Value) -> ProviderAdapterV2Event {
    serde_json::from_value(value).expect("event fixture")
}

fn identity(key: &str) -> ProviderEventRouteIdentity {
    ProviderEventRouteIdentity {
        thread_id: ThreadId::new(format!("thread:{key}")).unwrap(),
        run_id: RunId::new(format!("run:{key}")).unwrap(),
        attempt_id: RunAttemptId::new(format!("attempt:{key}")).unwrap(),
        provider_thread_id: ProviderThreadId::new(format!("provider-thread:{key}")).unwrap(),
    }
}

fn initial(
    identity: &ProviderEventRouteIdentity,
    provider_turn_id: Option<&str>,
) -> ProviderEventRoutingState {
    make_provider_event_routing_state(
        identity,
        &[],
        provider_turn_id
            .map(|id| turin_contracts::ProviderTurnId::new(id).unwrap())
            .as_ref(),
        &[],
        &[],
    )
}

fn provider_turn_event(
    identity: &ProviderEventRouteIdentity,
    turn: &str,
    status: &str,
) -> ProviderAdapterV2Event {
    event(json!({
        "type": "provider_turn.updated", "driver": "codex", "thread_id": identity.thread_id,
        "provider_turn": {
            "id": turn, "providerThreadId": identity.provider_thread_id,
            "nodeId": format!("node:{turn}"), "runAttemptId": identity.attempt_id,
            "nativeTurnRef": null, "ordinal": 1, "status": status, "startedAt": NOW,
            "completedAt": null, "tokenUsage": null, "turnTokenUsage": null,
        },
    }))
}

fn terminal(identity: &ProviderEventRouteIdentity, turn: &str) -> ProviderAdapterV2Event {
    event(json!({
        "type": "turn.terminal", "driver": "codex",
        "provider_thread_id": identity.provider_thread_id, "provider_turn_id": turn,
        "run_ordinal": 1, "status": "completed", "failure": null,
        "thread_disposition": "reusable",
    }))
}

#[test]
fn keeps_recoverable_turn_failures_reusable_and_reserves_error_for_broken_threads() {
    assert_eq!(
        final_provider_thread_status(OrchestrationV2ProviderThreadDisposition::Reusable),
        OrchestrationV2ProviderThreadStatus::Idle
    );
    assert_eq!(
        final_provider_thread_status(OrchestrationV2ProviderThreadDisposition::Broken),
        OrchestrationV2ProviderThreadStatus::Error
    );
}

#[test]
fn routes_shared_runtime_events_only_to_their_owning_root_run() {
    let first = identity("shared-runtime:first");
    let second = identity("shared-runtime:second");
    let first_turn = "provider-turn:shared-runtime:first";
    let turn_event = provider_turn_event(&first, first_turn, "running");
    let mut message = message(
        first.thread_id.as_str(),
        "message:shared-runtime:first",
        Some(first.run_id.as_str()),
        "assistant",
        "first only",
        NOW,
    );
    message["nodeId"] = json!("node:shared-runtime:first");
    let message_event =
        event(json!({"type": "message.updated", "driver": "codex", "message": message}));
    let terminal_event = terminal(&first, first_turn);

    let (first_accepted, first_after) = route(&turn_event, &first, &initial(&first, None));
    let (second_accepted, second_after) = route(&turn_event, &second, &initial(&second, None));

    assert!(first_accepted);
    assert!(!second_accepted);
    assert!(route(&message_event, &first, &first_after).0);
    assert!(!route(&message_event, &second, &second_after).0);
    assert!(route(&terminal_event, &first, &first_after).0);
    assert!(!route(&terminal_event, &second, &second_after).0);
}

fn child_created(child: &ThreadId, parent: &ThreadId, root: &ThreadId) -> ProviderAdapterV2Event {
    let mut app_thread = thread(child.as_str());
    app_thread["lineage"] = json!({
        "parentThreadId": parent, "relationshipToParent": "subagent", "rootThreadId": root,
    });
    event(json!({"type": "app_thread.created", "driver": "codex", "app_thread": app_thread}))
}

fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}

#[test]
fn routes_only_enrolled_workflow_members_after_the_root_turn_ends() {
    let identity = identity("late-child");
    let thread_id = identity.thread_id.clone();
    let root_turn = "provider-turn:late-child";
    let created = |child: &ThreadId, parent: &ThreadId| child_created(child, parent, &thread_id);
    let early_child = tid("thread:late-child:early");
    let late_child = tid("thread:late-child:late");

    let (early_accepted, live) = route(
        &created(&early_child, &thread_id),
        &identity,
        &initial(&identity, Some(root_turn)),
    );
    assert!(early_accepted);
    let existing_descendant = tid("thread:late-child:existing-descendant");
    let (descendant_accepted, with_descendant) = route(
        &created(&existing_descendant, &early_child),
        &identity,
        &live,
    );
    assert!(descendant_accepted);
    let (terminal_accepted, ended) =
        route(&terminal(&identity, root_turn), &identity, &with_descendant);
    assert!(terminal_accepted);
    // A child the root launched before it ended stays with this run.
    assert!(ended.owned_thread_ids.contains(&early_child));
    let (late_accepted, after_late) = route(&created(&late_child, &thread_id), &identity, &ended);
    assert!(!late_accepted);
    assert!(!after_late.owned_thread_ids.contains(&late_child));

    let member = tid("thread:late-child:workflow-member");
    assert!(!route(&created(&member, &early_child), &identity, &ended).0);
    let coordinator = {
        let subagent = json!({
            "id": "node:late-child:workflow", "threadId": thread_id, "runId": identity.run_id,
            "parentNodeId": "node:late-child", "origin": "provider_native", "createdBy": "agent",
            "driver": "codex", "providerInstanceId": "codex", "providerThreadId": null,
            "childThreadId": early_child, "nativeTaskRef": null, "prompt": "Review the code",
            "title": "Review", "model": null, "status": "running", "result": null,
            "startedAt": null, "completedAt": null, "updatedAt": "1970-01-01T00:00:00.000Z",
            "workflow": {
                "phases": [],
                "agents": [{"index": 0, "label": "Reviewer", "state": "running", "childThreadId": member}],
            },
        });
        subagent
    };
    let subagent_event = |subagent: Value| {
        event(json!({"type": "subagent.updated", "driver": "codex", "subagent": subagent}))
    };
    // A subagent of someone else's child: accepted as a child of an owned
    // thread, but it does not enroll a workflow member.
    let mut foreign = coordinator.clone();
    foreign["threadId"] = json!(early_child);
    foreign["childThreadId"] = json!(existing_descendant);
    foreign["runId"] = json!("run:late-child:new");
    let (foreign_accepted, after_foreign) = route(&subagent_event(foreign), &identity, &ended);
    assert!(foreign_accepted);
    assert!(
        !route(
            &created(&member, &existing_descendant),
            &identity,
            &after_foreign
        )
        .0
    );
    let (workflow_accepted, after_workflow) =
        route(&subagent_event(coordinator), &identity, &ended);
    assert!(workflow_accepted);
    // Enrolled under the coordinator child only.
    assert!(!route(&created(&member, &thread_id), &identity, &after_workflow).0);
    let (member_accepted, after_member) =
        route(&created(&member, &early_child), &identity, &after_workflow);
    assert!(member_accepted);
    assert!(after_member.owned_thread_ids.contains(&member));
    let (unrelated_accepted, _) = route(
        &created(&tid("thread:late-child:unrelated-member"), &late_child),
        &identity,
        &ended,
    );
    assert!(!unrelated_accepted);
    let unrelated_descendant = tid("thread:late-child:unrelated-descendant");
    let (unrelated_descendant_accepted, after_unrelated) = route(
        &created(&unrelated_descendant, &early_child),
        &identity,
        &after_workflow,
    );
    assert!(!unrelated_descendant_accepted);
    assert!(
        !after_unrelated
            .owned_thread_ids
            .contains(&unrelated_descendant)
    );
}

#[test]
fn does_not_route_a_superseded_attempt_through_a_reused_provider_thread() {
    let old_attempt = identity("shared-runtime:restart");
    let new_attempt = ProviderEventRouteIdentity {
        attempt_id: RunAttemptId::new("attempt:shared-runtime:restart:new").unwrap(),
        ..old_attempt.clone()
    };
    let old_turn = provider_turn_event(
        &old_attempt,
        "provider-turn:shared-runtime:restart:old",
        "interrupted",
    );
    let new_state = initial(&new_attempt, None);
    assert!(!route(&old_turn, &new_attempt, &new_state).0);
}

fn background_item(
    thread: &str,
    id: &str,
    run_id: &str,
    status: &str,
    kind: &str,
    provider_thread: &str,
) -> Value {
    let extra = match kind {
        "subagent" => json!({
            "subagentId": "node:bg", "origin": "provider_native", "driver": "codex",
            "providerInstanceId": "codex", "childThreadId": null, "prompt": "Review",
            "progress": null, "result": null,
        }),
        _ => json!({"text": "thinking", "streaming": false}),
    };
    let mut value = turn_item(thread, id, Some(run_id), 1, kind, status, extra);
    value["nodeId"] = Value::Null;
    value["providerThreadId"] = json!(provider_thread);
    value
}

#[test]
fn routes_only_exact_same_thread_background_items_inherited_from_settled_runs() {
    let identity = identity("inherited-background-routing:current");
    let thread = "thread:inherited-background-routing:current";
    let prior = "run:inherited-background-routing:prior";
    let item_id = TurnItemId::new("turn-item:inherited-background-routing").unwrap();
    let state = make_provider_event_routing_state(
        &identity,
        &[InheritedBackgroundTurnItemRoute {
            id: item_id.clone(),
            run_id: RunId::new(prior).unwrap(),
        }],
        None,
        &[],
        &[],
    );
    let item_event = |value: Value| {
        event(json!({"type": "turn_item.updated", "driver": "codex", "turn_item": value}))
    };
    let running = background_item(
        thread,
        item_id.as_str(),
        prior,
        "running",
        "subagent",
        "provider-thread:x",
    );
    let patched = |fields: Value| {
        let mut value = running.clone();
        for (key, field) in fields.as_object().unwrap() {
            value[key] = field.clone();
        }
        item_event(value)
    };

    let (running_accepted, after_running) = route(&item_event(running.clone()), &identity, &state);
    assert!(running_accepted);
    assert!(
        !route(
            &patched(json!({"runId": "run:inherited-background-routing:unrelated"})),
            &identity,
            &after_running
        )
        .0
    );
    assert!(
        !route(
            &patched(json!({"id": "turn-item:inherited-background-routing:unrelated"})),
            &identity,
            &after_running
        )
        .0
    );
    assert!(
        !route(
            &patched(json!({"threadId": "thread:inherited-background-routing:other"})),
            &identity,
            &after_running
        )
        .0
    );
    let ordinary = item_event(background_item(
        thread,
        item_id.as_str(),
        prior,
        "running",
        "reasoning",
        "provider-thread:x",
    ));
    assert!(!route(&ordinary, &identity, &after_running).0);

    let (terminal_accepted, after_terminal) = route(
        &patched(json!({"status": "completed", "completedAt": NOW})),
        &identity,
        &after_running,
    );
    assert!(terminal_accepted);
    assert!(
        !route(&item_event(running), &identity, &after_terminal).0,
        "a nonterminal replay must not resurrect an inherited terminal"
    );
}

#[test]
fn selects_only_live_background_items_from_non_completed_settled_prior_runs() {
    let thread_id = tid("thread:inherited-background-selection");
    let other_thread = "thread:inherited-background-selection:other";
    let current_provider = "provider-thread:inherited-background-selection:current";
    let foreign_provider = "provider-thread:inherited-background-selection:foreign-session";
    let run_ids = [
        (
            "interrupted",
            "run:inherited-background-selection:interrupted",
        ),
        ("failed", "run:inherited-background-selection:failed"),
        ("cancelled", "run:inherited-background-selection:cancelled"),
        ("completed", "run:inherited-background-selection:completed"),
        (
            "rolled_back",
            "run:inherited-background-selection:rolled-back",
        ),
    ];
    let make_run = |id: &str, ordinal: u64, status: &str, thread: &str| -> OrchestrationV2Run {
        serde_json::from_value(run(thread, id, ordinal, status)).expect("run")
    };
    let mut runs: Vec<OrchestrationV2Run> = run_ids
        .iter()
        .enumerate()
        .map(|(index, (status, id))| make_run(id, index as u64 + 1, status, thread_id.as_str()))
        .collect();
    runs.push(make_run(
        "run:inherited-background-selection:current",
        6,
        "running",
        thread_id.as_str(),
    ));
    runs.push(make_run(
        "run:inherited-background-selection:other",
        1,
        "interrupted",
        other_thread,
    ));
    let item = |id: &str,
                run_id: &str,
                status: &str,
                kind: &str,
                thread: &str,
                provider: &str|
     -> OrchestrationV2TurnItem {
        serde_json::from_value(background_item(thread, id, run_id, status, kind, provider))
            .expect("item")
    };
    let id = |suffix: &str| format!("turn-item:inherited-background-selection:{suffix}");
    let (interrupted, failed, cancelled, completed, rolled_back) = (
        run_ids[0].1,
        run_ids[1].1,
        run_ids[2].1,
        run_ids[3].1,
        run_ids[4].1,
    );
    let t = thread_id.as_str();
    let items = vec![
        item(
            &id("live"),
            interrupted,
            "running",
            "subagent",
            t,
            current_provider,
        ),
        item(
            &id("failed"),
            failed,
            "running",
            "subagent",
            t,
            current_provider,
        ),
        item(
            &id("cancelled"),
            cancelled,
            "running",
            "subagent",
            t,
            current_provider,
        ),
        item(
            &id("completed-run"),
            completed,
            "running",
            "subagent",
            t,
            current_provider,
        ),
        item(
            &id("terminal"),
            interrupted,
            "completed",
            "subagent",
            t,
            current_provider,
        ),
        item(
            &id("ordinary"),
            interrupted,
            "running",
            "reasoning",
            t,
            current_provider,
        ),
        item(
            &id("rolled-back"),
            rolled_back,
            "running",
            "subagent",
            t,
            current_provider,
        ),
        item(
            &id("other-thread"),
            interrupted,
            "running",
            "subagent",
            other_thread,
            current_provider,
        ),
        item(
            &id("foreign-provider"),
            interrupted,
            "running",
            "subagent",
            t,
            foreign_provider,
        ),
    ];

    let selected = select_inherited_background_turn_items(
        &thread_id,
        &ProviderThreadId::new(current_provider).unwrap(),
        6,
        &runs,
        &items,
    );

    let route_of = |suffix: &str, run_id: &str| InheritedBackgroundTurnItemRoute {
        id: TurnItemId::new(id(suffix)).unwrap(),
        run_id: RunId::new(run_id).unwrap(),
    };
    assert_eq!(
        selected,
        vec![
            route_of("live", interrupted),
            route_of("failed", failed),
            route_of("cancelled", cancelled),
        ]
    );
}

#[test]
fn does_not_carry_interrupted_or_still_running_child_ownership_into_later_attempts() {
    assert!(!can_route_related_subagent(
        OrchestrationV2SubagentStatus::Interrupted
    ));
    assert!(!can_route_related_subagent(
        OrchestrationV2SubagentStatus::Failed
    ));
    assert!(!can_route_related_subagent(
        OrchestrationV2SubagentStatus::Cancelled
    ));
    assert!(can_route_related_subagent(
        OrchestrationV2SubagentStatus::Completed
    ));
    // The launching run still ingests a running subagent's child thread.
    assert!(!can_route_related_subagent(
        OrchestrationV2SubagentStatus::Running
    ));
}
