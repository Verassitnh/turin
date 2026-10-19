//! Port of upstream/packages/provider-opencode/src/server/usageLimits.ts: the OpenCode Go
//! subscription windows (session, weekly, monthly), read from Go's usage endpoint.
//!
//! An external server owns its own credentials, so it reports no Go usage. Without a Go
//! key the result is `unsupported`, and any failure is `probeFailed`. The whole read has a
//! 5 second deadline, as upstream's `Effect.timeout`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use turin_contracts::{
    IsoDateTime, TrimmedNonEmptyString,
    provider_usage_limits::{
        ServerProviderUsageLimits, ServerProviderUsageUnavailableReason, ServerProviderUsageWindow,
        ServerProviderUsageWindowKind,
    },
};
use turin_provider_core::usage_limits::{
    clamp_percent, make_unavailable_usage_limits, make_usage_limits,
};

use crate::http_client::shared_http;

pub const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_FAILED_MESSAGE: &str = "OpenCode Go could not read usage.";

/// What the driver knows about the instance. `environment` supplies `HOME`,
/// `XDG_DATA_HOME`, `OPENCODE_AUTH_CONTENT` and `OPENCODE_API_KEY`.
pub struct GoUsageInput<'a> {
    pub enabled: bool,
    pub server_url: &'a str,
    pub environment: &'a BTreeMap<String, String>,
    pub checked_at: IsoDateTime,
}

/// Reads Go usage from the public endpoint.
pub async fn read_open_code_go_usage_limits(input: GoUsageInput<'_>) -> ServerProviderUsageLimits {
    read_from(input, OPENCODE_GO_USAGE_URL).await
}

async fn read_from(input: GoUsageInput<'_>, usage_url: &str) -> ServerProviderUsageLimits {
    let checked_at = input.checked_at.clone();
    let unsupported = make_unavailable_usage_limits(
        checked_at.clone(),
        ServerProviderUsageUnavailableReason::Unsupported,
        None,
    );
    if !input.enabled || !input.server_url.trim().is_empty() {
        return unsupported;
    }
    let read = tokio::time::timeout(READ_TIMEOUT, read_windows(&input, usage_url));
    match read.await {
        Ok(Ok(Some(windows))) => windows,
        Ok(Ok(None)) => unsupported,
        Ok(Err(())) | Err(_) => make_unavailable_usage_limits(
            checked_at,
            ServerProviderUsageUnavailableReason::ProbeFailed,
            Some(PROBE_FAILED_MESSAGE),
        ),
    }
}

/// `Ok(None)` is an account without Go; `Err` is a read that failed.
async fn read_windows(
    input: &GoUsageInput<'_>,
    usage_url: &str,
) -> Result<Option<ServerProviderUsageLimits>, ()> {
    let env = input.environment;
    let contents = match nonempty(env.get("OPENCODE_AUTH_CONTENT")) {
        Some(contents) => contents.to_owned(),
        None => {
            let path = auth_path(env).ok_or(())?;
            match std::fs::read_to_string(path) {
                Ok(contents) => contents,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
                Err(_) => return Err(()),
            }
        }
    };
    let auth: AuthFile = serde_json::from_str(&contents).map_err(|_| ())?;
    // OpenCode overlays stored API credentials after environment credentials.
    let api_key = match auth.go {
        Some(ApiAuth { kind, key }) if kind == "api" => Some(key),
        _ => env.get("OPENCODE_API_KEY").cloned(),
    };
    let Some(api_key) = api_key
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
    else {
        return Ok(None);
    };

    let response = shared_http()
        .get(usage_url)
        .bearer_auth(&api_key)
        .send()
        .await
        .map_err(|_| ())?;
    // A valid Zen key can exist without a Go subscription.
    if response.status().as_u16() == 403 {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(());
    }
    let body: UsageResponse = response.json().await.map_err(|_| ())?;
    let usage = body.usage;
    let windows = [
        window(
            "go_rolling",
            ServerProviderUsageWindowKind::Session,
            "Go · Session",
            Some(5 * 60),
            &usage.rolling,
        )?,
        window(
            "go_weekly",
            ServerProviderUsageWindowKind::Weekly,
            "Go · Weekly",
            Some(7 * 24 * 60),
            &usage.weekly,
        )?,
        window(
            "go_monthly",
            ServerProviderUsageWindowKind::Monthly,
            "Go · Monthly",
            None,
            &usage.monthly,
        )?,
    ];
    let mut limits = make_usage_limits(input.checked_at.clone(), windows);
    // Go's usage response has no account ID. The digest is unkeyed so it matches across
    // environments; it can be guessed offline, but Go keys are random.
    let digest = Sha256::digest(format!("opencode-go\0{api_key}").as_bytes());
    let fingerprint: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    limits.credential_fingerprint = TrimmedNonEmptyString::new(fingerprint).ok();
    Ok(Some(limits))
}

fn window(
    id: &str,
    kind: ServerProviderUsageWindowKind,
    label: &str,
    duration_mins: Option<u64>,
    source: &UsageWindow,
) -> Result<ServerProviderUsageWindow, ()> {
    Ok(ServerProviderUsageWindow {
        id: TrimmedNonEmptyString::new(id).map_err(|_| ())?,
        kind,
        label: TrimmedNonEmptyString::new(label).map_err(|_| ())?,
        used_percent: clamp_percent(source.percent),
        resets_at: Some(iso(&source.resets_at)?),
        window_duration_mins: duration_mins,
    })
}

