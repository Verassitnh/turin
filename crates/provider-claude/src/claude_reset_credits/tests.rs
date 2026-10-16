//! Port of upstream/apps/server/src/provider/claudeResetCredits.test.ts.
//!
//! Upstream mocks the HttpClient. Here a small local HTTP server answers the
//! calls, and the endpoint's base URL points at it. Nothing reads a real login:
//! each test writes its own `.credentials.json` and `.claude.json`.
use super::*;
use serde_json::json;
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const NOW_MS: i64 = 1_784_000_000_000;

fn grant(id: &str, resets_left: u64, extra: Value) -> Value {
    let mut value = json!({"id": id, "resets_left": resets_left, "usable_now": true, "ends_at": "2099-01-01T00:00:00Z"});
    for (key, val) in extra.as_object().expect("object").clone() {
        value[key] = val;
    }
    value
}

#[test]
fn counts_live_grants_and_pins_the_next_usable_one() {
    let block = json!({
        "eligible": true,
        "next_grant_id": "grant_b",
        "grants": [
            grant("grant_a", 1, json!({})),
            grant("grant_b", 2, json!({"ends_at": "2099-02-01T12:30:00.5+00:00"})),
            grant("grant_paused", 5, json!({"paused": true})),
            grant("grant_unusable", 5, json!({"usable_now": false})),
            grant("grant_expired", 5, json!({"ends_at": "2020-01-01T00:00:00Z"})),
            grant("grant_impossible_day", 5, json!({"ends_at": "2099-02-30T00:00:00Z"})),
            grant("Bad Id", 5, json!({})),
        ]
    });
    let credits = claude_reset_credits_to_contract(Some(&block), NOW_MS).expect("credits");
    assert_eq!(credits.available_count, 3);
    assert_eq!(
        credits.next_credit_id.as_ref().map(|id| id.as_str()),
        Some("grant_b")
    );
    assert_eq!(
        credits.next_expires_at.as_deref(),
        Some("2099-02-01T12:30:00.500Z")
    );
}

#[test]
fn offers_nothing_without_a_usable_next_grant_or_an_eligible_account() {
    let no_next = json!({"eligible": true, "next_grant_id": "gone", "grants": [grant("grant_a", 1, json!({}))]});
    let credits = claude_reset_credits_to_contract(Some(&no_next), NOW_MS).expect("credits");
    assert_eq!(credits.available_count, 0);
    assert_eq!(credits.next_credit_id, None);

    let ineligible = json!({"eligible": false, "next_grant_id": "grant_a", "grants": [grant("grant_a", 1, json!({}))]});
    assert_eq!(
        claude_reset_credits_to_contract(Some(&ineligible), NOW_MS),
        None
    );
    assert_eq!(claude_reset_credits_to_contract(None, NOW_MS), None);
}

/// A canned HTTP server: answers every request with `status` and `body`, or,
/// when `stall` is set, accepts the request and never answers.
async fn serve(status: u16, body: &'static str, stall: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut received = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let Ok(read) = socket.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        break;
                    }
                    received.extend_from_slice(&buffer[..read]);
                    if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers =
                            String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if received.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                if stall {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    return;
                }
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

fn endpoint(api_base: String, claim_timeout: Duration) -> ClaudeResetCreditsEndpoint {
    ClaudeResetCreditsEndpoint {
        api_base,
        read_timeout: Duration::from_secs(5),
        claim_timeout,
    }
}

/// A config dir with a login and an account record for organization `org-1`.
fn login_dir(token: Option<&str>, organization: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    if let Some(token) = token {
        std::fs::write(
            dir.path().join(".credentials.json"),
            json!({"claudeAiOauth": {"accessToken": token}}).to_string(),
        )
        .expect("credentials");
    }
    if let Some(organization) = organization {
        std::fs::write(
            dir.path().join(".claude.json"),
            json!({"oauthAccount": {"organizationUuid": organization}}).to_string(),
        )
        .expect("account");
    }
    dir
}

async fn claim(
    api_base: String,
    dir: &tempfile::TempDir,
    grant_id: &str,
    request_id: &str,
    claim_timeout: Duration,
) -> Result<ProviderConsumeResetCreditOutcome, ClaudeResetCreditError> {
    consume_claude_reset_credit(
        &reqwest::Client::new(),
        &endpoint(api_base, claim_timeout),
        Platform::Linux,
        ConsumeClaudeResetCreditInput {
            config_dir: dir.path(),
            account_config_path: &dir.path().join(".claude.json"),
            version: "2.1.288",
            grant_id,
            request_id,
        },
    )
    .await
}

