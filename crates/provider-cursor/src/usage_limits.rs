//! Port of upstream/packages/provider-cursor/src/server/usageLimits.ts.
//!
//! Cursor's plan limit windows from the dashboard API. Upstream threads the
//! HTTP client, file system, platform and Keychain through Effect services;
//! here they are the arguments of [`read_cursor_usage_limits`] so tests can
//! stand in for each.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use turin_contracts::provider_usage_limits::{
    ServerProviderUsageLimits, ServerProviderUsageUnavailableReason, ServerProviderUsageWindow,
    ServerProviderUsageWindowKind,
};
use turin_contracts::{IsoDateTime, TrimmedNonEmptyString};
use turin_provider_core::host_process::{Platform, ProcessEnv};
use turin_provider_core::usage_limits::{
    clamp_percent, make_unavailable_usage_limits, make_usage_limits,
};
use turin_shared::usage_limits::CURSOR_USAGE_WINDOWS;

use crate::cursor_keychain::CursorKeychain;

const DEFAULT_CURSOR_API_ENDPOINT: &str = "https://api2.cursor.sh";
const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum BillingCycleEnd {
    Text(String),
    Number(f64),
}

impl BillingCycleEnd {
    /// JS `Number(value)`: a numeric string converts, anything else is NaN.
    fn as_number(&self) -> f64 {
        match self {
            Self::Number(number) => *number,
            Self::Text(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    0.0
                } else {
                    trimmed.parse().unwrap_or(f64::NAN)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPlanUsage {
    pub total_percent_used: Option<f64>,
    pub auto_percent_used: Option<f64>,
    pub api_percent_used: Option<f64>,
}

impl CursorPlanUsage {
    fn percent(&self, id: &str) -> Option<f64> {
        match id {
            "totalPercentUsed" => self.total_percent_used,
            "autoPercentUsed" => self.auto_percent_used,
            "apiPercentUsed" => self.api_percent_used,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorUsageResponse {
    pub billing_cycle_end: Option<BillingCycleEnd>,
    pub plan_usage: Option<CursorPlanUsage>,
}

fn iso_millis(ms: i64) -> Option<IsoDateTime> {
    let date: DateTime<Utc> = Utc.timestamp_millis_opt(ms).single()?;
    Some(date.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn now_iso() -> IsoDateTime {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Cursor's dashboard percentages include bonus usage; spend / limit does not.
pub fn cursor_usage_response_to_limits(
    response: &CursorUsageResponse,
    checked_at: &str,
) -> ServerProviderUsageLimits {
    let cycle_end = response
        .billing_cycle_end
        .as_ref()
        .map_or(f64::NAN, BillingCycleEnd::as_number);
    let resets_at = if cycle_end > 0.0 && cycle_end.is_finite() {
        iso_millis(cycle_end as i64)
    } else {
        None
    };
    let mut windows = Vec::new();
    if let Some(plan_usage) = &response.plan_usage {
        for window in &CURSOR_USAGE_WINDOWS {
            let Some(used_percent) = plan_usage.percent(window.id) else {
                continue;
            };
            if !used_percent.is_finite() {
                continue;
            }
            let (Ok(id), Ok(label)) = (
                TrimmedNonEmptyString::new(window.id),
                TrimmedNonEmptyString::new(window.label),
            ) else {
                continue;
            };
            windows.push(ServerProviderUsageWindow {
                id,
                kind: ServerProviderUsageWindowKind::Monthly,
                label,
                used_percent: clamp_percent(used_percent),
                resets_at: resets_at.clone(),
                window_duration_mins: None,
            });
        }
    }
    if windows.is_empty() {
        make_unavailable_usage_limits(
            checked_at.to_owned(),
            ServerProviderUsageUnavailableReason::Unsupported,
            None,
        )
    } else {
        make_usage_limits(checked_at.to_owned(), windows)
    }
}

/// The `GetCurrentPeriodUsage` call. Production uses reqwest; tests inject a stub.
#[async_trait]
pub trait CursorUsageHttp: Send + Sync {
    /// The response body when the status is 2xx; any failure is `Err` and
    /// carries nothing, so credentials and bodies cannot leak into a message.
    async fn current_period_usage(&self, endpoint: &str, token: &str) -> Result<Value, ()>;
}

pub struct ReqwestCursorUsageHttp {
    client: reqwest::Client,
}

impl ReqwestCursorUsageHttp {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
        })
    }
}

#[async_trait]
impl CursorUsageHttp for ReqwestCursorUsageHttp {
    async fn current_period_usage(&self, endpoint: &str, token: &str) -> Result<Value, ()> {
        let response = self
            .client
            .post(format!(
                "{endpoint}/aiserver.v1.DashboardService/GetCurrentPeriodUsage"
            ))
            .bearer_auth(token)
            .header("connect-protocol-version", "1")
            .header("x-cursor-client-type", "cli")
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        response.json::<Value>().await.map_err(|_| ())
    }
}

/// What `readCursorUsageLimits` reads from its Effect context.
pub struct CursorUsageLimitsHost<'a> {
    pub platform: Platform,
    pub home_directory: &'a str,
    pub keychain: &'a CursorKeychain,
    pub http: &'a dyn CursorUsageHttp,
}

fn unsupported(checked_at: &str, message: Option<&str>) -> ServerProviderUsageLimits {
    make_unavailable_usage_limits(
        checked_at.to_owned(),
        ServerProviderUsageUnavailableReason::Unsupported,
        message,
    )
}

fn env_trimmed<'a>(environment: &'a ProcessEnv, key: &str) -> &'a str {
    environment.get(key).map_or("", |value| value.trim())
}

/// `Cursor`'s `auth.json` for the platform, read only when nothing else names a login.
fn credentials_path(platform: Platform, environment: &ProcessEnv, home_directory: &str) -> PathBuf {
    let non_empty = |key: &str| environment.get(key).filter(|value| !value.is_empty());
    let home = if platform == Platform::Win32 {
        non_empty("USERPROFILE")
    } else {
        non_empty("HOME")
    }
    .map_or_else(|| home_directory.to_owned(), Clone::clone);
    let directory = match platform {
        Platform::Win32 => non_empty("APPDATA")
            .map_or_else(
                || PathBuf::from(&home).join("AppData").join("Roaming"),
                PathBuf::from,
            )
            .join("Cursor"),
        Platform::Darwin => PathBuf::from(&home).join(".cursor"),
        _ => non_empty("XDG_CONFIG_HOME")
            .map_or_else(|| PathBuf::from(&home).join(".config"), PathBuf::from)
            .join("cursor"),
    };
    directory.join("auth.json")
}

/// `None` is a failed probe: an unreadable file, a bad body, a failed request.
async fn read_usage(
    api_endpoint: Option<&str>,
    environment: &ProcessEnv,
    allow_keychain: bool,
    host: &CursorUsageLimitsHost<'_>,
    checked_at: &str,
) -> Option<ServerProviderUsageLimits> {
    let configured = api_endpoint.map_or("", str::trim);
    let from_environment = env_trimmed(environment, "CURSOR_API_ENDPOINT");
    let endpoint = [configured, from_environment, DEFAULT_CURSOR_API_ENDPOINT]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CURSOR_API_ENDPOINT)
        .trim_end_matches('/')
        .to_owned();
    let mut token = env_trimmed(environment, "CURSOR_AUTH_TOKEN").to_owned();
    // An explicit API key can name a different account from the stored login.
    if token.is_empty() && !env_trimmed(environment, "CURSOR_API_KEY").is_empty() {
        return Some(unsupported(checked_at, None));
    }
    let credential_store = environment
        .get("AGENT_CLI_CREDENTIAL_STORE")
        .map(String::as_str);
    if token.is_empty() && credential_store == Some("memory") {
        return Some(unsupported(
            checked_at,
            Some("Cursor usage requires a CLI login or CURSOR_AUTH_TOKEN."),
        ));
    }
    if token.is_empty() && host.platform == Platform::Darwin && credential_store != Some("file") {
        if !allow_keychain {
            return Some(unsupported(
                checked_at,
                Some("Enable Cursor account usage in Turin to read its Keychain login."),
            ));
        }
        if endpoint != DEFAULT_CURSOR_API_ENDPOINT {
            return Some(unsupported(
                checked_at,
                Some(
                    "Cursor account usage requires the default Cursor endpoint when using Keychain.",
                ),
            ));
        }
        token = host
            .keychain
            .access_token()
            .await
            .ok()?
            .map(|value| value.trim().to_owned())
            .unwrap_or_default();
    } else if token.is_empty() {
        let path = credentials_path(host.platform, environment, host.home_directory);
        let contents = match tokio::fs::read_to_string(&path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
            Err(_) => return None,
        };
        let credentials: Value = serde_json::from_str(&contents).ok()?;
        token = match credentials.get("accessToken") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(value)) => value.trim().to_owned(),
            Some(_) => return None,
        };
    }
    if token.is_empty() {
        return Some(unsupported(checked_at, None));
    }
    let body = host
        .http
        .current_period_usage(&endpoint, &token)
        .await
        .ok()?;
    let response: CursorUsageResponse = serde_json::from_value(body).ok()?;
    Some(cursor_usage_response_to_limits(&response, checked_at))
}

/// Reads the plan limits for one Cursor instance. A probe that fails for any
/// reason reports `probeFailed` rather than failing the provider refresh.
pub async fn read_cursor_usage_limits(
    api_endpoint: Option<&str>,
    environment: &ProcessEnv,
    allow_keychain: bool,
    host: &CursorUsageLimitsHost<'_>,
) -> ServerProviderUsageLimits {
    let checked_at = now_iso();
    let outcome = tokio::time::timeout(
        READ_TIMEOUT,
        read_usage(api_endpoint, environment, allow_keychain, host, &checked_at),
    )
    .await
    .ok()
    .flatten();
    outcome.unwrap_or_else(|| {
        make_unavailable_usage_limits(
            checked_at.clone(),
            ServerProviderUsageUnavailableReason::ProbeFailed,
            Some("Cursor could not read usage limits."),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::cursor_keychain::{
        CursorKeychainBackend, CursorKeychainReadError, KeychainReadFuture,
    };

    const CHECKED_AT: &str = "2026-09-16T00:00:00.000Z";

    struct Keychain(Result<Option<String>, CursorKeychainReadError>);

    impl CursorKeychainBackend for Keychain {
        fn read_access_token(&self) -> KeychainReadFuture {
            let result = self.0.clone();
            Box::pin(async move { result })
        }
    }

    fn keychain(result: Result<Option<String>, CursorKeychainReadError>) -> CursorKeychain {
        CursorKeychain::new(Arc::new(Keychain(result)))
    }

    /// Records each request and answers with a fixed result.
    struct Http {
        answer: Result<Value, ()>,
        seen: Mutex<Vec<(String, String)>>,
    }

    impl Http {
        fn answering(answer: Result<Value, ()>) -> Self {
            Self {
                answer,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl CursorUsageHttp for Http {
        async fn current_period_usage(&self, endpoint: &str, token: &str) -> Result<Value, ()> {
            self.seen
                .lock()
                .expect("seen")
                .push((endpoint.to_owned(), token.to_owned()));
            self.answer.clone()
        }
    }

    fn env(pairs: &[(&str, &str)]) -> ProcessEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>()
    }

    async fn read(
        platform: Platform,
        home: &str,
        api_endpoint: Option<&str>,
        environment: &ProcessEnv,
        allow_keychain: bool,
        keychain: &CursorKeychain,
        http: &Http,
    ) -> ServerProviderUsageLimits {
        read_cursor_usage_limits(
            api_endpoint,
            environment,
            allow_keychain,
            &CursorUsageLimitsHost {
                platform,
                home_directory: home,
                keychain,
                http,
            },
        )
        .await
    }

    fn window(
        id: &str,
        label: &str,
        used: f64,
        resets_at: Option<&str>,
    ) -> ServerProviderUsageWindow {
        ServerProviderUsageWindow {
            id: TrimmedNonEmptyString::new(id).expect("id"),
            kind: ServerProviderUsageWindowKind::Monthly,
            label: TrimmedNonEmptyString::new(label).expect("label"),
            used_percent: used,
            resets_at: resets_at.map(str::to_owned),
            window_duration_mins: None,
        }
    }

    fn response(value: Value) -> CursorUsageResponse {
        serde_json::from_value(value).expect("response")
    }

    #[test]
    fn uses_the_advertised_percentages_and_billing_cycle_reset() {
        let limits = cursor_usage_response_to_limits(
            &response(json!({
                "billingCycleEnd": "1789876386000",
                "planUsage": {
                    "totalPercentUsed": 72.4,
                    "autoPercentUsed": 69.5,
                    "apiPercentUsed": 100,
                },
            })),
            CHECKED_AT,
        );
        let reset = Some("2026-09-20T03:53:06.000Z");
        for expected in [
            window("totalPercentUsed", "Overall", 72.4, reset),
            window("autoPercentUsed", "Cursor Models", 69.5, reset),
            window("apiPercentUsed", "Other Models", 100.0, reset),
        ] {
            assert!(limits.windows.contains(&expected), "{expected:?}");
        }
    }

    #[test]
    fn does_not_invent_unused_allowance_for_absent_buckets() {
        let reason = |value: Value| {
            cursor_usage_response_to_limits(&response(value), CHECKED_AT)
                .unavailable
                .map(|unavailable| unavailable.reason)
        };
        assert_eq!(
            reason(json!({ "planUsage": {} })),
            Some(ServerProviderUsageUnavailableReason::Unsupported)
        );
        let windows =
            |value: Value| cursor_usage_response_to_limits(&response(value), CHECKED_AT).windows;
        assert_eq!(
            windows(json!({ "planUsage": { "totalPercentUsed": 0 } })),
            vec![window("totalPercentUsed", "Overall", 0.0, None)]
        );
        assert_eq!(
            windows(json!({ "planUsage": { "totalPercentUsed": 150 } })),
            vec![window("totalPercentUsed", "Overall", 100.0, None)]
        );
    }

    #[tokio::test]
    async fn reads_the_instances_credentials_and_endpoint_even_when_usage_enabled_is_false() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        for sub in ["cursor", ".cursor"] {
            std::fs::create_dir_all(root.join(sub)).expect("dir");
            std::fs::write(
                root.join(sub).join("auth.json"),
                r#"{"accessToken":"instance-token"}"#,
            )
            .expect("auth");
        }
        let home = root.to_string_lossy().into_owned();
        let environment = env(&[
            ("XDG_CONFIG_HOME", &home),
            ("HOME", &home),
            ("AGENT_CLI_CREDENTIAL_STORE", "file"),
        ]);
        for platform in [Platform::Linux, Platform::Darwin] {
            let http = Http::answering(Ok(
                json!({ "enabled": false, "planUsage": { "totalPercentUsed": 42 } }),
            ));
            let limits = read(
                platform,
                &home,
                Some("https://cursor.example/"),
                &environment,
                false,
                &keychain(Err(CursorKeychainReadError { cause: None })),
                &http,
            )
            .await;
            assert_eq!(limits.windows[0].used_percent, 42.0);
            assert_eq!(
                http.seen.lock().expect("seen").clone(),
                vec![(
                    "https://cursor.example".to_owned(),
                    "instance-token".to_owned()
                )]
            );
        }
    }

    #[tokio::test]
    async fn never_reads_stale_files_for_keychain_or_memory_logins_but_accepts_an_explicit_auth_token()
     {
        // A directory that holds a credential file the reader must not touch.
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(directory.path().join(".cursor")).expect("dir");
        std::fs::write(
            directory.path().join(".cursor/auth.json"),
            r#"{"accessToken":"stale-token"}"#,
        )
        .expect("auth");
        let home = directory.path().to_string_lossy().into_owned();
        for platform in [Platform::Linux, Platform::Darwin] {
            for token in [None, Some("explicit-token")] {
                let store = if platform == Platform::Linux {
                    "memory"
                } else {
                    "default"
                };
                let mut pairs = vec![("AGENT_CLI_CREDENTIAL_STORE", store)];
                if let Some(token) = token {
                    pairs.push(("CURSOR_AUTH_TOKEN", token));
                }
                let http = Http::answering(Ok(json!({ "planUsage": { "totalPercentUsed": 10 } })));
                let limits = read(
                    platform,
                    &home,
                    Some(""),
                    &env(&pairs),
                    false,
                    &keychain(Ok(None)),
                    &http,
                )
                .await;
                match token {
                    Some(token) => {
                        assert_eq!(limits.windows[0].used_percent, 10.0);
                        assert_eq!(http.seen.lock().expect("seen")[0].1, token);
                    }
                    None => {
                        assert_eq!(
                            limits.unavailable.map(|unavailable| unavailable.reason),
                            Some(ServerProviderUsageUnavailableReason::Unsupported)
                        );
                        assert!(http.seen.lock().expect("seen").is_empty());
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn reports_failed_requests_without_exposing_credentials_or_response_bodies() {
        let http = Http::answering(Err(()));
        let limits = read(
            Platform::Linux,
            "/home/none",
            Some(""),
            &env(&[("CURSOR_AUTH_TOKEN", "private-token")]),
            false,
            &keychain(Ok(None)),
            &http,
        )
        .await;
        let unavailable = limits.unavailable.expect("unavailable");
        assert_eq!(
            unavailable.reason,
            ServerProviderUsageUnavailableReason::ProbeFailed
        );
        assert_eq!(
            unavailable
                .message
                .as_ref()
                .map(TrimmedNonEmptyString::as_str),
            Some("Cursor could not read usage limits.")
        );
    }

    #[tokio::test]
    async fn does_not_use_a_stored_login_for_an_explicit_api_key() {
        let http = Http::answering(Err(()));
        let limits = read(
            Platform::Linux,
            "/home/none",
            Some(""),
            &env(&[("CURSOR_API_KEY", "different-account")]),
            false,
            &keychain(Ok(None)),
            &http,
        )
        .await;
        assert_eq!(
            limits.unavailable.map(|unavailable| unavailable.reason),
            Some(ServerProviderUsageUnavailableReason::Unsupported)
        );
        assert!(http.seen.lock().expect("seen").is_empty());
    }

    #[tokio::test]
    async fn reads_the_default_macos_cursor_login_from_keychain_for_limits() {
        let http = Http::answering(Ok(json!({ "planUsage": { "totalPercentUsed": 42 } })));
        let limits = read(
            Platform::Darwin,
            "/home/none",
            Some(""),
            &env(&[]),
            true,
            &keychain(Ok(Some("keychain-token".to_owned()))),
            &http,
        )
        .await;
        assert_eq!(limits.windows[0].used_percent, 42.0);
        assert_eq!(http.seen.lock().expect("seen")[0].1, "keychain-token");
    }

    #[tokio::test]
    async fn reports_a_keychain_initialization_failure_without_failing_the_provider_refresh() {
        let http = Http::answering(Err(()));
        let limits = read(
            Platform::Darwin,
            "/home/none",
            Some(""),
            &env(&[]),
            true,
            &keychain(Err(CursorKeychainReadError {
                cause: Some("Keychain initialization failed".to_owned()),
            })),
            &http,
        )
        .await;
        assert_eq!(
            limits.unavailable.map(|unavailable| unavailable.reason),
            Some(ServerProviderUsageUnavailableReason::ProbeFailed)
        );
        assert!(http.seen.lock().expect("seen").is_empty());
    }

    #[tokio::test]
    async fn does_not_read_keychain_or_send_its_token_to_a_custom_endpoint() {
        for (api_endpoint, environment) in [
            ("http://localhost:3000", env(&[])),
            ("", env(&[("CURSOR_API_ENDPOINT", "http://localhost:3000")])),
            ("https://cursor-proxy.example", env(&[])),
            (
                "",
                env(&[("CURSOR_API_ENDPOINT", "https://cursor-proxy.example")]),
            ),
        ] {
            let http = Http::answering(Err(()));
            let limits = read(
                Platform::Darwin,
                "/home/none",
                Some(api_endpoint),
                &environment,
                true,
                &keychain(Ok(Some("keychain-token".to_owned()))),
                &http,
            )
            .await;
            let unavailable = limits.unavailable.expect("unavailable");
            assert_eq!(
                unavailable.reason,
                ServerProviderUsageUnavailableReason::Unsupported
            );
            assert!(
                unavailable
                    .message
                    .as_ref()
                    .is_some_and(|message| message.as_str().contains("default Cursor endpoint"))
            );
            assert!(http.seen.lock().expect("seen").is_empty());
        }
    }
}
