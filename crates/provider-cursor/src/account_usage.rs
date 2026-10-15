//! Cursor account usage from the dashboard API.
//!
//! Port of `upstream/packages/provider-cursor/src/server/accountUsage.ts`.
//! The reader takes a credential file or an injected token. The macOS
//! Keychain reader is not ported; a keychain outcome is supplied by the
//! caller. The session cookie never follows a redirect off cursor.com, and
//! it is not copied into error strings.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use turin_contracts::usage::{UsageProviderKind, UsageTokenTotals};

const DASHBOARD_USAGE_URL: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
const PAGE_SIZE: u64 = 1000;
const PAGE_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const GENERIC_ERROR: &str = "Cursor account usage could not be read.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSpeed {
    Standard,
    Fast,
    Ultrafast,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    pub provider: UsageProviderKind,
    pub timestamp_ms: f64,
    pub model: String,
    pub rate_model: String,
    pub session_id: String,
    pub totals: UsageTokenTotals,
    pub reported_cost_usd: Option<f64>,
    pub speed: UsageSpeed,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageRead {
    pub account_key: Option<String>,
    pub records: Vec<UsageRecord>,
    pub missing: bool,
    pub error: Option<String>,
}

impl AccountUsageRead {
    fn empty(missing: bool, error: Option<String>) -> Self {
        Self {
            account_key: None,
            records: Vec::new(),
            missing,
            error,
        }
    }
}

/// Why a keychain read did not produce a token. Turin does not open Keychain.
#[derive(Debug, Clone)]
pub enum KeychainOutcome {
    Token(String),
    Missing,
    Timeout,
    ReadFailed,
    NoReader,
}

#[derive(Debug, Clone)]
pub enum CredentialSource {
    File(std::path::PathBuf),
    Keychain(KeychainOutcome),
}

#[derive(Debug, Clone)]
pub struct DashboardRequest {
    pub url: String,
    pub origin: &'static str,
    pub content_type: &'static str,
    pub cookie: String,
    pub body: String,
    /// Implementations must fail redirects instead of following them.
    pub redirect: &'static str,
}

#[derive(Debug, Clone)]
pub struct DashboardResponse {
    pub status: u16,
    pub body: String,
}

#[async_trait]
pub trait DashboardClient: Send + Sync {
    async fn post(&self, request: DashboardRequest) -> Result<DashboardResponse, ()>;
}

pub struct ReqwestDashboard {
    client: reqwest::Client,
}

impl ReqwestDashboard {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
}

#[async_trait]
impl DashboardClient for ReqwestDashboard {
    async fn post(&self, request: DashboardRequest) -> Result<DashboardResponse, ()> {
        let response = self
            .client
            .post(&request.url)
            .header("Origin", request.origin)
            .header("Content-Type", request.content_type)
            .header("Cookie", request.cookie)
            .body(request.body)
            .send()
            .await
            .map_err(|_| ())?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(());
        }
        let body = response.text().await.map_err(|_| ())?;
        Ok(DashboardResponse { status, body })
    }
}

/// Maps Cursor's tiered names to the base model's rate-table key.
pub fn cursor_rate_model(model: &str) -> String {
    static SUFFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?:-thinking)?(?:-(?:none|minimal|low|medium|high|xhigh|max))?(?:-fast)?$",
        )
        .expect("rate suffix")
    });
    let stripped = model.strip_prefix("cursor-").unwrap_or(model);
    let base = SUFFIX.replace(stripped, "");
    if base.starts_with("grok-") {
        format!("xai/{base}")
    } else {
        base.into_owned()
    }
}

pub async fn read_cursor_account_usage(
    source: CredentialSource,
    since_ms: f64,
    end_date: f64,
    client: Arc<dyn DashboardClient>,
) -> AccountUsageRead {
    let token = match load_token(&source) {
        TokenLoad::Missing { error } => return AccountUsageRead::empty(true, error),
        TokenLoad::Failed { error } => return AccountUsageRead::empty(false, Some(error)),
        TokenLoad::Token(token) => token,
    };
    let read = tokio::time::timeout(
        READ_TIMEOUT,
        read_with_token(token, since_ms, end_date, client),
    )
    .await;
    match read {
        Ok(Ok(result)) => result,
        Ok(Err(failure)) => AccountUsageRead {
            account_key: failure.account_key,
            records: Vec::new(),
            missing: false,
            error: Some(GENERIC_ERROR.to_owned()),
        },
        Err(_) => AccountUsageRead::empty(false, Some(GENERIC_ERROR.to_owned())),
    }
}

