//! Port of upstream/apps/server/src/provider/providerStatusCache.ts.
//!
//! The last probe result for each provider instance, kept on disk so the first
//! snapshot after a restart shows what was known. Cached fields are only used
//! when the cache belongs to the same instance and driver as the fresh fallback.

use std::{
    io,
    path::{Path, PathBuf},
};

use turin_contracts::provider_instance::ProviderInstanceId;
use turin_shared::atomic_write::write_file_string_atomically;

use crate::server_provider::ServerProvider;

/// Built-in drivers in presentation order. Codex and Claude lead, the opt-in
/// providers follow, and unknown or fork drivers sort after every built-in.
const BUILT_IN_DRIVER_ORDER: [&str; 6] = [
    "codex",
    "claudeAgent",
    "cursor",
    "grok",
    "opencode",
    "antigravity",
];

fn driver_rank(driver: &str) -> usize {
    BUILT_IN_DRIVER_ORDER
        .iter()
        .position(|candidate| *candidate == driver)
        .unwrap_or(BUILT_IN_DRIVER_ORDER.len())
}

/// Upstream `orderProviderSnapshots`. `localeCompare` is a byte compare here;
/// the labels are ASCII-keyed driver and instance slugs.
pub fn order_provider_snapshots(providers: &[ServerProvider]) -> Vec<ServerProvider> {
    let mut ordered = providers.to_vec();
    ordered.sort_by(|left, right| {
        driver_rank(left.driver.as_str())
            .cmp(&driver_rank(right.driver.as_str()))
            .then_with(|| left.driver.as_str().cmp(right.driver.as_str()))
            .then_with(|| display_name_of(left).cmp(display_name_of(right)))
            .then_with(|| {
                left.instance_id
                    .to_string()
                    .cmp(&right.instance_id.to_string())
            })
    });
    ordered
}

fn display_name_of(provider: &ServerProvider) -> &str {
    provider
        .display_name
        .as_ref()
        .map_or("", |name| name.as_str())
}

/// Same instance id and same driver. The file name alone is not a routing key.
pub fn is_cached_provider_correlated(
    cached_provider: &ServerProvider,
    fallback_provider: &ServerProvider,
) -> bool {
    cached_provider.instance_id == fallback_provider.instance_id
        && cached_provider.driver == fallback_provider.driver
}

/// Cached models that the fallback does not already carry. Custom rows are
/// dropped: the fallback is built from current settings, so a cached custom row
/// that is missing from it was removed while the cache was stale.
fn merge_provider_models(
    fallback: &ServerProvider,
    cached: &ServerProvider,
) -> Vec<crate::server_provider::ServerProviderModel> {
    let mut models = fallback.models.clone();
    models.extend(
        cached
            .models
            .iter()
            .filter(|model| {
                !model.is_custom
                    && !fallback
                        .models
                        .iter()
                        .any(|candidate| candidate.slug == model.slug)
            })
            .cloned(),
    );
    models
}

/// Upstream `hydrateCachedProvider`. Keeps the fallback's settings-derived
/// fields and takes probe results from the cache, unless the instance is
/// disabled now or the cache does not belong to this instance.
pub fn hydrate_cached_provider(
    cached_provider: &ServerProvider,
    fallback_provider: &ServerProvider,
) -> ServerProvider {
    if !is_cached_provider_correlated(cached_provider, fallback_provider) {
        return fallback_provider.clone();
    }
    if !fallback_provider.enabled || cached_provider.enabled != fallback_provider.enabled {
        return fallback_provider.clone();
    }

    let mut hydrated = fallback_provider.clone();
    // The fallback's message is a pending-refresh placeholder; the cached message
    // (if any) replaces it below.
    hydrated.message = None;
    hydrated.models = merge_provider_models(fallback_provider, cached_provider);
    hydrated.installed = cached_provider.installed;
    hydrated.version = cached_provider.version.clone();
    hydrated.status = cached_provider.status;
    hydrated.auth = cached_provider.auth.clone();
    hydrated.checked_at = cached_provider.checked_at.clone();
    hydrated.slash_commands = cached_provider.slash_commands.clone();
    hydrated.skills = cached_provider.skills.clone();
    if let Some(message) = &cached_provider.message {
        hydrated.message = Some(message.clone());
    }
    hydrated
}

/// `<cacheDir>/<instanceId>.json`. For a built-in default instance the instance
/// id is the driver kind, so the legacy `<kind>.json` file keeps being read.
pub fn resolve_provider_status_cache_path(
    cache_dir: &Path,
    instance_id: &ProviderInstanceId,
) -> PathBuf {
    cache_dir.join(format!("{instance_id}.json"))
}

