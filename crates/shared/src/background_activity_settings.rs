//! Port of upstream/packages/shared/src/backgroundActivitySettings.ts
//!
//! Background activity presets, custom overrides on top of a preset, and the migration from the
//! legacy flat `backgroundActivityProfile` and interval settings. Durations are `std::time::Duration`
//! here; the contract stores them as milliseconds (`DurationFromMillis`).

use std::time::Duration;

use turin_contracts::settings::{
    BackgroundActivityOverrides, BackgroundActivityProfile, BackgroundActivityProfileSelection,
    BackgroundActivitySchemaVersion, BackgroundActivitySettings,
    DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL, DEFAULT_BACKGROUND_ACTIVITY_PROFILE,
    DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL, ServerSettings,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBackgroundActivitySettings {
    pub profile: BackgroundActivityProfile,
    pub automatic_git_fetch_interval: Duration,
    pub provider_health_refresh_interval: Duration,
    pub host_power_monitor_active_interval: Duration,
    pub host_power_monitor_idle_interval: Duration,
    pub idle_client_ttl: Duration,
    pub pause_when_host_locked: bool,
    pub pause_when_host_low_power: bool,
    pub pause_when_client_low_power: bool,
    pub pause_when_on_battery: bool,
}

fn preset_settings(profile: BackgroundActivityProfile) -> ResolvedBackgroundActivitySettings {
    match profile {
        BackgroundActivityProfile::Performance => ResolvedBackgroundActivitySettings {
            profile,
            automatic_git_fetch_interval: Duration::from_secs(15),
            provider_health_refresh_interval: Duration::from_secs(60),
            host_power_monitor_active_interval: Duration::from_secs(30),
            host_power_monitor_idle_interval: Duration::from_secs(120),
            idle_client_ttl: Duration::from_secs(45),
            pause_when_host_locked: true,
            pause_when_host_low_power: false,
            pause_when_client_low_power: false,
            pause_when_on_battery: false,
        },
        BackgroundActivityProfile::Balanced => ResolvedBackgroundActivitySettings {
            profile,
            automatic_git_fetch_interval: DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL,
            provider_health_refresh_interval: DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL,
            host_power_monitor_active_interval: Duration::from_secs(30),
            host_power_monitor_idle_interval: Duration::from_secs(300),
            idle_client_ttl: Duration::from_secs(45),
            pause_when_host_locked: true,
            pause_when_host_low_power: true,
            pause_when_client_low_power: true,
            pause_when_on_battery: false,
        },
        BackgroundActivityProfile::BatterySaver => ResolvedBackgroundActivitySettings {
            profile,
            automatic_git_fetch_interval: Duration::ZERO,
            provider_health_refresh_interval: Duration::from_secs(15 * 60),
            host_power_monitor_active_interval: Duration::from_secs(60),
            host_power_monitor_idle_interval: Duration::from_secs(10 * 60),
            idle_client_ttl: Duration::from_secs(45),
            pause_when_host_locked: true,
            pause_when_host_low_power: true,
            pause_when_client_low_power: true,
            pause_when_on_battery: true,
        },
    }
}

pub fn get_background_activity_preset_settings(
    profile: BackgroundActivityProfile,
) -> ResolvedBackgroundActivitySettings {
    preset_settings(profile)
}

fn selection_of(profile: BackgroundActivityProfile) -> BackgroundActivityProfileSelection {
    match profile {
        BackgroundActivityProfile::Balanced => BackgroundActivityProfileSelection::Balanced,
        BackgroundActivityProfile::Performance => BackgroundActivityProfileSelection::Performance,
        BackgroundActivityProfile::BatterySaver => BackgroundActivityProfileSelection::BatterySaver,
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

pub fn get_background_activity_base_profile(
    background_activity: &BackgroundActivitySettings,
) -> BackgroundActivityProfile {
    match background_activity.profile {
        BackgroundActivityProfileSelection::Custom => background_activity
            .base_profile
            .unwrap_or(DEFAULT_BACKGROUND_ACTIVITY_PROFILE),
        BackgroundActivityProfileSelection::Balanced => BackgroundActivityProfile::Balanced,
        BackgroundActivityProfileSelection::Performance => BackgroundActivityProfile::Performance,
        BackgroundActivityProfileSelection::BatterySaver => BackgroundActivityProfile::BatterySaver,
    }
}

pub fn resolve_background_activity_settings(
    background_activity: &BackgroundActivitySettings,
) -> ResolvedBackgroundActivitySettings {
    let base_profile = get_background_activity_base_profile(background_activity);
    let preset = preset_settings(base_profile);
    let empty = BackgroundActivityOverrides::default();
    let overrides = match background_activity.profile {
        BackgroundActivityProfileSelection::Custom => &background_activity.overrides,
        _ => &empty,
    };
    ResolvedBackgroundActivitySettings {
        profile: base_profile,
        automatic_git_fetch_interval: overrides
            .automatic_git_fetch_interval
            .map(Duration::from_millis)
            .unwrap_or(preset.automatic_git_fetch_interval),
        provider_health_refresh_interval: overrides
            .provider_health_refresh_interval
            .map(Duration::from_millis)
            .unwrap_or(preset.provider_health_refresh_interval),
        host_power_monitor_active_interval: overrides
            .host_power_monitor_active_interval
            .map(Duration::from_millis)
            .unwrap_or(preset.host_power_monitor_active_interval),
        host_power_monitor_idle_interval: overrides
            .host_power_monitor_idle_interval
            .map(Duration::from_millis)
            .unwrap_or(preset.host_power_monitor_idle_interval),
        idle_client_ttl: overrides
            .idle_client_ttl
            .map(Duration::from_millis)
            .unwrap_or(preset.idle_client_ttl),
        pause_when_host_locked: overrides
            .pause_when_host_locked
            .unwrap_or(preset.pause_when_host_locked),
        pause_when_host_low_power: overrides
            .pause_when_host_low_power
            .unwrap_or(preset.pause_when_host_low_power),
        pause_when_client_low_power: overrides
            .pause_when_client_low_power
            .unwrap_or(preset.pause_when_client_low_power),
        pause_when_on_battery: overrides
            .pause_when_on_battery
            .unwrap_or(preset.pause_when_on_battery),
    }
}

/// Durations compare in whole milliseconds, as the stored form does.
fn durations_equal(a: Duration, b: Duration) -> bool {
    millis(a) == millis(b)
}

fn resolved_settings_equal(
    a: &ResolvedBackgroundActivitySettings,
    b: &ResolvedBackgroundActivitySettings,
) -> bool {
    durations_equal(
        a.automatic_git_fetch_interval,
        b.automatic_git_fetch_interval,
    ) && durations_equal(
        a.provider_health_refresh_interval,
        b.provider_health_refresh_interval,
    ) && durations_equal(
        a.host_power_monitor_active_interval,
        b.host_power_monitor_active_interval,
    ) && durations_equal(
        a.host_power_monitor_idle_interval,
        b.host_power_monitor_idle_interval,
    ) && durations_equal(a.idle_client_ttl, b.idle_client_ttl)
        && a.pause_when_host_locked == b.pause_when_host_locked
        && a.pause_when_host_low_power == b.pause_when_host_low_power
        && a.pause_when_client_low_power == b.pause_when_client_low_power
        && a.pause_when_on_battery == b.pause_when_on_battery
}

/// A preset is stored as its bare profile. A custom set of values that matches a preset is stored
/// as that preset, and only values that differ from the base preset are kept as overrides.
pub fn normalize_background_activity_settings(
    background_activity: &BackgroundActivitySettings,
) -> BackgroundActivitySettings {
    if background_activity.profile != BackgroundActivityProfileSelection::Custom {
        return BackgroundActivitySettings {
            schema_version: BackgroundActivitySchemaVersion,
            profile: background_activity.profile,
            base_profile: None,
            overrides: BackgroundActivityOverrides::default(),
        };
    }

    let resolved = resolve_background_activity_settings(background_activity);
    let profiles = [
        get_background_activity_base_profile(background_activity),
        BackgroundActivityProfile::Balanced,
        BackgroundActivityProfile::Performance,
        BackgroundActivityProfile::BatterySaver,
    ];
    for profile in profiles {
        if resolved_settings_equal(&resolved, &preset_settings(profile)) {
            return BackgroundActivitySettings {
                schema_version: BackgroundActivitySchemaVersion,
                profile: selection_of(profile),
                base_profile: None,
                overrides: BackgroundActivityOverrides::default(),
            };
        }
    }

    let base_profile = get_background_activity_base_profile(background_activity);
    let preset = preset_settings(base_profile);
    let differs = |a: Duration, b: Duration| (!durations_equal(a, b)).then_some(millis(a));
    let overrides = BackgroundActivityOverrides {
        automatic_git_fetch_interval: differs(
            resolved.automatic_git_fetch_interval,
            preset.automatic_git_fetch_interval,
        ),
        provider_health_refresh_interval: differs(
            resolved.provider_health_refresh_interval,
            preset.provider_health_refresh_interval,
        ),
        host_power_monitor_active_interval: differs(
            resolved.host_power_monitor_active_interval,
            preset.host_power_monitor_active_interval,
        ),
        host_power_monitor_idle_interval: differs(
            resolved.host_power_monitor_idle_interval,
            preset.host_power_monitor_idle_interval,
        ),
        idle_client_ttl: differs(resolved.idle_client_ttl, preset.idle_client_ttl),
        pause_when_host_locked: (resolved.pause_when_host_locked != preset.pause_when_host_locked)
            .then_some(resolved.pause_when_host_locked),
        pause_when_host_low_power: (resolved.pause_when_host_low_power
            != preset.pause_when_host_low_power)
            .then_some(resolved.pause_when_host_low_power),
        pause_when_client_low_power: (resolved.pause_when_client_low_power
            != preset.pause_when_client_low_power)
            .then_some(resolved.pause_when_client_low_power),
        pause_when_on_battery: (resolved.pause_when_on_battery != preset.pause_when_on_battery)
            .then_some(resolved.pause_when_on_battery),
    };

    BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(base_profile),
        overrides,
    }
}

fn overrides_are_empty(overrides: &BackgroundActivityOverrides) -> bool {
    overrides == &BackgroundActivityOverrides::default()
}

/// The effective settings. A stored background-activity setting wins; a default one with the
/// legacy flat profile or interval fields set is read from those fields instead.
pub fn resolve_server_background_activity_settings(
    settings: &ServerSettings,
) -> ResolvedBackgroundActivitySettings {
    let background_activity_is_default = settings.background_activity.profile
        == BackgroundActivityProfileSelection::Balanced
        && settings.background_activity.base_profile.is_none()
        && overrides_are_empty(&settings.background_activity.overrides);
    let legacy_profile = settings.background_activity_profile;
    let default_fetch = millis(DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL);
    let default_health = millis(DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL);
    let has_legacy_overrides = legacy_profile != DEFAULT_BACKGROUND_ACTIVITY_PROFILE
        || settings.automatic_git_fetch_interval != default_fetch
        || settings.provider_health_refresh_interval != default_health;
    if !(background_activity_is_default && has_legacy_overrides) {
        return resolve_background_activity_settings(&settings.background_activity);
    }

    let legacy_preset = preset_settings(legacy_profile);
    let legacy_fetch = millis(legacy_preset.automatic_git_fetch_interval);
    let legacy_health = millis(legacy_preset.provider_health_refresh_interval);
    let matches_legacy_presets = settings.automatic_git_fetch_interval == legacy_fetch
        && settings.provider_health_refresh_interval == legacy_health;
    let profile = if matches_legacy_presets {
        selection_of(legacy_profile)
    } else {
        BackgroundActivityProfileSelection::Custom
    };
    resolve_background_activity_settings(&BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile,
        base_profile: Some(legacy_profile),
        overrides: BackgroundActivityOverrides {
            automatic_git_fetch_interval: (settings.automatic_git_fetch_interval != legacy_fetch)
                .then_some(settings.automatic_git_fetch_interval),
            provider_health_refresh_interval: (settings.provider_health_refresh_interval
                != legacy_health)
                .then_some(settings.provider_health_refresh_interval),
            ..BackgroundActivityOverrides::default()
        },
    })
}

pub fn normalize_server_background_activity_settings(
    settings: &ServerSettings,
) -> BackgroundActivitySettings {
    let resolved = resolve_server_background_activity_settings(settings);
    normalize_background_activity_settings(&BackgroundActivitySettings {
        schema_version: BackgroundActivitySchemaVersion,
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(resolved.profile),
        overrides: BackgroundActivityOverrides {
            automatic_git_fetch_interval: Some(millis(resolved.automatic_git_fetch_interval)),
            provider_health_refresh_interval: Some(millis(
                resolved.provider_health_refresh_interval,
            )),
            host_power_monitor_active_interval: Some(millis(
                resolved.host_power_monitor_active_interval,
            )),
            host_power_monitor_idle_interval: Some(millis(
                resolved.host_power_monitor_idle_interval,
            )),
            idle_client_ttl: Some(millis(resolved.idle_client_ttl)),
            pause_when_host_locked: Some(resolved.pause_when_host_locked),
            pause_when_host_low_power: Some(resolved.pause_when_host_low_power),
            pause_when_client_low_power: Some(resolved.pause_when_client_low_power),
            pause_when_on_battery: Some(resolved.pause_when_on_battery),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::settings::default_server_settings;

    fn custom(
        base: BackgroundActivityProfile,
        overrides: BackgroundActivityOverrides,
    ) -> BackgroundActivitySettings {
        BackgroundActivitySettings {
            schema_version: BackgroundActivitySchemaVersion,
            profile: BackgroundActivityProfileSelection::Custom,
            base_profile: Some(base),
            overrides,
        }
    }

    #[test]
    fn balanced_preset_is_the_contract_default() {
        let balanced = get_background_activity_preset_settings(BackgroundActivityProfile::Balanced);
        assert_eq!(
            balanced.automatic_git_fetch_interval,
            DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL
        );
        assert_eq!(
            balanced.provider_health_refresh_interval,
            DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL
        );
        assert!(balanced.pause_when_host_low_power);
        assert!(!balanced.pause_when_on_battery);
    }

    #[test]
    fn a_custom_setting_resolves_its_overrides_over_the_base_preset() {
        let resolved = resolve_background_activity_settings(&custom(
            BackgroundActivityProfile::Performance,
            BackgroundActivityOverrides {
                idle_client_ttl: Some(9_000),
                pause_when_on_battery: Some(true),
                ..BackgroundActivityOverrides::default()
            },
        ));
        assert_eq!(resolved.profile, BackgroundActivityProfile::Performance);
        assert_eq!(resolved.idle_client_ttl, Duration::from_millis(9_000));
        assert!(resolved.pause_when_on_battery);
        assert_eq!(
            resolved.automatic_git_fetch_interval,
            Duration::from_secs(15)
        );
    }

    #[test]
    fn a_custom_setting_matching_a_preset_normalizes_to_that_preset() {
        let normalized = normalize_background_activity_settings(&custom(
            BackgroundActivityProfile::Performance,
            BackgroundActivityOverrides {
                pause_when_host_low_power: Some(false),
                ..BackgroundActivityOverrides::default()
            },
        ));
        assert_eq!(
            normalized.profile,
            BackgroundActivityProfileSelection::Performance
        );
        assert_eq!(normalized.base_profile, None);
        assert_eq!(normalized.overrides, BackgroundActivityOverrides::default());
    }

    #[test]
    fn normalizing_keeps_only_the_values_that_differ_from_the_base_preset() {
        let normalized = normalize_background_activity_settings(&custom(
            BackgroundActivityProfile::Balanced,
            BackgroundActivityOverrides {
                idle_client_ttl: Some(9_000),
                ..BackgroundActivityOverrides::default()
            },
        ));
        assert_eq!(
            normalized.profile,
            BackgroundActivityProfileSelection::Custom
        );
        assert_eq!(
            normalized.base_profile,
            Some(BackgroundActivityProfile::Balanced)
        );
        assert_eq!(
            normalized.overrides,
            BackgroundActivityOverrides {
                idle_client_ttl: Some(9_000),
                ..BackgroundActivityOverrides::default()
            }
        );
    }

    #[test]
    fn legacy_flat_profile_is_read_when_the_stored_setting_is_default() {
        let mut settings = default_server_settings();
        settings.background_activity_profile = BackgroundActivityProfile::BatterySaver;
        settings.automatic_git_fetch_interval = 0;
        settings.provider_health_refresh_interval = millis(Duration::from_secs(15 * 60));
        let resolved = resolve_server_background_activity_settings(&settings);
        assert_eq!(resolved.profile, BackgroundActivityProfile::BatterySaver);
        assert!(resolved.pause_when_on_battery);
    }

    #[test]
    fn a_stored_custom_setting_wins_over_legacy_fields() {
        let mut settings = default_server_settings();
        settings.background_activity = custom(
            BackgroundActivityProfile::Balanced,
            BackgroundActivityOverrides {
                idle_client_ttl: Some(1_000),
                ..BackgroundActivityOverrides::default()
            },
        );
        settings.background_activity_profile = BackgroundActivityProfile::Performance;
        let resolved = resolve_server_background_activity_settings(&settings);
        assert_eq!(resolved.profile, BackgroundActivityProfile::Balanced);
        assert_eq!(resolved.idle_client_ttl, Duration::from_millis(1_000));
    }
}
