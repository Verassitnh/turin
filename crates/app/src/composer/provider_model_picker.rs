//! Port of upstream/apps/web/src/components/chat/ProviderModelPicker.tsx
//! (+ ProviderModelPicker.test.tsx): what the picker's trigger shows.
//!
//! The view (`views/provider_model_picker.rs`) paints the result. Upstream
//! renders to static markup in its test; the assertions here read the same
//! strings off [`ProviderModelTrigger`].

use turin_contracts::model::ANTIGRAVITY_DEFAULT_MODEL;

use crate::composer::catalog::ProviderInstanceEntry;
use crate::composer::model_display::get_trigger_display_model_name;
use crate::composer::model_picker::resolve_model_picker_selected_model;
use crate::composer::provider_state::ProviderModel;
use crate::state::helpers::provider_instance_display::{
    InstanceBadgeEntry, should_show_instance_badge,
};

/// `{ instanceId, model }` of a multi-model selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModel {
    pub instance_id: String,
    pub model: String,
}

/// A selected model in a multi-model trigger: its instance (if still known)
/// and the label it reads as.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedModelLabel {
    pub instance_id: String,
    pub model: String,
    pub entry: Option<ProviderInstanceEntry>,
    pub label: String,
}

pub struct ProviderModelTriggerInput<'a> {
    /// The instance currently selected in the composer.
    pub active_instance_id: &'a str,
    pub model: &'a str,
    pub instance_entries: &'a [ProviderInstanceEntry],
    /// `modelOptionsByInstance.get(instanceId) ?? []`.
    pub options_for: &'a dyn Fn(&str) -> &'a [ProviderModel],
    pub selected_models: Option<&'a [SelectedModel]>,
    /// Aggregate settings can show a neutral value without claiming one
    /// provider is selected.
    pub trigger_label: Option<&'a str>,
    /// `shortcutLabelForCommand(keybindings, "modelPicker.toggle")`.
    pub shortcut_label: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderModelTrigger {
    pub active_entry: Option<ProviderInstanceEntry>,
    /// `triggerTitle`: the model's short name, or the "Choose model" prompt.
    pub title: String,
    /// `triggerLabel` (the picker's, not the prop): title plus "(Unavailable)".
    pub label: String,
    /// What the trigger text reads: the prop, the multi-model summary, or the title.
    pub text: String,
    pub tooltip: String,
    pub aria_label: Option<String>,
    pub show_instance_badge: bool,
    /// The "Unavailable" badge after the label.
    pub show_unavailable_badge: bool,
    pub selected: Option<Vec<SelectedModelLabel>>,
}

fn badge_entry(entry: &ProviderInstanceEntry) -> InstanceBadgeEntry {
    InstanceBadgeEntry {
        driver_kind: entry.driver_kind.clone(),
        accent_color: entry.accent_color.clone(),
        acp_registry_agent_id: None,
    }
}

