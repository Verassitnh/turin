//! The provider event loop of `startRootRun`: which events a run ingests,
//! when it stops, and what its terminal write settles. Upstream:
//! `RunExecutionService.test.ts`, the background-item scenarios
//! (`runBackgroundItemScenario`) and the owned-child scenarios.

use std::sync::{Arc, atomic::Ordering};

use serde_json::{Value, json};
use turin_contracts::{ProviderTurnId, RunId, TurnItemId};
use turin_provider_core::provider_adapter::ProviderAdapterV2Event;

use super::{
    InheritedBackgroundTurnItemRoute,
    test_support::{Fixture, Recorder, ScriptedSession, default_service, ev},
};

fn root_turn(key: &str) -> String {
    format!("provider-turn:{key}")
}

#[derive(Default)]
struct Options {
    keep_open: bool,
    inherited: Option<Arc<dyn Fn() -> Vec<InheritedBackgroundTurnItemRoute> + Send + Sync>>,
    on_subscribe: Option<Box<dyn Fn() + Send>>,
    thread_probe: Option<Vec<bool>>,
}

struct Observed {
    recorder: Arc<Recorder>,
    session: Arc<ScriptedSession>,
}

impl Observed {
    /// The ingest log reduced to what the background scenarios compare.
    fn background_log(&self) -> Vec<String> {
        self.recorder
            .state
            .lock()
            .log
            .iter()
            .filter(|label| {
                label.starts_with("turn_item:")
                    || label.starts_with("subagent:")
                    || *label == "root-finalized"
            })
            .cloned()
            .collect()
    }
}

/// `runBackgroundItemScenario`: starts a root run over `events`, waits for the
/// event subscription to close, and returns what was ingested.
async fn scenario(
    key: &str,
    events: impl FnOnce(&Fixture) -> Vec<ProviderAdapterV2Event>,
    options: Options,
) -> Observed {
    let recorder = Recorder::new();
    let fixture = Fixture::new(key);
    let session = ScriptedSession::scripted(events(&fixture), options.keep_open);
    if let Some(hook) = options.on_subscribe {
        *session.on_subscribe.lock() = Some(hook);
    }
    if let Some(answers) = options.thread_probe {
        session.has_background_probe.store(true, Ordering::SeqCst);
        session.pending_background.lock().extend(answers);
    }
    let mut input = fixture.input(session.clone(), "Start background work and settle.");
    input.attempt.provider_turn_id = Some(ProviderTurnId::new(root_turn(key)).expect("turn id"));
    if let Some(load) = options.inherited {
        input.load_inherited_background_turn_items = Some(Arc::new(move || {
            let routes = load();
            Box::pin(async move { Ok(routes) })
        }));
    }
    default_service(&recorder)
        .start_root_run(input)
        .await
        .expect("start");
    session.closed.wait().await;
    Observed { recorder, session }
}

fn root_terminal(fixture: &Fixture, status: &str) -> ProviderAdapterV2Event {
    ev::terminal(
        &fixture.provider_thread_id(),
        &root_turn(&fixture.key),
        status,
    )
}

fn root_item(
    fixture: &Fixture,
    kind: &str,
    status: &str,
    ordinal: u64,
    item_id: Option<&str>,
) -> ProviderAdapterV2Event {
    ev::turn_item(
        &fixture.thread_id(),
        item_id.unwrap_or(&format!("turn-item:{}", fixture.key)),
        Some(&fixture.run_id()),
        Some(&root_turn(&fixture.key)),
        ordinal,
        kind,
        status,
    )
}

fn root_subagent(fixture: &Fixture, status: &str) -> ProviderAdapterV2Event {
    ev::subagent(ev::subagent_json(
        &format!("node:{}:subagent", fixture.key),
        &fixture.thread_id(),
        Some(&fixture.run_id()),
        "provider_native",
        None,
        status,
    ))
}

fn child_item(fixture: &Fixture, status: &str, ordinal: u64) -> ProviderAdapterV2Event {
    ev::turn_item(
        &format!("{}:child", fixture.thread_id()),
        &format!("turn-item:{}:child", fixture.key),
        None,
        None,
        ordinal,
        "command_execution",
        status,
    )
}

fn child_thread_created(fixture: &Fixture) -> ProviderAdapterV2Event {
    ev::app_thread_created(
        &format!("{}:child", fixture.thread_id()),
        Some(&fixture.thread_id()),
        &fixture.thread_id(),
    )
}

fn strings(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|label| (*label).to_owned()).collect()
}

#[tokio::test]
async fn keeps_ingesting_a_late_background_command_item_completion_after_root_terminal() {
    let observed = scenario(
        "bg-command",
        |f| {
            vec![
                root_item(f, "command_execution", "running", 1, None),
                root_terminal(f, "completed"),
                root_item(f, "command_execution", "completed", 2, None),
            ]
        },
        Options::default(),
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&["turn_item:running", "root-finalized", "turn_item:completed"])
    );
}

