//! Port of upstream/apps/web/src/components/chat/ProviderStatusBanner.tsx

use crate::composer::catalog::{
    AuthStatus, CompatibilityAdvisory, CompatibilityAdvisoryStatus, ProviderInstanceEntry,
    ProviderStatus, format_provider_driver_kind_label, get_provider_status_message,
};

const KEY_SEP: char = '\0';

fn incompatible_version(entry: &ProviderInstanceEntry) -> Option<&CompatibilityAdvisory> {
    let advisory = entry.compatibility_advisory.as_ref()?;
    if entry.status == ProviderStatus::Error && entry.auth_status == AuthStatus::Unauthenticated {
        return None;
    }
    if advisory.status == CompatibilityAdvisoryStatus::Broken {
        return Some(advisory);
    }
    if entry.status == ProviderStatus::Ready
        && advisory.status == CompatibilityAdvisoryStatus::Unsupported
    {
        return Some(advisory);
    }
    None
}

/// `getProviderStatusBannerKey`.
pub fn get_provider_status_banner_key(entry: &ProviderInstanceEntry) -> Option<String> {
    if entry.status == ProviderStatus::Disabled {
        return None;
    }
    if let Some(incompatible) = incompatible_version(entry) {
        return Some(
            [
                entry.instance_id.as_str(),
                match incompatible.status {
                    CompatibilityAdvisoryStatus::Broken => "broken",
                    CompatibilityAdvisoryStatus::Unsupported => "unsupported",
                    CompatibilityAdvisoryStatus::Supported => "supported",
                    CompatibilityAdvisoryStatus::Graceful => "graceful",
                },
                entry.version.as_deref().unwrap_or(""),
                incompatible.message.as_deref().unwrap_or(""),
            ]
            .join(&KEY_SEP.to_string()),
        );
    }
    if entry.status == ProviderStatus::Ready {
        return None;
    }
    if entry.driver_kind == "antigravity"
        && entry.installed
        && entry.status == ProviderStatus::Warning
        && entry.auth_status == AuthStatus::Unknown
    {
        return None;
    }
    Some(
        [
            entry.instance_id.as_str(),
            match entry.status {
                ProviderStatus::Ready => "ready",
                ProviderStatus::Warning => "warning",
                ProviderStatus::Error => "error",
                ProviderStatus::Disabled => "disabled",
            },
            match entry.auth_status {
                AuthStatus::Authenticated => "authenticated",
                AuthStatus::Unauthenticated => "unauthenticated",
                AuthStatus::Unknown => "unknown",
            },
            entry.status_message.as_deref().unwrap_or(""),
        ]
        .join(&KEY_SEP.to_string()),
    )
}

pub fn should_show_provider_status_banner(
    entry: &ProviderInstanceEntry,
    dismissed_banner_key: Option<&str>,
) -> bool {
    let banner_key = get_provider_status_banner_key(entry);
    banner_key.as_deref() != dismissed_banner_key && banner_key.is_some()
}

/// Broken-version guidance takes precedence over startup failures it can cause.
pub fn get_provider_status_message_with_compatibility(entry: &ProviderInstanceEntry) -> String {
    if entry.auth_status != AuthStatus::Unauthenticated
        && let Some(advisory) = entry.compatibility_advisory.as_ref()
        && advisory.status == CompatibilityAdvisoryStatus::Broken
        && let Some(message) = &advisory.message
    {
        return message.clone();
    }
    get_provider_status_message(entry)
}

pub fn provider_status_banner_title(entry: &ProviderInstanceEntry) -> String {
    let provider_name = if entry.display_name.trim().is_empty() {
        format_provider_driver_kind_label(&entry.driver_kind)
    } else {
        entry.display_name.trim().to_owned()
    };
    let is_unauthenticated =
        entry.status == ProviderStatus::Error && entry.auth_status == AuthStatus::Unauthenticated;
    if is_unauthenticated {
        return format!("{provider_name} is unauthenticated");
    }
    if let Some(incompatible) = incompatible_version(entry) {
        let version = entry.version.as_deref().unwrap_or("");
        let label = if incompatible.status == CompatibilityAdvisoryStatus::Broken {
            "known to be broken"
        } else {
            "unsupported"
        };
        return format!("{provider_name} {version} is {label}");
    }
    format!("{provider_name} provider status")
}

