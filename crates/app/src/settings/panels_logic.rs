//! Port of upstream/apps/web/src/components/settings/SettingsPanels.logic.ts
//! (and SettingsPanels.logic.test.ts).
//!
//! Not ported: `BrowserDefaultSettings` / `getChangedBrowserSettingLabels`
//! (the browser integration is cut; see PORTING.md Deviations).

use std::sync::atomic::{AtomicU8, Ordering};

use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::{ProviderInstanceConfig, ProviderInstanceId};
use turin_contracts::settings::{
    BackgroundActivityOverrides, BackgroundActivityProfile, BackgroundActivityProfileSelection,
    BackgroundActivitySchemaVersion, BackgroundActivitySettings, ClientSettings,
    DurationFromMillis, ServerSettings, ServerSettingsPatch, SidebarProjectGroupingMode,
    UnifiedSettings, default_unified_settings,
};
use turin_shared::server_settings::{
    ResolvedBackgroundActivity, normalize_background_activity_settings,
    normalize_server_background_activity_settings, resolve_server_background_activity_settings,
};

pub fn is_project_grouping_enabled(mode: SidebarProjectGroupingMode) -> bool {
    mode != SidebarProjectGroupingMode::Separate
}

pub fn project_grouping_mode_from_toggle(
    enabled: bool,
    last_enabled_mode: SidebarProjectGroupingMode,
) -> SidebarProjectGroupingMode {
    if !enabled {
        return SidebarProjectGroupingMode::Separate;
    }
    if last_enabled_mode == SidebarProjectGroupingMode::RepositoryPath {
        SidebarProjectGroupingMode::RepositoryPath
    } else {
        SidebarProjectGroupingMode::Repository
    }
}

/// Upstream keeps this in `localStorage` (`t3code:last-enabled-project-grouping-mode`).
/// Turin has no browser storage; the UI-state store (T22) persists it, and until
/// then it lives for the process.
static LAST_ENABLED_PROJECT_GROUPING_MODE: AtomicU8 = AtomicU8::new(0);

pub fn read_last_enabled_project_grouping_mode() -> SidebarProjectGroupingMode {
    if LAST_ENABLED_PROJECT_GROUPING_MODE.load(Ordering::Relaxed) == 1 {
        SidebarProjectGroupingMode::RepositoryPath
    } else {
        SidebarProjectGroupingMode::Repository
    }
}

pub fn remember_enabled_project_grouping_mode(mode: SidebarProjectGroupingMode) {
    let value = match mode {
        SidebarProjectGroupingMode::Separate => return,
        SidebarProjectGroupingMode::RepositoryPath => 1,
        SidebarProjectGroupingMode::Repository => 0,
    };
    LAST_ENABLED_PROJECT_GROUPING_MODE.store(value, Ordering::Relaxed);
}

/// The fields `hasChangedBackgroundActivitySettings` reads.
pub fn has_changed_background_activity_settings(settings: &ServerSettings) -> bool {
    let defaults = default_unified_settings().server;
    settings.background_activity != defaults.background_activity
        || settings.background_activity_profile != defaults.background_activity_profile
        || settings.automatic_git_fetch_interval != defaults.automatic_git_fetch_interval
        || settings.provider_health_refresh_interval != defaults.provider_health_refresh_interval
}

/// Labels the font rows whose family or size differs from the defaults.
pub fn get_changed_typography_setting_labels(settings: &ClientSettings) -> Vec<&'static str> {
    let defaults = default_unified_settings().client;
    let mut labels = Vec::new();
    if settings.font_family_sans != defaults.font_family_sans
        || settings.font_size_interface != defaults.font_size_interface
    {
        labels.push("Interface font");
    }
    if settings.font_family_composer != defaults.font_family_composer
        || settings.font_size_prompt != defaults.font_size_prompt
    {
        labels.push("Prompt font");
    }
    if settings.font_family_code != defaults.font_family_code
        || settings.font_size_code != defaults.font_size_code
    {
        labels.push("Code font");
    }
    if settings.font_family_terminal != defaults.font_family_terminal
        || settings.font_size_terminal != defaults.font_size_terminal
    {
        labels.push("Terminal font");
    }
    labels
}

