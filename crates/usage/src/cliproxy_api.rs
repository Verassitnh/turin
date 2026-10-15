//! Port of upstream/apps/server/src/usage/cliproxyApi.ts.
//!
//! Client for a CLIProxyAPI hub's management API: lists pooled accounts, reads
//! each account's quota windows through the hub, and redeems Codex reset credits.
//!
//! Deviation: the Codex and Claude window mappers (`codexUsageLimits.ts`,
//! `claudeUsageLimits.ts`) and `codexPlanLabel` belong to the provider crates
//! (T09/T10, not ported yet). They arrive through [`UsageLimitMappers`], which the
//! composition root implements. See PORTING.md.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::provider_usage_limits::{
    ProviderConsumeResetCreditOutcome, ProviderConsumeResetCreditResult,
    ServerProviderResetCredits, ServerProviderUsageLimits, ServerProviderUsageUnavailableReason,
    UsageLimitSourceAccount, UsageLimitSourceError,
};
use turin_contracts::settings::UsageLimitSourceConfig;
use turin_provider_core::usage_limits::make_unavailable_usage_limits;
use uuid::Uuid;

const CODEX_BASE: &str = "https://chatgpt.com/backend-api/wham";
const CREDIT_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
const MANAGEMENT_TIMEOUT: Duration = Duration::from_secs(15);
const ACCOUNT_READ_CONCURRENCY: usize = 4;

/// Namespace for redeem request ids. The UUIDv5 per account and credit also
/// deduplicates retries across T3 environments.
const CREDIT_REDEEM_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6f, 0x1c, 0x2a, 0x9e, 0x2d, 0x4b, 0x4c, 0x1e, 0x9a, 0x7f, 0x3b, 0x8d, 0x5e, 0x0c, 0x1a, 0x42,
]);