enum TokenLoad {
    Token(String),
    Missing { error: Option<String> },
    Failed { error: String },
}

fn load_token(source: &CredentialSource) -> TokenLoad {
    match source {
        CredentialSource::File(path) => match fs::read_to_string(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                TokenLoad::Missing { error: None }
            }
            Err(_) => TokenLoad::Failed {
                error: "Cursor credentials could not be read.".to_owned(),
            },
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(value) => match value.get("accessToken").and_then(Value::as_str) {
                    Some(token) if !token.is_empty() => TokenLoad::Token(token.to_owned()),
                    _ => TokenLoad::Missing { error: None },
                },
                Err(_) => TokenLoad::Failed {
                    error: "Cursor credentials could not be read.".to_owned(),
                },
            },
        },
        CredentialSource::Keychain(KeychainOutcome::Token(token)) => {
            TokenLoad::Token(token.clone())
        }
        CredentialSource::Keychain(KeychainOutcome::Missing) => TokenLoad::Missing {
            error: Some(
                "Cursor account history needs a macOS Keychain CLI login on this server."
                    .to_owned(),
            ),
        },
        CredentialSource::Keychain(KeychainOutcome::Timeout) => TokenLoad::Failed {
            error: "Allow Keychain access on the Mac running T3 Code, then refresh.".to_owned(),
        },
        CredentialSource::Keychain(KeychainOutcome::ReadFailed | KeychainOutcome::NoReader) => {
            TokenLoad::Failed {
                error: "Cursor Keychain credentials could not be read.".to_owned(),
            }
        }
    }
}

struct ReadFailure {
    account_key: Option<String>,
}

enum PageOutcome {
    Events { events: Vec<Value>, total: Option<u64> },
    Denied,
}

async fn read_with_token(
    access_token: String,
    since_ms: f64,
    end_date: f64,
    client: Arc<dyn DashboardClient>,
) -> Result<AccountUsageRead, ReadFailure> {
    let subject = jwt_subject(&access_token).map_err(|_| ReadFailure { account_key: None })?;
    let user_id = subject
        .rsplit('|')
        .next()
        .filter(|id| !id.is_empty())
        .ok_or(ReadFailure { account_key: None })?;
    let account_key = sha256_hex(subject.as_bytes());
    if !since_ms.is_finite() || !end_date.is_finite() || since_ms < 0.0 || since_ms > end_date {
        return Err(ReadFailure {
            account_key: Some(account_key),
        });
    }
    let cookie = format!(
        "WorkosCursorSessionToken={}",
        encode_uri_component(&format!("{user_id}::{access_token}"))
    );
    let mut total: Option<u64> = None;
    let mut ahead: VecDeque<tokio::task::JoinHandle<Result<PageOutcome, ()>>> = VecDeque::new();
    let mut pages: Vec<Vec<Value>> = Vec::new();
    let mut page = 1u64;
    let mut completed = false;
    loop {
        let target = prefetch_target(total, page);
        while ahead.len() < target {
            let next = page + ahead.len() as u64;
            let client = Arc::clone(&client);
            let cookie = cookie.clone();
            let known_total = total;
            ahead.push_back(tokio::spawn(async move {
                read_page(client, next, known_total, since_ms, end_date, cookie).await
            }));
        }
        let outcome = if let Some(handle) = ahead.pop_front() {
            match handle.await {
                Ok(result) => result,
                Err(_) => Err(()),
            }
        } else {
            read_page(
                Arc::clone(&client),
                page,
                total,
                since_ms,
                end_date,
                cookie.clone(),
            )
            .await
        };
        let outcome = outcome.map_err(|_| ReadFailure {
            account_key: Some(account_key.clone()),
        })?;
        match outcome {
            PageOutcome::Denied => {
                abort_ahead(&mut ahead);
                return Ok(AccountUsageRead {
                    account_key: Some(account_key),
                    records: Vec::new(),
                    missing: false,
                    error: Some("Sign in to Cursor again to read account usage.".to_owned()),
                });
            }
            PageOutcome::Events { events, total: page_total } => {
                if total.is_none() {
                    total = page_total;
                }
                let short = (events.len() as u64) < PAGE_SIZE;
                pages.push(events);
                if short {
                    completed = true;
                    break;
                }
            }
        }
        page += 1;
    }
    abort_ahead(&mut ahead);
    if !completed {
        return Err(ReadFailure {
            account_key: Some(account_key),
        });
    }
    assemble_records(pages, total, since_ms, end_date, account_key)
}

