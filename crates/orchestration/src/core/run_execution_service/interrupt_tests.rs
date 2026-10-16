//! Ownership loss and the interrupt cascade. Upstream:
//! `RunExecutionService.test.ts` from "drops late root provider-thread writes
//! from a superseded attempt" through "cascade helper is provider-neutral for
//! Claude and Codex-shaped child projections". These run the whole event
//! loop: a root run over scripted events, then what it wrote and ingested.

use std::sync::{Arc, atomic::Ordering};

use serde_json::{Value, json};
use turin_contracts::{ProviderTurnId, ThreadId};
use turin_provider_core::provider_adapter::ProviderAdapterV2Event;

use super::test_support::{Fixture, Recorder, ScriptedSession, default_service, ev};
use crate::projection::test_support::turn_item;

const NOW: &str = "2026-07-21T12:00:00.000Z";

fn root_turn(key: &str) -> String {
    format!("provider-turn:{key}")
}

struct Run {
    recorder: Arc<Recorder>,
    session: Arc<ScriptedSession>,
}

/// Starts a root run over `events` and waits for the subscription to close.
/// With `keep_open` the stream never ends, so only the run's own stop decision
/// can release it.
async fn run_events(
    fixture: &Fixture,
    events: Vec<ProviderAdapterV2Event>,
    related_threads: &[&str],
    keep_open: bool,
    configure: impl FnOnce(&Arc<Recorder>, &Arc<ScriptedSession>),
) -> Run {
    let recorder = Recorder::new();
    let session = ScriptedSession::scripted(events, keep_open);
    configure(&recorder, &session);
    let mut input = fixture.input(session.clone(), "Run.");
    input.attempt.provider_turn_id =
        Some(ProviderTurnId::new(root_turn(&fixture.key)).expect("turn id"));
    input.related_thread_ids = related_threads
        .iter()
        .map(|id| ThreadId::new(*id).expect("thread id"))
        .collect();
    default_service(&recorder)
        .start_root_run(input)
        .await
        .expect("start");
    session.closed.wait().await;
    Run { recorder, session }
}

fn terminal(fixture: &Fixture, status: &str) -> ProviderAdapterV2Event {
    ev::terminal(
        &fixture.provider_thread_id(),
        &root_turn(&fixture.key),
        status,
    )
}

fn strings(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|label| (*label).to_owned()).collect()
}

fn wire(event: &turin_contracts::orchestration_v2::OrchestrationV2DomainEvent) -> Value {
    serde_json::to_value(event).expect("event serializes")
}

fn patched(mut base: Value, fields: Value) -> Value {
    for (key, field) in fields.as_object().expect("object") {
        base[key] = field.clone();
    }
    base
}

fn roster() -> Value {
    json!([{"taskId": "bg-stale", "description": "sleep 20", "kind": "command"}])
}

/// A background command item of the root run, on the root provider turn.
fn background_item(fixture: &Fixture, status: &str, ordinal: u64) -> ProviderAdapterV2Event {
    ev::turn_item(
        &fixture.thread_id(),
        &format!("turn-item:{}", fixture.key),
        Some(&fixture.run_id()),
        Some(&root_turn(&fixture.key)),
        ordinal,
        "command_execution",
        status,
    )
}

// --- The root provider thread's writes after the root turn ended ---

#[tokio::test]
async fn drops_late_root_provider_thread_writes_from_a_superseded_attempt() {
    let key = "bg-roster-attempt-owner-lost";
    let fixture = Fixture::new(key);
    let events = vec![
        ev::provider_thread(
            &fixture,
            &fixture.provider_thread_id(),
            &fixture.thread_id(),
            "active",
            roster(),
        ),
        terminal(&fixture, "completed"),
        // Late snapshot after a replacement attempt claimed the same run
        // ordinal. Without attempt gating this would clobber it.
        ev::provider_thread(
            &fixture,
            &fixture.provider_thread_id(),
            &fixture.thread_id(),
            "idle",
            roster(),
        ),
    ];

    let run = run_events(&fixture, events, &[], true, |recorder, session| {
        // The probe stays true forever, so only ownership loss can release the
        // stream. The sink rejects the owner-gated write: nothing is stored.
        session.has_background_probe.store(true, Ordering::SeqCst);
        session.pending_background.lock().extend([true; 32]);
        recorder.state.lock().ingest_hook = Some(Box::new(|_, guard| {
            usize::from(guard.provider_thread_owner.is_none())
        }));
    })
    .await;

    let state = run.recorder.state.lock();
    assert_eq!(
        state.log,
        strings(&[
            "provider_thread",
            "turn_terminal:completed",
            "root-finalized",
            "provider_thread"
        ])
    );
    let (_, pre_terminal) = &state.ingested[0];
    assert!(pre_terminal.run_current.is_some());
    assert!(pre_terminal.provider_thread_owner.is_none());
    let (_, late) = &state.ingested[2];
    let owner = late.provider_thread_owner.as_ref().expect("owner gate");
    assert_eq!(owner.expected_last_run_ordinal, 1);
    assert_eq!(owner.active_attempt_id.as_str(), fixture.attempt_id());
    assert_eq!(owner.run_id.as_str(), fixture.run_id());
    assert!(run.session.closed.is_closed());
}