pub fn resolve_provider_model_trigger(
    input: &ProviderModelTriggerInput<'_>,
) -> ProviderModelTrigger {
    // Resolve the active instance entry by exact routing key. The composer
    // resolves fallbacks before rendering this; if the selected instance
    // disappears, do not infer a replacement from its driver kind.
    let active_entry = input
        .instance_entries
        .iter()
        .find(|entry| entry.instance_id == input.active_instance_id);
    let options = (input.options_for)(input.active_instance_id);
    // Account-specific catalogs must keep the selected model label while unavailable.
    let driver = active_entry.map(|entry| entry.driver_kind.as_str());
    let selected_model =
        resolve_model_picker_selected_model(driver, input.model, options).or_else(|| {
            if matches!(driver, Some("opencode" | "antigravity")) {
                None
            } else {
                options.first()
            }
        });
    let title = match selected_model {
        Some(model) => get_trigger_display_model_name(model),
        None if input.model == ANTIGRAVITY_DEFAULT_MODEL => "Choose model".to_owned(),
        None if input.model.is_empty() => "Choose model".to_owned(),
        None => input.model.to_owned(),
    };
    let label = match selected_model {
        Some(model) => format!(
            "{}{}",
            get_trigger_display_model_name(model),
            if model.is_unavailable {
                " (Unavailable)"
            } else {
                ""
            }
        ),
        None => title.clone(),
    };
    let show_instance_badge = active_entry.is_some_and(|entry| {
        let all: Vec<InstanceBadgeEntry> = input.instance_entries.iter().map(badge_entry).collect();
        should_show_instance_badge(&badge_entry(entry), all.iter())
    });

    let selected: Option<Vec<SelectedModelLabel>> = input.selected_models.map(|selected| {
        selected
            .iter()
            .map(|selection| {
                let entry = input
                    .instance_entries
                    .iter()
                    .find(|candidate| candidate.instance_id == selection.instance_id);
                let options = (input.options_for)(&selection.instance_id);
                let model = resolve_model_picker_selected_model(
                    entry.map(|entry| entry.driver_kind.as_str()),
                    &selection.model,
                    options,
                );
                SelectedModelLabel {
                    instance_id: selection.instance_id.clone(),
                    model: selection.model.clone(),
                    entry: entry.cloned(),
                    label: match model {
                        Some(model) => format!(
                            "{}{}",
                            get_trigger_display_model_name(model),
                            if model.is_unavailable {
                                " (Unavailable)"
                            } else {
                                ""
                            }
                        ),
                        None => selection.model.clone(),
                    },
                }
            })
            .collect()
    });
    let multiple_label = selected.as_ref().map(|selected| {
        if selected.is_empty() {
            "Choose models".to_owned()
        } else {
            let first: Vec<&str> = selected.iter().take(2).map(|s| s.label.as_str()).collect();
            format!(
                "{}{}",
                first.join(", "),
                if selected.len() > 2 {
                    format!(", {} more", selected.len() - 2)
                } else {
                    String::new()
                }
            )
        }
    });
    let all_model_names = selected.as_ref().map(|selected| {
        let joined = selected
            .iter()
            .map(|s| s.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if joined.is_empty() {
            "Choose models".to_owned()
        } else {
            joined
        }
    });
    let base_tooltip = input
        .trigger_label
        .map(str::to_owned)
        .or_else(|| all_model_names.clone())
        .unwrap_or_else(|| label.clone());
    let tooltip = match input.shortcut_label {
        Some(shortcut) => format!("{base_tooltip} · {shortcut}"),
        None => base_tooltip,
    };
    let text = input
        .trigger_label
        .map(str::to_owned)
        .or(multiple_label)
        .unwrap_or_else(|| title.clone());
    ProviderModelTrigger {
        active_entry: active_entry.cloned(),
        show_unavailable_badge: selected_model.is_some_and(|model| model.is_unavailable)
            && selected.is_none()
            && input.trigger_label.is_none(),
        aria_label: all_model_names,
        title,
        label,
        text,
        tooltip,
        show_instance_badge,
        selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::helpers::provider_instance_display::provider_instance_initials;

    struct Fixture {
        entries: Vec<ProviderInstanceEntry>,
        options: Vec<ProviderModel>,
    }

    impl Fixture {
        fn new(
            instance_id: &str,
            driver: &str,
            options: Vec<ProviderModel>,
            include_entry: bool,
        ) -> Self {
            let mut entry = ProviderInstanceEntry::new(instance_id, driver, driver);
            entry.is_default = instance_id == driver;
            Self {
                entries: if include_entry {
                    vec![entry]
                } else {
                    Vec::new()
                },
                options,
            }
        }

        fn render(
            &self,
            instance_id: &str,
            model: &str,
            trigger_label: Option<&str>,
        ) -> ProviderModelTrigger {
            let options = &self.options;
            let options_for = move |_: &str| -> &[ProviderModel] { options };
            resolve_provider_model_trigger(&ProviderModelTriggerInput {
                active_instance_id: instance_id,
                model,
                instance_entries: &self.entries,
                options_for: &options_for,
                selected_models: None,
                trigger_label,
                shortcut_label: None,
            })
        }
    }

    fn model(slug: &str, name: &str) -> ProviderModel {
        ProviderModel::simple(slug, name)
    }

    #[test]
    fn shows_a_neutral_aggregate_value_without_a_representative_model_or_availability_badge() {
        let fixture = Fixture::new(
            "codex_personal",
            "codex",
            vec![ProviderModel {
                is_unavailable: true,
                ..model("gpt-5", "GPT 5")
            }],
            true,
        );
        let trigger = fixture.render("codex_personal", "gpt-5", Some("Mixed values"));
        assert_eq!(trigger.text, "Mixed values");
        assert_ne!(trigger.text, "GPT 5");
        assert!(!trigger.show_unavailable_badge);
    }

    #[test]
    fn shows_a_choice_prompt_before_antigravity_has_an_account_catalog() {
        for model_id in ["", ANTIGRAVITY_DEFAULT_MODEL] {
            let fixture = Fixture::new("antigravity", "antigravity", Vec::new(), true);
            let trigger = fixture.render("antigravity", model_id, None);
            assert_eq!(trigger.text, "Choose model");
        }
    }

    #[test]
    fn shows_the_actual_default_model_for_an_antigravity_marker() {
        let defaults = [
            ProviderModel {
                aliases: vec![ANTIGRAVITY_DEFAULT_MODEL.to_owned()],
                ..model("gemini-pro", "Gemini Pro")
            },
            ProviderModel {
                is_default: true,
                ..model("gemini-pro", "Gemini Pro")
            },
        ];
        for pro in defaults {
            let fixture = Fixture::new(
                "google_work",
                "antigravity",
                vec![model("gemini-fast", "Gemini Fast"), pro],
                true,
            );
            let trigger = fixture.render("google_work", ANTIGRAVITY_DEFAULT_MODEL, None);
            assert_eq!(trigger.text, "Gemini Pro");
        }
    }

    #[test]
    fn keeps_the_selected_model_label_when_the_account_catalog_does_not_contain_it() {
        for driver in ["opencode", "antigravity"] {
            let fixture = Fixture::new(
                "team_runtime",
                driver,
                vec![model("fallback", "Fallback model")],
                true,
            );
            let trigger = fixture.render("team_runtime", "missing-model", None);
            assert_eq!(trigger.text, "missing-model");
        }
    }

    #[test]
    fn uses_the_first_option_label_for_a_missing_model() {
        for driver in ["codex", "claudeAgent", "cursor", "grok"] {
            let fixture = Fixture::new(
                &format!("{driver}_work"),
                driver,
                vec![model("fallback-model", "Fallback model")],
                true,
            );
            let trigger = fixture.render(&format!("{driver}_work"), "missing-model", None);
            assert_eq!(trigger.text, "Fallback model");
        }
    }

    #[test]
    fn prefers_a_matching_model_for_opencode() {
        let fixture = Fixture::new(
            "custom_runtime",
            "opencode",
            vec![
                model("openrouter/fallback", "Fallback model"),
                model("openrouter/selected", "Selected model"),
            ],
            true,
        );
        let trigger = fixture.render("custom_runtime", "openrouter/selected", None);
        assert_eq!(trigger.text, "Selected model");
    }

    #[test]
    fn uses_the_first_option_when_the_active_instance_entry_is_missing() {
        let fixture = Fixture::new(
            "missing_instance",
            "opencode",
            vec![model("fallback-model", "Fallback model")],
            false,
        );
        let trigger = fixture.render("missing_instance", "missing-model", None);
        assert_eq!(trigger.text, "Fallback model");
    }

    #[test]
    fn keeps_instance_initials_visible_in_the_resting_trigger() {
        let mut personal = ProviderInstanceEntry::new("codex_personal", "codex", "Codex Personal");
        personal.is_default = false;
        let entries = vec![
            ProviderInstanceEntry::new("codex", "codex", "Codex"),
            personal,
        ];
        let options_for = |_: &str| -> &[ProviderModel] { &[] };
        let trigger = resolve_provider_model_trigger(&ProviderModelTriggerInput {
            active_instance_id: "codex_personal",
            model: "gpt-5",
            instance_entries: &entries,
            options_for: &options_for,
            selected_models: None,
            trigger_label: None,
            shortcut_label: None,
        });
        assert!(trigger.show_instance_badge);
        let entry = trigger.active_entry.expect("active entry");
        assert_eq!(provider_instance_initials(&entry.display_name), "CP");
    }

    #[test]
    fn summarises_a_multi_model_selection() {
        let entries = vec![ProviderInstanceEntry::new("codex", "codex", "Codex")];
        let options = vec![model("a", "A"), model("b", "B"), model("c", "C")];
        let options_for = |_: &str| -> &[ProviderModel] { &options };
        let selected: Vec<SelectedModel> = ["a", "b", "c"]
            .iter()
            .map(|slug| SelectedModel {
                instance_id: "codex".into(),
                model: (*slug).into(),
            })
            .collect();
        let trigger = resolve_provider_model_trigger(&ProviderModelTriggerInput {
            active_instance_id: "codex",
            model: "a",
            instance_entries: &entries,
            options_for: &options_for,
            selected_models: Some(&selected),
            trigger_label: None,
            shortcut_label: Some("⌘⇧M"),
        });
        assert_eq!(trigger.text, "A, B, 1 more");
        assert_eq!(trigger.tooltip, "A, B, C · ⌘⇧M");
    }
}