/// The profile the General panel's select shows: the effective profile, or
/// `Advanced` when the resolved settings no longer match a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundActivityProfileOption {
    Profile(BackgroundActivityProfile),
    Advanced,
}

pub fn resolve_background_activity_profile_option(
    settings: &ServerSettings,
) -> BackgroundActivityProfileOption {
    let resolved = resolve_server_background_activity_settings(settings);
    let normalized = normalize_background_activity_settings(&BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(resolved.profile),
        overrides: overrides_from_resolved(&resolved),
    });
    match normalized.profile {
        BackgroundActivityProfileSelection::Custom => BackgroundActivityProfileOption::Advanced,
        BackgroundActivityProfileSelection::Balanced => {
            BackgroundActivityProfileOption::Profile(BackgroundActivityProfile::Balanced)
        }
        BackgroundActivityProfileSelection::Performance => {
            BackgroundActivityProfileOption::Profile(BackgroundActivityProfile::Performance)
        }
        BackgroundActivityProfileSelection::BatterySaver => {
            BackgroundActivityProfileOption::Profile(BackgroundActivityProfile::BatterySaver)
        }
    }
}

fn overrides_from_resolved(resolved: &ResolvedBackgroundActivity) -> BackgroundActivityOverrides {
    BackgroundActivityOverrides {
        automatic_git_fetch_interval: Some(resolved.automatic_git_fetch_interval),
        provider_health_refresh_interval: Some(resolved.provider_health_refresh_interval),
        host_power_monitor_active_interval: Some(resolved.host_power_monitor_active_interval),
        host_power_monitor_idle_interval: Some(resolved.host_power_monitor_idle_interval),
        idle_client_ttl: Some(resolved.idle_client_ttl),
        pause_when_host_locked: Some(resolved.pause_when_host_locked),
        pause_when_host_low_power: Some(resolved.pause_when_host_low_power),
        pause_when_client_low_power: Some(resolved.pause_when_client_low_power),
        pause_when_on_battery: Some(resolved.pause_when_on_battery),
    }
}

pub fn background_activity_shared_policy_settings(
    settings: &ServerSettings,
    profile: BackgroundActivityProfile,
) -> BackgroundActivitySettings {
    let normalized = normalize_server_background_activity_settings(settings);
    BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(profile),
        overrides: if normalized.profile == BackgroundActivityProfileSelection::Custom {
            normalized.overrides
        } else {
            BackgroundActivityOverrides::default()
        },
    }
}

fn collapse_otel_signals_url(traces_url: &str, metrics_url: &str) -> Option<String> {
    const TRACES_SUFFIX: &str = "/traces";
    const METRICS_SUFFIX: &str = "/metrics";
    let traces_base = traces_url.strip_suffix(TRACES_SUFFIX)?;
    let metrics_base = metrics_url.strip_suffix(METRICS_SUFFIX)?;
    if traces_base != metrics_base {
        return None;
    }
    Some(format!("{traces_base}/{{traces,metrics}}"))
}

pub struct DiagnosticsDescriptionInput<'a> {
    pub local_tracing_enabled: bool,
    pub otlp_traces_enabled: bool,
    pub otlp_traces_url: Option<&'a str>,
    pub otlp_metrics_enabled: bool,
    pub otlp_metrics_url: Option<&'a str>,
}

