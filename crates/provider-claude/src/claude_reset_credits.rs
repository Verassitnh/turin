//! Port of upstream/apps/server/src/provider/claudeResetCredits.ts.
//!
//! Claude banked resets (the CLI's `cedar_ember` program). The CLI reads the
//! grants from the OAuth usage endpoint and claims one against the
//! organization. This module does the same with the credentials the CLI keeps
//! in its config directory. macOS keeps them in the keychain, so there the
//! feature is not offered.
//!
//! The API base and request timeouts are fields so tests can point the calls
//! at a local server. Production uses [`ClaudeResetCreditsEndpoint::production`].
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use turin_contracts::provider_usage_limits::{
    ProviderConsumeResetCreditOutcome, ServerProviderResetCredits,
};
use turin_provider_core::host_process::Platform;

const PROGRAM: &str = "cedar_ember";

/// Where the reset calls go and how long they may take.
#[derive(Debug, Clone)]
pub struct ClaudeResetCreditsEndpoint {
    pub api_base: String,
    /// Budget for reading the banked resets.
    pub read_timeout: Duration,
    /// Budget for one claim, including its body.
    pub claim_timeout: Duration,
}

impl ClaudeResetCreditsEndpoint {
    pub fn production() -> Self {
        Self {
            api_base: "https://api.anthropic.com".to_owned(),
            read_timeout: Duration::from_secs(10),
            claim_timeout: Duration::from_secs(25),
        }
    }
}

/// `RESET_CREDIT_FAILURES`, keyed by reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeResetCreditFailure {
    MalformedCredit,
    LoginUnreadable,
    AccountUnreadable,
    SignedOut,
    RateLimited,
    CoolingDown,
    Unconfirmed,
    RequestFailed,
}

impl ClaudeResetCreditFailure {
    pub fn message(self) -> &'static str {
        match self {
            Self::MalformedCredit => "Claude returned a malformed reset credit.",
            Self::LoginUnreadable => "Claude could not read its login.",
            Self::AccountUnreadable => "Claude could not read its account.",
            Self::SignedOut => "Sign in to Claude again to redeem resets.",
            Self::RateLimited => "Claude is rate limiting resets. Try again soon.",
            Self::CoolingDown => "Claude resets are cooling down. Try again later.",
            Self::Unconfirmed => {
                "Claude could not confirm the reset. If you are still limited in a moment, try again."
            }
            Self::RequestFailed => "Claude could not redeem the reset.",
        }
    }
}

/// `ClaudeResetCreditError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .reason.message())]
pub struct ClaudeResetCreditError {
    pub reason: ClaudeResetCreditFailure,
}

/// Every failure except `RequestFailed` and `Unconfirmed` is final: Claude
/// answered, or nothing was sent. An unanswered or unconfirmed claim retries
/// with the same request id.
pub fn is_settled_claude_reset_credit_failure(error: &ClaudeResetCreditError) -> bool {
    !matches!(
        error.reason,
        ClaudeResetCreditFailure::RequestFailed | ClaudeResetCreditFailure::Unconfirmed
    )
}

fn grant_id_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[a-z0-9_-]{1,40}$").expect("grant id regex"))
}

fn request_id_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]{1,64}$").expect("request id regex"))
}

fn complete_timestamp_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$")
            .expect("timestamp regex")
    })
}

