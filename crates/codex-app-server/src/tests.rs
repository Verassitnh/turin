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
        AppServerError::ProtocolParse {
            operation: "decode-wire-message",
            ..
        }
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
        AppServerError::ProtocolParse {
            operation: "decode-wire-message",
            ..
        }
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
    let item=json!({"type":"agentMessage","id":"question-1","text":"Which package?","phase":"final_answer","delivery":"async","questions":[{"title":"Which package manager?","options":["pnpm","npm"]},{"title":"What should it be named?"}]});
    for name in ["ServerNotification__ThreadItem","V2ItemStartedNotification__ThreadItem","V2ItemCompletedNotification__ThreadItem","V2ThreadReadResponse__ThreadItem","V2ThreadResumeResponse__ThreadItem"] {
        assert!(schema::validate_type(name,&item).is_ok(),"{name}");
    }
    for name in ["ServerNotification__SubAgentActivityKind","V2ItemStartedNotification__SubAgentActivityKind","V2ItemCompletedNotification__SubAgentActivityKind","V2ThreadReadResponse__SubAgentActivityKind","V2ThreadResumeResponse__SubAgentActivityKind"] {
        assert!(schema::validate_type(name,&json!("completed")).is_ok(),"{name}");
    }
    for tool in ["sendMessage","followupTask","interruptAgent","listAgents"] {
        for name in ["ServerNotification__CollabAgentTool","V2ThreadResumeResponse__CollabAgentTool"] {
            assert!(schema::validate_type(name,&json!(tool)).is_ok(),"{name}: {tool}");
        }
    }
}