pub fn format_diagnostics_description(input: &DiagnosticsDescriptionInput<'_>) -> String {
    let mode = if input.local_tracing_enabled {
        "Local trace file"
    } else {
        "Terminal logs only"
    };
    // An empty URL is falsy in upstream's `if (tracesUrl && metricsUrl)`.
    let traces_url = input
        .otlp_traces_url
        .filter(|url| input.otlp_traces_enabled && !url.is_empty());
    let metrics_url = input
        .otlp_metrics_url
        .filter(|url| input.otlp_metrics_enabled && !url.is_empty());

    match (traces_url, metrics_url) {
        (Some(traces), Some(metrics)) => match collapse_otel_signals_url(traces, metrics) {
            Some(collapsed) => format!("{mode}. Exporting OTEL to {collapsed}."),
            None => format!("{mode}. Exporting OTEL traces to {traces} and metrics to {metrics}."),
        },
        (Some(traces), None) => format!("{mode}. Exporting OTEL traces to {traces}."),
        (None, Some(metrics)) => format!("{mode}. Exporting OTEL metrics to {metrics}."),
        (None, None) => format!("{mode}."),
    }
}

/// `Partial<UnifiedSettings>` of the server fields the provider panels write.
pub fn build_provider_instance_update_patch(
    settings: &ServerSettings,
    instance_id: &ProviderInstanceId,
    instance: ProviderInstanceConfig,
    text_generation_model_selection: Option<ModelSelection>,
) -> ServerSettingsPatch {
    let mut provider_instances = settings.provider_instances.clone();
    provider_instances.insert(instance_id.clone(), instance);
    ServerSettingsPatch {
        provider_instances: Some(provider_instances),
        text_generation_model_selection: text_generation_model_selection.map(|selection| {
            serde_json::from_value(serde_json::to_value(selection).unwrap()).unwrap()
        }),
        ..Default::default()
    }
}

// ── Background-activity interval helpers ─────────────────────────────
// Shared by the General panel's interval rows and the Providers panel's
// health-check row.

pub const PROVIDER_HEALTH_INTERVAL_STEP_SECONDS: u64 = 30;

/// `durationToSeconds`: `Math.round(ms / 1000)`.
pub fn duration_to_seconds(duration_ms: u64) -> u64 {
    (duration_ms as f64 / 1_000.).round() as u64
}

/// `normalizeIntervalSeconds`.
pub fn normalize_interval_seconds(value: Option<f64>, minimum: u64) -> u64 {
    match value {
        Some(value) if value.is_finite() => (value.round().max(minimum as f64)) as u64,
        _ => minimum,
    }
}

/// One override key, as the General panel's interval rows set them.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackgroundActivityOverridePatch {
    pub automatic_git_fetch_interval: Option<u64>,
    pub provider_health_refresh_interval: Option<u64>,
    pub host_power_monitor_active_interval: Option<u64>,
    pub host_power_monitor_idle_interval: Option<u64>,
    pub idle_client_ttl: Option<u64>,
    pub pause_when_host_locked: Option<bool>,
    pub pause_when_host_low_power: Option<bool>,
    pub pause_when_client_low_power: Option<bool>,
    pub pause_when_on_battery: Option<bool>,
}

/// `backgroundActivityOverrideSettings`: materializes every resolved value
/// as an override, then applies `overrides` on top.
pub fn background_activity_override_settings(
    current: &BackgroundActivitySettings,
    resolved: &ResolvedBackgroundActivity,
    overrides: BackgroundActivityOverridePatch,
) -> BackgroundActivitySettings {
    let base = overrides_from_resolved(resolved);
    BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(background_activity_base_profile(current)),
        overrides: BackgroundActivityOverrides {
            automatic_git_fetch_interval: overrides
                .automatic_git_fetch_interval
                .or(base.automatic_git_fetch_interval),
            provider_health_refresh_interval: overrides
                .provider_health_refresh_interval
                .or(base.provider_health_refresh_interval),
            host_power_monitor_active_interval: overrides
                .host_power_monitor_active_interval
                .or(base.host_power_monitor_active_interval),
            host_power_monitor_idle_interval: overrides
                .host_power_monitor_idle_interval
                .or(base.host_power_monitor_idle_interval),
            idle_client_ttl: overrides.idle_client_ttl.or(base.idle_client_ttl),
            pause_when_host_locked: overrides
                .pause_when_host_locked
                .or(base.pause_when_host_locked),
            pause_when_host_low_power: overrides
                .pause_when_host_low_power
                .or(base.pause_when_host_low_power),
            pause_when_client_low_power: overrides
                .pause_when_client_low_power
                .or(base.pause_when_client_low_power),
            pause_when_on_battery: overrides
                .pause_when_on_battery
                .or(base.pause_when_on_battery),
        },
    }
}

