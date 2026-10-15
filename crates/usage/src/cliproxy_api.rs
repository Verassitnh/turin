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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
    use super::*;

    #[test]
    fn credit_redeem_request_id_is_a_stable_uuid_v5_per_account_and_credit() {
        let first = credit_redeem_request_id("acct-1", "credit-a");
        assert_eq!(first, credit_redeem_request_id("acct-1", "credit-a"));
        assert_ne!(first, credit_redeem_request_id("acct-1", "credit-b"));
        assert_ne!(first, credit_redeem_request_id("acct-2", "credit-a"));
        let parsed = Uuid::parse_str(&first).expect("hyphenated uuid");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Sha1));
    }
}
