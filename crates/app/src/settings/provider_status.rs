//! Port of upstream/apps/web/src/components/settings/providerStatus.ts
//! (and providerStatus.test.ts).
//!
//! The Antigravity release-tag special case is dropped with the provider.

use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuthStatus, ServerProviderCompatibilityAdvisory,
    ServerProviderCompatibilityStatus, ServerProviderState, ServerProviderVersionAdvisory,
    ServerProviderVersionAdvisoryStatus,
};

/// Visual treatment for each server-reported provider status
/// (`PROVIDER_STATUS_STYLES`): the dot colour token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatusKey {
    Disabled,
    Error,
    Ready,
    Warning,
}

impl From<ServerProviderState> for ProviderStatusKey {
    fn from(state: ServerProviderState) -> Self {
        match state {
            ServerProviderState::Ready => Self::Ready,
            ServerProviderState::Warning => Self::Warning,
            ServerProviderState::Error => Self::Error,
            ServerProviderState::Disabled => Self::Disabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSummary {
    pub headline: String,
    pub detail: Option<String>,
}

fn summary(headline: impl Into<String>, detail: Option<String>) -> ProviderSummary {
    ProviderSummary {
        headline: headline.into(),
        detail,
    }
}

/// Derive the headline + detail copy shown under a provider's name in the
/// settings page. Prefers `provider.message` for server-supplied detail and
/// falls back to generic phrasing when the server has not yet reported any
/// state — which happens before the first probe or when an instance names a
/// driver this build does not ship. A ready provider without account metadata
/// remains available and does not imply an authentication failure.
pub fn get_provider_summary(provider: Option<&ServerProvider>) -> ProviderSummary {
    let Some(provider) = provider else {
        return summary(
            "Checking provider status",
            Some(
                "Waiting for the server to report installation and authentication details.".into(),
            ),
        );
    };
    let message = provider.message.as_ref().map(|m| m.as_str().to_owned());
    if !provider.enabled || provider.status == ServerProviderState::Disabled {
        return summary(
            "Disabled",
            Some(message.unwrap_or_else(|| {
                "This provider is installed but disabled for new sessions in T3 Code.".into()
            })),
        );
    }
    if !provider.installed {
        return summary(
            "Not found",
            Some(message.unwrap_or_else(|| "CLI not detected on PATH.".into())),
        );
    }
    let auth_label = || {
        provider
            .auth
            .label
            .as_ref()
            .or(provider.auth.type_.as_ref())
            .map(|label| label.as_str().to_owned())
    };
    if provider.auth.status == ServerProviderAuthStatus::Unauthenticated {
        return summary(
            match auth_label() {
                Some(label) => format!("Not authenticated · {label}"),
                None => "Not authenticated".into(),
            },
            message,
        );
    }
    if provider.status == ServerProviderState::Warning {
        return summary(
            "Needs attention",
            Some(message.unwrap_or_else(|| {
                "The provider is installed, but the server could not fully verify it.".into()
            })),
        );
    }
    if provider.status == ServerProviderState::Error {
        return summary(
            "Unavailable",
            Some(message.unwrap_or_else(|| "The provider failed its startup checks.".into())),
        );
    }
    if provider.auth.status == ServerProviderAuthStatus::Authenticated {
        return summary(
            match auth_label() {
                Some(label) => format!("Authenticated · {label}"),
                None => "Authenticated".into(),
            },
            message,
        );
    }
    summary("Available", message)
}

/// Normalize a version string for display. Adds the `v` prefix when the
/// driver reported a bare version (e.g. `1.2.3`) so cards render
/// consistently regardless of driver.
pub fn get_provider_version_label(version: Option<&str>) -> Option<String> {
    let version = version.filter(|version| !version.is_empty())?;
    // Only bare semver-like versions get a `v` prefix. Other tags are shown as-is.
    Some(if version.starts_with(|c: char| c.is_ascii_digit()) {
        format!("v{version}")
    } else {
        version.to_owned()
    })
}

fn compatibility_title(status: ServerProviderCompatibilityStatus) -> Option<&'static str> {
    match status {
        ServerProviderCompatibilityStatus::Graceful => Some("Limited support"),
        ServerProviderCompatibilityStatus::Unsupported => Some("Unsupported version"),
        ServerProviderCompatibilityStatus::Broken => Some("Known broken version"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryEmphasis {
    Normal,
    Strong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionAdvisoryPresentation {
    pub title: String,
    pub detail: String,
    pub update_command: Option<String>,
    pub emphasis: AdvisoryEmphasis,
    pub target_version: Option<String>,
}

/// Compatibility guidance shares the version popover, with safe install actions.
pub fn get_provider_version_advisory_presentation(
    advisory: Option<&ServerProviderVersionAdvisory>,
    compatibility: Option<&ServerProviderCompatibilityAdvisory>,
    show_compatibility: bool,
) -> Option<VersionAdvisoryPresentation> {
    let latest_is_incompatible = matches!(
        compatibility.and_then(|c| c.latest_version_status),
        Some(ServerProviderCompatibilityStatus::Broken)
            | Some(ServerProviderCompatibilityStatus::Unsupported)
    );
    if let Some(compatibility) = compatibility.filter(|_| show_compatibility)
        && let Some(title) = compatibility_title(compatibility.status)
    {
        let target_version = compatibility
            .recommended_version
            .as_ref()
            .map(|v| v.as_str().to_owned());
        let recommendation = get_provider_version_label(target_version.as_deref()).or_else(|| {
            compatibility
                .recommended_range
                .as_ref()
                .map(|r| r.as_str().to_owned())
        });
        return Some(VersionAdvisoryPresentation {
            title: title.to_owned(),
            detail: compatibility
                .message
                .as_ref()
                .map(|m| m.as_str().to_owned())
                .unwrap_or_else(|| match recommendation {
                    Some(recommendation) => format!("Use {recommendation} for full support."),
                    None => "Update for full support.".into(),
                }),
            update_command: if target_version.is_some() || latest_is_incompatible {
                None
            } else {
                advisory
                    .and_then(|a| a.update_command.as_ref())
                    .map(|c| c.as_str().to_owned())
            },
            emphasis: if compatibility.status == ServerProviderCompatibilityStatus::Graceful {
                AdvisoryEmphasis::Normal
            } else {
                AdvisoryEmphasis::Strong
            },
            target_version,
        });
    }
    let advisory = advisory?;
    if matches!(
        advisory.status,
        ServerProviderVersionAdvisoryStatus::Current | ServerProviderVersionAdvisoryStatus::Unknown
    ) || latest_is_incompatible
    {
        return None;
    }
    let label = "Update available";
    let version_label =
        get_provider_version_label(advisory.latest_version.as_ref().map(|v| v.as_str()));
    Some(VersionAdvisoryPresentation {
        title: label.to_owned(),
        detail: advisory
            .message
            .as_ref()
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| match version_label {
                Some(version_label) => format!("{label}: install {version_label}."),
                None => format!("{label}: install the latest provider version."),
            }),
        update_command: advisory
            .update_command
            .as_ref()
            .map(|c| c.as_str().to_owned()),
        emphasis: AdvisoryEmphasis::Normal,
        target_version: None,
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;

    fn provider_with(overrides: Value) -> ServerProvider {
        let mut base = json!({
            "instanceId": "codex",
            "driver": "codex",
            "enabled": true,
            "installed": true,
            "version": "1.0.0",
            "status": "ready",
            "auth": { "status": "authenticated", "label": "ChatGPT" },
            "checkedAt": "2026-08-23T00:00:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": []
        });
        for (key, value) in overrides.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).expect("provider decodes")
    }

    #[test]
    fn reports_ready_providers_with_unknown_authentication_as_available() {
        let provider = provider_with(json!({ "auth": { "status": "unknown" } }));
        assert_eq!(
            get_provider_summary(Some(&provider)),
            ProviderSummary {
                headline: "Available".into(),
                detail: None
            }
        );
    }

    #[test]
    fn does_not_hide_a_provider_error_behind_a_previous_authenticated_state() {
        let provider = provider_with(
            json!({ "status": "error", "message": "The provider process failed to start." }),
        );
        assert_eq!(
            get_provider_summary(Some(&provider)),
            ProviderSummary {
                headline: "Unavailable".into(),
                detail: Some("The provider process failed to start.".into())
            }
        );
    }

    #[test]
    fn does_not_hide_a_provider_warning_behind_an_authenticated_state() {
        let provider = provider_with(
            json!({ "status": "warning", "message": "The provider version is unsupported." }),
        );
        assert_eq!(
            get_provider_summary(Some(&provider)),
            ProviderSummary {
                headline: "Needs attention".into(),
                detail: Some("The provider version is unsupported.".into())
            }
        );
    }

    #[test]
    fn keeps_authentication_failures_actionable_when_their_provider_status_is_error() {
        let provider = provider_with(json!({
            "status": "error",
            "auth": { "status": "unauthenticated" },
            "message": "Run codex login."
        }));
        assert_eq!(
            get_provider_summary(Some(&provider)),
            ProviderSummary {
                headline: "Not authenticated".into(),
                detail: Some("Run codex login.".into())
            }
        );
    }

    #[test]
    fn treats_a_disabled_provider_status_as_disabled_even_before_its_enabled_flag_updates() {
        let provider = provider_with(json!({ "status": "disabled" }));
        assert_eq!(get_provider_summary(Some(&provider)).headline, "Disabled");
    }

    #[test]
    fn surfaces_an_auth_label_from_the_provider() {
        let provider = provider_with(json!({
            "status": "error",
            "auth": { "status": "unauthenticated", "type": "agent", "label": "Company login" },
            "message": "Complete this authentication method on the server."
        }));
        assert_eq!(
            get_provider_summary(Some(&provider)),
            ProviderSummary {
                headline: "Not authenticated · Company login".into(),
                detail: Some("Complete this authentication method on the server.".into())
            }
        );
    }

    #[test]
    fn waits_for_the_first_probe() {
        assert_eq!(
            get_provider_summary(None).headline,
            "Checking provider status"
        );
    }

    fn advisory(status: &str, current: &str, latest: &str) -> ServerProviderVersionAdvisory {
        serde_json::from_value(json!({
            "status": status,
            "currentVersion": current,
            "latestVersion": latest,
            "updateCommand": "npm install -g fixture@latest",
            "canUpdate": true,
            "checkedAt": "2026-08-23T00:00:00.000Z",
            "message": null
        }))
        .unwrap()
    }

    fn compat(value: Value) -> ServerProviderCompatibilityAdvisory {
        let mut base = json!({
            "status": "supported",
            "latestVersionStatus": "broken",
            "message": null,
            "recommendedRange": null,
            "recommendedVersion": null
        });
        for (key, v) in value.as_object().unwrap() {
            base[key] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn does_not_suggest_copying_a_command_that_installs_an_incompatible_latest_version() {
        let advisory = advisory("behind_latest", "1.0.0", "2.0.0");
        let compatibility = compat(json!({}));
        assert_eq!(
            get_provider_version_advisory_presentation(Some(&advisory), Some(&compatibility), true),
            None
        );
        assert_eq!(
            get_provider_version_advisory_presentation(
                Some(&advisory),
                Some(&compat(json!({ "status": "broken" }))),
                false
            ),
            None
        );
        assert!(
            get_provider_version_advisory_presentation(
                Some(&advisory),
                Some(&compat(json!({ "latestVersionStatus": "supported" }))),
                true
            )
            .is_some()
        );
    }

    #[test]
    fn shows_compatibility_in_the_version_popover_even_when_the_installed_version_is_current() {
        let advisory = advisory("current", "2.0.0", "2.0.0");
        let compatibility = compat(json!({
            "status": "broken",
            "message": "This version drops turns. Use 1.9.0.",
            "recommendedVersion": "1.9.0"
        }));
        assert_eq!(
            get_provider_version_advisory_presentation(Some(&advisory), Some(&compatibility), true),
            Some(VersionAdvisoryPresentation {
                title: "Known broken version".into(),
                detail: "This version drops turns. Use 1.9.0.".into(),
                update_command: None,
                emphasis: AdvisoryEmphasis::Strong,
                target_version: Some("1.9.0".into()),
            })
        );
        assert_eq!(
            get_provider_version_advisory_presentation(
                None,
                Some(&compat(json!({
                    "status": "graceful",
                    "message": null,
                    "recommendedVersion": null,
                    "recommendedRange": ">=2.1.0"
                }))),
                true
            ),
            Some(VersionAdvisoryPresentation {
                title: "Limited support".into(),
                detail: "Use >=2.1.0 for full support.".into(),
                update_command: None,
                emphasis: AdvisoryEmphasis::Normal,
                target_version: None,
            })
        );
    }

    #[test]
    fn labels_versions() {
        assert_eq!(
            get_provider_version_label(Some("1.2.3")).as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(
            get_provider_version_label(Some("nightly")).as_deref(),
            Some("nightly")
        );
        assert_eq!(get_provider_version_label(None), None);
    }
}