/// `DateTime.formatIso`: UTC with milliseconds and a `Z` suffix.
fn iso(text: &str) -> Result<IsoDateTime, ()> {
    let parsed = chrono::DateTime::parse_from_rfc3339(text).map_err(|_| ())?;
    Ok(parsed
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn auth_path(env: &BTreeMap<String, String>) -> Option<std::path::PathBuf> {
    let data_home = match nonempty(env.get("XDG_DATA_HOME")) {
        Some(data_home) => std::path::PathBuf::from(data_home),
        None => {
            // Upstream falls back to the host's home directory when the environment has none.
            let home = nonempty(env.get("HOME"))
                .or_else(|| nonempty(env.get("USERPROFILE")))
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))?;
            home.join(".local").join("share")
        }
    };
    Some(data_home.join("opencode").join("auth.json"))
}

fn nonempty(value: Option<&String>) -> Option<&str> {
    value.map(String::as_str).filter(|value| !value.is_empty())
}

#[derive(Deserialize)]
struct AuthFile {
    #[serde(rename = "opencode-go")]
    go: Option<ApiAuth>,
}

#[derive(Deserialize)]
struct ApiAuth {
    #[serde(rename = "type")]
    kind: String,
    key: String,
}

#[derive(Deserialize)]
struct UsageResponse {
    usage: Usage,
}

#[derive(Deserialize)]
struct Usage {
    rolling: UsageWindow,
    weekly: UsageWindow,
    monthly: UsageWindow,
}

#[derive(Deserialize)]
struct UsageWindow {
    percent: f64,
    #[serde(rename = "resetsAt")]
    resets_at: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeResponse, FakeServer};

    fn env_with_key(key: &str) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            "OPENCODE_AUTH_CONTENT".to_owned(),
            json!({"opencode-go": {"type": "api", "key": key}}).to_string(),
        );
        env
    }

    fn input<'a>(env: &'a BTreeMap<String, String>, server_url: &'a str) -> GoUsageInput<'a> {
        GoUsageInput {
            enabled: true,
            server_url,
            environment: env,
            checked_at: "2026-10-15T12:00:00.000Z".to_owned(),
        }
    }

    fn usage_body() -> String {
        json!({"usage": {
            "rolling": {"percent": 42.5, "resetsAt": "2026-10-15T13:00:00Z"},
            "weekly": {"percent": 180.0, "resetsAt": "2026-10-20T00:00:00.123Z"},
            "monthly": {"percent": 3.0, "resetsAt": "2026-11-01T00:00:00Z"}
        }})
        .to_string()
    }

    #[tokio::test]
    async fn go_usage_reports_three_windows_with_a_credential_fingerprint() {
        let server = FakeServer::spawn(|request| {
            assert_eq!(request.authorization.as_deref(), Some("Bearer go-key"));
            FakeResponse::json(200, &usage_body())
        })
        .await;
        let env = env_with_key("go-key");
        let limits = read_from(input(&env, ""), &format!("{}/usage", server.url)).await;
        assert!(limits.unavailable.is_none());
        let ids: Vec<&str> = limits.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["go_rolling", "go_weekly", "go_monthly"]);
        assert_eq!(limits.windows[0].used_percent, 42.5);
        assert_eq!(limits.windows[1].used_percent, 100.0, "percent is clamped");
        assert_eq!(limits.windows[0].window_duration_mins, Some(300));
        assert_eq!(
            limits.windows[1].resets_at.as_deref(),
            Some("2026-10-20T00:00:00.123Z")
        );
        let fingerprint = limits.credential_fingerprint.expect("fingerprint");
        assert_eq!(fingerprint.as_str().len(), 64);
        assert!(!fingerprint.as_str().contains("go-key"));
    }

    #[tokio::test]
    async fn a_zen_key_without_go_is_unsupported() {
        let server = FakeServer::spawn(|_| FakeResponse::json(403, "{}")).await;
        let env = env_with_key("zen-only");
        let limits = read_from(input(&env, ""), &format!("{}/usage", server.url)).await;
        assert_eq!(
            limits.unavailable.map(|u| u.reason),
            Some(ServerProviderUsageUnavailableReason::Unsupported)
        );
    }

    #[tokio::test]
    async fn a_server_error_is_a_probe_failure() {
        let server = FakeServer::spawn(|_| FakeResponse::json(500, "boom")).await;
        let env = env_with_key("go-key");
        let limits = read_from(input(&env, ""), &format!("{}/usage", server.url)).await;
        let unavailable = limits.unavailable.expect("unavailable");
        assert_eq!(
            unavailable.reason,
            ServerProviderUsageUnavailableReason::ProbeFailed
        );
        assert_eq!(
            unavailable.message.as_ref().map(|m| m.as_str()),
            Some(PROBE_FAILED_MESSAGE)
        );
    }

    #[tokio::test]
    async fn an_external_server_or_no_key_reads_nothing() {
        // A home with no auth.json: no stored key and no environment key.
        let mut env = BTreeMap::new();
        env.insert(
            "HOME".to_owned(),
            std::env::temp_dir()
                .join("turin-no-such-home")
                .display()
                .to_string(),
        );
        let external = read_from(input(&env, "http://elsewhere"), "http://unused").await;
        assert_eq!(
            external.unavailable.map(|u| u.reason),
            Some(ServerProviderUsageUnavailableReason::Unsupported)
        );
        let keyless = read_from(input(&env, ""), "http://unused").await;
        assert_eq!(
            keyless.unavailable.map(|u| u.reason),
            Some(ServerProviderUsageUnavailableReason::Unsupported)
        );
    }
}