/// `isFutureTimestamp`: a complete, calendar-valid timestamp after `now_ms`.
/// Rejects impossible dates such as February 30.
fn is_future_timestamp(value: &str, now_ms: i64) -> bool {
    if !complete_timestamp_pattern().is_match(value) {
        return false;
    }
    let Some(day) = value.get(..10) else {
        return false;
    };
    if NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err() {
        return false;
    }
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.timestamp_millis() > now_ms)
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
struct CedarEmber {
    eligible: bool,
    #[serde(default)]
    grants: Option<Vec<Value>>,
    #[serde(default)]
    next_grant_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Grant {
    id: String,
    resets_left: u64,
    #[serde(default)]
    ends_at: Option<String>,
    #[serde(default)]
    paused: Option<bool>,
    #[serde(default)]
    usable_now: Option<bool>,
}

fn decode_grant(raw: &Value) -> Option<Grant> {
    let grant: Grant = serde_json::from_value(raw.clone()).ok()?;
    grant_id_pattern().is_match(&grant.id).then_some(grant)
}

/// `claudeResetCreditsToContract`: counts live grants and pins the next usable one.
/// Grants that are paused or past `ends_at` cannot be claimed and do not count.
pub fn claude_reset_credits_to_contract(
    block: Option<&Value>,
    now_ms: i64,
) -> Option<ServerProviderResetCredits> {
    let parsed: CedarEmber = serde_json::from_value(block?.clone()).ok()?;
    if !parsed.eligible {
        return None;
    }
    let live: Vec<Grant> = parsed
        .grants
        .unwrap_or_default()
        .iter()
        .filter_map(decode_grant)
        .filter(|grant| {
            grant.paused != Some(true)
                && grant.usable_now == Some(true)
                && grant
                    .ends_at
                    .as_deref()
                    .is_none_or(|ends_at| is_future_timestamp(ends_at, now_ms))
        })
        .collect();
    let next = live
        .iter()
        .find(|grant| Some(grant.id.as_str()) == parsed.next_grant_id.as_deref());
    let next_expires_at = next
        .and_then(|grant| grant.ends_at.as_deref())
        .and_then(|ends_at| DateTime::parse_from_rfc3339(ends_at).ok())
        .map(|date| {
            date.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        });
    Some(ServerProviderResetCredits {
        available_count: if next.is_some() {
            live.iter().map(|grant| grant.resets_left).sum()
        } else {
            0
        },
        next_expires_at,
        next_credit_id: next
            .and_then(|grant| turin_contracts::TrimmedNonEmptyString::new(&grant.id).ok()),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeJsonCredentials {
    #[serde(default)]
    claude_ai_oauth: Option<OauthCredentials>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthCredentials {
    #[serde(default)]
    access_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAccountConfig {
    #[serde(default)]
    oauth_account: Option<OauthAccount>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthAccount {
    #[serde(default)]
    organization_uuid: Option<String>,
}

/// Reads a JSON file, treating a missing file as `{}` as upstream does.
async fn read_json_file<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => serde_json::from_str(&text).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error.to_string()),
    }
}

/// `readAccessToken`: the OAuth token from `.credentials.json`. macOS keeps it
/// in the keychain, so there is none to read.
async fn read_access_token(
    config_dir: &Path,
    platform: Platform,
) -> Result<Option<String>, String> {
    if platform == Platform::Darwin {
        return Ok(None);
    }
    let credentials: ClaudeJsonCredentials =
        read_json_file(&config_dir.join(".credentials.json")).await?;
    Ok(credentials
        .claude_ai_oauth
        .and_then(|oauth| oauth.access_token)
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty()))
}

fn claude_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    version: &str,
) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(
            "user-agent",
            format!("claude-cli/{version} (external, cli)"),
        )
}

/// `readClaudeResetCredits`: the banked resets for the login in `config_dir`.
/// Any failure reads as "no resets", so the usage bars never break on this
/// optional extra.
pub async fn read_claude_reset_credits(
    client: &reqwest::Client,
    endpoint: &ClaudeResetCreditsEndpoint,
    config_dir: &Path,
    version: &str,
    platform: Platform,
    now_ms: i64,
) -> Option<ServerProviderResetCredits> {
    let read = async {
        let token = read_access_token(config_dir, platform).await.ok()??;
        let url = format!("{}/api/oauth/usage", endpoint.api_base);
        let response = claude_request(client, reqwest::Method::GET, &url, &token, version)
            .query(&[("cedar_ember", "1"), ("skip_spend", "1")])
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let body: Value = response.json().await.ok()?;
        claude_reset_credits_to_contract(body.get("cedar_ember").filter(|v| !v.is_null()), now_ms)
    };
    tokio::time::timeout(endpoint.read_timeout, read)
        .await
        .ok()
        .flatten()
}

/// `readClaudeOrganizationId`: the organization the login is signed in to.
pub async fn read_claude_organization_id(account_config_path: &Path) -> Option<String> {
    let config: ClaudeAccountConfig = read_json_file(account_config_path).await.ok()?;
    config
        .oauth_account
        .and_then(|account| account.organization_uuid)
        .map(|uuid| uuid.trim().to_owned())
        .filter(|uuid| !uuid.is_empty())
}

/// `claudeAccountConfigPath`: the CLI's account record beside its config
/// directory, or in the home directory by default.
pub fn claude_account_config_path(config_dir: Option<&Path>, home: &str) -> PathBuf {
    match config_dir {
        Some(dir) => dir.join(".claude.json"),
        None => Path::new(home).join(".claude.json"),
    }
}

/// Inputs to [`consume_claude_reset_credit`].
pub struct ConsumeClaudeResetCreditInput<'a> {
    pub config_dir: &'a Path,
    pub account_config_path: &'a Path,
    pub version: &'a str,
    pub grant_id: &'a str,
    /// The idempotency key: a retry with the same id is the same claim.
    pub request_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct ClaimResponse {
    result: String,
}

/// `consumeClaudeResetCredit`. Ids are checked before anything is sent.
pub async fn consume_claude_reset_credit(
    client: &reqwest::Client,
    endpoint: &ClaudeResetCreditsEndpoint,
    platform: Platform,
    input: ConsumeClaudeResetCreditInput<'_>,
) -> Result<ProviderConsumeResetCreditOutcome, ClaudeResetCreditError> {
    let failure = |reason| ClaudeResetCreditError { reason };
    if !grant_id_pattern().is_match(input.grant_id)
        || !request_id_pattern().is_match(input.request_id)
    {
        return Err(failure(ClaudeResetCreditFailure::MalformedCredit));
    }
    let token = read_access_token(input.config_dir, platform)
        .await
        .map_err(|_| failure(ClaudeResetCreditFailure::LoginUnreadable))?;
    let config: ClaudeAccountConfig = read_json_file(input.account_config_path)
        .await
        .map_err(|_| failure(ClaudeResetCreditFailure::AccountUnreadable))?;
    let organization = config
        .oauth_account
        .and_then(|account| account.organization_uuid)
        .map(|uuid| uuid.trim().to_owned())
        .filter(|uuid| !uuid.is_empty());
    let (Some(token), Some(organization)) = (token, organization) else {
        return Err(failure(ClaudeResetCreditFailure::SignedOut));
    };

    let url = format!(
        "{}/api/organizations/{}/reset_rate_limits",
        endpoint.api_base,
        urlencoding_component(&organization)
    );
    let body = json!({
        "program": PROGRAM,
        "grant_id": input.grant_id,
        "request_id": input.request_id,
    });
    let claim = async {
        let response = claude_request(client, reqwest::Method::POST, &url, &token, input.version)
            .json(&body)
            .send()
            .await
            .map_err(|_| failure(ClaudeResetCreditFailure::RequestFailed))?;
        match response.status().as_u16() {
            429 => return Err(failure(ClaudeResetCreditFailure::RateLimited)),
            401 | 403 => return Err(failure(ClaudeResetCreditFailure::SignedOut)),
            _ => {}
        }
        let response = response
            .error_for_status()
            .map_err(|_| failure(ClaudeResetCreditFailure::RequestFailed))?;
        let claim: ClaimResponse = response
            .json()
            .await
            .map_err(|_| failure(ClaudeResetCreditFailure::RequestFailed))?;
        match claim.result.as_str() {
            "cooldown" => Err(failure(ClaudeResetCreditFailure::CoolingDown)),
            // Claude could not say whether the claim landed, so, like the CLI,
            // keep the request id and let the retry ask about the same claim.
            "unavailable" => Err(failure(ClaudeResetCreditFailure::Unconfirmed)),
            "reset" => Ok(ProviderConsumeResetCreditOutcome::Reset),
            "not_limited" => Ok(ProviderConsumeResetCreditOutcome::NothingToReset),
            "already_used" => Ok(ProviderConsumeResetCreditOutcome::AlreadyRedeemed),
            "ineligible" => Ok(ProviderConsumeResetCreditOutcome::NoCredit),
            _ => Err(failure(ClaudeResetCreditFailure::RequestFailed)),
        }
    };
    tokio::time::timeout(endpoint.claim_timeout, claim)
        .await
        .map_err(|_| failure(ClaudeResetCreditFailure::RequestFailed))?
}

/// `encodeURIComponent` for a path segment.
fn urlencoding_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests;