#[tokio::test]
async fn keeps_ingesting_a_late_background_turn_item_completion_after_ownership_loss_rejects_roster_writes()
 {
    let key = "bg-item-after-owner-lost";
    let fixture = Fixture::new(key);
    let events = vec![
        background_item(&fixture, "running", 1),
        terminal(&fixture, "completed"),
        // Ownership reject after a newer run claimed lastRunOrdinal.
        ev::provider_thread(
            &fixture,
            &fixture.provider_thread_id(),
            &fixture.thread_id(),
            "idle",
            roster(),
        ),
        // Late completion still writable (turn item writes are not
        // ownership-gated); the stream must stay open for it.
        background_item(&fixture, "completed", 2),
    ];

    let run = run_events(&fixture, events, &[], true, |recorder, session| {
        // Probe stays true: open background items must pin the stream past
        // ownership loss, so the late completion still lands.
        session.has_background_probe.store(true, Ordering::SeqCst);
        session.pending_background.lock().extend([true; 32]);
        recorder.state.lock().ingest_hook = Some(Box::new(|_, guard| {
            usize::from(guard.provider_thread_owner.is_none())
        }));
    })
    .await;

    assert_eq!(
        run.recorder.state.lock().log,
        strings(&[
            "turn_item:running",
            "turn_terminal:completed",
            "root-finalized",
            "provider_thread",
            "turn_item:completed",
        ])
    );
}

#[tokio::test]
async fn does_not_pin_ingestion_on_a_sibling_session_wide_pending_state_when_this_thread_has_no_roster()
 {
    let key = "bg-roster-sibling-not-pin";
    let fixture = Fixture::new(key);
    let sibling = format!("provider-thread:{key}:sibling");
    let own = fixture.provider_thread_id();
    let events = vec![
        terminal(&fixture, "completed"),
        // A sibling thread's update after the terminal. This root's scoped
        // pending work is false, so it releases without waiting for
        // sibling-driven session-wide pending work.
        ev::provider_thread(
            &fixture,
            &sibling,
            &format!("thread:{key}:sibling"),
            "idle",
            json!([{"taskId": "sibling-bg", "description": "other thread", "kind": "command"}]),
        ),
    ];

    let run = run_events(&fixture, events, &[], true, |_, session| {
        session.has_background_probe.store(true, Ordering::SeqCst);
        // Own thread has no roster; the sibling would report true if probed.
        *session.probe_by_thread.lock() = Some(Box::new(move |thread| thread.id.as_str() != own));
    })
    .await;

    let state = run.recorder.state.lock();
    assert!(state.log.contains(&"turn_terminal:completed".to_owned()));
    assert!(state.log.contains(&"root-finalized".to_owned()));
    let probed: Vec<String> = run
        .session
        .probed
        .lock()
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();
    assert!(probed.contains(&fixture.provider_thread_id()), "{probed:?}");
    assert!(!probed.contains(&sibling), "{probed:?}");
}

// --- The interrupt cascade over run-owned subagent work ---

struct Ids {
    fixture: Fixture,
}

impl Ids {
    fn new(key: &str) -> Self {
        Self {
            fixture: Fixture::new(key),
        }
    }
    fn thread(&self) -> String {
        self.fixture.thread_id()
    }
    fn child_thread(&self) -> String {
        format!("{}:child", self.thread())
    }
    fn run(&self) -> String {
        self.fixture.run_id()
    }
    fn subagent_node(&self) -> String {
        format!("node:{}:subagent", self.fixture.key)
    }
    fn child_root(&self) -> String {
        format!("{}:child-root", self.subagent_node())
    }
    fn item(&self) -> String {
        format!("turn-item:{}", self.fixture.key)
    }
    fn child_item(&self) -> String {
        format!("turn-item:{}:child", self.fixture.key)
    }
}