#[tokio::test]
async fn ingests_the_trailing_subagent_item_completion_after_the_subagent_row_completes() {
    let observed = scenario(
        "subagent-trailing-item",
        |f| {
            vec![
                root_subagent(f, "running"),
                root_item(f, "subagent", "running", 1, None),
                root_terminal(f, "completed"),
                root_subagent(f, "completed"),
                root_item(f, "subagent", "completed", 2, None),
            ]
        },
        Options::default(),
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&[
            "subagent:running",
            "turn_item:running",
            "root-finalized",
            "subagent:completed",
            "turn_item:completed",
        ])
    );
}

#[tokio::test]
async fn keeps_ingesting_a_child_threads_late_background_item_completion() {
    let observed = scenario(
        "bg-child-item",
        |f| {
            vec![
                child_thread_created(f),
                root_subagent(f, "running"),
                child_item(f, "running", 1),
                root_terminal(f, "completed"),
                root_subagent(f, "completed"),
                child_item(f, "completed", 2),
            ]
        },
        Options::default(),
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&[
            "subagent:running",
            "turn_item:running",
            "root-finalized",
            "subagent:completed",
            "turn_item:completed",
        ])
    );
}

#[tokio::test]
async fn keeps_ingesting_until_the_last_of_several_background_items_terminalizes() {
    let observed = scenario(
        "bg-multi",
        |f| {
            let second = "turn-item:bg-multi:second";
            vec![
                root_item(f, "command_execution", "running", 1, None),
                root_item(f, "dynamic_tool", "running", 2, Some(second)),
                root_terminal(f, "completed"),
                root_item(f, "command_execution", "completed", 3, None),
                root_item(f, "dynamic_tool", "completed", 4, Some(second)),
            ]
        },
        Options::default(),
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&[
            "turn_item:running",
            "turn_item:running",
            "root-finalized",
            "turn_item:completed",
            "turn_item:completed",
        ])
    );
}

#[tokio::test]
async fn does_not_pin_ingestion_on_background_items_when_the_root_turn_is_interrupted() {
    let observed = scenario(
        "bg-interrupted",
        |f| {
            vec![
                root_item(f, "command_execution", "running", 1, None),
                root_terminal(f, "interrupted"),
                root_item(f, "command_execution", "completed", 2, None),
            ]
        },
        Options::default(),
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&["turn_item:running", "root-finalized"])
    );
}

fn inherited_from(
    key: &str,
) -> Arc<dyn Fn() -> Vec<InheritedBackgroundTurnItemRoute> + Send + Sync> {
    let item = TurnItemId::new(format!("turn-item:{key}")).expect("item id");
    let run = RunId::new(format!("run:{key}:prior")).expect("run id");
    Arc::new(move || {
        vec![InheritedBackgroundTurnItemRoute {
            id: item.clone(),
            run_id: run.clone(),
        }]
    })
}

fn prior_run_item(fixture: &Fixture, status: &str, ordinal: u64) -> ProviderAdapterV2Event {
    ev::turn_item(
        &fixture.thread_id(),
        &format!("turn-item:{}", fixture.key),
        Some(&format!("run:{}:prior", fixture.key)),
        Some(&root_turn(&fixture.key)),
        ordinal,
        "subagent",
        status,
    )
}

#[tokio::test]
async fn seeds_inherited_background_items_before_their_next_update() {
    let key = "inherited-background-seeded";
    let observed = scenario(
        key,
        |f| {
            vec![
                root_terminal(f, "completed"),
                prior_run_item(f, "completed", 1),
            ]
        },
        Options {
            inherited: Some(inherited_from(key)),
            ..Options::default()
        },
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&["root-finalized", "turn_item:completed"])
    );
}

#[tokio::test]
async fn releases_the_live_run_after_an_inherited_background_item_terminalizes() {
    let key = "inherited-background-terminal";
    let observed = scenario(
        key,
        |f| {
            vec![
                prior_run_item(f, "running", 1),
                root_terminal(f, "completed"),
                prior_run_item(f, "completed", 2),
            ]
        },
        Options {
            inherited: Some(inherited_from(key)),
            ..Options::default()
        },
    )
    .await;
    assert_eq!(
        observed.background_log(),
        strings(&["turn_item:running", "root-finalized", "turn_item:completed"])
    );
}