#[tokio::test]
async fn reads_the_grants_with_the_login_and_skips_keychain_logins() {
    let body = r#"{"cedar_ember":{"eligible":true,"next_grant_id":"grant_a","grants":[{"id":"grant_a","resets_left":2,"usable_now":true,"ends_at":null}]}}"#;
    let base = serve(200, body, false).await;
    let dir = login_dir(Some("test-token"), Some("org-1"));
    let credits = read_claude_reset_credits(
        &reqwest::Client::new(),
        &endpoint(base.clone(), Duration::from_secs(5)),
        dir.path(),
        "2.1.288",
        Platform::Linux,
        NOW_MS,
    )
    .await
    .expect("credits read");
    assert_eq!(credits.available_count, 2);

    // Keychain logins on macOS are not read, so nothing is offered.
    let keychain = read_claude_reset_credits(
        &reqwest::Client::new(),
        &endpoint(base, Duration::from_secs(5)),
        dir.path(),
        "2.1.288",
        Platform::Darwin,
        NOW_MS,
    )
    .await;
    assert_eq!(keychain, None);
}

#[tokio::test]
async fn a_failed_read_offers_no_resets() {
    let base = serve(500, "{}", false).await;
    let dir = login_dir(Some("test-token"), None);
    assert_eq!(
        read_claude_reset_credits(
            &reqwest::Client::new(),
            &endpoint(base, Duration::from_secs(5)),
            dir.path(),
            "2.1.288",
            Platform::Linux,
            NOW_MS,
        )
        .await,
        None
    );
}

#[tokio::test]
async fn claims_the_grant_for_the_organization() {
    let base = serve(200, r#"{"result":"reset"}"#, false).await;
    let dir = login_dir(Some("test-token"), Some("org-1"));
    assert_eq!(
        claim(base, &dir, "grant_a", "r-1", Duration::from_secs(5)).await,
        Ok(ProviderConsumeResetCreditOutcome::Reset)
    );
}

#[tokio::test]
async fn maps_each_answer_to_an_outcome_or_a_failure() {
    let dir = login_dir(Some("test-token"), Some("org-1"));
    let cases: [(
        u16,
        &'static str,
        Result<ProviderConsumeResetCreditOutcome, ClaudeResetCreditFailure>,
    ); 9] = [
        (
            200,
            r#"{"result":"reset"}"#,
            Ok(ProviderConsumeResetCreditOutcome::Reset),
        ),
        (
            200,
            r#"{"result":"not_limited"}"#,
            Ok(ProviderConsumeResetCreditOutcome::NothingToReset),
        ),
        (
            200,
            r#"{"result":"already_used"}"#,
            Ok(ProviderConsumeResetCreditOutcome::AlreadyRedeemed),
        ),
        (
            200,
            r#"{"result":"ineligible"}"#,
            Ok(ProviderConsumeResetCreditOutcome::NoCredit),
        ),
        (
            200,
            r#"{"result":"cooldown"}"#,
            Err(ClaudeResetCreditFailure::CoolingDown),
        ),
        (
            200,
            r#"{"result":"unavailable"}"#,
            Err(ClaudeResetCreditFailure::Unconfirmed),
        ),
        (429, "{}", Err(ClaudeResetCreditFailure::RateLimited)),
        (401, "{}", Err(ClaudeResetCreditFailure::SignedOut)),
        (500, "{}", Err(ClaudeResetCreditFailure::RequestFailed)),
    ];
    for (status, body, expected) in cases {
        let base = serve(status, body, false).await;
        let result = claim(base, &dir, "grant_a", "r-1", Duration::from_secs(5)).await;
        assert_eq!(
            result.map_err(|error| error.reason),
            expected,
            "status {status} body {body}"
        );
    }
}

#[tokio::test]
async fn a_stalled_claim_times_out_as_a_retryable_request_failure() {
    let base = serve(200, "{}", true).await;
    let dir = login_dir(Some("test-token"), Some("org-1"));
    let error = claim(base, &dir, "grant_a", "r-1", Duration::from_millis(200))
        .await
        .expect_err("stalled claim fails");
    assert_eq!(error.reason, ClaudeResetCreditFailure::RequestFailed);
    assert!(!is_settled_claude_reset_credit_failure(&error));
}

#[tokio::test]
async fn refuses_malformed_ids_and_signed_out_logins_without_sending() {
    // Nothing listens on this port, so any request would fail as RequestFailed.
    let unreachable = "http://127.0.0.1:9".to_owned();
    let dir = login_dir(Some("test-token"), Some("org-1"));
    let malformed = claim(
        unreachable.clone(),
        &dir,
        "Grant With Spaces",
        "r-1",
        Duration::from_secs(1),
    )
    .await
    .expect_err("malformed grant id");
    assert_eq!(malformed.reason, ClaudeResetCreditFailure::MalformedCredit);
    assert!(is_settled_claude_reset_credit_failure(&malformed));

    let signed_out_dir = login_dir(None, Some("org-1"));
    let signed_out = claim(
        unreachable,
        &signed_out_dir,
        "grant_a",
        "r-1",
        Duration::from_secs(1),
    )
    .await
    .expect_err("no token");
    assert_eq!(signed_out.reason, ClaudeResetCreditFailure::SignedOut);
}

#[test]
fn a_claim_failure_names_its_reason_in_the_message() {
    let error = ClaudeResetCreditError {
        reason: ClaudeResetCreditFailure::RateLimited,
    };
    assert_eq!(
        error.to_string(),
        "Claude is rate limiting resets. Try again soon."
    );
}