fn abort_ahead(ahead: &mut VecDeque<tokio::task::JoinHandle<Result<PageOutcome, ()>>>) {
    for handle in ahead.drain(..) {
        handle.abort();
    }
}

fn prefetch_target(total: Option<u64>, page: u64) -> usize {
    let pages = match total {
        None => 0i64,
        Some(total) => i64::from(u32::try_from(total.div_ceil(PAGE_SIZE)).unwrap_or(u32::MAX)),
    };
    let room = pages - i64::try_from(page).unwrap_or(i64::MAX) + 1;
    if room <= 0 {
        0
    } else {
        (room as usize).min(6)
    }
}

async fn read_page(
    client: Arc<dyn DashboardClient>,
    page: u64,
    total: Option<u64>,
    since_ms: f64,
    end_date: f64,
    cookie: String,
) -> Result<PageOutcome, ()> {
    let limit = match total {
        None => 1000,
        Some(total) => total.div_ceil(PAGE_SIZE).saturating_mul(2).saturating_add(1),
    };
    if page > limit {
        return Err(());
    }
    let body = serde_json::json!({
        "page": page,
        "pageSize": PAGE_SIZE,
        "startDate": js_number_string(since_ms),
        "endDate": js_number_string(end_date),
    })
    .to_string();
    let request = DashboardRequest {
        url: DASHBOARD_USAGE_URL.to_owned(),
        origin: "https://cursor.com",
        content_type: "application/json",
        cookie,
        body,
        redirect: "error",
    };
    let response = tokio::time::timeout(PAGE_TIMEOUT, client.post(request))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    if response.status == 401 || response.status == 403 {
        return Ok(PageOutcome::Denied);
    }
    if !(200..300).contains(&response.status) {
        return Err(());
    }
    let parsed: Value = serde_json::from_str(&response.body).map_err(|_| ())?;
    let Value::Object(object) = parsed else {
        return Err(());
    };
    parse_page_object(&object, total).ok_or(())
}

fn parse_page_object(body: &Map<String, Value>, expected_total: Option<u64>) -> Option<PageOutcome> {
    if body.contains_key("error") || body.contains_key("message") || body.contains_key("code") {
        return None;
    }
    let keys: Vec<&str> = body.keys().map(String::as_str).collect();
    let count = if keys.is_empty() {
        Some(0)
    } else {
        match body.get("totalUsageEventsCount") {
            None => None,
            Some(value) => Some(number_u64(value)?),
        }
    };
    if let (Some(expected), Some(count)) = (expected_total, count)
        && expected != count
    {
        return None;
    }
    let events = if keys.is_empty() || (keys.len() == 1 && keys[0] == "totalUsageEventsCount") {
        Vec::new()
    } else {
        match body.get("usageEventsDisplay") {
            Some(Value::Array(events)) => events.clone(),
            _ => return None,
        }
    };
    if events.len() as u64 > PAGE_SIZE {
        return None;
    }
    if count.is_none() && !matches!(body.get("usageEventsDisplay"), Some(Value::Array(_))) {
        return None;
    }
    Some(PageOutcome::Events {
        events,
        total: count,
    })
}

fn number_u64(value: &Value) -> Option<u64> {
    value.as_u64().filter(|count| *count <= i64::MAX as u64)
}

