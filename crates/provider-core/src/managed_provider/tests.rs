//! Port of upstream/packages/provider-core/src/server/managedProvider.test.ts
//! (`makeManagedServerProvider`, 12 cases).
//!
//! Upstream drives time with `TestClock`; here the cases that move time run on
//! tokio's paused clock (`start_paused`) and the rest on the real clock, where
//! their one-hour interval never fires. `Deferred` is a one-shot gate, and the
//! host is a [`DesktopProviderHost`] with a fixed background-work answer.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    provider_instance::ProviderDriverKind,
    provider_usage_limits::ProviderUsageLimitsUpdate,
    settings::{BackgroundActivityProfileSelection, ServerSettings, default_server_settings},
};

use super::*;
use crate::{
    maintenance::ProviderMaintenanceCommandAction,
    provider_host_live::{DesktopProviderHost, DesktopSettingsHandle, ShouldRunBackgroundWork},
};

#[derive(Clone, Debug, PartialEq)]
struct TestSettings {
    enabled: bool,
}

/// `Deferred<void>`: completes once, can be awaited any number of times.
#[derive(Clone)]
struct Deferred(Arc<watch::Sender<bool>>);

impl Deferred {
    fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    fn succeed(&self) {
        self.0.send_replace(true);
    }

    async fn wait(&self) {
        let mut receiver = self.0.subscribe();
        receiver.wait_for(|done| *done).await.unwrap();
    }
}

/// Spins on `condition`, yielding between checks. Bounded by attempts, not by
/// a timer: a timer never fires while the paused clock has a task to run, so a
/// timeout would turn a regression into a hang.
async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition was not met");
}

/// Lets spawned tasks run to their next await.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

fn maintenance_capabilities() -> ProviderMaintenanceCapabilities {
    ProviderMaintenanceCapabilities {
        provider: ProviderDriverKind::new("codex").unwrap(),
        package_name: Some("@openai/codex".to_owned()),
        update: Some(ProviderMaintenanceCommandAction {
            command: "npm install -g @openai/codex@latest".to_owned(),
            executable: "npm".to_owned(),
            args: vec![
                "install".to_owned(),
                "-g".to_owned(),
                "@openai/codex@latest".to_owned(),
            ],
            lock_key: "npm-global".to_owned(),
            env: None,
        }),
        latest_version: None,
        compare_versions: None,
    }
}