fn prior_run_and_item(
    key: &str,
    item_provider_thread: &str,
    item_status: &str,
) -> (
    Vec<turin_contracts::orchestration_v2::OrchestrationV2Run>,
    Vec<turin_contracts::orchestration_v2::OrchestrationV2TurnItem>,
) {
    let fixture = Fixture::new(key);
    let mut run = fixture.run_json();
    run["id"] = json!(format!("run:{key}:prior"));
    run["status"] = json!("interrupted");
    let mut item = crate::projection::test_support::turn_item(
        &fixture.thread_id(),
        &format!("turn-item:{key}"),
        Some(&format!("run:{key}:prior")),
        1,
        "subagent",
        item_status,
        json!({
            "subagentId": "node:bg:subagent", "origin": "provider_native", "driver": "codex",
            "providerInstanceId": "codex", "childThreadId": null, "prompt": "hold",
            "progress": null, "result": null,
        }),
    );
    item["providerThreadId"] = json!(item_provider_thread);
    (
        vec![serde_json::from_value(run).expect("run")],
        vec![serde_json::from_value(item).expect("item")],
    )
}

#[tokio::test]
async fn does_not_hold_the_live_stream_open_for_a_foreign_providers_background_item() {
    let key = "inherited-background-foreign-provider";
    let fixture = Fixture::new(key);
    let (runs, items) = prior_run_and_item(
        key,
        &format!("provider-thread:{key}:foreign-session"),
        "running",
    );
    let inherited = super::select_inherited_background_turn_items(
        &turin_contracts::ThreadId::new(fixture.thread_id()).expect("thread id"),
        &turin_contracts::ProviderThreadId::new(fixture.provider_thread_id())
            .expect("provider thread id"),
        2,
        &runs,
        &items,
    );
    assert!(inherited.is_empty());

    let observed = scenario(
        key,
        |f| vec![root_terminal(f, "completed")],
        Options {
            keep_open: true,
            inherited: Some(Arc::new(move || inherited.clone())),
            ..Options::default()
        },
    )
    .await;
    assert_eq!(observed.background_log(), strings(&["root-finalized"]));
}

#[tokio::test]
async fn refreshes_inherited_background_items_after_event_subscription() {
    let key = "inherited-background-subscription-refresh";
    let fixture = Fixture::new(key);
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let load = {
        let completed = completed.clone();
        move || {
            let status = if completed.load(Ordering::SeqCst) {
                "completed"
            } else {
                "running"
            };
            let (runs, items) =
                prior_run_and_item(key, &Fixture::new(key).provider_thread_id(), status);
            super::select_inherited_background_turn_items(
                &turin_contracts::ThreadId::new(Fixture::new(key).thread_id()).expect("thread id"),
                &turin_contracts::ProviderThreadId::new(Fixture::new(key).provider_thread_id())
                    .expect("provider thread id"),
                2,
                &runs,
                &items,
            )
        }
    };
    let observed = scenario(
        key,
        |f| vec![root_terminal(f, "completed")],
        Options {
            keep_open: true,
            inherited: Some(Arc::new(load.clone())),
            on_subscribe: Some(Box::new({
                let completed = completed.clone();
                move || completed.store(true, Ordering::SeqCst)
            })),
            ..Options::default()
        },
    )
    .await;

    // The item settled between the first read and the subscription, so the
    // second read drops it and the run releases after its terminal.
    assert!(load().is_empty());
    assert_eq!(observed.background_log(), strings(&["root-finalized"]));
    drop(fixture);
}

#[tokio::test]
async fn ingests_new_member_threads_of_an_owned_child_after_the_root_turn_terminalizes() {
    let key = "run-execution-late-child";
    let observed = scenario(
        key,
        |f| {
            let thread = f.thread_id();
            let child = format!("{thread}:child");
            let member = format!("{thread}:member");
            let child_provider_thread = format!("{}:child", f.provider_thread_id());
            let child_turn = format!("provider-turn:{key}:child");
            let child_node = format!("node:{key}:child");
            let mut coordinator = ev::subagent_json(
                &format!("node:{key}:subagent"),
                &thread,
                Some(&f.run_id()),
                "provider_native",
                Some(&child),
                "running",
            );
            coordinator["workflow"] = json!({
                "phases": [],
                "agents": [{"index": 0, "label": "Reviewer", "state": "running", "childThreadId": member}],
            });
            vec![
                ev::app_thread_created(&child, Some(&thread), &thread),
                ev::provider_thread(f, &child_provider_thread, &child, "active", json!([])),
                ev::subagent(coordinator),
                ev::provider_turn(&child_turn, &child_provider_thread, &child_node, Some(&child), "running"),
                root_terminal(f, "completed"),
                ev::app_thread_created(&member, Some(&child), &thread),
                ev::message(&format!("message:{key}:child"), &member, &child_node, "Hello."),
                ev::provider_turn(&child_turn, &child_provider_thread, &child_node, Some(&child), "completed"),
                ev::subagent(ev::subagent_json(
                    &format!("node:{key}:subagent"),
                    &thread,
                    Some(&f.run_id()),
                    "provider_native",
                    None,
                    "completed",
                )),
            ]
        },
        Options::default(),
    )
    .await;
    let order: Vec<String> = observed
        .recorder
        .state
        .lock()
        .log
        .iter()
        .filter(|label| {
            *label == "root-finalized"
                || *label == "app_thread:thread:run-execution-late-child:member"
                || *label == "message:thread:run-execution-late-child:member"
        })
        .cloned()
        .collect();
    assert_eq!(
        order,
        strings(&[
            "root-finalized",
            "app_thread:thread:run-execution-late-child:member",
            "message:thread:run-execution-late-child:member",
        ])
    );
}

