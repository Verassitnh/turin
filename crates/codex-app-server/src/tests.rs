//! Ports of protocol.test.ts, client.test.ts, schema.test.ts and replay.ts logic.
use crate::{
    errors::{AppServerError, ProtocolError},
    generated::meta,
    protocol::Protocol,
    replay::{ReplayEntry, ReplayTranscript, make_replay_client},
    schema,
};
use futures::FutureExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::oneshot,
    time::{Duration, timeout},
};

fn pair() -> (
    Arc<Protocol>,
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
) {
    let (reader, peer_writer) = tokio::io::duplex(8 * 1024 * 1024);
    let (peer_reader, writer) = tokio::io::duplex(8 * 1024 * 1024);
    (
        Arc::new(Protocol::new(reader, writer)),
        peer_reader,
        peer_writer,
    )
}
async fn write_frame(peer: &mut tokio::io::DuplexStream, value: Value) {
    peer.write_all(format!("{}\n", value).as_bytes())
        .await
        .unwrap();
}
#[tokio::test]
async fn protocol_routes_while_request_pending() {
    let (protocol, peer_reader, mut peer_writer) = pair();
    protocol.notify("initialized", None).unwrap();
    let mut outbound = BufReader::new(peer_reader).lines();
    assert_eq!(
        serde_json::from_str::<Value>(&outbound.next_line().await.unwrap().unwrap()).unwrap(),
        json!({"method":"initialized"})
    );
    let (approval_tx, approval_rx) = oneshot::channel::<()>();
    let approval_rx = std::sync::Mutex::new(Some(approval_rx));
    protocol.set_request_handler(Arc::new(move |method, _| {
        assert_eq!(method, "approval/request");
        let rx = approval_rx.lock().unwrap().take().unwrap();
        async move {
            let _ = rx.await;
            Ok(Some(json!({"decision":"accept"})))
        }
        .boxed()
    }));
    let pending = {
        let protocol = protocol.clone();
        tokio::spawn(async move {
            protocol
                .request("initialize", Some(json!({"clientInfo":{"name":"test"}})))
                .await
        })
    };
    let outbound_request: Value =
        serde_json::from_str(&outbound.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        outbound_request,
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"test"}}})
    );
    write_frame(
        &mut peer_writer,
        json!({"id":77,"method":"approval/request","params":{}}),
    )
    .await;
    write_frame(
        &mut peer_writer,
        json!({"method":"event","params":{"delta":"ok"}}),
    )
    .await;
    write_frame(&mut peer_writer, json!({"id":1,"result":{"ok":true}})).await;
    assert_eq!(pending.await.unwrap().unwrap(), Some(json!({"ok":true})));
    assert_eq!(protocol.next_request().await.unwrap().id, Some(json!(77)));
    assert_eq!(
        protocol.next_notification().await.unwrap().params,
        Some(json!({"delta":"ok"}))
    );
    approval_tx.send(()).unwrap();
    let approval: Value =
        serde_json::from_str(&outbound.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(approval, json!({"id":77,"result":{"decision":"accept"}}));
    protocol
        .respond_error(json!(78), ProtocolError::method_not_found("x/test"))
        .unwrap();
    let error: Value = serde_json::from_str(&outbound.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        error,
        json!({"id":78,"error":{"code":-32601,"message":"Method not found: x/test"}})
    );
}
#[tokio::test]
async fn fragmented_utf8_and_final_line() {
    let (protocol, _peer_reader, mut peer_writer) = pair();
    let frames = "\n \t\r\n{\"method\":\"first\",\"params\":{\"text\":\"hé🙂\"}}\r\n{\"method\":\"final\",\"params\":{\"text\":\"最後\"}}";
    for byte in frames.as_bytes() {
        peer_writer.write_all(&[*byte]).await.unwrap();
    }
    drop(peer_writer);
    assert_eq!(
        protocol.next_notification().await.unwrap().params,
        Some(json!({"text":"hé🙂"}))
    );
    assert_eq!(
        protocol.next_notification().await.unwrap().params,
        Some(json!({"text":"最後"}))
    );
}
#[tokio::test]
async fn malformed_final_line_fails_pending() {
    let (protocol, peer_reader, mut peer_writer) = pair();
    let pending = {
        let protocol = protocol.clone();
        tokio::spawn(async move { protocol.request("thread/read", Some(json!({}))).await })
    };
    let mut lines = BufReader::new(peer_reader).lines();
    lines.next_line().await.unwrap().unwrap();
    peer_writer
        .write_all(b"{\"id\":1,\"result\":")
        .await
        .unwrap();
    drop(peer_writer);
    let error = pending.await.unwrap().unwrap_err();
    assert!(matches!(
        error,
        AppServerError::ProtocolParse(info) if info.operation == "decode-wire-message"
    ));
}
#[tokio::test]
async fn notifications_slide_to_last_32() {
    let (protocol, _peer_reader, mut peer_writer) = pair();
    for index in 0..64 {
        write_frame(
            &mut peer_writer,
            json!({"method":"event","params":{"index":index}}),
        )
        .await;
    }
    // Wait until the reader has consumed all input without relying on a timer.
    write_frame(&mut peer_writer, json!({"id":1,"result":{}})).await;
    for _ in 0..100 {
        if protocol.notifications_len_for_test() == 32
            && protocol.last_notification_index_for_test() == Some(63)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let mut indices = Vec::new();
    for _ in 0..32 {
        indices.push(
            protocol.next_notification().await.unwrap().params.unwrap()["index"]
                .as_u64()
                .unwrap(),
        );
    }
    assert_eq!(indices, (32..64).collect::<Vec<_>>());
}
#[tokio::test]
async fn schema_and_method_mapping() {
    assert_eq!(
        meta::method_meta(meta::CLIENT_REQUEST_METHODS, "account/usage/read")
            .unwrap()
            .response,
        Some("V2GetAccountTokenUsageResponse")
    );
    assert!(schema::validate_type("V2GetAccountTokenUsageResponse",&json!({"dailyUsageBuckets":[{"startDate":"2026-06-10","tokens":42}],"summary":{"lifetimeTokens":42}})).is_ok());
    assert!(schema::validate_type("V2ConsumeAccountRateLimitResetCreditParams",&json!({"idempotencyKey":"8ae96ff3-3425-4f4c-8772-b6fd61502868","creditId":"RateLimitResetCredit_1"})).is_ok());
    assert!(schema::validate_type("V2GetAccountResponse",&json!({"account":{"email":"user@example.com","planType":"some_future_plan","type":"chatgpt"},"requiresOpenaiAuth":true})).is_ok());
}
#[tokio::test]
async fn replay_matches_outbound_and_emits_response() {
    let transcript = ReplayTranscript {
        provider: "codex".into(),
        protocol: "codex.app-server".into(),
        version: "1".into(),
        scenario: "initialize".into(),
        metadata: None,
        entries: vec![
            ReplayEntry::ExpectOutbound {
                label: None,
                frame: json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"test","version":"0.1"}}}),
            },
            ReplayEntry::EmitInbound {
                label: None,
                frame: json!({"id":1,"result":{"userAgent":"mock"}}),
                after_ms: None,
            },
            ReplayEntry::RuntimeExit {
                status: crate::replay::ExitStatus::Success,
                error: None,
            },
        ],
    };
    let (client, driver) = make_replay_client(transcript);
    assert_eq!(
        client
            .raw
            .request(
                "initialize",
                Some(json!({"clientInfo":{"name":"test","version":"1.2"}}))
            )
            .await
            .unwrap(),
        Some(json!({"userAgent":"mock"}))
    );
    timeout(Duration::from_secs(1), async {
        loop {
            if driver.state.lock().cursor == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    driver.verify_complete().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn child_exit_fails_pending_with_exit_code() {
    let client = crate::client::Client::spawn("/bin/sh", &["-c", "exit 7"], None).unwrap();
    let error = timeout(Duration::from_secs(2), client.raw.request("x/test", None))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, AppServerError::ProcessExited(Some(7))));
}

#[tokio::test]
async fn typed_client_handles_approval_and_notification_during_request() {
    let (client_read, mut peer_writer) = tokio::io::duplex(1024 * 1024);
    let (peer_reader, client_write) = tokio::io::duplex(1024 * 1024);
    let client = Arc::new(crate::client::Client::from_io(client_read, client_write));
    client.handle_server_request("item/tool/requestUserInput", |_params| async {
        Ok(Some(json!({"answers":{"approved":{"answers":["yes"]}}})))
    });
    let (event_tx, event_rx) = oneshot::channel();
    let event_tx = std::sync::Mutex::new(Some(event_tx));
    client.handle_server_notification("item/agentMessage/delta", move |params| {
        let tx = event_tx.lock().unwrap().take();
        async move {
            if let Some(tx) = tx {
                let _ = tx.send(params);
            }
            Ok(())
        }
    });
    let mut lines = BufReader::new(peer_reader).lines();
    let initialize = {
        let client = client.clone();
        tokio::spawn(async move {
            client.request("initialize",Some(json!({"clientInfo":{"name":"test","title":"Test","version":"0.1"},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":null}}))).await
        })
    };
    let initial: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(initial["method"], "initialize");
    write_frame(&mut peer_writer,json!({"id":1,"result":{"userAgent":"mock","codexHome":"/tmp","platformFamily":"unix","platformOs":"macos"}})).await;
    assert_eq!(
        initialize.await.unwrap().unwrap().unwrap()["userAgent"],
        "mock"
    );
    client.notify("initialized", None).unwrap();
    let initialized: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(initialized, json!({"method":"initialized"}));
    let account = {
        let client = client.clone();
        tokio::spawn(async move { client.request("account/read", Some(json!({}))).await })
    };
    let account_request: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(account_request["method"], "account/read");
    write_frame(&mut peer_writer,json!({"method":"item/agentMessage/delta","params":{"delta":"Ready","itemId":"item-1","threadId":"thread-1","turnId":"turn-1"}})).await;
    write_frame(&mut peer_writer,json!({"id":77,"method":"item/tool/requestUserInput","params":{"isBlocking":true,"itemId":"approval-1","threadId":"thread-1","turnId":"turn-1","questions":[{"id":"approved","header":"Approve","question":"Continue?"}]}})).await;
    assert_eq!(
        timeout(Duration::from_secs(1), event_rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap()["delta"],
        "Ready"
    );
    let approval: Value = serde_json::from_str(
        &timeout(Duration::from_secs(1), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        approval,
        json!({"id":77,"result":{"answers":{"approved":{"answers":["yes"]}}}})
    );
    write_frame(&mut peer_writer,json!({"id":2,"result":{"account":{"type":"chatgpt","email":"mock@example.com","planType":"plus"},"requiresOpenaiAuth":false}})).await;
    assert_eq!(
        account.await.unwrap().unwrap().unwrap()["requiresOpenaiAuth"],
        false
    );
}

#[tokio::test]
async fn decode_failure_logs_only_structural_diagnostics() {
    use crate::protocol::{Direction, ProtocolOptions, Stage};
    let (reader, mut peer_writer) = tokio::io::duplex(1024);
    let (_peer_reader, writer) = tokio::io::duplex(1024);
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let logged = events.clone();
    let (failure_tx, failure_rx) = oneshot::channel();
    let failure_tx = std::sync::Mutex::new(Some(failure_tx));
    let protocol = Protocol::new_with_options(
        reader,
        writer,
        ProtocolOptions {
            log_incoming: true,
            logger: Some(Arc::new(move |event| logged.lock().push(event))),
            on_termination: Some(Arc::new(move |error| {
                if let Some(tx) = failure_tx.lock().unwrap().take() {
                    let _ = tx.send(error);
                }
            })),
            ..ProtocolOptions::default()
        },
    );
    peer_writer
        .write_all(b"{\"secret\":\"do not copy\",")
        .await
        .unwrap();
    drop(peer_writer);
    let error = timeout(Duration::from_secs(1), failure_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        error,
        AppServerError::ProtocolParse(info) if info.operation == "decode-wire-message"
    ));
    let events = events.lock();
    assert_eq!(events.len(), 2);
    assert_eq!(
        (events[1].direction, events[1].stage),
        (Direction::Incoming, Stage::DecodeFailed)
    );
    assert_eq!(
        events[1].payload,
        json!({"operation":"decode-wire-message"})
    );
    drop(protocol);
}

#[tokio::test]
async fn replay_mismatch_fails_client_and_driver() {
    let transcript = ReplayTranscript {
        provider: "codex".into(),
        protocol: "codex.app-server".into(),
        version: "1".into(),
        scenario: "mismatch".into(),
        metadata: None,
        entries: vec![ReplayEntry::ExpectOutbound {
            label: None,
            frame: json!({"id":1,"method":"expected"}),
        }],
    };
    let (client, driver) = make_replay_client(transcript);
    let error = timeout(Duration::from_secs(1), client.raw.request("actual", None))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error,
        AppServerError::Transport {
            operation: "read-input-stream",
            ..
        }
    ));
    assert!(matches!(
        driver.verify_complete(),
        Err(crate::replay::ReplayError::FrameMismatch { .. })
    ));
}

#[test]
fn schema_keeps_async_questions_and_new_agent_values() {
    let item = json!({"type":"agentMessage","id":"question-1","text":"Which package?","phase":"final_answer","delivery":"async","questions":[{"title":"Which package manager?","options":["pnpm","npm"]},{"title":"What should it be named?"}]});
    for name in [
        "ServerNotification__ThreadItem",
        "V2ItemStartedNotification__ThreadItem",
        "V2ItemCompletedNotification__ThreadItem",
        "V2ThreadReadResponse__ThreadItem",
        "V2ThreadResumeResponse__ThreadItem",
    ] {
        assert!(schema::validate_type(name, &item).is_ok(), "{name}");
    }
    for name in [
        "ServerNotification__SubAgentActivityKind",
        "V2ItemStartedNotification__SubAgentActivityKind",
        "V2ItemCompletedNotification__SubAgentActivityKind",
        "V2ThreadReadResponse__SubAgentActivityKind",
        "V2ThreadResumeResponse__SubAgentActivityKind",
    ] {
        assert!(
            schema::validate_type(name, &json!("completed")).is_ok(),
            "{name}"
        );
    }
    for tool in [
        "sendMessage",
        "followupTask",
        "interruptAgent",
        "listAgents",
    ] {
        for name in [
            "ServerNotification__CollabAgentTool",
            "V2ThreadResumeResponse__CollabAgentTool",
        ] {
            assert!(
                schema::validate_type(name, &json!(tool)).is_ok(),
                "{name}: {tool}"
            );
        }
    }
}

fn error_text(info: &crate::errors::ProtocolParseInfo) -> String {
    format!("{info:?}")
}

fn idle_thread(id: &str) -> Value {
    json!({
        "cliVersion": "0.159.0",
        "createdAt": 0,
        "cwd": "/tmp",
        "ephemeral": false,
        "id": id,
        "modelProvider": "openai",
        "preview": "",
        "projectId": null,
        "sessionId": "session",
        "source": "cli",
        "status": {"type": "idle"},
        "turns": [],
        "updatedAt": 1
    })
}

#[tokio::test]
async fn notifications_run_inline_in_arrival_order() {
    let (protocol, _peer_reader, mut peer_writer) = pair();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = order.clone();
    protocol.set_notification_handler(Arc::new(move |_method, params| {
        let recorded = recorded.clone();
        async move {
            tokio::task::yield_now().await;
            recorded
                .lock()
                .unwrap()
                .push(params.unwrap()["index"].as_u64().unwrap());
        }
        .boxed()
    }));
    write_frame(
        &mut peer_writer,
        json!({"method":"event","params":{"index":1}}),
    )
    .await;
    write_frame(
        &mut peer_writer,
        json!({"method":"event","params":{"index":2}}),
    )
    .await;
    timeout(Duration::from_secs(1), async {
        loop {
            if order.lock().unwrap().as_slice() == [1, 2] {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn overloaded_requests_receive_server_overloaded() {
    let (protocol, mut peer_reader, mut peer_writer) = pair();
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = started.clone();
    protocol.set_request_handler(Arc::new(move |_method, _params| {
        let counter = counter.clone();
        let mut release_rx = release_rx.clone();
        async move {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            while !*release_rx.borrow() {
                if release_rx.changed().await.is_err() {
                    break;
                }
            }
            Ok(Some(json!({"decision":"accept"})))
        }
        .boxed()
    }));
    let mut frames = String::new();
    for id in 1..=33 {
        frames.push_str(&format!(
            "{}\n",
            json!({"id": id, "method": "item/tool/requestUserInput", "params": {}})
        ));
    }
    peer_writer.write_all(frames.as_bytes()).await.unwrap();
    let mut lines = BufReader::new(&mut peer_reader).lines();
    let error: Value = serde_json::from_str(
        &timeout(Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        error,
        json!({"id": 33, "error": {"code": -32001, "message": "Too many Codex requests are already active."}})
    );
    timeout(Duration::from_secs(1), async {
        while started.load(std::sync::atomic::Ordering::SeqCst) < 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release_tx.send(true).unwrap();
}

#[tokio::test]
async fn unroutable_line_fails_pending_without_the_payload() {
    let (protocol, mut peer_reader, mut peer_writer) = pair();
    let pending = {
        let protocol = protocol.clone();
        tokio::spawn(async move { protocol.request("thread/read", Some(json!({}))).await })
    };
    let mut lines = BufReader::new(&mut peer_reader).lines();
    lines.next_line().await.unwrap().unwrap();
    let secret = "codex-unroutable-secret-sentinel";
    write_frame(
        &mut peer_writer,
        json!({"id": true, "method": "thread/start", "params": {"token": secret}}),
    )
    .await;
    let error = timeout(Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    let AppServerError::ProtocolParse(info) = error else {
        panic!("unexpected {error}");
    };
    assert_eq!(info.operation, "route-wire-message");
    assert_eq!(info.method.as_deref(), Some("thread/start"));
    assert_eq!(info.request_id, None);
    assert_eq!(info.payload_kind, Some("object"));
    assert_eq!(
        info.present_fields.as_deref(),
        Some(["id", "method", "params"].as_slice())
    );
    assert!(!error_text(&info).contains("codex-unroutable-secret-sentinel"));
}

#[tokio::test]
async fn revert_uses_the_response_thread_and_does_not_wait() {
    use crate::extensions::{response_thread, revert_codex_thread};
    let (client_read, mut peer_writer) = tokio::io::duplex(1024 * 1024);
    let (peer_reader, client_write) = tokio::io::duplex(1024 * 1024);
    let client = Arc::new(crate::client::Client::from_io(client_read, client_write));
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { revert_codex_thread(&client, "thread", 3).await })
    };
    let mut lines = BufReader::new(peer_reader).lines();
    let first: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        first["params"],
        json!({"threadId":"thread","cursor":null,"limit":3,"sortDirection":"desc","itemsView":"summary"})
    );
    write_frame(
        &mut peer_writer,
        json!({"id":1,"result":{"data":[{"id":"newest","items":[],"status":"completed"},{"id":"middle","items":[],"status":"completed"}],"nextCursor":"older"}}),
    )
    .await;
    let second: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(second["params"]["cursor"], "older");
    assert_eq!(second["params"]["limit"], 1);
    write_frame(
        &mut peer_writer,
        json!({"id":2,"result":{"data":[{"id":"boundary","items":[],"status":"completed"}],"nextCursor":null}}),
    )
    .await;
    let revert: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(revert["method"], "thread/revert");
    assert_eq!(
        revert["params"],
        json!({"threadId":"thread","beforeTurnId":"boundary"})
    );
    write_frame(
        &mut peer_writer,
        json!({"id":3,"result":{"thread": idle_thread("thread")}}),
    )
    .await;
    let response = timeout(Duration::from_secs(1), worker)
        .await
        .expect("revert waited for thread/reverted")
        .unwrap()
        .unwrap();
    assert_eq!(response_thread(&response).unwrap()["id"], "thread");
}

#[tokio::test]
async fn revert_stops_when_a_cursor_repeats() {
    use crate::extensions::revert_codex_thread;
    let (client_read, mut peer_writer) = tokio::io::duplex(1024 * 1024);
    let (peer_reader, client_write) = tokio::io::duplex(1024 * 1024);
    let client = Arc::new(crate::client::Client::from_io(client_read, client_write));
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { revert_codex_thread(&client, "thread", 3).await })
    };
    let mut lines = BufReader::new(peer_reader).lines();
    lines.next_line().await.unwrap().unwrap();
    write_frame(
        &mut peer_writer,
        json!({"id":1,"result":{"data":[{"id":"newest","items":[],"status":"completed"}],"nextCursor":"again"}}),
    )
    .await;
    lines.next_line().await.unwrap().unwrap();
    write_frame(
        &mut peer_writer,
        json!({"id":2,"result":{"data":[],"nextCursor":"again"}}),
    )
    .await;
    let error = timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.request_message(),
        Some("Thread history pagination repeated a cursor.")
    );
}

#[test]
fn raw_additions_match_the_adapter_contract() {
    use crate::extensions::{
        ThreadHistoryMode, decode_background_terminal_terminate, decode_background_terminals_list,
        decode_thread_history_metadata, is_archived_session_error, legacy_approval_reply,
        provider_probe_returns_early, sandbox_policy, thread_runtime_config, user_agent_version,
        validate_turn_start_params,
    };
    let metadata = decode_thread_history_metadata(&json!({
        "thread": {"historyMode": "paginated", "status": {"type": "notLoaded"}}
    }))
    .unwrap();
    assert_eq!(metadata.history_mode, Some(ThreadHistoryMode::Paginated));
    assert!(!metadata.loaded);
    let loaded =
        decode_thread_history_metadata(&json!({"thread": {"status": {"type": "idle"}}})).unwrap();
    assert_eq!(loaded.history_mode, None);
    assert!(loaded.loaded);
    let page = decode_background_terminals_list(&json!({
        "data": [{"processId": "proc-1", "extra": true}],
        "nextCursor": null
    }))
    .unwrap();
    assert_eq!(page.data[0].process_id, "proc-1");
    assert_eq!(page.next_cursor, None);
    assert!(decode_background_terminal_terminate(&json!({"terminated": true})).unwrap());
    assert!(
        validate_turn_start_params(Some(&json!({
            "threadId": "thread-1",
            "input": [{"type": "text", "text": "hi"}],
            "collaborationMode": {"mode": "plan", "settings": {"model": "gpt-5.6"}},
            "additionalContext": {"app": {"kind": "untrusted", "value": "hello"}}
        })))
        .is_ok()
    );
    assert!(
        validate_turn_start_params(Some(&json!({
            "threadId": "thread-1",
            "input": [{"type": "text", "text": "hi"}],
            "collaborationMode": {"mode": "nope"}
        })))
        .is_err()
    );
    assert_eq!(
        thread_runtime_config(),
        json!({"tools.update_plan.enabled": true, "mcp_optional_startup_grace_ms": 10000})
    );
    assert_eq!(sandbox_policy("readOnly"), json!({"type": "readOnly"}));
    assert_eq!(
        legacy_approval_reply("decline"),
        json!({"decision": {"denied": {"rejection": "User declined the request."}}})
    );
    assert_eq!(
        legacy_approval_reply("accept"),
        json!({"decision": "approved"})
    );
    assert!(is_archived_session_error(
        "session saved-thread is archived"
    ));
    assert!(is_archived_session_error(
        "Run `codex unarchive saved-thread` to unarchive it first."
    ));
    assert!(is_archived_session_error("Session X IS ARCHIVED"));
    assert!(!is_archived_session_error("workspace is archived"));
    assert!(!is_archived_session_error(
        "permission denied reading archived_sessions/saved-thread"
    ));
    assert_eq!(user_agent_version("codex/0.159.0 (macos)"), Some("0.159.0"));
    assert!(provider_probe_returns_early(None, true));
    assert!(provider_probe_returns_early(Some(&Value::Null), true));
    assert!(!provider_probe_returns_early(
        Some(&json!({"type": "chatgpt"})),
        true
    ));
    let secret = "codex-schema-payload-secret";
    let detail = schema::validate_type(
        "V2TurnStartParams",
        &json!({"threadId": secret, "input": "nope"}),
    )
    .unwrap_err();
    assert!(!detail.contains(secret), "{detail}");
}

#[cfg(unix)]
#[tokio::test]
async fn stderr_is_drained_before_the_child_responds() {
    let script = r#"
use strict;
binmode STDERR;
print STDERR "x" x (512 * 1024);
STDOUT->autoflush(1);
while (my $line = <STDIN>) {
  if ($line =~ /"method":"initialize"/ && $line =~ /"id":(\d+)/) {
    my $id = $1;
    print qq({"id":$id,"result":{"userAgent":"mock","codexHome":"/tmp","platformFamily":"unix","platformOs":"macos"}}\n);
    exit 0;
  }
}
"#;
    let client = crate::client::Client::spawn("perl", &["-e", script], None).unwrap();
    let result = timeout(
        Duration::from_secs(2),
        client.request(
            "initialize",
            Some(json!({
                "clientInfo": {"name": "test", "title": "Test", "version": "0.1"},
                "capabilities": {"experimentalApi": true, "optOutNotificationMethods": null}
            })),
        ),
    )
    .await
    .expect("stderr filled the pipe")
    .unwrap()
    .unwrap();
    assert_eq!(result["userAgent"], "mock");
}
