//! Port of upstream/apps/server/src/provider/CodexProvider.ts (account,
//! model/skill/usage probe and app-server managed login methods).
use crate::adapter::CodexSession;
use chrono::DateTime;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::{sleep, timeout};

use crate::codex_usage_limits::codex_rate_limits_failure_message;

/// `RATE_LIMITS_PROBE_TIMEOUT_MS`.
const RATE_LIMITS_PROBE_TIMEOUT_SECS: u64 = 3;
use turin_codex_app_server::{
    client::Client, errors::AppServerError, extensions::provider_probe_returns_early,
};

#[derive(Debug, Clone)]
pub struct ProbeSnapshot {
    pub account: Value,
    pub models: Vec<Value>,
    /// The raw `skills/list` response; `codex_skills` maps it for the cwd.
    pub skills: Value,
    /// `None` when the native usage read is skipped. `Err` carries the
    /// client-safe failure message. Usage failure degrades the probe, as upstream does.
    pub rate_limits: Option<Result<Value, String>>,
    pub version: Option<String>,
}

fn payload(response: Option<Value>, method: &str) -> Result<Value, AppServerError> {
    response.ok_or_else(|| AppServerError::InvalidPayload {
        method: method.into(),
        detail: "missing response".into(),
    })
}

pub async fn all_models(client: &Client) -> Result<Vec<Value>, AppServerError> {
    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(cursor.clone()) {
            return Err(AppServerError::InvalidPayload {
                method: "model/list".into(),
                detail: "repeated cursor".into(),
            });
        }
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
        let page = payload(
            client.request("model/list", Some(params)).await?,
            "model/list",
        )?;
        let data = page.get("data").and_then(Value::as_array).ok_or_else(|| {
            AppServerError::InvalidPayload {
                method: "model/list".into(),
                detail: "missing data".into(),
            }
        })?;
        models.extend(data.iter().cloned());
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            return Ok(models);
        }
    }
}

async fn cache_fetched_at(home: &Path) -> Option<i64> {
    let bytes = tokio::fs::read(home.join("models_cache.json")).await.ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let stamp = value.get("fetched_at")?.as_str()?;
    Some(DateTime::parse_from_rfc3339(stamp).ok()?.timestamp_millis())
}

pub async fn chatgpt_models(
    client: &Client,
    home: &Path,
    spawned_at_ms: i64,
) -> Result<Vec<Value>, AppServerError> {
    let first = all_models(client).await?;
    let refreshed = timeout(Duration::from_secs(3), async {
        loop {
            if cache_fetched_at(home)
                .await
                .is_some_and(|stamp| stamp >= spawned_at_ms)
            {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .is_ok();
    if refreshed {
        all_models(client).await
    } else {
        Ok(first)
    }
}

pub async fn probe(
    session: &CodexSession,
    cwd: &str,
    skip_native_usage: bool,
    spawned_at_ms: i64,
) -> Result<ProbeSnapshot, AppServerError> {
    let client = session.client();
    let account = payload(
        client.request("account/read", Some(json!({}))).await?,
        "account/read",
    )?;
    if provider_probe_returns_early(
        account.get("account"),
        account
            .get("requiresOpenaiAuth")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ) {
        return Ok(ProbeSnapshot {
            account,
            models: vec![],
            skills: json!({"data": []}),
            rate_limits: None,
            version: session.version.clone(),
        });
    }
    let models = async {
        if account.pointer("/account/type").and_then(Value::as_str) == Some("chatgpt")
            && let Some(home) = session.codex_home.as_deref()
        {
            return chatgpt_models(client, Path::new(home), spawned_at_ms).await;
        }
        all_models(client).await
    };
    let skills = client.request("skills/list", Some(json!({"cwds":[cwd]})));
    // Usage is an enrichment: a failure or a slow answer degrades to "no usage
    // this probe" rather than costing the account and models.
    let usage = async {
        if skip_native_usage {
            return None;
        }
        Some(
            match timeout(
                Duration::from_secs(RATE_LIMITS_PROBE_TIMEOUT_SECS),
                client.request("account/rateLimits/read", Some(Value::Null)),
            )
            .await
            {
                Err(_) => Err("Codex did not answer the usage request.".to_owned()),
                Ok(Err(error)) => Err(codex_rate_limits_failure_message(&error)),
                Ok(Ok(None)) => Err("Codex did not answer the usage request.".to_owned()),
                Ok(Ok(Some(response))) => Ok(response),
            },
        )
    };
    let (skills, models, rate_limits) = tokio::join!(skills, models, usage);
    Ok(ProbeSnapshot {
        account,
        models: models?,
        skills: payload(skills?, "skills/list")?,
        rate_limits,
        version: session.version.clone(),
    })
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl CodexSession {
    pub async fn start_chatgpt_login(&self) -> Result<Value, AppServerError> {
        payload(
            self.client()
                .request("account/login/start", Some(json!({"type":"chatgpt"})))
                .await?,
            "account/login/start",
        )
    }
    pub async fn start_device_code_login(&self) -> Result<Value, AppServerError> {
        payload(
            self.client()
                .request(
                    "account/login/start",
                    Some(json!({"type":"chatgptDeviceCode"})),
                )
                .await?,
            "account/login/start",
        )
    }
    pub async fn cancel_login(&self, login_id: &str) -> Result<(), AppServerError> {
        self.client()
            .request("account/login/cancel", Some(json!({"loginId":login_id})))
            .await?;
        Ok(())
    }
    pub async fn logout(&self) -> Result<(), AppServerError> {
        self.client().request("account/logout", None).await?;
        Ok(())
    }
}