/// One window of a Codex rate-limit snapshot, as the hub reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexWindowInput {
    pub used_percent: f64,
    /// Epoch seconds, as the Codex API reports them.
    pub resets_at: Option<f64>,
    pub window_duration_mins: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexLimitsInput {
    pub checked_at: String,
    pub plan_type: Option<String>,
    pub primary: Option<CodexWindowInput>,
    pub secondary: Option<CodexWindowInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeWindowInput {
    pub utilization: f64,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeModelWindowInput {
    pub display_name: String,
    pub utilization: f64,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeLimitsInput {
    pub checked_at: String,
    pub five_hour: Option<ClaudeWindowInput>,
    pub seven_day: Option<ClaudeWindowInput>,
    pub model_scoped: Vec<ClaudeModelWindowInput>,
}

/// The provider-owned mappers from raw hub windows to the usage contract.
pub trait UsageLimitMappers: Send + Sync {
    /// `codexPlanLabel`: the display name of a Codex plan type.
    fn codex_plan_label(&self, plan_type: Option<&str>) -> String;
    /// `codexRateLimitsToLimits`.
    fn codex_limits(&self, input: CodexLimitsInput) -> ServerProviderUsageLimits;
    /// `claudeUsageResponseToLimits(...).limits`.
    fn claude_limits(&self, input: ClaudeLimitsInput) -> ServerProviderUsageLimits;
}

#[derive(Deserialize)]
struct AuthFile {
    id: String,
    auth_index: String,
    provider: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    id_token: Option<IdToken>,
}

#[derive(Deserialize)]
struct IdToken {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    chatgpt_plan_type: Option<String>,
}

#[derive(Deserialize)]
struct AuthFiles {
    files: Vec<AuthFile>,
}

#[derive(Deserialize)]
struct ApiResponse {
    status_code: f64,
    body: String,
}

#[derive(Deserialize)]
struct CodexWindow {
    used_percent: f64,
    #[serde(default)]
    reset_at: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<f64>,
}

#[derive(Deserialize)]
struct CodexRateLimit {
    #[serde(default)]
    primary_window: Option<CodexWindow>,
    #[serde(default)]
    secondary_window: Option<CodexWindow>,
}

#[derive(Deserialize)]
struct CodexUsage {
    #[serde(default)]
    plan_type: Option<String>,
    rate_limit: Option<CodexRateLimit>,
}

#[derive(Deserialize)]
struct ClaudeWindow {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeDisplayModel {
    display_name: String,
}

#[derive(Deserialize)]
struct ClaudeScope {
    #[serde(default)]
    model: Option<ClaudeDisplayModel>,
}

#[derive(Deserialize)]
struct ClaudeLimit {
    kind: String,
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<ClaudeScope>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    five_hour: Option<ClaudeWindow>,
    #[serde(default)]
    seven_day: Option<ClaudeWindow>,
    #[serde(default)]
    limits: Option<Vec<ClaudeLimit>>,
}

#[derive(Deserialize)]
struct Credit {
    id: String,
    status: String,
    reset_type: String,
    expires_at: String,
}

#[derive(Deserialize)]
struct CreditList {
    credits: Vec<Credit>,
}

#[derive(Deserialize)]
struct ConsumeResponse {
    code: String,
}

fn source_error(detail: impl Into<String>) -> UsageLimitSourceError {
    UsageLimitSourceError::SourceError {
        detail: detail.into(),
    }
}

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `creditRedeemRequestId`: a UUIDv5 over `accountId:creditId`.
pub fn credit_redeem_request_id(account_id: &str, credit_id: &str) -> String {
    Uuid::new_v5(
        &CREDIT_REDEEM_NAMESPACE,
        format!("{account_id}:{credit_id}").as_bytes(),
    )
    .to_string()
}

#[derive(Clone)]
pub struct CliproxyApi {
    http: reqwest::Client,
    mappers: Arc<dyn UsageLimitMappers>,
}

impl CliproxyApi {
    pub fn new(mappers: Arc<dyn UsageLimitMappers>) -> Self {
        Self {
            http: reqwest::Client::new(),
            mappers,
        }
    }

    /// One call to the hub's management API. `body` makes it a POST.
    async fn management(
        &self,
        config: &UsageLimitSourceConfig,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, UsageLimitSourceError> {
        let url = reqwest::Url::parse(config.url.as_str())
            .and_then(|base| base.join(&format!("/v0/management/{path}")))
            .map_err(|_| source_error("The hub URL is not valid."))?;
        let request = match body {
            None => self.http.get(url),
            Some(body) => self.http.post(url).json(&body),
        };
        let response = request
            .header(
                "Authorization",
                format!("Bearer {}", config.management_key.as_str()),
            )
            .timeout(MANAGEMENT_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| source_error("The hub management request failed."))?;
        response
            .json::<Value>()
            .await
            .map_err(|_| source_error("The hub management request failed."))
    }

    async fn auth_files(
        &self,
        config: &UsageLimitSourceConfig,
    ) -> Result<Vec<AuthFile>, UsageLimitSourceError> {
        let response = self.management(config, "auth-files", None).await?;
        serde_json::from_value::<AuthFiles>(response)
            .map(|files| files.files)
            .map_err(|_| source_error("The hub management request failed."))
    }

    /// Calls a provider API through the hub, which holds the account's token.
    async fn api_call(
        &self,
        config: &UsageLimitSourceConfig,
        account: &AuthFile,
        url: &str,
        data: Option<Value>,
    ) -> Result<String, UsageLimitSourceError> {
        let header = if account.provider == "codex" {
            let mut header = json!({
                "Authorization": "Bearer $TOKEN$",
                "Content-Type": "application/json",
                "OpenAI-Beta": "codex-1",
                "Originator": "Codex Desktop",
            });
            if let Some(account_id) = account
                .id_token
                .as_ref()
                .and_then(|token| token.chatgpt_account_id.as_ref())
            {
                header["Chatgpt-Account-Id"] = json!(account_id);
            }
            header
        } else {
            json!({ "Authorization": "Bearer $TOKEN$", "anthropic-beta": "oauth-2025-04-20" })
        };
        let mut body = json!({
            "auth_index": account.auth_index,
            "method": if data.is_none() { "GET" } else { "POST" },
            "url": url,
            "header": header,
        });
        if let Some(data) = data {
            // The hub forwards `data` as a JSON string, as the TS encoder does.
            body["data"] = json!(data.to_string());
        }
        let raw = self.management(config, "api-call", Some(body)).await?;
        let response: ApiResponse = serde_json::from_value(raw)
            .map_err(|_| source_error("The hub management request failed."))?;
        if !(200.0..300.0).contains(&response.status_code) {
            return Err(source_error(format!(
                "The provider refused the hub request (HTTP {}).",
                response.status_code
            )));
        }
        Ok(response.body)
    }

    /// Credits still redeemable, soonest-expiring first.
    async fn credits(
        &self,
        config: &UsageLimitSourceConfig,
        account: &AuthFile,
    ) -> Result<Vec<Credit>, UsageLimitSourceError> {
        let body = self.api_call(config, account, CREDIT_URL, None).await?;
        let list: CreditList = serde_json::from_str(&body)
            .map_err(|_| source_error("The hub management request failed."))?;
        let now = Utc::now().timestamp_millis();
        let mut credits: Vec<Credit> = list
            .credits
            .into_iter()
            .filter(|credit| {
                credit.reset_type == "codex_rate_limits"
                    && credit.status == "available"
                    && parse_millis(&credit.expires_at).is_some_and(|expires| expires > now)
            })
            .collect();
        credits.sort_by_key(|credit| parse_millis(&credit.expires_at).unwrap_or(i64::MAX));
        Ok(credits)
    }

    async fn read_account(
        &self,
        config: &UsageLimitSourceConfig,
        account: &AuthFile,
    ) -> UsageLimitSourceAccount {
        let checked_at = now_iso();
        let driver = ProviderDriverKind::new(if account.provider == "codex" {
            "codex"
        } else {
            "claudeAgent"
        })
        .expect("driver kinds are non-empty");
        let workspace_id = account
            .id_token
            .as_ref()
            .and_then(|token| token.chatgpt_account_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let base = |plan: Option<String>, usage_limits: ServerProviderUsageLimits| {
            UsageLimitSourceAccount {
                id: trimmed(&account.id),
                driver: driver.clone(),
                email: account
                    .email
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map(trimmed),
                plan: plan.as_deref().map(trimmed),
                workspace_id: workspace_id.map(trimmed),
                usage_limits,
            }
        };

        match self.read_limits(config, account, &checked_at).await {
            Ok((plan, usage_limits)) => base(Some(plan), usage_limits),
            // A failing account keeps its row with an unavailable marker, so one bad account
            // never hides the others on the same hub.
            Err(_) => base(
                None,
                make_unavailable_usage_limits(
                    checked_at.clone(),
                    ServerProviderUsageUnavailableReason::ProbeFailed,
                    Some("The hub could not read this account's usage."),
                ),
            ),
        }
    }

    async fn read_limits(
        &self,
        config: &UsageLimitSourceConfig,
        account: &AuthFile,
        checked_at: &str,
    ) -> Result<(String, ServerProviderUsageLimits), UsageLimitSourceError> {
        if account.provider == "claude" {
            let body = self
                .api_call(
                    config,
                    account,
                    "https://api.anthropic.com/api/oauth/usage",
                    None,
                )
                .await?;
            let usage: ClaudeUsage = serde_json::from_str(&body)
                .map_err(|_| source_error("The hub management request failed."))?;
            let model_scoped = usage
                .limits
                .unwrap_or_default()
                .into_iter()
                .filter_map(|limit| {
                    let model = limit.scope?.model?;
                    if limit.kind != "weekly_scoped" {
                        return None;
                    }
                    Some(ClaudeModelWindowInput {
                        display_name: model.display_name,
                        utilization: limit.percent?,
                        resets_at: limit.resets_at,
                    })
                })
                .collect();
            let window = |window: Option<ClaudeWindow>| {
                window.map(|window| ClaudeWindowInput {
                    utilization: window.utilization,
                    resets_at: window.resets_at,
                })
            };
            let limits = self.mappers.claude_limits(ClaudeLimitsInput {
                checked_at: checked_at.to_owned(),
                five_hour: window(usage.five_hour),
                seven_day: window(usage.seven_day),
                model_scoped,
            });
            return Ok(("Claude Subscription".to_owned(), limits));
        }

        let body = self
            .api_call(config, account, &format!("{CODEX_BASE}/usage"), None)
            .await?;
        let usage: CodexUsage = serde_json::from_str(&body)
            .map_err(|_| source_error("The hub management request failed."))?;
        let to_window = |window: Option<CodexWindow>| {
            window.map(|window| CodexWindowInput {
                used_percent: window.used_percent,
                resets_at: window.reset_at,
                window_duration_mins: window.limit_window_seconds.map(|seconds| seconds / 60.0),
            })
        };
        let (primary, secondary) = match usage.rate_limit {
            Some(rate_limit) => (
                to_window(rate_limit.primary_window),
                to_window(rate_limit.secondary_window),
            ),
            None => (None, None),
        };
        // The label falls back to the id token's plan; the snapshot keeps only the API's plan.
        let label_plan = usage.plan_type.clone().or_else(|| {
            account
                .id_token
                .as_ref()
                .and_then(|token| token.chatgpt_plan_type.clone())
        });
        let plan = self.mappers.codex_plan_label(label_plan.as_deref());
        let mut limits = self.mappers.codex_limits(CodexLimitsInput {
            checked_at: checked_at.to_owned(),
            plan_type: usage.plan_type,
            primary,
            secondary,
        });
        // A credits outage must not hide successfully fetched quota windows.
        if let Ok(available) = self.credits(config, account).await {
            limits.reset_credits = Some(reset_credits(&available));
        }
        Ok((plan, limits))
    }

    /// Reads every enabled Codex and Claude account on the hub. Disabled accounts
    /// and other providers are skipped.
    pub async fn read_accounts(
        &self,
        config: &UsageLimitSourceConfig,
    ) -> Result<Vec<UsageLimitSourceAccount>, UsageLimitSourceError> {
        let accounts = self
            .auth_files(config)
            .await
            .map_err(|_| source_error("The hub could not list accounts."))?;
        let reads: Vec<_> = accounts
            .iter()
            .filter(|account| {
                !account.disabled.unwrap_or(false)
                    && (account.provider == "codex" || account.provider == "claude")
            })
            .map(|account| self.read_account(config, account))
            .collect();
        // Accounts read four at a time; results keep the hub's order.
        Ok(futures::stream::iter(reads)
            .buffered(ACCOUNT_READ_CONCURRENCY)
            .collect()
            .await)
    }

    /// Redeems one Codex reset credit on the hub, then clears the account's hub cooldown.
    pub async fn consume(
        &self,
        config: &UsageLimitSourceConfig,
        account_id: &str,
        credit_id: &str,
    ) -> Result<ProviderConsumeResetCreditResult, UsageLimitSourceError> {
        let account = self
            .auth_files(config)
            .await?
            .into_iter()
            .find(|account| account.id == account_id);
        let account = match account {
            Some(account) if !account.disabled.unwrap_or(false) && account.provider == "codex" => {
                account
            }
            _ => {
                return Err(source_error(
                    "The Codex hub account is missing or disabled.",
                ));
            }
        };
        let request_account = account
            .id_token
            .as_ref()
            .and_then(|token| token.chatgpt_account_id.clone())
            .unwrap_or_else(|| account.id.clone());
        let body = self
            .api_call(
                config,
                &account,
                &format!("{CREDIT_URL}/consume"),
                Some(json!({
                    "redeem_request_id": credit_redeem_request_id(&request_account, credit_id),
                    "credit_id": credit_id,
                })),
            )
            .await?;
        let response: ConsumeResponse = serde_json::from_str(&body)
            .map_err(|_| source_error("The hub returned an unexpected reset-credit response."))?;
        let outcome = match response.code.as_str() {
            "reset" => ProviderConsumeResetCreditOutcome::Reset,
            "nothing_to_reset" => ProviderConsumeResetCreditOutcome::NothingToReset,
            "no_credit" => ProviderConsumeResetCreditOutcome::NoCredit,
            "already_redeemed" => ProviderConsumeResetCreditOutcome::AlreadyRedeemed,
            _ => {
                return Err(source_error(
                    "The hub returned an unexpected reset-credit response.",
                ));
            }
        };
        if !matches!(
            outcome,
            ProviderConsumeResetCreditOutcome::Reset
                | ProviderConsumeResetCreditOutcome::AlreadyRedeemed
        ) {
            return Ok(ProviderConsumeResetCreditResult {
                outcome,
                warning: None,
            });
        }
        let cleared = self
            .management(
                config,
                "reset-quota",
                Some(json!({ "auth_index": account.auth_index })),
            )
            .await;
        let warning = cleared.err().map(|_| {
            trimmed(
                "Credit redeemed, but the hub cooldown could not be cleared. Routing may resume after its cooldown expires.",
            )
        });
        Ok(ProviderConsumeResetCreditResult { outcome, warning })
    }
}

fn trimmed(value: impl AsRef<str>) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value.as_ref())
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("non-empty"))
}

fn parse_millis(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

fn reset_credits(available: &[Credit]) -> ServerProviderResetCredits {
    let next = available.first();
    ServerProviderResetCredits {
        available_count: available.len() as u64,
        next_credit_id: next.map(|credit| trimmed(&credit.id)),
        next_expires_at: next.and_then(|credit| {
            DateTime::parse_from_rfc3339(&credit.expires_at)
                .ok()
                .map(|parsed| {
                    parsed
                        .with_timezone(&Utc)
                        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                        .to_string()
                })
        }),
    }
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/usage/cliproxyApi.test.ts. The hub is a local
    //! HTTP server that routes management and api-call requests the way CLIProxyAPI
    //! does. Window mapping belongs to the provider crates, so the mapper here records
    //! its inputs and the tests check what the hub sent, not the windows built from it.

    use super::*;
    use parking_lot::Mutex;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use turin_contracts::provider_usage_limits::ServerProviderUsageUnavailableReason;
    use turin_provider_core::usage_limits::make_usage_limits;

    const MANAGEMENT_KEY: &str = "management-secret";

    #[derive(Default)]
    struct RecordingMappers {
        codex: Mutex<Vec<CodexLimitsInput>>,
        claude: Mutex<Vec<ClaudeLimitsInput>>,
    }

    impl UsageLimitMappers for RecordingMappers {
        fn codex_plan_label(&self, plan_type: Option<&str>) -> String {
            format!("plan:{}", plan_type.unwrap_or("none"))
        }

        fn codex_limits(&self, input: CodexLimitsInput) -> ServerProviderUsageLimits {
            let checked_at = input.checked_at.clone();
            self.codex.lock().push(input);
            make_usage_limits(checked_at, Vec::new())
        }

        fn claude_limits(&self, input: ClaudeLimitsInput) -> ServerProviderUsageLimits {
            let checked_at = input.checked_at.clone();
            self.claude.lock().push(input);
            make_usage_limits(checked_at, Vec::new())
        }
    }

    #[derive(Debug, Clone)]
    struct Recorded {
        path: String,
        authorization: Option<String>,
        body: Option<Value>,
    }

    impl Recorded {
        fn auth_index(&self) -> Option<String> {
            self.body.as_ref()?["auth_index"]
                .as_str()
                .map(str::to_owned)
        }

        fn upstream_url(&self) -> Option<String> {
            self.body.as_ref()?["url"].as_str().map(str::to_owned)
        }

        fn is_api_call(&self) -> bool {
            self.path.ends_with("/api-call")
        }

        fn is_consume(&self) -> bool {
            self.upstream_url()
                .is_some_and(|url| url.ends_with("/consume"))
        }

        fn data(&self) -> Option<String> {
            self.body.as_ref()?["data"].as_str().map(str::to_owned)
        }
    }

    /// A management response wrapping an upstream reply, as the hub returns it.
    fn upstream(status: u16, body: Value) -> Value {
        json!({ "status_code": status, "body": body.to_string() })
    }

    struct AbortOnDrop(tokio::task::JoinHandle<()>);

    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    type Handler = Arc<dyn Fn(&Recorded) -> (u16, Value) + Send + Sync>;

    struct Hub {
        url: String,
        requests: Arc<Mutex<Vec<Recorded>>>,
        _accept: AbortOnDrop,
    }

    impl Hub {
        async fn start(handler: Handler) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = requests.clone();
            let accept = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let handler = handler.clone();
                    let recorded = recorded.clone();
                    tokio::spawn(async move {
                        let Some((head, body)) = read_request(&mut socket).await else {
                            return;
                        };
                        let mut lines = head.lines();
                        let path = lines
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_owned();
                        let authorization = lines
                            .filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                            .map(|(_, value)| value.trim().to_owned());
                        let body = if body.is_empty() {
                            None
                        } else {
                            serde_json::from_slice(&body).ok()
                        };
                        let request = Recorded {
                            path,
                            authorization,
                            body,
                        };
                        let (status, payload) = handler(&request);
                        recorded.lock().push(request);
                        let text = payload.to_string();
                        let response = format!(
                            "HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                            text.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self {
                url,
                requests,
                _accept: AbortOnDrop(accept),
            }
        }

        fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().clone()
        }

        fn config(&self) -> UsageLimitSourceConfig {
            serde_json::from_value(json!({
                "kind": "cliproxy",
                "url": self.url,
                "managementKey": MANAGEMENT_KEY,
                "enabled": true,
            }))
            .unwrap()
        }
    }

    /// Reads one HTTP request: its head, and its body when `Content-Length` says so.
    async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
        let length: usize = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .unwrap_or(0);
        let mut body = buffer[head_end + 4..].to_vec();
        while body.len() < length {
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        Some((head, body))
    }

    fn handler(f: impl Fn(&Recorded) -> (u16, Value) + Send + Sync + 'static) -> Handler {
        Arc::new(f)
    }

    fn codex_accounts() -> Value {
        json!({
            "files": [
                {
                    "id": "first.json",
                    "auth_index": "a",
                    "provider": "codex",
                    "email": "first@example.com",
                    "id_token": { "chatgpt_account_id": "account-a" },
                },
                {
                    "id": "second.json",
                    "auth_index": "b",
                    "provider": "codex",
                    "email": "second@example.com",
                    "id_token": { "chatgpt_account_id": "account-b" },
                },
            ],
        })
    }

    fn credit(id: &str, expires_at: &str, status: &str) -> Value {
        json!({ "id": id, "expires_at": expires_at, "status": status, "reset_type": "codex_rate_limits" })
    }

    fn default_credits() -> Value {
        json!({
            "credits": [
                credit("later", "2099-02-01T00:00:00Z", "available"),
                credit("first", "2099-01-01T00:00:00Z", "available"),
                credit("expired", "2000-01-01T00:00:00Z", "available"),
                credit("used", "2099-01-01T00:00:00Z", "redeemed"),
            ],
        })
    }

    fn default_usage() -> Value {
        json!({
            "plan_type": "pro",
            "rate_limit": {
                "secondary_window": {
                    "used_percent": 78,
                    "reset_at": 4070908800u64,
                    "limit_window_seconds": 604800,
                },
            },
        })
    }

    /// Routes codex management calls: accounts, credits, usage, and redemption.
    fn codex_hub(consume: Value) -> Handler {
        handler(move |request| {
            if request.path.ends_with("/auth-files") {
                return (200, codex_accounts());
            }
            if request.path.ends_with("/reset-quota") {
                return (200, json!({}));
            }
            let url = request.upstream_url().unwrap_or_default();
            let body = if url.ends_with("/consume") {
                consume.clone()
            } else if url.ends_with("/rate-limit-reset-credits") {
                default_credits()
            } else {
                default_usage()
            };
            (200, upstream(200, body))
        })
    }

    fn account_by_id<'a>(
        accounts: &'a [UsageLimitSourceAccount],
        id: &str,
    ) -> &'a UsageLimitSourceAccount {
        accounts
            .iter()
            .find(|account| account.id.as_str() == id)
            .unwrap()
    }

    #[tokio::test]
    async fn reads_both_accounts_and_their_earliest_unexpired_credits_without_plugin_endpoints() {
        let hub = Hub::start(codex_hub(json!({ "code": "reset" }))).await;
        let mappers = Arc::new(RecordingMappers::default());
        let api = CliproxyApi::new(mappers.clone());
        let result = api.read_accounts(&hub.config()).await.unwrap();

        // Credits are attached after mapping: available, unexpired, earliest first.
        assert_eq!(
            account_by_id(&result, "first.json")
                .usage_limits
                .reset_credits,
            Some(ServerProviderResetCredits {
                available_count: 2,
                next_expires_at: Some("2099-01-01T00:00:00.000Z".to_owned()),
                next_credit_id: Some(TrimmedNonEmptyString::new("first").unwrap()),
            })
        );
        let calls = hub
            .requests()
            .iter()
            .filter(|request| request.is_api_call())
            .count();
        assert_eq!(calls, 4);

        let codex_inputs = mappers.codex.lock().clone();
        assert_eq!(codex_inputs.len(), 2);
        let secondary = codex_inputs[0].secondary.clone().unwrap();
        assert_eq!(secondary.used_percent, 78.0);
        assert_eq!(secondary.resets_at, Some(4070908800.0));
        assert_eq!(secondary.window_duration_mins, Some(10080.0));
        assert!(codex_inputs[0].primary.is_none());
        assert_eq!(codex_inputs[0].plan_type.as_deref(), Some("pro"));
        assert_eq!(
            account_by_id(&result, "first.json")
                .plan
                .as_ref()
                .map(|plan| plan.as_str()),
            Some("plan:pro")
        );

        let mut auth_indexes: Vec<String> = hub
            .requests()
            .iter()
            .filter(|request| request.is_api_call())
            .filter_map(Recorded::auth_index)
            .collect();
        auth_indexes.sort();
        assert_eq!(auth_indexes, ["a", "a", "b", "b"]);
        let account_b_call = hub
            .requests()
            .into_iter()
            .find(|request| request.is_api_call() && request.auth_index().as_deref() == Some("b"))
            .unwrap();
        assert_eq!(
            account_b_call.body.as_ref().unwrap()["header"]["Chatgpt-Account-Id"],
            "account-b"
        );
        assert_eq!(
            hub.requests()[0].authorization.as_deref(),
            Some("Bearer management-secret")
        );
    }

    #[tokio::test]
    async fn keeps_usage_when_the_credits_endpoint_fails() {
        let hub = Hub::start(handler(|request| {
            if request.path.ends_with("/auth-files") {
                return (200, codex_accounts());
            }
            let url = request.upstream_url().unwrap_or_default();
            if url.ends_with("rate-limit-reset-credits") {
                (200, upstream(503, json!({ "token": "do-not-publish" })))
            } else {
                (
                    200,
                    upstream(
                        200,
                        json!({ "rate_limit": { "primary_window": { "used_percent": 12 } } }),
                    ),
                )
            }
        }))
        .await;
        let mappers = Arc::new(RecordingMappers::default());
        let result = CliproxyApi::new(mappers.clone())
            .read_accounts(&hub.config())
            .await
            .unwrap();

        assert_eq!(
            mappers.codex.lock()[0]
                .primary
                .as_ref()
                .unwrap()
                .used_percent,
            12.0
        );
        assert_eq!(result[0].usage_limits.reset_credits, None);
    }

    #[tokio::test]
    async fn isolates_a_failed_account_and_never_publishes_upstream_error_bodies() {
        let hub = Hub::start(handler(|request| {
            if request.path.ends_with("/auth-files") {
                return (200, codex_accounts());
            }
            if request.auth_index().as_deref() == Some("a") {
                return (200, upstream(401, json!({ "token": "do-not-publish" })));
            }
            let url = request.upstream_url().unwrap_or_default();
            let body = if url.ends_with("rate-limit-reset-credits") {
                json!({ "credits": [] })
            } else {
                json!({ "rate_limit": { "primary_window": { "used_percent": 12 } } })
            };
            (200, upstream(200, body))
        }))
        .await;
        let mappers = Arc::new(RecordingMappers::default());
        let result = CliproxyApi::new(mappers.clone())
            .read_accounts(&hub.config())
            .await
            .unwrap();

        let failed = account_by_id(&result, "first.json");
        assert_eq!(
            failed
                .usage_limits
                .unavailable
                .as_ref()
                .map(|unavailable| unavailable.reason),
            Some(ServerProviderUsageUnavailableReason::ProbeFailed)
        );
        let healthy = account_by_id(&result, "second.json");
        assert_eq!(
            healthy.usage_limits.reset_credits,
            Some(ServerProviderResetCredits {
                available_count: 0,
                next_expires_at: None,
                next_credit_id: None,
            })
        );
        assert_eq!(mappers.codex.lock().len(), 1);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("do-not-publish")
        );
    }

    #[tokio::test]
    async fn maps_claude_scoped_windows_from_the_hub_usage_response() {
        let hub = Hub::start(handler(|request| {
            if request.path.ends_with("/auth-files") {
                return (
                    200,
                    json!({ "files": [{ "id": "claude.json", "auth_index": "c", "provider": "claude" }] }),
                );
            }
            (
                200,
                upstream(
                    200,
                    json!({
                        "five_hour": { "utilization": 10, "resets_at": null },
                        "seven_day": { "utilization": 50, "resets_at": "2099-01-01T00:00:00Z" },
                        "limits": [{
                            "kind": "weekly_scoped",
                            "percent": 80,
                            "resets_at": null,
                            "scope": { "model": { "display_name": "Fable" } },
                        }],
                    }),
                ),
            )
        }))
        .await;
        let mappers = Arc::new(RecordingMappers::default());
        let result = CliproxyApi::new(mappers.clone())
            .read_accounts(&hub.config())
            .await
            .unwrap();

        let claude = mappers.claude.lock()[0].clone();
        assert_eq!(
            claude.five_hour,
            Some(ClaudeWindowInput {
                utilization: 10.0,
                resets_at: None
            })
        );
        assert_eq!(
            claude.seven_day,
            Some(ClaudeWindowInput {
                utilization: 50.0,
                resets_at: Some("2099-01-01T00:00:00Z".to_owned())
            })
        );
        assert_eq!(
            claude.model_scoped,
            vec![ClaudeModelWindowInput {
                display_name: "Fable".to_owned(),
                utilization: 80.0,
                resets_at: None
            }]
        );
        assert_eq!(
            result[0].plan.as_ref().map(|plan| plan.as_str()),
            Some("Claude Subscription")
        );
    }

    #[tokio::test]
    async fn pins_redemption_to_the_displayed_credit_and_clears_only_that_accounts_cooldown() {
        let hub = Hub::start(codex_hub(json!({ "code": "reset" }))).await;
        let api = CliproxyApi::new(Arc::new(RecordingMappers::default()));

        assert_eq!(
            api.consume(&hub.config(), "second.json", "credit-b")
                .await
                .unwrap()
                .outcome,
            ProviderConsumeResetCreditOutcome::Reset
        );
        assert_eq!(
            api.consume(&hub.config(), "second.json", "credit-b")
                .await
                .unwrap()
                .outcome,
            ProviderConsumeResetCreditOutcome::Reset
        );
        let requests = hub.requests();
        let redemptions: Vec<&Recorded> = requests
            .iter()
            .filter(|request| request.is_consume())
            .collect();
        assert_eq!(redemptions.len(), 2);
        assert_eq!(redemptions[0].data(), redemptions[1].data());
        assert_eq!(
            redemptions[0].data().unwrap(),
            // UUIDv5 of "account-b:credit-b"; must stay stable so retries deduplicate.
            json!({ "redeem_request_id": "519d5243-011a-5b7b-91f3-44d85f095705", "credit_id": "credit-b" })
                .to_string()
        );
        let cleared: Vec<String> = requests
            .iter()
            .filter(|request| request.path.ends_with("/reset-quota"))
            .filter_map(Recorded::auth_index)
            .collect();
        assert_eq!(cleared, ["b", "b"]);
    }

    #[tokio::test]
    async fn reports_the_redemption_outcomes_and_clears_cooldown_only_for_already_redeemed() {
        for (code, outcome) in [
            (
                "nothing_to_reset",
                ProviderConsumeResetCreditOutcome::NothingToReset,
            ),
            ("no_credit", ProviderConsumeResetCreditOutcome::NoCredit),
            (
                "already_redeemed",
                ProviderConsumeResetCreditOutcome::AlreadyRedeemed,
            ),
        ] {
            let hub = Hub::start(codex_hub(json!({ "code": code }))).await;
            let api = CliproxyApi::new(Arc::new(RecordingMappers::default()));
            assert_eq!(
                api.consume(&hub.config(), "first.json", "credit")
                    .await
                    .unwrap()
                    .outcome,
                outcome
            );
            let cleared = hub
                .requests()
                .iter()
                .any(|request| request.path.ends_with("/reset-quota"));
            assert_eq!(cleared, code == "already_redeemed", "{code}");
        }
    }

    #[tokio::test]
    async fn reports_redemption_success_even_if_cooldown_clearing_fails() {
        let hub = Hub::start(handler(|request| {
            if request.path.ends_with("/reset-quota") {
                return (404, json!({}));
            }
            if request.path.ends_with("/auth-files") {
                return (200, codex_accounts());
            }
            (200, upstream(200, json!({ "code": "reset" })))
        }))
        .await;
        let api = CliproxyApi::new(Arc::new(RecordingMappers::default()));
        let result = api
            .consume(&hub.config(), "first.json", "credit")
            .await
            .unwrap();

        assert_eq!(result.outcome, ProviderConsumeResetCreditOutcome::Reset);
        assert!(result.warning.unwrap().as_str().contains("cooldown"));
    }

    #[tokio::test]
    async fn skips_disabled_accounts_and_rejects_redemption_on_them() {
        let hub = Hub::start(handler(|_| {
            (
                200,
                json!({ "files": [{
                    "id": "first.json",
                    "auth_index": "a",
                    "provider": "codex",
                    "disabled": true,
                    "id_token": { "chatgpt_account_id": "account-a" },
                }] }),
            )
        }))
        .await;
        let api = CliproxyApi::new(Arc::new(RecordingMappers::default()));

        assert!(api.read_accounts(&hub.config()).await.unwrap().is_empty());
        assert!(
            api.consume(&hub.config(), "first.json", "credit")
                .await
                .is_err()
        );
        assert!(
            hub.requests()
                .iter()
                .all(|request| request.path.ends_with("/auth-files"))
        );
    }

    #[tokio::test]
    async fn keeps_the_same_redemption_id_after_an_uncertain_upstream_failure() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let hub = Hub::start(handler(move |request| {
            if request.path.ends_with("/auth-files") {
                return (200, codex_accounts());
            }
            if request.path.ends_with("/reset-quota") {
                return (200, json!({}));
            }
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                (200, upstream(503, json!({})))
            } else {
                (200, upstream(200, json!({ "code": "already_redeemed" })))
            }
        }))
        .await;
        let api = CliproxyApi::new(Arc::new(RecordingMappers::default()));

        assert!(
            api.consume(&hub.config(), "first.json", "credit")
                .await
                .is_err()
        );
        assert_eq!(
            api.consume(&hub.config(), "first.json", "credit")
                .await
                .unwrap()
                .outcome,
            ProviderConsumeResetCreditOutcome::AlreadyRedeemed
        );
        let requests = hub.requests();
        let data: Vec<String> = requests
            .iter()
            .filter(|request| request.is_consume())
            .filter_map(Recorded::data)
            .collect();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0], data[1]);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path.ends_with("/reset-quota"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn rejects_unknown_accounts_without_forwarding_a_redemption() {
        let hub = Hub::start(codex_hub(json!({ "code": "reset" }))).await;
        let api = CliproxyApi::new(Arc::new(RecordingMappers::default()));

        assert!(
            api.consume(&hub.config(), "missing.json", "credit")
                .await
                .is_err()
        );
        assert_eq!(hub.requests().len(), 1);
    }
}