fn subagent_row(ids: &Ids, status: &str) -> Value {
    patched(
        ev::subagent_json(
            &ids.subagent_node(),
            &ids.thread(),
            Some(&ids.run()),
            "provider_native",
            Some(&ids.child_thread()),
            status,
        ),
        json!({
            "parentNodeId": format!("node:{}", ids.fixture.key),
            "nativeTaskRef": {"driver": "codex", "nativeId": format!("task:{}", ids.subagent_node()), "strength": "strong"},
            "title": "Live-test subagent hold", "startedAt": NOW, "updatedAt": NOW,
        }),
    )
}

fn subagent_turn_item(ids: &Ids, status: &str) -> Value {
    let mut value = turn_item(
        &ids.thread(),
        &ids.item(),
        Some(&ids.run()),
        3,
        "subagent",
        status,
        json!({
            "subagentId": ids.subagent_node(), "origin": "provider_native", "driver": "codex",
            "providerInstanceId": "codex", "childThreadId": ids.child_thread(),
            "prompt": "hold", "progress": null, "result": null,
        }),
    );
    value["nodeId"] = json!(ids.subagent_node());
    value["providerThreadId"] = json!(ids.fixture.provider_thread_id());
    value["providerTurnId"] = json!(root_turn(&ids.fixture.key));
    value["nativeItemRef"] = json!({"driver": "codex", "nativeId": format!("task:{}", ids.subagent_node()), "strength": "strong"});
    value["title"] = json!("Live-test subagent hold");
    value["startedAt"] = json!(NOW);
    value["updatedAt"] = json!(NOW);
    value
}

fn node_row(
    id: &str,
    thread: &str,
    run: Option<&str>,
    kind: &str,
    status: &str,
    root: &str,
) -> Value {
    json!({
        "id": id, "threadId": thread, "runId": run, "parentNodeId": null, "rootNodeId": root,
        "kind": kind, "status": status, "countsForRun": false, "providerThreadId": null,
        "providerTurnId": null, "nativeItemRef": null, "runtimeRequestId": null,
        "checkpointScopeId": null, "startedAt": NOW, "completedAt": null,
    })
}

fn child_item(ids: &Ids, kind: &str) -> Value {
    let extra = match kind {
        "assistant_message" => {
            json!({"messageId": format!("{}:message", ids.child_item()), "text": "partial response", "streaming": true})
        }
        "reasoning" => json!({"text": "partial progress", "streaming": true}),
        _ => json!({"input": "sleep 300"}),
    };
    let mut value = turn_item(
        &ids.child_thread(),
        &ids.child_item(),
        None,
        1,
        kind,
        "running",
        extra,
    );
    value["nodeId"] = json!(ids.child_root());
    value["title"] = json!("Working");
    value["startedAt"] = json!(NOW);
    value["updatedAt"] = json!(NOW);
    value
}

fn event_of(kind: &str, key: &str, value: Value) -> ProviderAdapterV2Event {
    serde_json::from_value(json!({"type": kind, "driver": "codex", key: value})).expect("event")
}

fn subagent_event(value: Value) -> ProviderAdapterV2Event {
    event_of("subagent.updated", "subagent", value)
}
fn node_event(value: Value) -> ProviderAdapterV2Event {
    event_of("node.updated", "node", value)
}
fn item_event(value: Value) -> ProviderAdapterV2Event {
    event_of("turn_item.updated", "turn_item", value)
}