/// `backgroundActivityOverrideSettings` in SourceControlSettings.tsx: sets or
/// clears only `automaticGitFetchInterval` (`None` clears it) and keeps the other
/// saved overrides as they are, so they keep following the base profile. The
/// General helper above materializes every resolved value, which would pin them.
pub fn background_activity_fetch_interval_settings(
    current: &BackgroundActivitySettings,
    automatic_git_fetch_interval: Option<DurationFromMillis>,
) -> BackgroundActivitySettings {
    let mut overrides = current.overrides.clone();
    overrides.automatic_git_fetch_interval = automatic_git_fetch_interval;
    BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(background_activity_base_profile(current)),
        overrides,
    }
}

fn background_activity_base_profile(
    settings: &BackgroundActivitySettings,
) -> BackgroundActivityProfile {
    turin_shared::server_settings::get_background_activity_base_profile(settings)
}

/// Convenience for panels: the defaults as a [`UnifiedSettings`].
pub fn defaults() -> UnifiedSettings {
    default_unified_settings()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::settings::{
        FontFamilyPreference, InterfaceFontSize, default_server_settings,
    };
    use turin_shared::server_settings::get_background_activity_preset_settings;

    use super::*;

    #[test]
    fn typography_settings_restore_detects_family_and_size_changes_by_font_row() {
        let defaults = default_unified_settings().client;
        assert_eq!(
            get_changed_typography_setting_labels(&defaults),
            Vec::<&str>::new()
        );
        let mut changed = defaults.clone();
        changed.font_size_interface = InterfaceFontSize::new(18).unwrap();
        changed.font_family_code = FontFamilyPreference::new("Fira Code").unwrap();
        assert_eq!(
            get_changed_typography_setting_labels(&changed),
            vec!["Interface font", "Code font"]
        );
    }

    #[test]
    fn detects_legacy_interval_values_even_when_the_structured_setting_is_at_its_default() {
        let mut settings = default_server_settings();
        settings.automatic_git_fetch_interval = 45_000;
        assert!(has_changed_background_activity_settings(&settings));

        let mut settings = default_server_settings();
        settings.provider_health_refresh_interval = 7 * 60_000;
        assert!(has_changed_background_activity_settings(&settings));

        assert!(!has_changed_background_activity_settings(
            &default_server_settings()
        ));
    }

    #[test]
    fn detects_a_legacy_profile_override_so_restoring_defaults_clears_it() {
        let mut settings = default_server_settings();
        settings.background_activity_profile = BackgroundActivityProfile::Performance;
        assert!(has_changed_background_activity_settings(&settings));
    }

    #[test]
    fn shows_the_effective_legacy_preset_and_marks_custom_legacy_intervals_as_advanced() {
        let performance =
            get_background_activity_preset_settings(BackgroundActivityProfile::Performance);
        let mut settings = default_server_settings();
        settings.background_activity_profile = BackgroundActivityProfile::Performance;
        settings.automatic_git_fetch_interval = performance.automatic_git_fetch_interval;
        settings.provider_health_refresh_interval = performance.provider_health_refresh_interval;
        assert_eq!(
            resolve_background_activity_profile_option(&settings),
            BackgroundActivityProfileOption::Profile(BackgroundActivityProfile::Performance)
        );

        settings.automatic_git_fetch_interval = 45_000;
        settings.provider_health_refresh_interval = 7 * 60_000;
        assert_eq!(
            resolve_background_activity_profile_option(&settings),
            BackgroundActivityProfileOption::Advanced
        );
    }

    #[test]
    fn source_control_fetch_interval_keeps_other_overrides_following_the_profile() {
        let current = BackgroundActivitySettings {
            schema_version: BackgroundActivitySchemaVersion,
            profile: BackgroundActivityProfileSelection::Custom,
            base_profile: Some(BackgroundActivityProfile::Balanced),
            overrides: BackgroundActivityOverrides {
                pause_when_on_battery: Some(true),
                idle_client_ttl: Some(9_000),
                ..Default::default()
            },
        };
        let set = background_activity_fetch_interval_settings(&current, Some(60_000));
        assert_eq!(set.overrides.automatic_git_fetch_interval, Some(60_000));
        assert_eq!(set.overrides.pause_when_on_battery, Some(true));
        assert_eq!(set.overrides.idle_client_ttl, Some(9_000));
        assert_eq!(
            serde_json::to_value(&set.overrides).unwrap(),
            json!({
                "automaticGitFetchInterval": 60_000,
                "idleClientTtl": 9_000,
                "pauseWhenOnBattery": true,
            })
        );

        // Clearing removes only the fetch interval; nothing is materialized.
        let cleared = background_activity_fetch_interval_settings(&set, None);
        assert_eq!(cleared.overrides.automatic_git_fetch_interval, None);
        assert_eq!(
            serde_json::to_value(&cleared.overrides).unwrap(),
            json!({ "idleClientTtl": 9_000, "pauseWhenOnBattery": true })
        );
    }

    #[test]
    fn preserves_advanced_overrides_when_the_shared_policy_changes() {
        let mut settings = default_server_settings();
        settings.background_activity = BackgroundActivitySettings {
            schema_version: BackgroundActivitySchemaVersion,
            profile: BackgroundActivityProfileSelection::Custom,
            base_profile: Some(BackgroundActivityProfile::Balanced),
            overrides: BackgroundActivityOverrides {
                automatic_git_fetch_interval: Some(42_000),
                pause_when_on_battery: Some(true),
                ..Default::default()
            },
        };
        assert_eq!(
            background_activity_shared_policy_settings(
                &settings,
                BackgroundActivityProfile::Performance
            ),
            BackgroundActivitySettings {
                schema_version: BackgroundActivitySchemaVersion,
                profile: BackgroundActivityProfileSelection::Custom,
                base_profile: Some(BackgroundActivityProfile::Performance),
                overrides: BackgroundActivityOverrides {
                    automatic_git_fetch_interval: Some(42_000),
                    pause_when_on_battery: Some(true),
                    ..Default::default()
                },
            }
        );
    }

    #[test]
    fn materializes_legacy_advanced_overrides_before_changing_the_shared_policy() {
        let mut settings = default_server_settings();
        settings.automatic_git_fetch_interval = 42_000;
        assert_eq!(
            background_activity_shared_policy_settings(
                &settings,
                BackgroundActivityProfile::BatterySaver
            ),
            BackgroundActivitySettings {
                schema_version: BackgroundActivitySchemaVersion,
                profile: BackgroundActivityProfileSelection::Custom,
                base_profile: Some(BackgroundActivityProfile::BatterySaver),
                overrides: BackgroundActivityOverrides {
                    automatic_git_fetch_interval: Some(42_000),
                    ..Default::default()
                },
            }
        );
    }

    #[test]
    fn project_grouping_toggle_enables_repository_grouping_and_disables_into_separate_projects() {
        use SidebarProjectGroupingMode::*;
        assert!(is_project_grouping_enabled(Repository));
        assert!(is_project_grouping_enabled(RepositoryPath));
        assert!(!is_project_grouping_enabled(Separate));
        assert_eq!(
            project_grouping_mode_from_toggle(true, Repository),
            Repository
        );
        assert_eq!(
            project_grouping_mode_from_toggle(false, Repository),
            Separate
        );
    }

    #[test]
    fn project_grouping_toggle_restores_repository_path_grouping_when_cycled() {
        use SidebarProjectGroupingMode::*;
        assert_eq!(
            project_grouping_mode_from_toggle(false, RepositoryPath),
            Separate
        );
        assert_eq!(
            project_grouping_mode_from_toggle(true, RepositoryPath),
            RepositoryPath
        );
    }

    #[test]
    fn diagnostics_description_collapses_trace_and_metric_urls_that_share_the_same_base_path() {
        assert_eq!(
            format_diagnostics_description(&DiagnosticsDescriptionInput {
                local_tracing_enabled: true,
                otlp_traces_enabled: true,
                otlp_traces_url: Some("http://localhost:4318/v1/traces"),
                otlp_metrics_enabled: true,
                otlp_metrics_url: Some("http://localhost:4318/v1/metrics"),
            }),
            "Local trace file. Exporting OTEL to http://localhost:4318/v1/{traces,metrics}."
        );
    }

    #[test]
    fn diagnostics_description_keeps_separate_urls_when_their_base_paths_differ() {
        assert_eq!(
            format_diagnostics_description(&DiagnosticsDescriptionInput {
                local_tracing_enabled: true,
                otlp_traces_enabled: true,
                otlp_traces_url: Some("http://localhost:4318/v1/traces"),
                otlp_metrics_enabled: true,
                otlp_metrics_url: Some("http://localhost:9000/v1/metrics"),
            }),
            "Local trace file. Exporting OTEL traces to http://localhost:4318/v1/traces and metrics to http://localhost:9000/v1/metrics."
        );
    }

    #[test]
    fn diagnostics_description_omits_otel_text_when_no_exporter_is_enabled() {
        assert_eq!(
            format_diagnostics_description(&DiagnosticsDescriptionInput {
                local_tracing_enabled: true,
                otlp_traces_enabled: false,
                otlp_traces_url: None,
                otlp_metrics_enabled: false,
                otlp_metrics_url: None,
            }),
            "Local trace file."
        );
    }

    #[test]
    fn build_provider_instance_update_patch_replaces_one_instance_and_keeps_the_others() {
        let instance_id = ProviderInstanceId::new("codex").unwrap();
        let other_id = ProviderInstanceId::new("codex_personal").unwrap();
        let other: ProviderInstanceConfig = serde_json::from_value(json!({
            "driver": "codex",
            "config": { "homePath": "/Users/example/.codex-personal" }
        }))
        .unwrap();
        let next_instance: ProviderInstanceConfig = serde_json::from_value(json!({
            "driver": "codex",
            "enabled": true,
            "config": { "binaryPath": "/opt/t3/codex" }
        }))
        .unwrap();
        let mut settings = default_server_settings();
        settings
            .provider_instances
            .insert(other_id.clone(), other.clone());

        let patch = build_provider_instance_update_patch(
            &settings,
            &instance_id,
            next_instance.clone(),
            None,
        );

        let instances = patch.provider_instances.expect("providerInstances patched");
        assert_eq!(instances.get(&other_id), Some(&other));
        assert_eq!(instances.get(&instance_id), Some(&next_instance));
        assert!(patch.text_generation_model_selection.is_none());
    }

    #[test]
    fn interval_helpers_round_and_clamp() {
        assert_eq!(duration_to_seconds(1_499), 1);
        assert_eq!(duration_to_seconds(1_500), 2);
        assert_eq!(normalize_interval_seconds(None, 5), 5);
        assert_eq!(normalize_interval_seconds(Some(f64::NAN), 5), 5);
        assert_eq!(normalize_interval_seconds(Some(2.4), 5), 5);
        assert_eq!(normalize_interval_seconds(Some(61.6), 5), 62);
    }
}