pub fn provider_status_banner_is_warning(entry: &ProviderInstanceEntry) -> bool {
    incompatible_version(entry).is_some_and(|a| a.status != CompatibilityAdvisoryStatus::Broken)
        || entry.status == ProviderStatus::Warning
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::catalog::CompatibilityAdvisory;

    fn provider() -> ProviderInstanceEntry {
        let mut entry = ProviderInstanceEntry::new("codex-work", "codex", "Codex");
        entry.version = Some("1.0.0".into());
        entry.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Unsupported,
            message: Some("Unsupported version. Use 2.0.0.".into()),
        });
        entry
    }

    #[test]
    fn shows_and_dismisses_a_warning_on_a_healthy_provider() {
        let entry = provider();
        assert!(should_show_provider_status_banner(&entry, None));
        let key = get_provider_status_banner_key(&entry).unwrap();
        assert!(!should_show_provider_status_banner(&entry, Some(&key)));
        let mut bumped = entry.clone();
        bumped.version = Some("1.0.1".into());
        assert!(should_show_provider_status_banner(&bumped, Some(&key)));
        let mut relaxed = entry.clone();
        relaxed.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Supported,
            message: None,
        });
        assert!(get_provider_status_banner_key(&relaxed).is_none());
        let mut disabled = entry.clone();
        disabled.status = ProviderStatus::Disabled;
        assert!(get_provider_status_banner_key(&disabled).is_none());
        let mut graceful = entry.clone();
        graceful.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Graceful,
            message: None,
        });
        assert!(get_provider_status_banner_key(&graceful).is_none());
    }

    #[test]
    fn shows_downgrade_guidance_instead_of_a_broken_inventory_timeout() {
        let message = "This provider version is known to be incompatible. Use 1.14.19.";
        let mut broken = provider();
        broken.driver_kind = "opencode".into();
        broken.version = Some("2.0.3".into());
        broken.status = ProviderStatus::Error;
        broken.auth_status = AuthStatus::Unknown;
        broken.status_message = Some(
            "Failed to load OpenCode provider inventory: Timed out waiting for server start."
                .into(),
        );
        broken.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Broken,
            message: Some(message.into()),
        });
        assert!(should_show_provider_status_banner(&broken, None));
        let timeout_only_key = get_provider_status_banner_key(&{
            let mut entry = broken.clone();
            entry.compatibility_advisory = Some(CompatibilityAdvisory {
                status: CompatibilityAdvisoryStatus::Supported,
                message: None,
            });
            entry
        })
        .unwrap();
        assert!(should_show_provider_status_banner(
            &broken,
            Some(&timeout_only_key)
        ));
        let broken_key = get_provider_status_banner_key(&broken).unwrap();
        assert!(!should_show_provider_status_banner(
            &broken,
            Some(&broken_key)
        ));
        let mut bumped = broken.clone();
        bumped.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Broken,
            message: Some("Use 1.14.20.".into()),
        });
        assert!(should_show_provider_status_banner(
            &bumped,
            Some(&broken_key)
        ));
        assert_eq!(
            get_provider_status_message_with_compatibility(&broken),
            message
        );
        let mut supported = broken.clone();
        supported.compatibility_advisory = Some(CompatibilityAdvisory {
            status: CompatibilityAdvisoryStatus::Supported,
            message: None,
        });
        assert_eq!(
            get_provider_status_message_with_compatibility(&supported),
            broken.status_message.as_deref().unwrap()
        );
        let mut unauth = broken.clone();
        unauth.auth_status = AuthStatus::Unauthenticated;
        assert_eq!(
            get_provider_status_message_with_compatibility(&unauth),
            broken.status_message.as_deref().unwrap()
        );
    }

    #[test]
    fn keeps_authentication_failures_ahead_of_compatibility_warnings() {
        let mut unauthenticated = provider();
        unauthenticated.status = ProviderStatus::Error;
        unauthenticated.auth_status = AuthStatus::Unauthenticated;
        unauthenticated.compatibility_advisory = None;
        assert_eq!(
            get_provider_status_message_with_compatibility(&unauthenticated),
            "Sign in via the CLI to authenticate again."
        );
        unauthenticated.status_message = Some("Credentials expired".into());
        assert_eq!(
            get_provider_status_message_with_compatibility(&unauthenticated),
            "Credentials expired"
        );
    }
}
