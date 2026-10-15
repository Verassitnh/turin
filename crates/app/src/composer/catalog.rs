//! The provider catalogue the composer reads: the parts of upstream's
//! `ServerProvider` / `ProviderInstanceEntry` (providerInstances.ts) and
//! `ServerProviderSkill` / `ServerProviderSlashCommand` that composer features
//! use. T14's reducers produce these from the real snapshot.

use crate::composer::provider_state::ProviderModel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderStatus {
    #[default]
    Ready,
    Warning,
    Error,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthStatus {
    #[default]
    Authenticated,
    Unauthenticated,
    Unknown,
}

/// `ServerProviderSkill`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderSkill {
    pub name: String,
    pub description: Option<String>,
    pub path: String,
    pub scope: Option<String>,
    pub enabled: bool,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub user_invocation_only: bool,
    /// `userInvocable !== false`.
    pub user_invocable: bool,
}

impl ProviderSkill {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            path: format!("/skills/{name}/SKILL.md"),
            enabled: true,
            user_invocable: true,
            ..Default::default()
        }
    }
}

/// `ServerProviderSlashCommand`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderSlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub input_hint: Option<String>,
}

/// `ProviderInstanceEntry` plus the snapshot fields the picker reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderInstanceEntry {
    pub instance_id: String,
    pub driver_kind: String,
    pub display_name: String,
    pub accent_color: Option<String>,
    pub continuation_group_key: Option<String>,
    pub enabled: bool,
    pub installed: bool,
    pub status: ProviderStatus,
    pub is_default: bool,
    pub is_available: bool,
    pub auth_status: AuthStatus,
    /// `setup.canAuthenticate || setup.canInstall`.
    pub can_setup: bool,
    pub status_message: Option<String>,
    pub models: Vec<ProviderModel>,
    pub skills: Vec<ProviderSkill>,
    pub slash_commands: Vec<ProviderSlashCommand>,
    /// `formatProviderUpdateRequiredNotice` for the current search.
    pub update_required_notice: Option<String>,
    /// Drivers whose context window streams (the meter reserves space).
    pub reports_context_window: bool,
    pub show_interaction_mode_toggle: bool,
}

impl ProviderInstanceEntry {
    pub fn new(instance_id: &str, driver_kind: &str, display_name: &str) -> Self {
        Self {
            instance_id: instance_id.to_owned(),
            driver_kind: driver_kind.to_owned(),
            display_name: display_name.to_owned(),
            enabled: true,
            installed: true,
            is_default: instance_id == driver_kind,
            is_available: true,
            ..Default::default()
        }
    }

    /// `isProviderInstancePickerReady`: can currently contribute models to an
    /// interactive picker.
    pub fn picker_ready(&self) -> bool {
        self.enabled && self.is_available && self.status == ProviderStatus::Ready
    }

    /// `isProviderInstancePickerVisible`: picker rails contain configured,
    /// enabled instances only.
    pub fn picker_visible(&self) -> bool {
        self.enabled
    }

    /// `hasProviderSetup`.
    pub fn has_setup(&self) -> bool {
        self.driver_kind == "antigravity" || self.can_setup
    }
}

/// `formatProviderDriverKindLabel`.
pub fn format_provider_driver_kind_label(provider: &str) -> String {
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for ch in provider.chars() {
        if previous.is_some_and(|p| p.is_ascii_lowercase()) && ch.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(ch);
        previous = Some(ch);
    }
    let words: Vec<String> = spaced
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or(String::new(), |first| first.to_uppercase().chain(chars).collect())
        })
        .collect();
    words.join(" ")
}

/// `getProviderStatusMessage` (ProviderStatusBanner.tsx), minus the
/// compatibility advisory.
pub fn get_provider_status_message(entry: &ProviderInstanceEntry) -> String {
    if let Some(message) = &entry.status_message {
        return message.clone();
    }
    let provider_name = if entry.display_name.trim().is_empty() {
        format_provider_driver_kind_label(&entry.driver_kind)
    } else {
        entry.display_name.trim().to_owned()
    };
    if !entry.installed && entry.has_setup() {
        return format!(
            "Open provider setup to install {} on this environment.",
            format_provider_driver_kind_label(&entry.driver_kind)
        );
    }
    if entry.auth_status == AuthStatus::Unauthenticated {
        return if entry.has_setup() {
            if entry.driver_kind == "antigravity" {
                "Open provider setup to sign in with Google.".to_owned()
            } else {
                "Open provider setup to sign in.".to_owned()
            }
        } else {
            "Sign in via the CLI to authenticate again.".to_owned()
        };
    }
    match entry.status {
        ProviderStatus::Ready => "No models are available for this provider.".to_owned(),
        ProviderStatus::Error => format!("{provider_name} provider is unavailable."),
        _ => format!("{provider_name} provider has limited availability."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_driver_kind_labels() {
        assert_eq!(format_provider_driver_kind_label("claudeAgent"), "Claude Agent");
        assert_eq!(format_provider_driver_kind_label("opencode"), "Opencode");
        assert_eq!(format_provider_driver_kind_label("acp_registry"), "Acp Registry");
    }

    #[test]
    fn picker_readiness_follows_enabled_available_and_ready() {
        let mut entry = ProviderInstanceEntry::new("codex", "codex", "Codex");
        assert!(entry.picker_ready() && entry.picker_visible());
        entry.status = ProviderStatus::Error;
        assert!(!entry.picker_ready() && entry.picker_visible());
        entry.status = ProviderStatus::Ready;
        entry.enabled = false;
        assert!(!entry.picker_ready() && !entry.picker_visible());
    }

    #[test]
    fn status_messages_guide_the_user() {
        let mut entry = ProviderInstanceEntry::new("codex", "codex", "Codex");
        entry.installed = false;
        entry.can_setup = true;
        assert_eq!(get_provider_status_message(&entry), "Open provider setup to install Codex on this environment.");
        entry.installed = true;
        entry.auth_status = AuthStatus::Unauthenticated;
        assert_eq!(get_provider_status_message(&entry), "Open provider setup to sign in.");
        entry.can_setup = false;
        assert_eq!(get_provider_status_message(&entry), "Sign in via the CLI to authenticate again.");
        entry.auth_status = AuthStatus::Authenticated;
        entry.status = ProviderStatus::Error;
        assert_eq!(get_provider_status_message(&entry), "Codex provider is unavailable.");
        entry.status = ProviderStatus::Warning;
        assert_eq!(get_provider_status_message(&entry), "Codex provider has limited availability.");
        entry.status = ProviderStatus::Ready;
        assert_eq!(get_provider_status_message(&entry), "No models are available for this provider.");
        entry.status_message = Some("custom".into());
        assert_eq!(get_provider_status_message(&entry), "custom");
    }
}