fn snapshot(overrides: Value) -> ServerProvider {
    let mut value = json!({
        "instanceId": "codex",
        "driver": "codex",
        "enabled": true,
        "installed": true,
        "version": "1.0.0",
        "status": "ready",
        "auth": { "status": "authenticated" },
        "checkedAt": "2026-04-10T00:00:01.000Z",
        "models": [],
        "slashCommands": [],
        "skills": [],
    });
    for (key, entry) in overrides.as_object().unwrap() {
        value[key] = entry.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn initial_snapshot() -> ServerProvider {
    snapshot(json!({
        "version": null,
        "status": "warning",
        "auth": { "status": "unknown" },
        "checkedAt": "2026-04-10T00:00:00.000Z",
        "message": "Checking provider availability...",
    }))
}

fn refreshed_snapshot() -> ServerProvider {
    snapshot(json!({}))
}

fn fast_mode_model(slug: &str, name: &str, capabilities: Value) -> Value {
    json!({ "slug": slug, "name": name, "isCustom": false, "capabilities": capabilities })
}

fn enriched_snapshot() -> ServerProvider {
    snapshot(json!({
        "checkedAt": "2026-04-10T00:00:02.000Z",
        "models": [fast_mode_model(
            "composer-2",
            "Composer 2",
            json!({ "optionDescriptors": [{ "id": "fastMode", "label": "Fast Mode", "type": "boolean" }] }),
        )],
    }))
}

fn refreshed_snapshot_second() -> ServerProvider {
    snapshot(json!({
        "checkedAt": "2026-04-10T00:00:03.000Z",
        "message": "Refreshed provider availability again.",
    }))
}

fn enriched_snapshot_second() -> ServerProvider {
    snapshot(json!({
        "checkedAt": "2026-04-10T00:00:04.000Z",
        "message": "Refreshed provider availability again.",
        "models": [fast_mode_model(
            "gpt-5.4",
            "GPT-5.4",
            json!({ "optionDescriptors": [] }),
        )],
    }))
}

/// A host whose background demand is fixed. The temp dir lives as long as the
/// returned value.
struct TestHost {
    host: Arc<ProviderHost>,
    settings: DesktopSettingsHandle,
    _dir: tempfile::TempDir,
}

fn test_host(run_background_work: bool, settings: ServerSettings) -> TestHost {
    let dir = tempfile::tempdir().unwrap();
    let should_run: ShouldRunBackgroundWork =
        Arc::new(move |_scope| Box::pin(async move { run_background_work }));
    let live = DesktopProviderHost::new(dir.path(), dir.path(), settings, should_run).unwrap();
    TestHost {
        settings: live.settings_handle(),
        host: Arc::new(live.host),
        _dir: dir,
    }
}

fn always_run() -> TestHost {
    test_host(true, default_server_settings())
}

fn never_run() -> TestHost {
    test_host(false, default_server_settings())
}

/// An input with upstream's defaults for these cases; each case overrides what
/// it exercises.
fn input(
    check_provider: impl Fn() -> BoxFuture<'static, ServerProvider> + Send + Sync + 'static,
) -> ManagedServerProviderInput<TestSettings> {
    ManagedServerProviderInput {
        resolve_maintenance: Arc::new(|_fresh| Box::pin(async { maintenance_capabilities() })),
        get_settings: Arc::new(|| Box::pin(async { Ok(TestSettings { enabled: true }) })),
        have_settings_changed: Arc::new(|previous, next| previous.enabled != next.enabled),
        initial_snapshot: Arc::new(|_settings| Box::pin(async { initial_snapshot() })),
        check_provider: Arc::new(move || {
            let probed = check_provider();
            Box::pin(async move { Ok(probed.await) })
        }),
        enrich_snapshot: None,
        refresh_interval: Some(Duration::from_secs(3600)),
        refresh_on_interval: true,
        check_provider_on_settings_change: None,
    }
}

fn ready(snapshot: ServerProvider) -> impl Fn() -> BoxFuture<'static, ServerProvider> {
    move || {
        let snapshot = snapshot.clone();
        Box::pin(async move { snapshot })
    }
}

async fn start(
    input: ManagedServerProviderInput<TestSettings>,
    host: &TestHost,
) -> ManagedServerProvider<TestSettings> {
    start_with_settings(input, host, Box::pin(futures::stream::empty())).await
}

async fn start_with_settings(
    input: ManagedServerProviderInput<TestSettings>,
    host: &TestHost,
    settings_stream: BoxStream<'static, TestSettings>,
) -> ManagedServerProvider<TestSettings> {
    ManagedServerProvider::start(input, settings_stream, Arc::clone(&host.host))
        .await
        .unwrap()
}

/// Collects the next `count` snapshots published after this call.
fn take_updates(
    provider: &ManagedServerProvider<TestSettings>,
    count: usize,
) -> tokio::task::JoinHandle<Vec<ServerProvider>> {
    let stream = provider.stream_changes();
    tokio::spawn(async move { stream.take(count).collect::<Vec<_>>().await })
}

// ── Cases ──────────────────────────────────────────────────────────

#[tokio::test]
async fn runs_the_initial_provider_check_in_the_background_and_streams_the_refreshed_snapshot() {
    let check_calls = Arc::new(AtomicUsize::new(0));
    let release_check = Deferred::new();
    let host = always_run();
    let provider = start(
        input({
            let check_calls = Arc::clone(&check_calls);
            let release_check = release_check.clone();
            move || {
                check_calls.fetch_add(1, Ordering::SeqCst);
                let release_check = release_check.clone();
                Box::pin(async move {
                    release_check.wait().await;
                    refreshed_snapshot()
                })
            }
        }),
        &host,
    )
    .await;

    let initial = provider.get_snapshot().await;
    assert_eq!(initial, initial_snapshot());

    let updates = take_updates(&provider, 1);
    wait_until(|| check_calls.load(Ordering::SeqCst) == 1).await;

    release_check.succeed();

    let updates = updates.await.unwrap();
    let latest = provider.get_snapshot().await;

    assert_eq!(updates, vec![refreshed_snapshot()]);
    assert_eq!(latest, refreshed_snapshot());
    assert_eq!(check_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_enrich_a_disabled_providers_snapshot() {
    let enrichment_calls = Arc::new(AtomicUsize::new(0));
    let host = always_run();
    let mut managed_input = input(ready(snapshot(json!({ "enabled": false }))));
    managed_input.get_settings =
        Arc::new(|| Box::pin(async { Ok(TestSettings { enabled: false }) }));
    managed_input.refresh_on_interval = false;
    managed_input.initial_snapshot =
        Arc::new(|_settings| Box::pin(async { snapshot(json!({ "enabled": false })) }));
    managed_input.enrich_snapshot = Some({
        let enrichment_calls = Arc::clone(&enrichment_calls);
        Arc::new(move |_input| {
            enrichment_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        })
    });
    let provider = start(managed_input, &host).await;

    provider.refresh().await;
    settle().await;

    assert_eq!(enrichment_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn skips_periodic_provider_refreshes_without_foreground_provider_status_demand() {
    let check_calls = Arc::new(AtomicUsize::new(0));
    let initial_check_done = Deferred::new();
    let host = never_run();
    let mut managed_input = input({
        let check_calls = Arc::clone(&check_calls);
        let initial_check_done = initial_check_done.clone();
        move || {
            if check_calls.fetch_add(1, Ordering::SeqCst) + 1 == 1 {
                initial_check_done.succeed();
            }
            Box::pin(async { refreshed_snapshot() })
        }
    });
    managed_input.refresh_interval = Some(Duration::from_secs(1));
    let _provider = start(managed_input, &host).await;

    initial_check_done.wait().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;

    assert_eq!(check_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn disables_periodic_provider_refreshes_when_the_explicit_interval_is_zero() {
    let check_calls = Arc::new(AtomicUsize::new(0));
    let initial_check_done = Deferred::new();
    let host = always_run();
    let mut managed_input = input({
        let check_calls = Arc::clone(&check_calls);
        let initial_check_done = initial_check_done.clone();
        move || {
            check_calls.fetch_add(1, Ordering::SeqCst);
            initial_check_done.succeed();
            Box::pin(async { refreshed_snapshot() })
        }
    });
    managed_input.refresh_interval = Some(Duration::ZERO);
    let _provider = start(managed_input, &host).await;

    initial_check_done.wait().await;
    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    settle().await;

    assert_eq!(check_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn keeps_manual_refresh_when_interval_refresh_is_disabled() {
    let check_calls = Arc::new(AtomicUsize::new(0));
    let initial_check_done = Deferred::new();
    let host = always_run();
    let mut managed_input = input({
        let check_calls = Arc::clone(&check_calls);
        let initial_check_done = initial_check_done.clone();
        move || {
            if check_calls.fetch_add(1, Ordering::SeqCst) + 1 == 1 {
                initial_check_done.succeed();
            }
            Box::pin(async { refreshed_snapshot() })
        }
    });
    managed_input.refresh_interval = Some(Duration::from_secs(1));
    managed_input.refresh_on_interval = false;
    let provider = start(managed_input, &host).await;

    initial_check_done.wait().await;
    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    settle().await;
    assert_eq!(check_calls.load(Ordering::SeqCst), 1);

    provider.refresh().await;
    assert_eq!(check_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn wakes_a_sleeping_provider_refresh_loop_when_its_interval_changes() {
    let with_interval = |millis: u64| {
        let mut settings = default_server_settings();
        settings.background_activity.profile = BackgroundActivityProfileSelection::Custom;
        settings
            .background_activity
            .overrides
            .provider_health_refresh_interval = Some(millis);
        settings
    };
    let host = test_host(true, with_interval(60 * 60 * 1000));
    let check_calls = Arc::new(AtomicUsize::new(0));
    let initial_check_done = Deferred::new();
    let periodic_check_done = Deferred::new();
    let mut managed_input = input({
        let check_calls = Arc::clone(&check_calls);
        let initial_check_done = initial_check_done.clone();
        let periodic_check_done = periodic_check_done.clone();
        move || {
            if check_calls.fetch_add(1, Ordering::SeqCst) + 1 == 1 {
                initial_check_done.succeed();
            } else {
                periodic_check_done.succeed();
            }
            Box::pin(async { refreshed_snapshot() })
        }
    });
    // The interval comes from the host's server settings.
    managed_input.refresh_interval = None;
    let _provider = start(managed_input, &host).await;

    initial_check_done.wait().await;
    host.settings.update_settings(with_interval(1000)).await;
    settle().await;

    tokio::time::advance(Duration::from_millis(999)).await;
    settle().await;
    assert_eq!(check_calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    // Not `periodic_check_done.wait()`: an idle paused clock would jump to the
    // old one-hour wake-up and pass even if the interval change never woke the loop.
    wait_until(|| check_calls.load(Ordering::SeqCst) == 2).await;
    assert!(*periodic_check_done.0.borrow());
    assert_eq!(check_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reruns_the_provider_check_when_streamed_settings_change() {
    let settings = Arc::new(Mutex::new(TestSettings { enabled: true }));
    let (settings_changes, settings_receiver) = mpsc::unbounded_channel::<TestSettings>();
    let check_calls = Arc::new(AtomicUsize::new(0));
    let release_initial_check = Deferred::new();
    let release_settings_check = Deferred::new();
    let host = always_run();
    let mut managed_input = input({
        let check_calls = Arc::clone(&check_calls);
        let release_initial_check = release_initial_check.clone();
        let release_settings_check = release_settings_check.clone();
        move || {
            let count = check_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let release_initial_check = release_initial_check.clone();
            let release_settings_check = release_settings_check.clone();
            Box::pin(async move {
                if count == 1 {
                    release_initial_check.wait().await;
                    refreshed_snapshot()
                } else {
                    release_settings_check.wait().await;
                    refreshed_snapshot_second()
                }
            })
        }
    });
    managed_input.get_settings = {
        let settings = Arc::clone(&settings);
        Arc::new(move || {
            let current = settings.lock().clone();
            Box::pin(async move { Ok(current) })
        })
    };
    let provider = start_with_settings(
        managed_input,
        &host,
        Box::pin(UnboundedReceiverStream::new(settings_receiver)),
    )
    .await;

    let updates = take_updates(&provider, 2);
    settle().await;

    release_initial_check.succeed();
    *settings.lock() = TestSettings { enabled: false };
    settings_changes
        .send(TestSettings { enabled: false })
        .unwrap();
    release_settings_check.succeed();

    let updates = updates.await.unwrap();
    let latest = provider.get_snapshot().await;

    assert_eq!(
        updates,
        vec![refreshed_snapshot(), refreshed_snapshot_second()]
    );
    assert_eq!(latest, refreshed_snapshot_second());
    assert_eq!(check_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn can_update_settings_and_disable_periodic_checks_without_probing_again() {
    let (settings_changes, settings_receiver) = mpsc::unbounded_channel::<TestSettings>();
    let check_calls = Arc::new(AtomicUsize::new(0));
    let initial_check_done = Deferred::new();
    let enrichment_calls = Arc::new(AtomicUsize::new(0));
    let host = always_run();
    let mut managed_input = input({
        let check_calls = Arc::clone(&check_calls);
        let initial_check_done = initial_check_done.clone();
        move || {
            check_calls.fetch_add(1, Ordering::SeqCst);
            initial_check_done.succeed();
            Box::pin(async { refreshed_snapshot() })
        }
    });
    managed_input.check_provider_on_settings_change = Some(Arc::new(|_previous, _next| false));
    managed_input.refresh_on_interval = false;
    managed_input.enrich_snapshot = Some({
        let enrichment_calls = Arc::clone(&enrichment_calls);
        Arc::new(move |_input| {
            enrichment_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        })
    });
    managed_input.refresh_interval = Some(Duration::from_secs(1));
    let _provider = start_with_settings(
        managed_input,
        &host,
        Box::pin(UnboundedReceiverStream::new(settings_receiver)),
    )
    .await;

    initial_check_done.wait().await;
    settings_changes
        .send(TestSettings { enabled: false })
        .unwrap();
    wait_until(|| enrichment_calls.load(Ordering::SeqCst) == 2).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;

    assert_eq!(check_calls.load(Ordering::SeqCst), 1);
    assert_eq!(enrichment_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn streams_supplemental_snapshot_updates_after_the_base_provider_check_completes() {
    let release_enrichment = Deferred::new();
    let release_check = Deferred::new();
    let host = always_run();
    let mut managed_input = input({
        let release_check = release_check.clone();
        move || {
            let release_check = release_check.clone();
            Box::pin(async move {
                release_check.wait().await;
                refreshed_snapshot()
            })
        }
    });
    managed_input.enrich_snapshot = Some({
        let release_enrichment = release_enrichment.clone();
        Arc::new(move |input: EnrichInput<TestSettings>| {
            let release_enrichment = release_enrichment.clone();
            Box::pin(async move {
                release_enrichment.wait().await;
                (input.publish_snapshot)(enriched_snapshot()).await;
            })
        })
    });
    let provider = start(managed_input, &host).await;

    let updates = take_updates(&provider, 2);
    settle().await;

    release_check.succeed();
    release_enrichment.succeed();

    let updates = updates.await.unwrap();
    let latest = provider.get_snapshot().await;

    assert_eq!(updates, vec![refreshed_snapshot(), enriched_snapshot()]);
    assert_eq!(latest, enriched_snapshot());
}

type PublishCallback = Arc<dyn Fn(ServerProvider) -> BoxFuture<'static, ()> + Send + Sync>;

#[tokio::test]
async fn ignores_stale_enrichment_callbacks_after_a_newer_refresh_advances_generation() {
    let publish_callbacks: Arc<Mutex<Vec<PublishCallback>>> = Arc::default();
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let first_callback_ready = Deferred::new();
    let second_callback_ready = Deferred::new();
    let allow_first_refresh = Deferred::new();
    let host = always_run();
    let mut managed_input = input({
        let refresh_count = Arc::clone(&refresh_count);
        let allow_first_refresh = allow_first_refresh.clone();
        move || {
            let count = refresh_count.fetch_add(1, Ordering::SeqCst) + 1;
            let allow_first_refresh = allow_first_refresh.clone();
            Box::pin(async move {
                if count == 1 {
                    allow_first_refresh.wait().await;
                    refreshed_snapshot()
                } else {
                    refreshed_snapshot_second()
                }
            })
        }
    });
    managed_input.enrich_snapshot = Some({
        let publish_callbacks = Arc::clone(&publish_callbacks);
        let first_callback_ready = first_callback_ready.clone();
        let second_callback_ready = second_callback_ready.clone();
        Arc::new(move |input: EnrichInput<TestSettings>| {
            let mut callbacks = publish_callbacks.lock();
            callbacks.push(input.publish_snapshot);
            match callbacks.len() {
                1 => first_callback_ready.succeed(),
                2 => second_callback_ready.succeed(),
                _ => {}
            }
            Box::pin(async {})
        })
    });
    let provider = start(managed_input, &host).await;

    let updates = take_updates(&provider, 3);
    settle().await;

    allow_first_refresh.succeed();
    first_callback_ready.wait().await;

    provider.refresh().await;
    second_callback_ready.wait().await;

    let callbacks = publish_callbacks.lock().clone();
    callbacks[0](enriched_snapshot()).await;
    callbacks[1](enriched_snapshot_second()).await;

    let updates = updates.await.unwrap();
    let latest = provider.get_snapshot().await;

    assert_eq!(
        updates,
        vec![
            refreshed_snapshot(),
            refreshed_snapshot_second(),
            enriched_snapshot_second()
        ]
    );
    assert_eq!(latest, enriched_snapshot_second());
}

fn usage_update(windows: Value) -> ProviderUsageLimitsUpdate {
    serde_json::from_value(json!({ "windows": windows })).unwrap()
}

#[tokio::test]
async fn applies_runtime_usage_updates_onto_the_published_snapshot() {
    let host = always_run();
    let provider = start(
        input(ready(snapshot(json!({
            "usageLimits": {
                "checkedAt": "2026-04-10T00:00:01.000Z",
                "windows": [
                    { "id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 10 },
                    {
                        "id": "seven_day", "kind": "weekly", "label": "Weekly",
                        "usedPercent": 20, "resetsAt": "2026-04-17T00:00:00.000Z",
                    },
                ],
            },
        })))),
        &host,
    )
    .await;
    wait_until(|| {
        let state = provider.inner.state.lock();
        state.snapshot.usage_limits.is_some()
    })
    .await;

    let updates = take_updates(&provider, 1);

    // Percent-only weekly update: keeps the probe's reset time.
    provider
        .apply_usage_limits(
            usage_update(json!([
                { "id": "seven_day", "kind": "weekly", "label": "Weekly", "usedPercent": 25 },
            ])),
            "2026-04-10T00:05:00.000Z",
        )
        .await;
    // No windows: nothing to merge, nothing published.
    provider
        .apply_usage_limits(usage_update(json!([])), "2026-04-10T00:06:00.000Z")
        .await;

    let updates = updates.await.unwrap();
    let update = &updates[0];
    assert_eq!(
        serde_json::to_value(&update.usage_limits).unwrap(),
        json!({
            "checkedAt": "2026-04-10T00:05:00.000Z",
            "windows": [
                { "id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 10 },
                {
                    "id": "seven_day", "kind": "weekly", "label": "Weekly",
                    "usedPercent": 25, "resetsAt": "2026-04-17T00:00:00.000Z",
                },
            ],
        })
    );
    assert_eq!(&provider.get_snapshot().await, update);
}

#[tokio::test]
async fn keeps_live_usage_windows_across_a_failed_probe_and_a_stale_enrichment() {
    let release_enrichment = Deferred::new();
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let probed_limits = json!({
        "checkedAt": "2026-04-10T00:00:01.000Z",
        "windows": [{ "id": "primary", "kind": "session", "label": "Session", "usedPercent": 10 }],
    });
    let host = always_run();
    let mut managed_input = input({
        let refresh_count = Arc::clone(&refresh_count);
        move || {
            let count = refresh_count.fetch_add(1, Ordering::SeqCst) + 1;
            let probed = if count == 1 {
                snapshot(json!({
                    "auth": { "status": "authenticated", "workspaceId": "ws-a" },
                    "usageLimits": probed_limits,
                }))
            } else {
                snapshot(json!({
                    "checkedAt": "2026-04-10T00:00:03.000Z",
                    "message": "Refreshed provider availability again.",
                    "auth": if count == 3 {
                        json!({ "status": "authenticated", "workspaceId": "ws-b" })
                    } else {
                        json!({ "status": "authenticated" })
                    },
                    "usageLimits": {
                        "checkedAt": "2026-04-10T00:00:03.000Z",
                        "windows": [],
                        "unavailable": { "reason": "probeFailed" },
                    },
                }))
            };
            Box::pin(async move { probed })
        }
    });
    managed_input.enrich_snapshot = Some({
        let release_enrichment = release_enrichment.clone();
        Arc::new(move |input: EnrichInput<TestSettings>| {
            let release_enrichment = release_enrichment.clone();
            Box::pin(async move {
                release_enrichment.wait().await;
                let mut next = input.snapshot.clone();
                next.models = enriched_snapshot().models.clone();
                (input.publish_snapshot)(next).await;
            })
        })
    });
    let provider = start(managed_input, &host).await;
    wait_until(|| provider.inner.state.lock().snapshot.usage_limits.is_some()).await;

    let live_window = json!({
        "id": "primary", "kind": "session", "label": "Session", "usedPercent": 60,
    });
    provider
        .apply_usage_limits(
            usage_update(json!([live_window])),
            "2026-04-10T00:00:02.000Z",
        )
        .await;

    // Enrichment computed from the pre-update snapshot lands afterwards.
    let enriched = take_updates(&provider, 1);
    release_enrichment.succeed();
    let enriched = enriched.await.unwrap().remove(0);
    assert_eq!(enriched.models, enriched_snapshot().models);
    assert_eq!(
        serde_json::to_value(&enriched.usage_limits.as_ref().unwrap().windows).unwrap(),
        json!([live_window])
    );

    // A probe that could not read usage keeps the last good windows.
    let refreshed = provider.refresh().await;
    assert_eq!(
        refreshed.message.as_ref().map(|message| message.as_str()),
        Some("Refreshed provider availability again.")
    );
    assert_eq!(
        serde_json::to_value(&refreshed.usage_limits.as_ref().unwrap().windows).unwrap(),
        json!([live_window])
    );
    // ...and the workspace they were read for.
    assert_eq!(
        refreshed.auth.workspace_id.as_ref().map(|id| id.as_str()),
        Some("ws-a")
    );

    // A failed read for another workspace does not inherit those windows.
    let switched = provider.refresh().await;
    assert_eq!(
        switched.auth.workspace_id.as_ref().map(|id| id.as_str()),
        Some("ws-b")
    );
    assert_eq!(
        serde_json::to_value(&switched.usage_limits.as_ref().unwrap().unavailable).unwrap(),
        json!({ "reason": "probeFailed" })
    );
}
