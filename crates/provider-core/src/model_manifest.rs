//! Port of upstream/apps/server/src/provider/ModelManifest.ts (remote fetch,
//! disk cache, bundled fallback, catalog resolution) and the adapter-payload
//! check from upstream/apps/server/src/provider/ClaudeModelManifest.ts.
//!
//! Provider catalogs and legacy classification live in `model-manifest.json`.
//! The bundled copy ships with the binary; at runtime the service refreshes it
//! from the same file on `main`. Preference order is remote, then the last
//! successful on-disk copy, then the bundle. A failed fetch never fails a
//! provider check.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use turin_contracts::{
    TrimmedNonEmptyString, js_trim, model::ModelCapabilities, provider_instance::ProviderDriverKind,
};

use crate::{
    rt::BoxFuture,
    semver::{compare_semver_versions, is_valid_semver},
};

pub const MODEL_MANIFEST_URL: &str = "https://raw.githubusercontent.com/pingdotgg/t3code/main/apps/server/src/provider/model-manifest.json";

/// How long a fetched manifest stays fresh before the next probe re-fetches.
pub const MANIFEST_TTL_MS: i64 = 60 * 60 * 1_000;
/// Minimum gap between fetch attempts after a failure, so an offline server
/// does not pay a network timeout on every provider check.
pub const MANIFEST_RETRY_MS: i64 = 5 * 60 * 1_000;
pub const FETCH_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelManifestData {
    /// Only `1` decodes; see `validate_manifest`.
    pub version: u8,
    /// Last edit date. A release bundles its manifest, and a cached older edit
    /// must not outrank it. Absent counts as older than any dated bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// `ProviderCompatibilityPolicy` entries. Passed through unvalidated until
    /// the compatibility port lands (see PORTING.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<Vec<Value>>,
    pub current_models: IndexMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<IndexMap<String, ManifestProviderCatalog>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProviderCatalog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<ManifestProviderDefaults>,
    pub profiles: IndexMap<String, ManifestModelProfile>,
    pub models: Vec<ManifestProviderModel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestProviderDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestModelProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProviderModel {
    pub slug: TrimmedNonEmptyString,
    pub name: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<TrimmedNonEmptyString>>,
    pub status: ManifestModelStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<ManifestBadge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestModelStatus {
    Current,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestBadge {
    New,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCatalogModel {
    pub slug: String,
    pub name: String,
    pub short_name: Option<String>,
    pub sub_provider: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub status: ManifestModelStatus,
    pub badge: Option<ManifestBadge>,
    pub capabilities: Option<ModelCapabilities>,
    pub adapter: Option<Value>,
    pub profile_adapter: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCatalog {
    pub models: Vec<ProviderCatalogModel>,
    pub default_chat_model: Option<String>,
}

/// `hasValidProviderCatalogReferences`, per catalog: unique slugs, existing
/// profile references, and a default chat model that is one of the models.
fn catalog_references_valid(catalog: &ManifestProviderCatalog) -> bool {
    let mut slugs = HashSet::new();
    let models_valid = catalog.models.iter().all(|model| {
        if !slugs.insert(model.slug.as_str()) {
            return false;
        }
        model
            .profile
            .as_ref()
            .is_none_or(|profile| catalog.profiles.contains_key(profile.as_str()))
    });
    models_valid
        && catalog
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.chat.as_ref())
            .is_none_or(|chat| slugs.contains(chat.as_str()))
}

/// A driver's catalog entry as provider packages see it, or `None` when the
/// manifest has none or its references are invalid.
pub fn resolve_provider_catalog(
    manifest: &ModelManifestData,
    driver: &ProviderDriverKind,
) -> Option<ProviderCatalog> {
    let catalog = manifest.providers.as_ref()?.get(driver.as_str())?;
    if !catalog_references_valid(catalog) {
        return None;
    }
    let models = catalog
        .models
        .iter()
        .map(|entry| {
            let profile = match &entry.profile {
                Some(name) => Some(catalog.profiles.get(name.as_str())?),
                None => None,
            };
            Some(ProviderCatalogModel {
                slug: entry.slug.as_str().to_owned(),
                name: entry.name.as_str().to_owned(),
                short_name: entry
                    .short_name
                    .as_ref()
                    .map(|name| name.as_str().to_owned()),
                sub_provider: entry
                    .sub_provider
                    .as_ref()
                    .map(|name| name.as_str().to_owned()),
                aliases: entry
                    .aliases
                    .as_ref()
                    .map(|names| names.iter().map(|name| name.as_str().to_owned()).collect()),
                status: entry.status,
                badge: entry.badge,
                capabilities: profile.and_then(|profile| profile.capabilities.clone()),
                adapter: entry.adapter.clone(),
                profile_adapter: profile.and_then(|profile| profile.adapter.clone()),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ProviderCatalog {
        models,
        default_chat_model: catalog
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.chat.as_ref())
            .map(|chat| chat.as_str().to_owned()),
    })
}

/// A Struct-shaped value: `None` unless it is an object. Unknown keys are
/// ignored, as in Effect's default decoding.
fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// `TrimmedNonEmptyString` decoding: trims, then rejects blank.
fn trimmed_text(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let trimmed = js_trim(text);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A present key that must be a valid semantic version (`ClaudeVersionSchema`).
fn semver_field(map: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match map.get(key) {
        None => Ok(None),
        Some(value) => match trimmed_text(value) {
            Some(text) if is_valid_semver(&text) => Ok(Some(text)),
            _ => Err(()),
        },
    }
}

/// `ClaudeProfileAdapterSchema.claudeCode` (`ClaudeCodeProfileSchema`).
fn valid_claude_code_profile(code: &Map<String, Value>) -> bool {
    let effort_map = match code.get("effortMap") {
        None => true,
        Some(map) => object(map).is_some_and(|map| {
            map.iter().all(|(key, value)| {
                trimmed_text(&Value::String(key.clone())).is_some()
                    && (value.is_null() || trimmed_text(value).is_some())
            })
        }),
    };
    let model_suffixes = match code.get("modelSuffixes") {
        None => true,
        Some(map) => object(map).is_some_and(|map| {
            map.iter().all(|(key, inner)| {
                trimmed_text(&Value::String(key.clone())).is_some()
                    && object(inner).is_some_and(|inner| {
                        inner.iter().all(|(suffix_key, suffix)| {
                            trimmed_text(&Value::String(suffix_key.clone())).is_some()
                                && trimmed_text(suffix).is_some()
                        })
                    })
            })
        }),
    };
    let context_windows = match code.get("contextWindowTokens") {
        None => true,
        Some(map) => object(map).is_some_and(|map| {
            map.iter().all(|(key, value)| {
                trimmed_text(&Value::String(key.clone())).is_some() && value.is_number()
            })
        }),
    };
    let fixed_window = code
        .get("fixedContextWindowTokens")
        .is_none_or(Value::is_number);
    effort_map && model_suffixes && context_windows && fixed_window
}

/// `ClaudeModelAdapterSchema.claudeCode` (`ClaudeCodeCompatibilitySchema`):
/// valid versions, and `minVersion` below `maxVersionExclusive` when both are set.
fn valid_claude_code_compatibility(code: &Map<String, Value>) -> bool {
    let (Ok(minimum), Ok(maximum)) = (
        semver_field(code, "minVersion"),
        semver_field(code, "maxVersionExclusive"),
    ) else {
        return false;
    };
    match (minimum, maximum) {
        (Some(minimum), Some(maximum)) => compare_semver_versions(&minimum, &maximum).is_lt(),
        _ => true,
    }
}

/// Validates `adapter` under `?? {}`: absent or null is an empty object, which
/// passes. `check_code` validates the `claudeCode` value when it is present.
fn valid_adapter(adapter: Option<&Value>, check_code: fn(&Map<String, Value>) -> bool) -> bool {
    let Some(adapter) = adapter.filter(|value| !value.is_null()) else {
        return true;
    };
    let Some(adapter) = object(adapter) else {
        return false;
    };
    match adapter.get("claudeCode") {
        None => true,
        Some(code) => object(code).is_some_and(check_code),
    }
}

/// `hasValidClaudeManifestAdapters`: every `claudeAgent` profile and model
/// adapter decodes against its schema. Other providers are not checked here.
fn validate_claude_adapters(manifest: &ModelManifestData) -> bool {
    let Some(catalog) = manifest
        .providers
        .as_ref()
        .and_then(|providers| providers.get("claudeAgent"))
    else {
        return true;
    };
    catalog
        .profiles
        .values()
        .all(|profile| valid_adapter(profile.adapter.as_ref(), valid_claude_code_profile))
        && catalog
            .models
            .iter()
            .all(|model| valid_adapter(model.adapter.as_ref(), valid_claude_code_compatibility))
}

fn validate_manifest(manifest: &ModelManifestData) -> bool {
    if manifest.version != 1 {
        return false;
    }
    let providers_valid = manifest
        .providers
        .as_ref()
        .is_none_or(|providers| providers.values().all(catalog_references_valid));
    providers_valid && validate_claude_adapters(manifest)
}

/// Decodes and validates a manifest; `None` when either fails. The caller
/// keeps the bundle or last-good copy in that case.
pub fn decode_manifest(value: Value) -> Option<ModelManifestData> {
    let manifest: ModelManifestData = serde_json::from_value(value).ok()?;
    validate_manifest(&manifest).then_some(manifest)
}

pub fn bundled_model_manifest() -> ModelManifestData {
    decode_manifest(
        serde_json::from_str(include_str!("../model-manifest.json")).expect("bundled JSON"),
    )
    .expect("bundled manifest is valid")
}

/// Epoch millis of `updated_at`, or 0 when absent or unparsable.
fn updated_at_ms(manifest: &ModelManifestData) -> i64 {
    manifest
        .updated_at
        .as_deref()
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
        .map_or(0, |date| date.timestamp_millis())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestCache {
    fetched_at_ms: i64,
    manifest: ModelManifestData,
}

struct ManifestState {
    manifest: ModelManifestData,
    fetched_at_ms: Option<i64>,
    last_attempt_ms: Option<i64>,
}

/// Answers the `enableProviderUpdateChecks` setting. It stops network fetches
/// only; a manifest already held from an earlier fetch stays in effect.
pub type UpdateChecks = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

struct ManifestInner {
    state: Mutex<ManifestState>,
    cache_path: PathBuf,
    remote_url: String,
    enable_checks: UpdateChecks,
    client: reqwest::Client,
    disk_loaded: OnceCell<()>,
    /// Only `refresh` takes this; `current` never waits behind a fetch.
    refresh_lock: AsyncMutex<()>,
}

pub struct ModelManifest {
    inner: Arc<ManifestInner>,
    jobs: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl ModelManifest {
    pub fn new(state_dir: impl Into<PathBuf>, enable_checks: UpdateChecks) -> Self {
        Self::with_url(state_dir, enable_checks, MODEL_MANIFEST_URL)
    }

    /// `new` with an explicit remote URL, for tests that serve a local manifest.
    pub fn with_url(
        state_dir: impl Into<PathBuf>,
        enable_checks: UpdateChecks,
        remote_url: impl Into<String>,
    ) -> Self {
        Self {
            inner: Arc::new(ManifestInner {
                state: Mutex::new(ManifestState {
                    manifest: bundled_model_manifest(),
                    fetched_at_ms: None,
                    last_attempt_ms: None,
                }),
                cache_path: state_dir.into().join("model-manifest.json"),
                remote_url: remote_url.into(),
                enable_checks,
                client: reqwest::Client::new(),
                disk_loaded: OnceCell::new(),
                refresh_lock: AsyncMutex::new(()),
            }),
            jobs: Mutex::new(Vec::new()),
        }
    }

    /// Manifest already in memory (disk cache or bundle). Never fetches.
    pub async fn current(&self) -> ModelManifestData {
        self.inner.ensure_disk_loaded().await;
        self.inner.state.lock().manifest.clone()
    }

    /// Manifest after a TTL- and retry-gated remote refresh. Never fails.
    pub async fn refresh(&self) -> ModelManifestData {
        self.inner.refresh(false).await
    }

    /// Bypasses the freshness and retry timers, keeping last-good data on failure.
    pub async fn force_refresh(&self) -> ModelManifestData {
        self.inner.refresh(true).await
    }

    /// Forks a refresh owned by this service, so it outlives the provider
    /// check that asked for it.
    pub fn refresh_in_background(&self) {
        let inner = Arc::clone(&self.inner);
        let mut jobs = self.jobs.lock();
        jobs.retain(|job| !job.is_finished());
        jobs.push(tokio::spawn(async move {
            inner.refresh(false).await;
        }));
    }
}

impl Drop for ModelManifest {
    fn drop(&mut self) {
        for job in self.jobs.get_mut().drain(..) {
            job.abort();
        }
    }
}

impl ManifestInner {
    async fn ensure_disk_loaded(&self) {
        self.disk_loaded
            .get_or_init(|| async {
                let Ok(raw) = tokio::fs::read_to_string(&self.cache_path).await else {
                    return;
                };
                let Ok(cache) = serde_json::from_str::<ManifestCache>(&raw) else {
                    return;
                };
                if !validate_manifest(&cache.manifest) {
                    return;
                }
                let mut state = self.state.lock();
                // A bundle edited after the cached copy wins; the next refresh replaces the cache.
                if updated_at_ms(&state.manifest) > updated_at_ms(&cache.manifest) {
                    return;
                }
                state.manifest = cache.manifest;
                state.fetched_at_ms = Some(cache.fetched_at_ms);
            })
            .await;
    }

    async fn refresh(&self, force: bool) -> ModelManifestData {
        self.ensure_disk_loaded().await;
        let _guard = self.refresh_lock.lock().await;
        let now = chrono::Utc::now().timestamp_millis();
        // A timestamp in the future means the wall clock moved backwards; that
        // reads as expired, and the refetch rewrites both timestamps.
        let within = |since: Option<i64>, window: i64| {
            since.is_some_and(|since| now >= since && now - since < window)
        };
        {
            let state = self.state.lock();
            if !force
                && (within(state.fetched_at_ms, MANIFEST_TTL_MS)
                    || within(state.last_attempt_ms, MANIFEST_RETRY_MS))
            {
                return state.manifest.clone();
            }
        }
        if !(self.enable_checks)().await {
            return self.state.lock().manifest.clone();
        }
        self.state.lock().last_attempt_ms = Some(now);
        let fetched = tokio::time::timeout(Duration::from_millis(FETCH_TIMEOUT_MS), async {
            let response = self
                .client
                .get(&self.remote_url)
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            decode_manifest(response.json::<Value>().await.ok()?)
        })
        .await
        .ok()
        .flatten();
        let Some(fetched) = fetched else {
            return self.state.lock().manifest.clone();
        };
        {
            let mut state = self.state.lock();
            // A CDN can serve an earlier edit after a release; ignore it so it cannot undo bundled gates.
            if updated_at_ms(&fetched) < updated_at_ms(&state.manifest) {
                return state.manifest.clone();
            }
            state.manifest = fetched.clone();
            state.fetched_at_ms = Some(now);
        }
        self.write_cache(&ManifestCache {
            fetched_at_ms: now,
            manifest: fetched.clone(),
        })
        .await;
        fetched
    }

    /// Best-effort atomic write: a temp file renamed over the cache. Failures
    /// are ignored, as upstream's `ignoreCause` does.
    async fn write_cache(&self, cache: &ManifestCache) {
        let Ok(contents) = serde_json::to_vec(cache) else {
            return;
        };
        if let Some(parent) = self.cache_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let temp = self
            .cache_path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        if tokio::fs::write(&temp, contents).await.is_ok()
            && tokio::fs::rename(&temp, &self.cache_path).await.is_err()
        {
            let _ = tokio::fs::remove_file(temp).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    fn disabled() -> UpdateChecks {
        Arc::new(|| Box::pin(async { false }))
    }

    fn enabled() -> UpdateChecks {
        Arc::new(|| Box::pin(async { true }))
    }

    fn remote(updated_at: &str, codex_model: &str) -> String {
        json!({
            "version": 1,
            "updatedAt": updated_at,
            "currentModels": { "codex": [codex_model] },
        })
        .to_string()
    }

    /// A local HTTP server that answers one request per entry, in order. The
    /// returned counter is the number of requests received.
    async fn serve(responses: Vec<(u16, String)>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/model-manifest.json",
            listener.local_addr().unwrap()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                let reply = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        (url, hits)
    }

    fn codex_models(manifest: &ModelManifestData) -> Vec<String> {
        manifest.current_models["codex"].clone()
    }

    #[test]
    fn resolves_profiles_and_rejects_bad_references() {
        let mut value = json!({"version":1,"currentModels":{},"providers":{"synthetic":{"defaults":{"chat":"next"},"profiles":{"standard":{"adapter":{"opaque":true}}},"models":[{"slug":"next","name":"Next","status":"current","profile":"standard"}]}}});
        let manifest = decode_manifest(value.clone()).unwrap();
        let catalog =
            resolve_provider_catalog(&manifest, &ProviderDriverKind::new("synthetic").unwrap())
                .unwrap();
        assert_eq!(catalog.default_chat_model.as_deref(), Some("next"));
        assert_eq!(
            catalog.models[0].profile_adapter,
            Some(json!({"opaque":true}))
        );
        value["providers"]["synthetic"]["defaults"]["chat"] = json!("missing");
        assert!(decode_manifest(value.clone()).is_none());
        value["providers"]["synthetic"]["defaults"]["chat"] = json!("next");
        value["providers"]["synthetic"]["models"][0]["profile"] = json!("gone");
        assert!(decode_manifest(value).is_none());
    }

    #[test]
    fn rejects_duplicate_slugs_and_blank_names() {
        let duplicate = json!({"version":1,"currentModels":{},"providers":{"synthetic":{"profiles":{},"models":[
            {"slug":"a","name":"A","status":"current"},
            {"slug":"a","name":"A again","status":"legacy"}]}}});
        assert!(decode_manifest(duplicate).is_none());
        let blank = json!({"version":1,"currentModels":{},"providers":{"synthetic":{"profiles":{},"models":[
            {"slug":"a","name":"   ","status":"current"}]}}});
        assert!(decode_manifest(blank).is_none());
    }

    #[test]
    fn validates_claude_adapter_payloads() {
        let manifest = |profile: Value, model: Value| {
            json!({"version":1,"currentModels":{},"providers":{"claudeAgent":{
                "profiles":{"standard":{"adapter":profile}},
                "models":[{"slug":"opus","name":"Opus","status":"current","adapter":model}]}}})
        };
        let valid = manifest(
            json!({"claudeCode":{"effortMap":{"high":null,"max":"max"},"modelSuffixes":{"1m":{"low":"[1m]"}},"contextWindowTokens":{"opus":1000000},"fixedContextWindowTokens":200000}}),
            json!({"claudeCode":{"minVersion":"2.1.0","maxVersionExclusive":"3.0.0"}}),
        );
        assert!(decode_manifest(valid).is_some());
        for (profile, model) in [
            (json!({"claudeCode":null}), json!({})),
            (json!({"claudeCode":{"effortMap":{"high":3}}}), json!({})),
            (
                json!({"claudeCode":{"contextWindowTokens":{"opus":"big"}}}),
                json!({}),
            ),
            (json!({}), json!({"claudeCode":{"minVersion":"abc"}})),
            (
                json!({}),
                json!({"claudeCode":{"minVersion":"3.0.0","maxVersionExclusive":"2.1.0"}}),
            ),
            (json!({}), json!({"claudeCode":{"minVersion":null}})),
            (json!("claude"), json!({})),
        ] {
            assert!(
                decode_manifest(manifest(profile.clone(), model.clone())).is_none(),
                "{profile} {model}"
            );
        }
    }

    #[tokio::test]
    async fn stale_cache_does_not_override_newer_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let old = ModelManifestData {
            version: 1,
            updated_at: Some("2000-01-01T00:00:00Z".into()),
            compatibility: None,
            current_models: IndexMap::new(),
            providers: None,
        };
        tokio::fs::write(
            dir.path().join("model-manifest.json"),
            serde_json::to_vec(&ManifestCache {
                fetched_at_ms: 0,
                manifest: old,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let service = ModelManifest::new(dir.path(), disabled());
        assert_eq!(
            service.current().await.updated_at,
            bundled_model_manifest().updated_at
        );
    }

    #[tokio::test]
    async fn a_fetched_manifest_wins_over_the_bundle_and_is_cached_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = serve(vec![(200, remote("2999-01-01T00:00:00Z", "gpt-fetched"))]).await;
        let service = ModelManifest::with_url(dir.path(), enabled(), url);
        assert_eq!(codex_models(&service.refresh().await), ["gpt-fetched"]);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // A fresh process reads the cache without touching the network.
        let reloaded = ModelManifest::with_url(dir.path(), disabled(), "http://127.0.0.1:1/unused");
        assert_eq!(codex_models(&reloaded.current().await), ["gpt-fetched"]);
    }

    #[tokio::test]
    async fn explicit_refresh_bypasses_fresh_memory() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = serve(vec![
            (200, remote("2999-01-01T00:00:00Z", "first")),
            (200, remote("2999-01-02T00:00:00Z", "second")),
        ])
        .await;
        let service = ModelManifest::with_url(dir.path(), enabled(), url);
        assert_eq!(codex_models(&service.refresh().await), ["first"]);
        assert_eq!(codex_models(&service.refresh().await), ["first"]);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(codex_models(&service.force_refresh().await), ["second"]);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failure_keeps_the_bundle_and_waits_out_the_retry_window_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = serve(vec![
            (500, String::new()),
            (200, remote("2999-01-01T00:00:00Z", "recovered")),
        ])
        .await;
        let service = ModelManifest::with_url(dir.path(), enabled(), url);
        assert_eq!(
            service.refresh().await.updated_at,
            bundled_model_manifest().updated_at
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        service.refresh().await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(codex_models(&service.force_refresh().await), ["recovered"]);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn malformed_payload_keeps_the_last_good_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let (url, _hits) = serve(vec![
            (200, remote("2999-01-01T00:00:00Z", "good")),
            (200, r#"{"version":2,"currentModels":{}}"#.to_owned()),
        ])
        .await;
        let service = ModelManifest::with_url(dir.path(), enabled(), url);
        assert_eq!(codex_models(&service.refresh().await), ["good"]);
        assert_eq!(codex_models(&service.force_refresh().await), ["good"]);
    }

    #[tokio::test]
    async fn an_older_remote_edit_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (url, _hits) = serve(vec![(200, remote("2000-01-01T00:00:00Z", "stale"))]).await;
        let service = ModelManifest::with_url(dir.path(), enabled(), url);
        let manifest = service.refresh().await;
        assert_eq!(manifest.updated_at, bundled_model_manifest().updated_at);
        assert!(!dir.path().join("model-manifest.json").exists());
    }

    #[tokio::test]
    async fn disabled_update_checks_never_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = serve(Vec::new()).await;
        let service = ModelManifest::with_url(dir.path(), disabled(), url);
        assert_eq!(
            service.refresh().await.updated_at,
            bundled_model_manifest().updated_at
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