fn assemble_records(
    pages: Vec<Vec<Value>>,
    total: Option<u64>,
    since_ms: f64,
    end_date: f64,
    account_key: String,
) -> Result<AccountUsageRead, ReadFailure> {
    let raw_count: usize = pages.iter().map(Vec::len).sum();
    if let Some(total) = total
        && raw_count < total as usize
    {
        return Err(ReadFailure {
            account_key: Some(account_key),
        });
    }
    let mut removals_remaining = total.map(|total| raw_count - total as usize).unwrap_or(0);
    let mut previous_keys: Vec<String> = Vec::new();
    let mut records = Vec::new();
    let mut occurrences: HashMap<String, u64> = HashMap::new();
    for events in &pages {
        let event_keys = if removals_remaining > 0 {
            events.iter().map(|event| sha256_hex(canonical_json(event).as_bytes())).collect()
        } else {
            Vec::new()
        };
        let removal_count = removals_remaining.min(boundary_overlap(&previous_keys, &event_keys));
        removals_remaining -= removal_count;
        previous_keys = event_keys;
        for raw in events.iter().skip(removal_count) {
            let Some(record) = record_from_event(raw, since_ms, end_date, &account_key, &mut occurrences)
                .map_err(|_| ReadFailure {
                    account_key: Some(account_key.clone()),
                })?
            else {
                continue;
            };
            records.push(record);
        }
    }
    if removals_remaining != 0 {
        return Err(ReadFailure {
            account_key: Some(account_key),
        });
    }
    Ok(AccountUsageRead {
        account_key: Some(account_key),
        records,
        missing: false,
        error: None,
    })
}

fn record_from_event(
    raw: &Value,
    since_ms: f64,
    end_date: f64,
    account_key: &str,
    occurrences: &mut HashMap<String, u64>,
) -> Result<Option<UsageRecord>, ()> {
    let event = raw.as_object().ok_or(())?;
    let Some(usage_value) = event.get("tokenUsage") else {
        return Ok(None);
    };
    if usage_value.is_null() {
        return Ok(None);
    }
    let usage = usage_value.as_object().ok_or(())?;
    for key in [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "totalCents",
    ] {
        if let Some(value) = usage.get(key)
            && !is_non_negative_number(value)
        {
            return Err(());
        }
    }
    let timestamp_ms = match event.get("timestamp") {
        Some(Value::String(text)) if !text.trim().is_empty() => text.trim().parse::<f64>().map_err(|_| ())?,
        Some(Value::Number(number)) => number.as_f64().ok_or(())?,
        _ => return Err(()),
    };
    if !timestamp_ms.is_finite() {
        return Err(());
    }
    let model = event.get("model").and_then(Value::as_str).filter(|model| !model.is_empty()).ok_or(())?;
    if timestamp_ms < since_ms || timestamp_ms > end_date {
        return Ok(None);
    }
    let totals = UsageTokenTotals {
        uncached_input_tokens: tokens(usage.get("inputTokens")),
        cached_input_tokens: tokens(usage.get("cacheReadTokens")),
        cache_creation_tokens: tokens(usage.get("cacheWriteTokens")),
        output_tokens: tokens(usage.get("outputTokens")),
        reasoning_tokens: 0,
    };
    let reported_cost_usd = usage.get("totalCents").and_then(Value::as_f64).map(|cents| cents / 100.0);
    let session_id = event.get("conversationId").and_then(Value::as_str).unwrap_or("").to_owned();
    let identity = billed_event_identity(timestamp_ms, model, &session_id, &totals, reported_cost_usd);
    let key = sha256_hex(identity.as_bytes());
    let occurrence = occurrences.entry(key.clone()).or_insert(0);
    let dedupe_key = format!("cursor-account:{account_key}:{key}:{occurrence}");
    *occurrence += 1;
    Ok(Some(UsageRecord {
        provider: UsageProviderKind::Cursor,
        timestamp_ms,
        model: model.to_owned(),
        rate_model: cursor_rate_model(model),
        session_id,
        totals,
        reported_cost_usd,
        speed: UsageSpeed::Standard,
        dedupe_key,
    }))
}