#[tokio::test]
async fn ends_a_nested_native_subagent_and_its_thread_when_the_root_run_is_interrupted() {
    let key = "run-execution-nested-native";
    let observed = scenario(
        key,
        |f| {
            let thread = f.thread_id();
            let child = format!("{thread}:child");
            let grandchild = format!("{thread}:grandchild");
            vec![
                ev::app_thread_created(&child, Some(&thread), &thread),
                ev::app_thread_created(&grandchild, Some(&child), &thread),
                ev::subagent(ev::subagent_json(
                    &format!("node:{key}:nested"),
                    &child,
                    None,
                    "provider_native",
                    Some(&grandchild),
                    "running",
                )),
                ev::node(
                    &format!("node:{key}:grandchild-root"),
                    &grandchild,
                    None,
                    "root_turn",
                    "running",
                ),
                root_terminal(f, "interrupted"),
            ]
        },
        Options::default(),
    )
    .await;
    let ended: Vec<(String, String, String)> = observed
        .recorder
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).expect("event"))
        .filter(|event| event["type"] == "subagent.updated" || event["type"] == "node.updated")
        .map(|event| {
            (
                event["type"].as_str().unwrap_or_default().to_owned(),
                event["payload"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                event["payload"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    let has = |kind: &str, id: &str| {
        ended
            .iter()
            .any(|(k, i, s)| k == kind && i == id && s == "interrupted")
    };
    assert!(
        has("subagent.updated", &format!("node:{key}:nested")),
        "{ended:?}"
    );
    assert!(
        has("node.updated", &format!("node:{key}:grandchild-root")),
        "{ended:?}"
    );
    drop(observed.session);
}

fn roster_task() -> Value {
    json!([{"taskId": "bg-1", "description": "sleep 20", "kind": "command"}])
}

#[tokio::test]
async fn keeps_ingesting_a_late_empty_provider_thread_roster_while_thread_scoped_pending_work_is_true()
 {
    let key = "bg-roster-pending-work";
    let recorder = Recorder::new();
    let fixture = Fixture::new(key);
    // The late empty roster clears the thread's pending work.
    recorder.state.lock().ingest_hook = Some(Box::new(|input, _| {
        let _ = input;
        1
    }));
    let session = ScriptedSession::scripted(
        vec![
            ev::provider_thread(
                &fixture,
                &fixture.provider_thread_id(),
                &fixture.thread_id(),
                "active",
                roster_task(),
            ),
            root_terminal(&fixture, "completed"),
            ev::provider_thread(
                &fixture,
                &fixture.provider_thread_id(),
                &fixture.thread_id(),
                "idle",
                json!([]),
            ),
        ],
        false,
    );
    session.has_background_probe.store(true, Ordering::SeqCst);
    // Pending while the roster is non-empty; the clear releases it.
    session.pending_background.lock().extend([true, false]);
    let mut input = fixture.input(session.clone(), "Start background work and settle.");
    input.attempt.provider_turn_id = Some(ProviderTurnId::new(root_turn(key)).expect("turn id"));
    default_service(&recorder)
        .start_root_run(input)
        .await
        .expect("start");
    session.closed.wait().await;

    let state = recorder.state.lock();
    assert_eq!(
        state.log,
        strings(&[
            "provider_thread",
            "turn_terminal:completed",
            "root-finalized",
            "provider_thread"
        ])
    );
    let roster_update = &state.ingested[0];
    assert!(roster_update.1.run_current.is_some());
    assert!(roster_update.1.provider_thread_owner.is_none());
    let late_clear = &state.ingested[2];
    // A late empty roster must not use the stale running-gate; it gates on
    // provider-thread ownership.
    assert!(late_clear.1.run_current.is_none());
    let owner = late_clear
        .1
        .provider_thread_owner
        .as_ref()
        .expect("owner gate");
    assert_eq!(owner.expected_last_run_ordinal, 1);
    assert_eq!(owner.run_id.as_str(), fixture.run_id());
    assert_eq!(owner.active_attempt_id.as_str(), fixture.attempt_id());
}
