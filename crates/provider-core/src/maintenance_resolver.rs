//! Port of the pure helpers in
//! upstream/packages/provider-core/src/server/maintenanceResolver.ts.
//!
//! The async installer probe (npm, Homebrew, mise, volta) is not in this
//! module yet. `build_server_provider` only needs the manual advisory, which
//! is what upstream emits when it has not resolved a latest version.

use std::cmp::Ordering;

use turin_contracts::{TrimmedNonEmptyString, provider_instance::ProviderDriverKind};

use crate::{
    maintenance::{ProviderMaintenanceCapabilities, ProviderMaintenanceCommandAction},
    semver::compare_semver_versions,
    server_provider::{ServerProviderVersionAdvisory, ServerProviderVersionAdvisoryStatus},
};

const PROVIDER_UPDATE_ACTION_TOAST_MESSAGE: &str =
    "Install the update now or review provider settings.";

/// Capabilities for a provider whose update command is not known yet.
pub fn make_manual_only_provider_maintenance_capabilities(
    provider: ProviderDriverKind,
) -> ProviderMaintenanceCapabilities {
    ProviderMaintenanceCapabilities {
        provider,
        package_name: None,
        update: None,
        latest_version: None,
        compare_versions: None,
    }
}

/// Pin only package-manager actions we own, preserving prefix, scripts, env and lock.
pub fn make_targeted_provider_update_action(
    capabilities: &ProviderMaintenanceCapabilities,
    version: &str,
) -> Option<ProviderMaintenanceCommandAction> {
    if !is_plain_semver(version) {
        return None;
    }
    let update = capabilities.update.as_ref()?;
    let package_name = capabilities.package_name.as_deref()?;
    if !is_pinnable_lock_key(&update.lock_key) {
        return None;
    }
    let package_index = update.args.iter().position(|arg| {
        arg == &format!("{package_name}@latest") || arg == package_name
    })?;
    let previous = update.args[package_index].clone();
    let pinned = format!("{package_name}@{version}");
    let mut args = update.args.clone();
    args[package_index] = pinned.clone();
    let command = match update.command.rfind(&previous) {
        Some(index) => {
            let mut command = String::with_capacity(update.command.len() + version.len());
            command.push_str(&update.command[..index]);
            command.push_str(&pinned);
            command.push_str(&update.command[index + previous.len()..]);
            command
        }
        None => update.command.clone(),
    };
    Some(ProviderMaintenanceCommandAction {
        command,
        executable: update.executable.clone(),
        args,
        lock_key: update.lock_key.clone(),
        env: update.env.clone(),
    })
}

pub struct ProviderVersionAdvisoryInput<'a> {
    pub driver: &'a ProviderDriverKind,
    pub current_version: Option<&'a str>,
    pub latest_version: Option<&'a str>,
    pub checked_at: Option<&'a str>,
    pub maintenance: Option<&'a ProviderMaintenanceCapabilities>,
}

pub fn create_provider_version_advisory(
    input: ProviderVersionAdvisoryInput<'_>,
) -> ServerProviderVersionAdvisory {
    let owned = input.maintenance.is_none().then(|| {
        make_manual_only_provider_maintenance_capabilities(input.driver.clone())
    });
    let capabilities = input.maintenance.unwrap_or_else(|| owned.as_ref().expect("manual"));
    let current = non_empty(input.current_version);
    let latest = non_empty(input.latest_version);
    let (status, message) = derive_version_advisory(current, latest, capabilities);
    ServerProviderVersionAdvisory {
        status,
        current_version: current.and_then(|version| TrimmedNonEmptyString::new(version).ok()),
        latest_version: latest.and_then(|version| TrimmedNonEmptyString::new(version).ok()),
        update_command: capabilities
            .update
            .as_ref()
            .and_then(|update| TrimmedNonEmptyString::new(&update.command).ok()),
        can_update: capabilities.update.is_some(),
        can_install_version: Some(make_targeted_provider_update_action(capabilities, "0.0.0").is_some()),
        checked_at: input.checked_at.map(str::to_owned),
        message: message.and_then(|text| TrimmedNonEmptyString::new(text).ok()),
    }
}

fn derive_version_advisory(
    current: Option<&str>,
    latest: Option<&str>,
    capabilities: &ProviderMaintenanceCapabilities,
) -> (ServerProviderVersionAdvisoryStatus, Option<&'static str>) {
    let (Some(current), Some(latest)) = (current, latest) else {
        return (ServerProviderVersionAdvisoryStatus::Unknown, None);
    };
    let ordering = match &capabilities.compare_versions {
        Some(compare) => compare(current, latest),
        None => compare_semver_versions(current, latest),
    };
    if ordering == Ordering::Less {
        (
            ServerProviderVersionAdvisoryStatus::BehindLatest,
            Some(PROVIDER_UPDATE_ACTION_TOAST_MESSAGE),
        )
    } else {
        (ServerProviderVersionAdvisoryStatus::Current, None)
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn is_plain_semver(version: &str) -> bool {
    let mut parts = version.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !major.is_empty()
        && !minor.is_empty()
        && !patch.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor.bytes().all(|byte| byte.is_ascii_digit())
        && patch.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_pinnable_lock_key(lock_key: &str) -> bool {
    lock_key.starts_with("npm-global:")
        || lock_key == "bun-global"
        || lock_key == "pnpm-global"
        || lock_key == "vite-plus-global"
        || lock_key == "yarn-global"
        || lock_key == "volta"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(lock_key: &str, package: &str) -> ProviderMaintenanceCapabilities {
        ProviderMaintenanceCapabilities {
            provider: ProviderDriverKind::new("codex").unwrap(),
            package_name: Some(package.to_owned()),
            update: Some(ProviderMaintenanceCommandAction {
                command: format!("npm install -g {package}@latest"),
                executable: "npm".to_owned(),
                args: vec!["install".to_owned(), "-g".to_owned(), format!("{package}@latest")],
                lock_key: lock_key.to_owned(),
                env: None,
            }),
            latest_version: None,
            compare_versions: None,
        }
    }

    #[test]
    fn pins_package_manager_actions_and_rejects_the_rest() {
        let npm = action("npm-global:@openai/codex", "@openai/codex");
        let pinned = make_targeted_provider_update_action(&npm, "1.2.3").unwrap();
        assert_eq!(pinned.args[2], "@openai/codex@1.2.3");
        assert!(pinned.command.contains("@openai/codex@1.2.3"));
        assert!(make_targeted_provider_update_action(&npm, "1.2").is_none());

        for lock_key in ["homebrew", "native", "mise"] {
            assert!(make_targeted_provider_update_action(&action(lock_key, "@openai/codex"), "1.2.3").is_none());
        }
        for lock_key in ["bun-global", "pnpm-global", "vite-plus-global", "yarn-global", "volta"] {
            assert!(make_targeted_provider_update_action(&action(lock_key, "pkg"), "9.0.0").is_some());
        }
    }

    #[test]
    fn manual_advisory_is_unknown_without_a_latest_version() {
        let driver = ProviderDriverKind::new("codex").unwrap();
        let advisory = create_provider_version_advisory(ProviderVersionAdvisoryInput {
            driver: &driver,
            current_version: Some("1.0.0"),
            latest_version: None,
            checked_at: Some("2026-01-01T00:00:00.000Z"),
            maintenance: None,
        });
        assert_eq!(advisory.status, ServerProviderVersionAdvisoryStatus::Unknown);
        assert!(!advisory.can_update);
        assert_eq!(advisory.can_install_version, Some(false));
        assert!(advisory.message.is_none());
    }
}