fn billed_event_identity(
    timestamp_ms: f64,
    model: &str,
    session_id: &str,
    totals: &UsageTokenTotals,
    reported_cost_usd: Option<f64>,
) -> String {
    let cost = match reported_cost_usd {
        Some(cost) => Value::from(cost),
        None => Value::Null,
    };
    serde_json::json!([
        timestamp_ms,
        model,
        session_id,
        {
            "uncachedInputTokens": totals.uncached_input_tokens,
            "cachedInputTokens": totals.cached_input_tokens,
            "cacheCreationTokens": totals.cache_creation_tokens,
            "outputTokens": totals.output_tokens,
            "reasoningTokens": totals.reasoning_tokens,
        },
        cost
    ])
    .to_string()
}

fn is_non_negative_number(value: &Value) -> bool {
    value.as_f64().is_some_and(|number| number.is_finite() && number >= 0.0)
}

fn tokens(value: Option<&Value>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    let Some(number) = value.as_f64() else {
        return 0;
    };
    if number.is_finite() && number > 0.0 {
        number.trunc() as u64
    } else {
        0
    }
}

fn jwt_subject(access_token: &str) -> Result<String, ()> {
    let payload = access_token.split('.').nth(1).ok_or(())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| ())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    value
        .get("sub")
        .and_then(Value::as_str)
        .filter(|subject| !subject.is_empty())
        .map(str::to_owned)
        .ok_or(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(&object[key])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_owned()),
    }
}

fn boundary_overlap(previous: &[String], current: &[String]) -> usize {
    let mut sequence = Vec::with_capacity(current.len() + 1 + previous.len());
    sequence.extend(current.iter().cloned());
    sequence.push(String::new());
    sequence.extend(previous.iter().cloned());
    if sequence.is_empty() {
        return 0;
    }
    let mut lengths = vec![0usize; sequence.len()];
    for index in 1..sequence.len() {
        let mut length = lengths[index - 1];
        while length > 0 && sequence[index] != sequence[length] {
            length = lengths[length - 1];
        }
        if sequence[index] == sequence[length] {
            length += 1;
        }
        lengths[index] = length;
    }
    lengths[sequence.len() - 1]
}

fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
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
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn js_number_string(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < i64::MAX as f64 {
        format!("{}", value as i64)
    } else {
        serde_json::json!(value).to_string()
    }
}

