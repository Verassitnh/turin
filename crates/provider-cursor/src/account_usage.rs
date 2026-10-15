//! Cursor account usage from the dashboard API.
//!
//! Port of `upstream/packages/provider-cursor/src/server/accountUsage.ts`.
//! The reader takes a credential file or an injected token. The macOS
//! Keychain reader is not ported; a keychain outcome is supplied by the
//! caller. The session cookie never follows a redirect off cursor.com, and
//! it is not copied into error strings.

use std::collections::{HashMap, VecDeque};
use std::fs;
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

    /// A read that ended without records. The account key is kept when it is known.
    fn failed(account_key: Option<String>) -> Self {
        Self {
            account_key,
            records: Vec::new(),
            missing: false,
            error: Some(GENERIC_ERROR.to_owned()),
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
    // The credential file is blocking IO, so it stays off the async workers.
    let loaded = tokio::task::spawn_blocking(move || load_token(&source))
        .await
        .unwrap_or(TokenLoad::Failed {
            error: "Cursor credentials could not be read.".to_owned(),
        });
    let token = match loaded {
        TokenLoad::Missing { error } => return AccountUsageRead::empty(true, error),
        TokenLoad::Failed { error } => return AccountUsageRead::empty(false, Some(error)),
        TokenLoad::Token(token) => token,
    };
    // Identity is decoded before the timed read so a timeout can still report it.
    let Some((account_key, user_id)) = account_identity(&token) else {
        return AccountUsageRead::failed(None);
    };
    let cookie = format!(
        "WorkosCursorSessionToken={}",
        encode_uri_component(&format!("{user_id}::{token}"))
    );
    let read = tokio::time::timeout(
        READ_TIMEOUT,
        read_with_token(account_key.clone(), cookie, since_ms, end_date, client),
    )
    .await;
    read.unwrap_or_else(|_| AccountUsageRead::failed(Some(account_key)))
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

enum PageOutcome {
    Events {
        events: Vec<Value>,
        total: Option<u64>,
    },
    Denied,
}

async fn read_with_token(
    account_key: String,
    cookie: String,
    since_ms: f64,
    end_date: f64,
    client: Arc<dyn DashboardClient>,
) -> AccountUsageRead {
    if !since_ms.is_finite() || !end_date.is_finite() || since_ms < 0.0 || since_ms > end_date {
        return AccountUsageRead::failed(Some(account_key));
    }
    let mut total: Option<u64> = None;
    let mut ahead = Prefetch::default();
    let mut pages: Vec<Vec<Value>> = Vec::new();
    let mut page = 1u64;
    loop {
        let target = prefetch_target(total, page);
        while ahead.pages.len() < target {
            let next = page + ahead.pages.len() as u64;
            let client = Arc::clone(&client);
            let cookie = cookie.clone();
            let known_total = total;
            ahead.pages.push_back(tokio::spawn(async move {
                read_page(client, next, known_total, since_ms, end_date, cookie).await
            }));
        }
        let outcome = match ahead.pages.front_mut() {
            // Await the handle in place so dropping this future still reaches it through `ahead`.
            Some(handle) => {
                let joined = handle.await;
                ahead.pages.pop_front();
                joined.unwrap_or(Err(()))
            }
            None => {
                read_page(
                    Arc::clone(&client),
                    page,
                    total,
                    since_ms,
                    end_date,
                    cookie.clone(),
                )
                .await
            }
        };
        let Ok(outcome) = outcome else {
            ahead.shutdown().await;
            return AccountUsageRead::failed(Some(account_key));
        };
        match outcome {
            PageOutcome::Denied => {
                ahead.shutdown().await;
                return AccountUsageRead {
                    account_key: Some(account_key),
                    records: Vec::new(),
                    missing: false,
                    error: Some("Sign in to Cursor again to read account usage.".to_owned()),
                };
            }
            PageOutcome::Events {
                events,
                total: page_total,
            } => {
                if total.is_none() {
                    total = page_total;
                }
                let short = (events.len() as u64) < PAGE_SIZE;
                pages.push(events);
                if short {
                    break;
                }
            }
        }
        page += 1;
    }
    ahead.shutdown().await;
    assemble_records(pages, total, since_ms, end_date, account_key)
}

/// Prefetched page reads. Dropping it aborts every read still in flight, so a
/// read that is cancelled by its caller does not leave cookie-bearing requests
/// running. Upstream forks them into the read's scope for the same reason.
#[derive(Default)]
struct Prefetch {
    pages: VecDeque<tokio::task::JoinHandle<Result<PageOutcome, ()>>>,
}

impl Prefetch {
    /// Aborts the reads still in flight and waits until each has stopped, so
    /// no request is open once the read has returned.
    async fn shutdown(&mut self) {
        for handle in &self.pages {
            handle.abort();
        }
        for handle in self.pages.drain(..) {
            let _ = handle.await;
        }
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        for handle in &self.pages {
            handle.abort();
        }
    }
}

fn prefetch_target(total: Option<u64>, page: u64) -> usize {
    let pages = match total {
        None => 0i64,
        Some(total) => i64::from(u32::try_from(total.div_ceil(PAGE_SIZE)).unwrap_or(u32::MAX)),
    };
    let room = pages - i64::try_from(page).unwrap_or(i64::MAX) + 1;
    if room <= 0 { 0 } else { (room as usize).min(6) }
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
        Some(total) => total
            .div_ceil(PAGE_SIZE)
            .saturating_mul(2)
            .saturating_add(1),
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

fn parse_page_object(
    body: &Map<String, Value>,
    expected_total: Option<u64>,
) -> Option<PageOutcome> {
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
) -> AccountUsageRead {
    let raw_count: usize = pages.iter().map(Vec::len).sum();
    if let Some(total) = total
        && raw_count < total as usize
    {
        return AccountUsageRead::failed(Some(account_key));
    }
    let mut removals_remaining = total.map(|total| raw_count - total as usize).unwrap_or(0);
    let mut previous_keys: Vec<String> = Vec::new();
    let mut records = Vec::new();
    let mut occurrences: HashMap<String, u64> = HashMap::new();
    for events in &pages {
        let event_keys = if removals_remaining > 0 {
            events
                .iter()
                .map(|event| sha256_hex(canonical_json(event).as_bytes()))
                .collect()
        } else {
            Vec::new()
        };
        let removal_count = removals_remaining.min(boundary_overlap(&previous_keys, &event_keys));
        removals_remaining -= removal_count;
        previous_keys = event_keys;
        for raw in events.iter().skip(removal_count) {
            let record =
                match record_from_event(raw, since_ms, end_date, &account_key, &mut occurrences) {
                    Ok(Some(record)) => record,
                    Ok(None) => continue,
                    Err(()) => return AccountUsageRead::failed(Some(account_key)),
                };
            records.push(record);
        }
    }
    if removals_remaining != 0 {
        return AccountUsageRead::failed(Some(account_key));
    }
    AccountUsageRead {
        account_key: Some(account_key),
        records,
        missing: false,
        error: None,
    }
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
        Some(Value::String(text)) if !text.trim().is_empty() => {
            text.trim().parse::<f64>().map_err(|_| ())?
        }
        Some(Value::Number(number)) => number.as_f64().ok_or(())?,
        _ => return Err(()),
    };
    if !timestamp_ms.is_finite() {
        return Err(());
    }
    let model = event
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or(())?;
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
    let reported_cost_usd = usage
        .get("totalCents")
        .and_then(Value::as_f64)
        .map(|cents| cents / 100.0);
    let session_id = event
        .get("conversationId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let identity =
        billed_event_identity(timestamp_ms, model, &session_id, &totals, reported_cost_usd);
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

/// `JSON.stringify([timestampMs, model, sessionId, totals, reportedCostUsd])`,
/// byte for byte. serde_json writes integral floats as `1700000000000.0`, so
/// numbers go through [`js_number_string`].
fn billed_event_identity(
    timestamp_ms: f64,
    model: &str,
    session_id: &str,
    totals: &UsageTokenTotals,
    reported_cost_usd: Option<f64>,
) -> String {
    let cost = reported_cost_usd.map_or_else(|| "null".to_owned(), js_number_string);
    format!(
        r#"[{},{},{},{{"uncachedInputTokens":{},"cachedInputTokens":{},"cacheCreationTokens":{},"outputTokens":{},"reasoningTokens":{}}},{}]"#,
        js_number_string(timestamp_ms),
        json_string(model),
        json_string(session_id),
        totals.uncached_input_tokens,
        totals.cached_input_tokens,
        totals.cache_creation_tokens,
        totals.output_tokens,
        totals.reasoning_tokens,
        cost,
    )
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn is_non_negative_number(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number >= 0.0)
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

/// The account key (a hash of the JWT subject, never the token) and the user
/// id that goes in the session cookie. `None` means the token is unusable.
fn account_identity(access_token: &str) -> Option<(String, String)> {
    let subject = jwt_subject(access_token).ok()?;
    let user_id = subject
        .rsplit('|')
        .next()
        .filter(|id| !id.is_empty())?
        .to_owned();
    Some((sha256_hex(subject.as_bytes()), user_id))
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

/// `Number.prototype.toString` for finite values, which is what `String(n)` and
/// `JSON.stringify(n)` produce in upstream: `1700000000000`, `1e+21`, `1e-7`.
fn js_number_string(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // Rust's `{:e}` prints the shortest digits that round-trip, as JS does.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("scientific form has an exponent");
    let exponent: i32 = exponent.parse().expect("exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign_e = if e < 0 { "-" } else { "+" };
        let (head, tail) = digits.split_at(1);
        let mantissa = if tail.is_empty() {
            head.to_owned()
        } else {
            format!("{head}.{tail}")
        };
        format!("{mantissa}e{sign_e}{}", e.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    fn token(sub: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({"sub": sub}).to_string().as_bytes());
        format!("header.{payload}.signature")
    }

    type ScriptFuture =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<DashboardResponse, ()>> + Send>>;

    struct Script {
        answer: Box<dyn Fn(DashboardRequest) -> ScriptFuture + Send + Sync>,
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
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
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
                assert!(
                    request
                        .cookie
                        .contains("WorkosCursorSessionToken=demo%3A%3A")
                );
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
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
        for body in [
            serde_json::json!({}),
            serde_json::json!({"totalUsageEventsCount": 0}),
            serde_json::json!({"totalUsageEventsCount": 0, "usageEventsDisplay": []}),
        ] {
            let client = script({
                let body = body.clone();
                move |_| {
                    let body = body.clone();
                    async move { Ok(json_response(200, body)) }
                }
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
            let client = script({
                let body = body.clone();
                move |_| {
                    let body = body.clone();
                    async move { Ok(json_response(200, body)) }
                }
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
        fs::write(
            &path,
            serde_json::json!({"accessToken": access.clone()}).to_string(),
        )
        .unwrap();
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
            let keys: std::collections::HashSet<_> = result
                .records
                .iter()
                .map(|record| record.dedupe_key.clone())
                .collect();
            assert_eq!(keys.len(), total as usize);
        }
        let denied = script(move |_| {
            let body = access.clone();
            async move { Ok(DashboardResponse { status: 401, body }) }
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

    /// Marks a dashboard request that has started and not yet finished. An
    /// aborted request drops its future, which is what ends it here.
    struct OpenRequest(Arc<AtomicUsize>);

    impl OpenRequest {
        fn start(open: &Arc<AtomicUsize>) -> Self {
            open.fetch_add(1, Ordering::SeqCst);
            Self(Arc::clone(open))
        }
    }

    impl Drop for OpenRequest {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn js_number_string_matches_json_stringify() {
        assert_eq!(js_number_string(1_700_000_000_000.0), "1700000000000");
        assert_eq!(js_number_string(0.25), "0.25");
        assert_eq!(js_number_string(-0.0), "0");
        assert_eq!(js_number_string(123.456), "123.456");
        assert_eq!(js_number_string(0.000_001), "0.000001");
        assert_eq!(js_number_string(1e-7), "1e-7");
        assert_eq!(js_number_string(1e20), "100000000000000000000");
        assert_eq!(js_number_string(1e21), "1e+21");
        assert_eq!(js_number_string(1.5e300), "1.5e+300");
        assert_eq!(js_number_string(-42.0), "-42");
    }

    #[test]
    fn billed_identity_is_the_json_stringify_of_the_fields() {
        let totals = UsageTokenTotals {
            uncached_input_tokens: 10,
            cached_input_tokens: 30,
            cache_creation_tokens: 2,
            output_tokens: 5,
            reasoning_tokens: 0,
        };
        assert_eq!(
            billed_event_identity(1_700_000_000_000.0, "gpt-5", "conv", &totals, Some(0.25)),
            r#"[1700000000000,"gpt-5","conv",{"uncachedInputTokens":10,"cachedInputTokens":30,"cacheCreationTokens":2,"outputTokens":5,"reasoningTokens":0},0.25]"#
        );
        assert_eq!(
            billed_event_identity(1_700_000_000_000.0, "gpt-5", "", &totals, None),
            r#"[1700000000000,"gpt-5","",{"uncachedInputTokens":10,"cachedInputTokens":30,"cacheCreationTokens":2,"outputTokens":5,"reasoningTokens":0},null]"#
        );
    }

    #[tokio::test]
    async fn failed_page_read_leaves_no_request_open() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
        let full_page = serde_json::json!({
            "totalUsageEventsCount": 8001,
            "usageEventsDisplay": (0..1000)
                .map(|_| serde_json::json!({"tokenUsage": null}))
                .collect::<Vec<_>>(),
        });
        // Page 3 fails while pages 4 onwards are still waiting on Cursor.
        for (second_status, second_body, expected) in [
            (
                200,
                full_page.clone(),
                "Cursor account usage could not be read.",
            ),
            (
                401,
                Value::Null,
                "Sign in to Cursor again to read account usage.",
            ),
        ] {
            let open = Arc::new(AtomicUsize::new(0));
            let open_task = Arc::clone(&open);
            let first = full_page.clone();
            let client = script(move |request| {
                let page = serde_json::from_str::<Value>(&request.body).unwrap()["page"]
                    .as_u64()
                    .unwrap();
                let open = Arc::clone(&open_task);
                let first = first.clone();
                let second_body = second_body.clone();
                async move {
                    match page {
                        1 => Ok(json_response(200, first)),
                        2 => Ok(json_response(second_status, second_body)),
                        3 => Err(()),
                        _ => {
                            let _request = OpenRequest::start(&open);
                            std::future::pending::<()>().await;
                            Err(())
                        }
                    }
                }
            });
            let result = read_cursor_account_usage(
                CredentialSource::File(path.clone()),
                0.0,
                1_781_000_000_000.0,
                client,
            )
            .await;
            assert_eq!(result.error.as_deref(), Some(expected));
            assert!(result.records.is_empty());
            assert_eq!(open.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn reads_past_one_hundred_pages_until_a_short_page() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_task = Arc::clone(&requests);
        let client = script(move |_| {
            let seen = requests_task.fetch_add(1, Ordering::SeqCst) + 1;
            let length = if seen <= 100 { 1000 } else { 1 };
            let events: Vec<Value> = (0..length)
                .map(|_| serde_json::json!({"tokenUsage": null}))
                .collect();
            async move {
                Ok(json_response(
                    200,
                    serde_json::json!({
                        "totalUsageEventsCount": 100_001,
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
        assert_eq!(requests.load(Ordering::SeqCst), 101);
        assert!(result.records.is_empty());
    }

    #[tokio::test]
    async fn full_page_at_the_reported_count_still_needs_a_terminal_page() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_task = Arc::clone(&requests);
        let client = script(move |_| {
            let seen = requests_task.fetch_add(1, Ordering::SeqCst) + 1;
            let body = if seen == 1 {
                let events: Vec<Value> = (0..1000)
                    .map(|index| {
                        serde_json::json!({
                            "timestamp": (1_780_000_000_000u64 + index).to_string(),
                            "model": "gpt-5",
                            "tokenUsage": {"inputTokens": 10, "outputTokens": 5}
                        })
                    })
                    .collect();
                serde_json::json!({"totalUsageEventsCount": 1000, "usageEventsDisplay": events})
            } else {
                serde_json::json!({"totalUsageEventsCount": 1000})
            };
            async move { Ok(json_response(200, body)) }
        });
        let result = read_cursor_account_usage(
            CredentialSource::File(path),
            0.0,
            1_781_000_000_000.0,
            client,
        )
        .await;
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(result.records.len(), 1000);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn truncated_pages_are_not_presented_as_complete_history() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
        let client = script(|_| async {
            Ok(json_response(
                200,
                serde_json::json!({"totalUsageEventsCount": 101, "usageEventsDisplay": []}),
            ))
        });
        let result = read_cursor_account_usage(
            CredentialSource::File(path),
            0.0,
            1_781_000_000_000.0,
            client,
        )
        .await;
        assert!(result.error.is_some());
        assert!(result.records.is_empty());
    }

    #[tokio::test]
    async fn unreadable_credentials_stop_before_any_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, "{not json").unwrap();
        let client = script(|_| async { panic!("no request") });
        let result = read_cursor_account_usage(
            CredentialSource::File(path),
            0.0,
            1_781_000_000_000.0,
            client,
        )
        .await;
        assert_eq!(
            result.error.as_deref(),
            Some("Cursor credentials could not be read.")
        );
        assert!(!result.missing);
        assert!(result.account_key.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn read_that_never_answers_keeps_the_account_key() {
        let access = token("auth|demo");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::json!({"accessToken": access}).to_string(),
        )
        .unwrap();
        let client =
            script(|_| async { std::future::pending::<Result<DashboardResponse, ()>>().await });
        let result = read_cursor_account_usage(
            CredentialSource::File(path),
            0.0,
            1_781_000_000_000.0,
            client,
        )
        .await;
        assert_eq!(
            result.error.as_deref(),
            Some("Cursor account usage could not be read.")
        );
        assert_eq!(result.account_key, Some(sha256_hex(b"auth|demo")));
    }
}