#[tokio::test]
async fn cascade_terminalizes_run_owned_subagent_rows_on_interrupt_before_root_finalization() {
    let ids = Ids::new("subagent-interrupt-cascade");
    let f = &ids.fixture;
    let unrelated_thread = "thread:subagent-interrupt-cascade:unrelated-child";
    let member_thread = "thread:subagent-interrupt-cascade:member";
    let foreign_run = "run:subagent-interrupt-cascade:foreign";

    let running_subagent = subagent_row(&ids, "running");
    let running_turn_item = subagent_turn_item(&ids, "running");
    let running_node = patched(
        node_row(
            &ids.subagent_node(),
            &ids.thread(),
            Some(&ids.run()),
            "subagent",
            "running",
            &format!("node:{}", f.key),
        ),
        json!({"parentNodeId": format!("node:{}", f.key), "providerThreadId": f.provider_thread_id(), "providerTurnId": root_turn(&f.key)}),
    );
    let running_child_node = node_row(
        &ids.child_root(),
        &ids.child_thread(),
        None,
        "root_turn",
        "running",
        &ids.child_root(),
    );
    let member_id = "node:subagent-interrupt-cascade:member";
    let running_member = patched(
        running_subagent.clone(),
        json!({"id": member_id, "threadId": ids.child_thread(), "runId": null, "parentNodeId": ids.child_root(), "childThreadId": member_thread}),
    );
    let foreign_subagent = patched(
        running_member.clone(),
        json!({"id": "node:subagent-interrupt-cascade:foreign-subagent", "runId": foreign_run, "childThreadId": unrelated_thread}),
    );
    let member_root_id = "node:subagent-interrupt-cascade:member-root";
    let running_member_root = patched(
        running_child_node.clone(),
        json!({"id": member_root_id, "threadId": member_thread, "rootNodeId": member_root_id}),
    );
    let foreign_child_node = patched(
        running_child_node.clone(),
        json!({"id": "node:subagent-interrupt-cascade:foreign-child", "runId": foreign_run}),
    );
    let running_child_item = child_item(&ids, "command_execution");
    let foreign_child_item = patched(
        running_child_item.clone(),
        json!({"id": "turn-item:subagent-interrupt-cascade:foreign-child", "runId": foreign_run}),
    );
    let suppressed_id = "turn-item:subagent-interrupt-cascade:suppressed-assistant";
    let suppressed_assistant = patched(
        child_item(&ids, "assistant_message"),
        json!({"id": suppressed_id}),
    );
    let unrelated_node_id = "node:subagent-interrupt-cascade:unrelated-child";
    let unrelated_child_node = patched(
        running_child_node.clone(),
        json!({"id": unrelated_node_id, "threadId": unrelated_thread, "runId": ids.run(), "rootNodeId": unrelated_node_id}),
    );
    let unrelated_child_item = patched(
        running_child_item.clone(),
        json!({"id": "turn-item:subagent-interrupt-cascade:unrelated-child", "threadId": unrelated_thread, "nodeId": unrelated_node_id}),
    );

    let events = vec![
        ev::app_thread_created(&ids.child_thread(), Some(&ids.thread()), &ids.thread()),
        subagent_event(running_subagent.clone()),
        node_event(running_node.clone()),
        node_event(running_child_node.clone()),
        node_event(unrelated_child_node.clone()),
        item_event(running_turn_item.clone()),
        item_event(running_child_item.clone()),
        item_event(suppressed_assistant.clone()),
        item_event(unrelated_child_item.clone()),
        subagent_event(running_member.clone()),
        subagent_event(foreign_subagent.clone()),
        node_event(running_member_root.clone()),
        node_event(foreign_child_node.clone()),
        item_event(foreign_child_item.clone()),
        terminal(f, "interrupted"),
        // Late provider completion after interrupt must not be ingested.
        subagent_event(patched(
            running_subagent.clone(),
            json!({"status": "completed", "result": "should-not-apply", "completedAt": NOW}),
        )),
        item_event(patched(
            running_turn_item.clone(),
            json!({"status": "completed", "completedAt": NOW}),
        )),
        item_event(patched(
            running_child_item.clone(),
            json!({"status": "completed", "completedAt": NOW}),
        )),
        node_event(patched(
            running_child_node.clone(),
            json!({"status": "completed", "completedAt": NOW}),
        )),
    ];

    let run = run_events(
        f,
        events,
        &[unrelated_thread, member_thread],
        false,
        |_, _| {},
    )
    .await;

    let written: Vec<Value> = run.recorder.events().iter().map(wire).collect();
    let id_of = |event: &Value| {
        event["payload"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let status_of = |event: &Value| {
        event["payload"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let kind_of = |event: &Value| event["type"].as_str().unwrap_or_default().to_owned();
    let find = |kind: &str, id: &str| {
        written
            .iter()
            .find(|e| kind_of(e) == kind && id_of(e) == id)
    };
    let any = |kind: &str, id: &str, status: Option<&str>| {
        written.iter().any(|e| {
            kind_of(e) == kind && id_of(e) == id && status.is_none_or(|s| status_of(e) == s)
        })
    };
    let run_updated_index = written
        .iter()
        .position(|e| kind_of(e) == "run.updated")
        .expect("root run.updated must be written");

    let subagent_events: Vec<&Value> = written
        .iter()
        .filter(|e| kind_of(e) == "subagent.updated")
        .collect();
    assert_eq!(subagent_events.len(), 2);
    let terminal_member = find("subagent.updated", member_id).expect("member cascaded");
    assert_eq!(status_of(terminal_member), "interrupted");
    assert_eq!(terminal_member["payload"]["runId"], Value::Null);
    assert_eq!(terminal_member["payload"]["childThreadId"], member_thread);
    assert!(any("node.updated", member_root_id, Some("interrupted")));
    assert!(!any(
        "subagent.updated",
        "node:subagent-interrupt-cascade:foreign-subagent",
        None
    ));
    assert!(!any(
        "node.updated",
        "node:subagent-interrupt-cascade:foreign-child",
        None
    ));
    assert!(!any(
        "turn-item.updated",
        "turn-item:subagent-interrupt-cascade:foreign-child",
        None
    ));
    let terminal_subagent =
        find("subagent.updated", &ids.subagent_node()).expect("subagent cascaded");
    assert_eq!(status_of(terminal_subagent), "interrupted");
    assert_eq!(
        terminal_subagent["payload"]["childThreadId"],
        ids.child_thread()
    );
    assert_eq!(terminal_subagent["payload"]["result"], Value::Null);
    assert!(!terminal_subagent["payload"]["completedAt"].is_null());

    let subagent_items: Vec<&Value> = written
        .iter()
        .filter(|e| kind_of(e) == "turn-item.updated" && e["payload"]["type"] == "subagent")
        .collect();
    assert_eq!(subagent_items.len(), 1);
    assert_eq!(status_of(subagent_items[0]), "interrupted");
    assert_eq!(
        subagent_items[0]["payload"]["childThreadId"],
        ids.child_thread()
    );
    assert_eq!(subagent_items[0]["payload"]["result"], Value::Null);

    let child_items: Vec<&Value> = written
        .iter()
        .filter(|e| {
            kind_of(e) == "turn-item.updated"
                && id_of(e) == ids.child_item()
                && status_of(e) == "interrupted"
        })
        .collect();
    assert_eq!(child_items.len(), 1);
    assert_eq!(child_items[0]["threadId"], ids.child_thread());
    assert_eq!(child_items[0]["payload"]["threadId"], ids.child_thread());
    assert_eq!(child_items[0]["payload"]["runId"], Value::Null);
    assert_eq!(child_items[0]["payload"]["type"], "command_execution");
    assert!(
        !any(
            "turn-item.updated",
            "turn-item:subagent-interrupt-cascade:unrelated-child",
            Some("interrupted")
        ),
        "related but unlinked child turn items must not cascade"
    );
    assert!(
        !any("turn-item.updated", suppressed_id, None),
        "suppressed streaming child items must not be created by the cascade"
    );

    let node_events: Vec<&Value> = written
        .iter()
        .filter(|e| {
            kind_of(e) == "node.updated"
                && status_of(e) == "interrupted"
                && (id_of(e) == ids.subagent_node() || id_of(e) == ids.child_root())
        })
        .collect();
    assert_eq!(node_events.len(), 2);
    assert!(
        node_events
            .iter()
            .any(|e| id_of(e) == ids.subagent_node() && e["payload"]["kind"] == "subagent")
    );
    assert!(
        !any("node.updated", unrelated_node_id, Some("interrupted")),
        "owned child threads without a live run-owned subagent link must not cascade"
    );
    let child_node = node_events
        .iter()
        .find(|e| id_of(e) == ids.child_root())
        .expect("child root");
    assert_eq!(child_node["runId"], ids.run());
    assert_eq!(child_node["payload"]["threadId"], ids.child_thread());
    assert_eq!(child_node["payload"]["kind"], "root_turn");

    for (index, event) in written.iter().enumerate() {
        let cascaded = (kind_of(event) == "subagent.updated" && status_of(event) == "interrupted")
            || (kind_of(event) == "turn-item.updated"
                && status_of(event) == "interrupted"
                && (event["payload"]["type"] == "subagent" || id_of(event) == ids.child_item()))
            || (kind_of(event) == "node.updated"
                && status_of(event) == "interrupted"
                && (id_of(event) == ids.subagent_node() || id_of(event) == ids.child_root()));
        if cascaded {
            assert!(
                index < run_updated_index,
                "subagent cascade must precede run.updated"
            );
        }
    }
    assert!(
        !written.iter().any(|e| {
            (kind_of(e) == "subagent.updated" && status_of(e) == "completed")
                || (kind_of(e) == "turn-item.updated"
                    && e["payload"]["type"] == "subagent"
                    && status_of(e) == "completed")
        }),
        "late provider completion must not reopen cascaded subagent rows"
    );
    let ingested = run.recorder.state.lock();
    assert!(
        !ingested.ingested.iter().any(|(input, _)| {
            let event = serde_json::to_value(&input.event).expect("event");
            match event["type"].as_str().unwrap_or_default() {
                "subagent.updated" => event["subagent"]["status"] == "completed",
                "turn_item.updated" => event["turn_item"]["status"] == "completed",
                "node.updated" => event["node"]["status"] == "completed",
                _ => false,
            }
        }),
        "late provider completion must not be ingested after interrupt"
    );
}

#[tokio::test]
async fn cascades_linked_child_thread_nodes_after_run_owned_subagent_and_turn_item_terminalize() {
    let ids = Ids::new("subagent-link-survives-terminal");
    let f = &ids.fixture;
    let unrelated_thread = "thread:subagent-link-survives-terminal:unrelated-child";
    let running_subagent = subagent_row(&ids, "running");
    let running_turn_item = subagent_turn_item(&ids, "running");
    let running_node = patched(
        node_row(
            &ids.subagent_node(),
            &ids.thread(),
            Some(&ids.run()),
            "subagent",
            "running",
            &format!("node:{}", f.key),
        ),
        json!({"parentNodeId": format!("node:{}", f.key)}),
    );
    let running_child_node = node_row(
        &ids.child_root(),
        &ids.child_thread(),
        None,
        "root_turn",
        "running",
        &ids.child_root(),
    );
    let unrelated_node_id = "node:subagent-link-survives-terminal:unrelated-child";
    let unrelated_child_node = patched(
        running_child_node.clone(),
        json!({"id": unrelated_node_id, "threadId": unrelated_thread, "runId": ids.run(), "rootNodeId": unrelated_node_id}),
    );

    let events = vec![
        ev::app_thread_created(&ids.child_thread(), Some(&ids.thread()), &ids.thread()),
        subagent_event(running_subagent.clone()),
        node_event(running_node.clone()),
        node_event(running_child_node.clone()),
        node_event(unrelated_child_node.clone()),
        item_event(running_turn_item.clone()),
        // The subagent and its turn item settle before the root interrupt;
        // the link must still prove the open child-thread node cascadeable.
        subagent_event(patched(
            running_subagent.clone(),
            json!({"status": "completed", "result": "subagent finished first", "completedAt": NOW}),
        )),
        item_event(patched(
            running_turn_item.clone(),
            json!({"status": "completed", "completedAt": NOW, "updatedAt": NOW}),
        )),
        node_event(patched(
            running_node.clone(),
            json!({"status": "completed", "completedAt": NOW}),
        )),
        terminal(f, "interrupted"),
    ];

    let run = run_events(f, events, &[unrelated_thread], false, |_, _| {}).await;

    let written: Vec<Value> = run.recorder.events().iter().map(wire).collect();
    let run_updated_index = written
        .iter()
        .position(|e| e["type"] == "run.updated")
        .expect("root run.updated must be written");
    let cascaded: Vec<(usize, &Value)> = written
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e["type"] == "node.updated"
                && e["payload"]["id"] == ids.child_root()
                && e["payload"]["status"] == "interrupted"
        })
        .collect();
    assert_eq!(
        cascaded.len(),
        1,
        "open linked child-thread node must cascade"
    );
    assert!(
        cascaded[0].0 < run_updated_index,
        "child-thread cascade must precede run.updated"
    );
    assert_eq!(cascaded[0].1["payload"]["threadId"], ids.child_thread());
    assert_eq!(cascaded[0].1["payload"]["kind"], "root_turn");
    assert!(
        !written.iter().any(|e| e["type"] == "node.updated"
            && e["payload"]["id"] == unrelated_node_id
            && e["payload"]["status"] == "interrupted"),
        "related but unlinked child threads must not cascade"
    );
    assert!(
        !written
            .iter()
            .any(|e| e["type"] == "subagent.updated" && e["payload"]["status"] == "interrupted"),
        "already-terminal subagent rows must not be re-cascaded"
    );
    assert!(
        !written.iter().any(|e| e["type"] == "turn-item.updated"
            && e["payload"]["type"] == "subagent"
            && e["payload"]["status"] == "interrupted"),
        "already-terminal subagent turn items must not be re-cascaded"
    );
}

// --- The cascade helper itself ---

#[test]
fn cascade_helper_is_provider_neutral_for_claude_and_codex_shaped_child_projections() {
    use turin_contracts::{
        EventId, NodeId, TurnItemId,
        orchestration_v2::{
            OrchestrationV2ExecutionNode, OrchestrationV2Run, OrchestrationV2Subagent,
            OrchestrationV2TurnItem,
        },
    };

    use super::{
        OpenRunOwnedSubagentProjection, RunOwnedSubagentTerminalStatus,
        cascade_terminalize_run_owned_subagents,
    };

    let now: turin_contracts::UtcDateTime = serde_json::from_value(json!(NOW)).unwrap();
    let mut next = 0;
    let mut allocate = || {
        next += 1;
        EventId::new(format!("event:cascade-helper:{next}")).unwrap()
    };
    for (driver, terminal_status) in [
        ("claudeAgent", RunOwnedSubagentTerminalStatus::Failed),
        ("codex", RunOwnedSubagentTerminalStatus::Cancelled),
    ] {
        let status_name = if driver == "claudeAgent" {
            "failed"
        } else {
            "cancelled"
        };
        let run_id = format!("run:cascade-helper:{driver}");
        let thread = format!("thread:cascade-helper:{driver}");
        let child_thread = format!("{thread}:child");
        let subagent_id = format!("node:cascade-helper:{driver}:subagent");
        let child_node_id = format!("node:cascade-helper:{driver}:child-root");
        let root = format!("node:cascade-helper:{driver}:root");
        let native =
            json!({"driver": driver, "nativeId": format!("native-{driver}"), "strength": "strong"});
        let run: OrchestrationV2Run = serde_json::from_value({
            let mut run = crate::projection::test_support::run(&thread, &run_id, 1, "running");
            run["providerInstanceId"] = json!(driver);
            run["modelSelection"] = json!({"instanceId": driver, "model": "m"});
            run
        })
        .unwrap();
        let subagent: OrchestrationV2Subagent = serde_json::from_value(json!({
            "id": subagent_id, "threadId": thread, "runId": run_id, "parentNodeId": root,
            "origin": "provider_native", "createdBy": "agent", "driver": driver,
            "providerInstanceId": driver, "providerThreadId": null, "childThreadId": child_thread,
            "nativeTaskRef": native, "prompt": "hold", "title": "hold", "model": null,
            "status": "running", "progress": "partial progress", "result": "partial result",
            "startedAt": NOW, "completedAt": null, "updatedAt": NOW,
        }))
        .unwrap();
        let mut item_json = turn_item(
            &thread,
            &format!("turn-item:cascade-helper:{driver}"),
            Some(&run_id),
            3,
            "subagent",
            "running",
            json!({
                "subagentId": subagent_id, "origin": "provider_native", "driver": driver,
                "providerInstanceId": driver, "childThreadId": child_thread, "prompt": "hold",
                "progress": "partial progress", "result": "partial result",
            }),
        );
        item_json["nodeId"] = json!(subagent_id);
        item_json["nativeItemRef"] = native.clone();
        item_json["title"] = json!("hold");
        let turn_item_row: OrchestrationV2TurnItem = serde_json::from_value(item_json).unwrap();
        let node: OrchestrationV2ExecutionNode = serde_json::from_value(patched(
            node_row(
                &subagent_id,
                &thread,
                Some(&run_id),
                "subagent",
                "running",
                &root,
            ),
            json!({"parentNodeId": root, "nativeItemRef": native}),
        ))
        .unwrap();
        let open_child_node: OrchestrationV2ExecutionNode = serde_json::from_value(node_row(
            &child_node_id,
            &child_thread,
            None,
            "root_turn",
            "running",
            &child_node_id,
        ))
        .unwrap();
        let child_item_json = if driver == "claudeAgent" {
            let mut value = turn_item(
                &child_thread,
                &format!("turn-item:cascade-helper:{driver}:child-reasoning"),
                None,
                1,
                "reasoning",
                "running",
                json!({"text": "partial progress", "streaming": true}),
            );
            value["nodeId"] = json!(child_node_id);
            value
        } else {
            let mut value = turn_item(
                &child_thread,
                &format!("turn-item:cascade-helper:{driver}:child-command"),
                None,
                1,
                "command_execution",
                "running",
                json!({"input": "sleep 300"}),
            );
            value["nodeId"] = json!(child_node_id);
            value
        };
        let child_item: OrchestrationV2TurnItem = serde_json::from_value(child_item_json).unwrap();
        let linked = turin_contracts::ThreadId::new(&child_thread).unwrap();

        let mut open = OpenRunOwnedSubagentProjection::default();
        open.subagents
            .insert(NodeId::new(&subagent_id).unwrap(), subagent);
        open.turn_items
            .insert(NodeId::new(&subagent_id).unwrap(), turn_item_row);
        open.nodes.insert(NodeId::new(&subagent_id).unwrap(), node);
        open.linked_child_thread_ids.insert(linked.clone());
        let events = cascade_terminalize_run_owned_subagents(
            &run,
            &open,
            terminal_status,
            &now,
            &mut allocate,
        )
        .unwrap();

        assert_eq!(events.len(), 3, "{driver}: subagent + node + turn item");
        let wires: Vec<Value> = events.iter().map(wire).collect();
        let subagent_event = wires
            .iter()
            .find(|e| e["type"] == "subagent.updated")
            .expect("subagent");
        assert_eq!(subagent_event["payload"]["status"], status_name);
        assert_eq!(subagent_event["payload"]["childThreadId"], child_thread);
        assert_eq!(subagent_event["payload"]["progress"], "partial progress");
        assert_eq!(subagent_event["payload"]["result"], "partial result");
        assert_eq!(subagent_event["payload"]["driver"], driver);
        let item_event = wires
            .iter()
            .find(|e| e["type"] == "turn-item.updated" && e["payload"]["type"] == "subagent")
            .expect("subagent item");
        assert_eq!(item_event["payload"]["status"], status_name);
        assert_eq!(item_event["payload"]["childThreadId"], child_thread);
        assert_eq!(item_event["payload"]["progress"], "partial progress");
        assert_eq!(item_event["payload"]["result"], "partial result");

        // Shared cascade path: after the subagent and turn item rows are gone,
        // only the preserved link may prove an open child-thread node cascadeable.
        let mut after = OpenRunOwnedSubagentProjection::default();
        after.child_turn_items.insert(
            TurnItemId::new(
                crate::projection::turn_item::common(&child_item)
                    .id
                    .as_str(),
            )
            .unwrap(),
            child_item.clone(),
        );
        after
            .nodes
            .insert(NodeId::new(&child_node_id).unwrap(), open_child_node);
        after.linked_child_thread_ids.insert(linked);
        let events = cascade_terminalize_run_owned_subagents(
            &run,
            &after,
            terminal_status,
            &now,
            &mut allocate,
        )
        .unwrap();

        assert_eq!(
            events.len(),
            2,
            "{driver}: linked child node and turn item cascade"
        );
        let wires: Vec<Value> = events.iter().map(wire).collect();
        let node_event = wires
            .iter()
            .find(|e| e["type"] == "node.updated")
            .expect("child node");
        assert_eq!(node_event["payload"]["id"], child_node_id);
        assert_eq!(node_event["payload"]["threadId"], child_thread);
        assert_eq!(node_event["payload"]["status"], status_name);
        assert_eq!(node_event["payload"]["kind"], "root_turn");
        let child_event = wires
            .iter()
            .find(|e| e["type"] == "turn-item.updated")
            .expect("child item");
        assert_eq!(child_event["threadId"], child_thread);
        assert_eq!(child_event["payload"]["status"], status_name);
        assert_eq!(child_event["payload"]["runId"], Value::Null);
        assert_eq!(
            child_event["payload"]["type"],
            serde_json::to_value(&child_item).unwrap()["type"]
        );
        if child_event["payload"]["type"] == "reasoning" {
            assert_eq!(child_event["payload"]["streaming"], false);
        }
    }
}