pub fn read_access_token_file(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    fn token(sub: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({"sub": sub}).to_string().as_bytes(),
        );
        format!("header.{payload}.signature")
    }

    struct Script {
        answer: Box<
            dyn Fn(DashboardRequest) -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<DashboardResponse, ()>> + Send>,
                > + Send
                + Sync,
        >,
    }

    #[async_trait]
    impl DashboardClient for Script {
        async fn post(&self, request: DashboardRequest) -> Result<DashboardResponse, ()> {
            (self.answer)(request).await
        }
    }

    fn script<F, Fut>(answer: F) -> Arc<dyn DashboardClient>
    where
        F: Fn(DashboardRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<DashboardResponse, ()>> + Send + 'static,
    {
        Arc::new(Script {
            answer: Box::new(move |request| Box::pin(answer(request))),
        })
    }

    fn json_response(status: u16, body: Value) -> DashboardResponse {
        DashboardResponse {
            status,
            body: body.to_string(),
        }
    }

    #[test]
    fn rate_model_strips_cursor_tiers_and_prefixes_grok() {
        assert_eq!(
            cursor_rate_model("cursor-grok-4.6-high-fast"),
            "xai/grok-4.6"
        );
        assert_eq!(
            cursor_rate_model("claude-fable-5-1-thinking-high"),
            "claude-fable-5-1"
        );
        assert_eq!(cursor_rate_model("claude-sonnet-4-5"), "claude-sonnet-4-5");
        assert_eq!(cursor_rate_model("cursor-gpt-5"), "gpt-5");
    }

    #[tokio::test]
    async fn reads_paginated_history_and_separates_cache_tokens() {
        let access = token("auth|demo");
        let pages = Arc::new(AtomicUsize::new(0));
        let pages_task = Arc::clone(&pages);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, serde_json::json!({"accessToken": access}).to_string()).unwrap();
        let client = script(move |request| {
            pages_task.fetch_add(1, Ordering::SeqCst);
            let page = serde_json::from_str::<Value>(&request.body).unwrap()["page"]
                .as_u64()
                .unwrap() as usize;
            async move {
                assert_eq!(request.url, DASHBOARD_USAGE_URL);
                assert_eq!(request.redirect, "error");
                assert_eq!(request.origin, "https://cursor.com");
                assert_eq!(request.content_type, "application/json");
                assert!(request.cookie.contains("WorkosCursorSessionToken=demo%3A%3A"));
                let length = if page == 1 { 1000 } else { 1 };
                let events: Vec<Value> = (0..length)
                    .map(|index| {
                        serde_json::json!({
                            "timestamp": (1_780_000_000_000u64 + ((page as u64 - 1) * 1000 + index) * 1000).to_string(),
                            "model": "claude-sonnet-4-5",
                            "conversationId": format!("conversation-{page}"),
                            "isHeadless": page == 2,
                            "tokenUsage": {
                                "inputTokens": 10,
                                "outputTokens": 5,
                                "cacheReadTokens": 30,
                                "cacheWriteTokens": 2,
                                "totalCents": 25
                            }
                        })
                    })
                    .collect();
                Ok(json_response(
                    200,
                    serde_json::json!({
                        "totalUsageEventsCount": 1001,
                        "usageEventsDisplay": events
                    }),
                ))
            }
        });
        let result = read_cursor_account_usage(
            CredentialSource::File(path),
            0.0,
            1_781_000_000_000.0,
            client,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(pages.load(Ordering::SeqCst), 2);
        assert_eq!(result.records.len(), 1001);
        assert_eq!(result.records.last().unwrap().session_id, "conversation-2");
        assert_eq!(result.records[0].totals.uncached_input_tokens, 10);
        assert_eq!(result.records[0].totals.cached_input_tokens, 30);
        assert_eq!(result.records[0].totals.cache_creation_tokens, 2);
        assert_eq!(result.records[0].totals.output_tokens, 5);
        assert_eq!(result.records[0].reported_cost_usd, Some(0.25));
        assert!(!result.account_key.unwrap().contains("demo"));
    }

    #[tokio::test]
    async fn accepts_empty_pages_and_rejects_error_envelopes() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, serde_json::json!({"accessToken": access}).to_string()).unwrap();
        for body in [
            serde_json::json!({}),
            serde_json::json!({"totalUsageEventsCount": 0}),
            serde_json::json!({"totalUsageEventsCount": 0, "usageEventsDisplay": []}),
        ] {
            let body = body.clone();
            let client = script(move |_| {
                let body = body.clone();
                async move { Ok(json_response(200, body)) }
            });
            let result = read_cursor_account_usage(
                CredentialSource::File(path.clone()),
                0.0,
                1_781_000_000_000.0,
                client,
            )
            .await;
            assert!(result.error.is_none(), "{body} {:?}", result.error);
            assert!(result.records.is_empty());
            assert!(!result.missing);
        }
        for body in [
            serde_json::json!({"error": "upstream error"}),
            serde_json::json!({"detail": "unknown error envelope"}),
            serde_json::json!({"totalUsageEventsCount": 0, "error": "upstream error"}),
            Value::Null,
            serde_json::json!([]),
            Value::String("invalid".to_owned()),
            serde_json::json!(0),
        ] {
            let body = body.clone();
            let client = script(move |_| {
                let body = body.clone();
                async move { Ok(json_response(200, body)) }
            });
            let result = read_cursor_account_usage(
                CredentialSource::File(path.clone()),
                0.0,
                1_781_000_000_000.0,
                client,
            )
            .await;
            assert!(result.error.is_some(), "{body}");
            assert!(result.records.is_empty());
        }
    }

    #[tokio::test]
    async fn removes_boundary_copies_and_hides_tokens_from_auth_errors() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, serde_json::json!({"accessToken": access.clone()}).to_string()).unwrap();
        let event = |index: u64| {
            serde_json::json!({
                "timestamp": (1_780_000_000_000u64 + index).to_string(),
                "model": "gpt-5",
                "tokenUsage": {"inputTokens": 10, "outputTokens": 5, "totalCents": 1}
            })
        };
        for total in [2000u64, 2001] {
            let requests = AtomicUsize::new(0);
            let client = script(move |_| {
                let n = requests.fetch_add(1, Ordering::SeqCst) + 1;
                let events = if n == 1 {
                    (0..1000).map(event).collect::<Vec<_>>()
                } else if n == 2 {
                    (0..1000).map(|index| event(999 + index)).collect()
                } else {
                    vec![event(1999)]
                };
                async move {
                    Ok(json_response(
                        200,
                        serde_json::json!({
                            "totalUsageEventsCount": total,
                            "usageEventsDisplay": events
                        }),
                    ))
                }
            });
            let result = read_cursor_account_usage(
                CredentialSource::File(path.clone()),
                0.0,
                1_781_000_000_000.0,
                client,
            )
            .await;
            assert!(result.error.is_none(), "{total} {:?}", result.error);
            assert_eq!(result.records.len(), total as usize);
            let duplicates = result
                .records
                .iter()
                .filter(|record| record.timestamp_ms == 1_780_000_000_999.0)
                .count();
            assert_eq!(duplicates, if total == 2000 { 1 } else { 2 });
            let keys: std::collections::HashSet<_> =
                result.records.iter().map(|record| record.dedupe_key.clone()).collect();
            assert_eq!(keys.len(), total as usize);
        }
        let denied = script(move |_| {
            let body = access.clone();
            async move {
                Ok(DashboardResponse {
                    status: 401,
                    body,
                })
            }
        });
        let result = read_cursor_account_usage(
            CredentialSource::File(path.clone()),
            0.0,
            1_781_000_000_000.0,
            denied,
        )
        .await;
        let error = result.error.unwrap();
        assert!(!error.contains("header."));
        assert!(result.records.is_empty());
        let missing = script(|_| async { panic!("no request") });
        let result = read_cursor_account_usage(
            CredentialSource::File(dir.path().join("missing.json")),
            0.0,
            1_781_000_000_000.0,
            missing,
        )
        .await;
        assert!(result.missing);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn keychain_timeout_is_a_fixed_message_and_pages_stay_ordered() {
        let timeout = read_cursor_account_usage(
            CredentialSource::Keychain(KeychainOutcome::Timeout),
            0.0,
            1.0,
            script(|_| async { panic!("no request") }),
        )
        .await;
        assert_eq!(
            timeout.error.as_deref(),
            Some("Allow Keychain access on the Mac running T3 Code, then refresh.")
        );
        assert!(timeout.records.is_empty());

        let access = token("auth|demo");
        let in_flight = Arc::new(AtomicU64::new(0));
        let most = Arc::new(AtomicU64::new(0));
        let in_flight_task = Arc::clone(&in_flight);
        let most_task = Arc::clone(&most);
        let client = script(move |request| {
            let in_flight_task = Arc::clone(&in_flight_task);
            let most_task = Arc::clone(&most_task);
            async move {
                let page = serde_json::from_str::<Value>(&request.body).unwrap()["page"]
                    .as_u64()
                    .unwrap();
                let now = in_flight_task.fetch_add(1, Ordering::SeqCst) + 1;
                most_task.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                in_flight_task.fetch_sub(1, Ordering::SeqCst);
                let length = if page == 9 { 1 } else { 1000 };
                let events: Vec<Value> = (0..length)
                    .map(|index| {
                        serde_json::json!({
                            "timestamp": (1_780_000_000_000u64 + (page - 1) * 1000 + index).to_string(),
                            "model": "gpt-5",
                            "tokenUsage": {"inputTokens": 1}
                        })
                    })
                    .collect();
                Ok(json_response(
                    200,
                    serde_json::json!({
                        "totalUsageEventsCount": 8001,
                        "usageEventsDisplay": events
                    }),
                ))
            }
        });
        let result = read_cursor_account_usage(
            CredentialSource::Keychain(KeychainOutcome::Token(access)),
            0.0,
            1_781_000_000_000.0,
            client,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(most.load(Ordering::SeqCst), 6);
        assert_eq!(result.records.len(), 8001);
        assert_eq!(result.records[0].timestamp_ms, 1_780_000_000_000.0);
        assert_eq!(
            result.records.last().unwrap().timestamp_ms,
            1_780_000_008_000.0
        );
    }
}