/// Reads one cached snapshot. A missing, empty, or undecodable file is `None`.
/// The decode error is logged by its kind only, so the cache contents never
/// reach the log.
pub async fn read_provider_status_cache(file_path: &Path) -> Option<ServerProvider> {
    let raw = tokio::fs::read_to_string(file_path).await.ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str::<ServerProvider>(trimmed) {
        Ok(provider) => Some(provider),
        Err(error) => {
            tracing::warn!(
                path = %file_path.display(),
                error_kind = ?error.classify(),
                "failed to parse provider status cache, ignoring"
            );
            None
        }
    }
}

/// Writes one cached snapshot atomically, leaving out the transient
/// `updateState` the probe attaches.
pub async fn write_provider_status_cache(
    file_path: &Path,
    provider: &ServerProvider,
) -> io::Result<()> {
    let mut cacheable = provider.clone();
    cacheable.update_state = None;
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&cacheable).map_err(io::Error::other)?
    );
    let file_path = file_path.to_owned();
    tokio::task::spawn_blocking(move || write_file_string_atomically(&file_path, &contents))
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/providerStatusCache.test.ts.
    //! The legacy `provider`-keyed cache shape from the upstream correlation case
    //! cannot be built as a typed `ServerProvider`, so it is covered by the
    //! undecodable-file read below. The log assertions of "logs structural
    //! diagnostics without retaining invalid cache contents" are ported with a
    //! capturing `tracing` layer: the field is `error_kind` (a serde error class),
    //! where upstream's is `errorTag`.

    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::*;

    fn make_provider(driver: &str, overrides: Value) -> ServerProvider {
        let mut base = json!({
            "instanceId": driver,
            "driver": driver,
            "enabled": true,
            "installed": true,
            "version": "1.0.0",
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": "2026-04-11T00:00:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": [],
        });
        if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
            base.extend(overrides);
        }
        serde_json::from_value(base).expect("provider fixture")
    }

    fn model(slug: &str, is_custom: bool) -> Value {
        json!({
            "slug": slug,
            "name": slug,
            "isCustom": is_custom,
            "capabilities": { "optionDescriptors": [] },
        })
    }

    #[tokio::test]
    async fn an_invalid_cache_file_reads_as_none_without_retaining_its_contents() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("provider.json");
        tokio::fs::write(&path, r#"{ "token": "secret-cache-value" }"#)
            .await
            .expect("write fixture");
        let (_guard, captured) = capture_logs();
        assert!(read_provider_status_cache(&path).await.is_none());
        let events = captured.lock().unwrap().clone();
        let failure = events
            .iter()
            .find(|fields| fields.iter().any(|(name, _)| name == "path"))
            .expect("the failure is logged with its path");
        let field = |name: &str| {
            failure
                .iter()
                .find(|(field, _)| field == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(field("path"), Some(path.display().to_string().as_str()));
        assert!(field("error_kind").is_some_and(|kind| !kind.is_empty()));
        assert_eq!(field("cause"), None);
        assert_eq!(field("issues"), None);
        assert!(
            events
                .iter()
                .flatten()
                .all(|(_, value)| !value.contains("secret-cache-value")),
            "the cache contents reached the log: {events:?}"
        );
    }

    type CapturedEvents = std::sync::Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>;

    /// Installs a thread-local `tracing` subscriber (until the guard drops) that
    /// records every event's fields as `(name, rendered value)`. The test runtime
    /// is current-thread, so awaited code logs on this thread.
    fn capture_logs() -> (tracing::subscriber::DefaultGuard, CapturedEvents) {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::{Layer, layer::SubscriberExt};

        #[derive(Default)]
        struct Fields(Vec<(String, String)>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push((field.name().to_owned(), format!("{value:?}")));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push((field.name().to_owned(), value.to_owned()));
            }
        }
        struct Capture(CapturedEvents);
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut fields = Fields::default();
                event.record(&mut fields);
                self.0.lock().unwrap().push(fields.0);
            }
        }

        let events: CapturedEvents = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Capture(Arc::clone(&events)));
        (tracing::subscriber::set_default(subscriber), events)
    }

    #[tokio::test]
    async fn writes_and_reads_provider_status_snapshots() {
        let dir = tempdir().expect("tempdir");
        let codex = make_provider("codex", json!({}));
        let claude = make_provider(
            "claudeAgent",
            json!({ "status": "warning", "auth": { "status": "unknown" } }),
        );
        let opencode = make_provider(
            "opencode",
            json!({ "status": "warning", "auth": { "status": "unknown", "type": "opencode" } }),
        );
        for provider in [&codex, &claude, &opencode] {
            let path = resolve_provider_status_cache_path(dir.path(), &provider.instance_id);
            write_provider_status_cache(&path, provider)
                .await
                .expect("write cache");
            assert_eq!(
                read_provider_status_cache(&path).await,
                Some(provider.clone())
            );
        }
    }

    #[test]
    fn resolves_the_cache_path_from_the_instance_id() {
        let dir = Path::new("/cache");
        assert_eq!(
            resolve_provider_status_cache_path(
                dir,
                &ProviderInstanceId::new("codex_personal").expect("id")
            ),
            PathBuf::from("/cache/codex_personal.json")
        );
        assert_eq!(
            resolve_provider_status_cache_path(dir, &ProviderInstanceId::new("codex").expect("id")),
            PathBuf::from("/cache/codex.json")
        );
    }

    #[test]
    fn hydrates_cached_provider_status_while_preserving_current_settings_derived_models() {
        let cached = make_provider(
            "codex",
            json!({
                "checkedAt": "2026-04-10T12:00:00.000Z",
                "models": [model("gpt-5-mini", false)],
                "message": "Cached message",
                "skills": [{
                    "name": "github:gh-fix-ci",
                    "path": "/tmp/skills/gh-fix-ci/SKILL.md",
                    "enabled": true,
                    "displayName": "CI Debug",
                }],
            }),
        );
        let fallback = make_provider(
            "codex",
            json!({
                "models": [model("gpt-5.4", false)],
                "message": "Pending refresh",
            }),
        );

        let hydrated = hydrate_cached_provider(&cached, &fallback);

        let expected = make_provider(
            "codex",
            json!({
                "models": [model("gpt-5.4", false), model("gpt-5-mini", false)],
                "installed": cached.installed,
                "version": cached.version,
                "status": "ready",
                "auth": { "status": "authenticated" },
                "checkedAt": "2026-04-10T12:00:00.000Z",
                "slashCommands": [],
                "skills": cached.skills,
                "message": "Cached message",
            }),
        );
        assert_eq!(hydrated, expected);
    }

    #[test]
    fn does_not_resurrect_cached_custom_models_that_settings_no_longer_declare() {
        let built_in = model("gpt-5.4", false);
        let cached = make_provider(
            "codex",
            json!({ "models": [built_in.clone(), model("removed-custom", true)] }),
        );
        let fallback = make_provider("codex", json!({ "models": [built_in.clone()] }));

        assert_eq!(
            hydrate_cached_provider(&cached, &fallback).models,
            make_provider("codex", json!({ "models": [built_in] })).models
        );
    }

    #[test]
    fn ignores_stale_cached_enabled_state_when_the_provider_is_now_disabled() {
        let cached = make_provider(
            "codex",
            json!({ "checkedAt": "2026-04-10T12:00:00.000Z", "message": "Cached ready status" }),
        );
        let disabled = make_provider(
            "codex",
            json!({
                "enabled": false,
                "installed": false,
                "version": null,
                "status": "disabled",
                "auth": { "status": "unknown" },
                "message": "Codex is disabled in T3 Code settings.",
            }),
        );

        assert_eq!(hydrate_cached_provider(&cached, &disabled), disabled);
    }

    #[test]
    fn rejects_cached_snapshots_that_are_not_correlated_to_the_fallback_instance() {
        let fallback = make_provider("codex", json!({ "models": [model("gpt-5.4", false)] }));
        let mismatched = make_provider("codex", json!({ "instanceId": "codex_personal" }));

        assert!(!is_cached_provider_correlated(&mismatched, &fallback));
        assert_eq!(hydrate_cached_provider(&mismatched, &fallback), fallback);
        // A different driver under the same instance id is not correlated either.
        let other_driver = make_provider("claudeAgent", json!({ "instanceId": "codex" }));
        assert!(!is_cached_provider_correlated(&other_driver, &fallback));
    }

    #[test]
    fn orders_built_in_drivers_first_and_unknown_drivers_last() {
        let ordered = order_provider_snapshots(&[
            make_provider("customFork", json!({})),
            make_provider("opencode", json!({})),
            make_provider("codex", json!({})),
            make_provider("claudeAgent", json!({})),
        ]);
        let drivers: Vec<&str> = ordered.iter().map(|p| p.driver.as_str()).collect();
        assert_eq!(drivers, ["codex", "claudeAgent", "opencode", "customFork"]);
    }
}
