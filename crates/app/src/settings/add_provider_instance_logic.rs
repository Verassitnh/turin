//! Port of upstream/apps/web/src/components/settings/AddProviderInstanceDialog.logic.ts
//! (and AddProviderInstanceDialog.test.ts) for the kept drivers.
//!
//! Not ported: the ACP registry wizard (`ACP_REGISTRY_WIZARD_STEPS`,
//! `LOCAL_ACP_WIZARD_STEPS`, `isConfiguredAcpRegistryAgent`,
//! `resolveAcpRegistryWizardNavigation`): the ACP registry is cut.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardNavigation {
    Navigate { step: usize },
    Blocked { step: usize, error: String },
}

const IDENTITY_STEP: usize = 1;

pub const ADD_PROVIDER_WIZARD_STEPS: [&str; 3] = ["Provider", "Identity", "Config"];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderIdentityDraft {
    pub label: String,
    pub accent_color: String,
    pub instance_id_override: Option<String>,
}

pub fn get_provider_identity_draft(
    drafts: &HashMap<String, ProviderIdentityDraft>,
    driver: &str,
) -> ProviderIdentityDraft {
    drafts.get(driver).cloned().unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
pub struct ProviderIdentityDraftUpdate {
    pub label: Option<String>,
    pub accent_color: Option<String>,
    pub instance_id_override: Option<Option<String>>,
}

pub fn update_provider_identity_draft(
    drafts: &HashMap<String, ProviderIdentityDraft>,
    driver: &str,
    update: ProviderIdentityDraftUpdate,
) -> HashMap<String, ProviderIdentityDraft> {
    let mut next = drafts.clone();
    let mut draft = get_provider_identity_draft(drafts, driver);
    if let Some(label) = update.label {
        draft.label = label;
    }
    if let Some(accent_color) = update.accent_color {
        draft.accent_color = accent_color;
    }
    if let Some(instance_id_override) = update.instance_id_override {
        draft.instance_id_override = instance_id_override;
    }
    next.insert(driver.to_owned(), draft);
    next
}

/// `deriveAvailableInstanceId`: `base`, then `base_2`, `base_3`, … clipped so
/// the whole id stays within 64 UTF-16 units.
pub fn derive_available_instance_id(
    derive: impl Fn(&str) -> String,
    label: &str,
    existing: &HashSet<String>,
) -> String {
    let base = derive(label);
    if base.is_empty() || !existing.contains(&base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let suffix_text = format!("_{suffix}");
        let keep = 64usize.saturating_sub(suffix_text.len());
        let head: String = base.chars().take(keep).collect();
        let candidate = format!("{head}{suffix_text}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

pub struct WizardValidation {
    pub instance_id_error: Option<String>,
    pub identity_step: Option<usize>,
    pub prerequisite: Option<WizardPrerequisite>,
}

pub struct WizardPrerequisite {
    pub step: usize,
    pub error: Option<String>,
}

/// Resolve navigation within the add-provider wizard.
///
/// Moving forward past Identity requires a valid instance id, whether the user
/// advances one step at a time or skips directly to Config from a step header.
/// A blocked skip lands on Identity so its existing inline validation is
/// visible. Backward navigation is always preserved.
pub fn resolve_wizard_navigation(
    current_step: usize,
    requested_step: isize,
    step_count: usize,
    validation: &WizardValidation,
) -> WizardNavigation {
    let last_step = step_count.saturating_sub(1);
    let target_step = requested_step.clamp(0, last_step as isize) as usize;
    let identity_step = validation.identity_step.unwrap_or(IDENTITY_STEP);

    if let Some(prerequisite) = &validation.prerequisite
        && let Some(error) = &prerequisite.error
        && current_step <= prerequisite.step
        && target_step > prerequisite.step
    {
        return WizardNavigation::Blocked {
            step: prerequisite.step.min(last_step),
            error: error.clone(),
        };
    }

    let moves_forward_past_identity = current_step <= identity_step && target_step > identity_step;
    if moves_forward_past_identity && let Some(error) = &validation.instance_id_error {
        return WizardNavigation::Blocked {
            step: identity_step.min(last_step),
            error: error.clone(),
        };
    }

    WizardNavigation::Navigate { step: target_step }
}

/// `readCodexSetupMode` (CodexSetupSection.logic.ts).
pub fn read_codex_setup_mode(config: Option<&serde_json::Value>) -> CodexSetupMode {
    if config
        .and_then(|config| config.get("setupMode"))
        .and_then(|mode| mode.as_str())
        == Some("managed")
    {
        CodexSetupMode::Managed
    } else {
        CodexSetupMode::Existing
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexSetupMode {
    Managed,
    Existing,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn invalid_id() -> WizardValidation {
        WizardValidation {
            instance_id_error: Some("Instance ID is required.".into()),
            identity_step: None,
            prerequisite: None,
        }
    }

    fn valid_id() -> WizardValidation {
        WizardValidation {
            instance_id_error: None,
            identity_step: None,
            prerequisite: None,
        }
    }

    fn blocked(step: usize) -> WizardNavigation {
        WizardNavigation::Blocked {
            step,
            error: "Instance ID is required.".into(),
        }
    }

    #[test]
    fn allows_moving_from_driver_to_identity_before_the_instance_id_is_valid() {
        assert_eq!(
            resolve_wizard_navigation(0, 1, 3, &invalid_id()),
            WizardNavigation::Navigate { step: 1 }
        );
    }

    #[test]
    fn blocks_next_from_identity_to_config_while_the_instance_id_is_invalid() {
        assert_eq!(resolve_wizard_navigation(1, 2, 3, &invalid_id()), blocked(1));
    }

    #[test]
    fn stops_a_direct_driver_to_config_skip_at_identity_and_surfaces_its_error() {
        assert_eq!(resolve_wizard_navigation(0, 2, 3, &invalid_id()), blocked(1));
    }

    #[test]
    fn allows_advancing_and_skipping_forward_once_the_instance_id_is_valid() {
        assert_eq!(
            resolve_wizard_navigation(1, 2, 3, &valid_id()),
            WizardNavigation::Navigate { step: 2 }
        );
        assert_eq!(
            resolve_wizard_navigation(0, 2, 3, &valid_id()),
            WizardNavigation::Navigate { step: 2 }
        );
    }

    #[test]
    fn always_preserves_backward_navigation() {
        for (from, to) in [(2, 1), (2, 0), (1, 0)] {
            assert_eq!(
                resolve_wizard_navigation(from, to, 3, &invalid_id()),
                WizardNavigation::Navigate { step: to as usize }
            );
        }
    }

    #[test]
    fn clamps_requested_steps_to_the_wizard_bounds() {
        assert_eq!(
            resolve_wizard_navigation(2, 8, 3, &valid_id()),
            WizardNavigation::Navigate { step: 2 }
        );
        assert_eq!(
            resolve_wizard_navigation(0, -1, 3, &invalid_id()),
            WizardNavigation::Navigate { step: 0 }
        );
    }

    #[test]
    fn a_failed_prerequisite_blocks_at_its_own_step() {
        let validation = WizardValidation {
            instance_id_error: None,
            identity_step: Some(1),
            prerequisite: Some(WizardPrerequisite {
                step: 0,
                error: Some("Select one.".into()),
            }),
        };
        assert_eq!(
            resolve_wizard_navigation(0, 1, 3, &validation),
            WizardNavigation::Blocked {
                step: 0,
                error: "Select one.".into()
            }
        );
    }

    #[test]
    fn derives_a_collision_free_instance_id_without_exceeding_the_slug_limit() {
        let derive = |label: &str| format!("codex_{label}");
        let existing: HashSet<String> =
            ["codex_work".to_owned(), "codex_work_2".to_owned()].into();
        assert_eq!(
            derive_available_instance_id(derive, "work", &existing),
            "codex_work_3"
        );

        let long_base = format!("codex_{}", "a".repeat(58));
        let existing: HashSet<String> = [long_base.clone()].into();
        assert_eq!(
            derive_available_instance_id(|_| long_base.clone(), "ignored", &existing).len(),
            64
        );
    }

    #[test]
    fn keeps_identity_drafts_separate_per_driver() {
        let first = update_provider_identity_draft(
            &HashMap::new(),
            "claudeAgent",
            ProviderIdentityDraftUpdate {
                label: Some("Work".into()),
                instance_id_override: Some(Some("claudeAgent_work".into())),
                ..Default::default()
            },
        );
        assert_eq!(
            get_provider_identity_draft(&first, "codex"),
            ProviderIdentityDraft::default()
        );
        let drafts = update_provider_identity_draft(
            &first,
            "codex",
            ProviderIdentityDraftUpdate {
                label: Some("Personal".into()),
                instance_id_override: Some(Some("codex_personal".into())),
                ..Default::default()
            },
        );
        let claude = get_provider_identity_draft(&drafts, "claudeAgent");
        assert_eq!(claude.label, "Work");
        assert_eq!(claude.instance_id_override.as_deref(), Some("claudeAgent_work"));
    }

    #[test]
    fn reads_the_codex_setup_mode() {
        assert_eq!(
            read_codex_setup_mode(Some(&json!({ "setupMode": "managed" }))),
            CodexSetupMode::Managed
        );
        assert_eq!(read_codex_setup_mode(Some(&json!({}))), CodexSetupMode::Existing);
        assert_eq!(read_codex_setup_mode(None), CodexSetupMode::Existing);
    }
}
