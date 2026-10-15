//! Ports of composerProviderState.test.tsx and TraitsPicker.test.ts.

use super::*;
use crate::composer::provider_options::test_support::{boolean, select};
use crate::composer::provider_options::{OptionChoice, SelectionContext};

const MODEL: &str = "test-model";

fn model_with(descriptors: Vec<OptionDescriptor>) -> Vec<ProviderModel> {
    vec![ProviderModel {
        capabilities: Some(ModelCapabilities { option_descriptors: descriptors }),
        ..ProviderModel::simple(MODEL, MODEL)
    }]
}

fn selections(entries: &[(&str, OptionValue)]) -> Vec<OptionSelection> {
    entries
        .iter()
        .map(|(id, value)| OptionSelection { id: (*id).to_owned(), value: value.clone() })
        .collect()
}

fn s(value: &str) -> OptionValue {
    OptionValue::Str(value.to_owned())
}

fn state(provider: &str, models: &[ProviderModel], model: &str, options: &[OptionSelection], plan: bool) -> ComposerProviderState {
    get_composer_provider_state(&ComposerProviderStateInput {
        provider,
        model,
        models,
        prompt_injection_state: ComposerPromptInjectionState::None,
        model_options: options,
        plan_mode_enabled: plan,
    })
}

#[test]
fn derives_a_stable_prompt_injection_state() {
    assert_eq!(get_composer_prompt_injection_state("Investigate this failure"), ComposerPromptInjectionState::None);
    assert_eq!(
        get_composer_prompt_injection_state("Ultrathink:\nInvestigate this failure"),
        ComposerPromptInjectionState::Ultrathink
    );
}

#[test]
fn uses_descriptor_defaults_for_display_without_dispatching_them() {
    let models = model_with(vec![select("effort", &[("low", "Low", false), ("high", "High", true)], &[])]);
    assert_eq!(
        state("codex", &models, MODEL, &[], true),
        ComposerProviderState {
            provider: "codex".into(),
            prompt_effort: Some("high".into()),
            model_options_for_dispatch: None,
            ultrathink_active: false,
        }
    );
}

#[test]
fn lets_selections_override_defaults_and_propagates_them() {
    let models = model_with(vec![
        select("effort", &[("low", "Low", false), ("high", "High", true)], &[]),
        boolean("fastMode", None),
    ]);
    let picked = selections(&[("effort", s("low")), ("fastMode", OptionValue::Bool(true))]);
    let result = state("codex", &models, MODEL, &picked, true);
    assert_eq!(result.prompt_effort.as_deref(), Some("low"));
    assert_eq!(result.model_options_for_dispatch, Some(picked));
}

#[test]
fn preserves_selections_that_match_defaults() {
    let models = model_with(vec![select("effort", &[("high", "High", true)], &[]), boolean("fastMode", None)]);
    let picked = selections(&[("effort", s("high")), ("fastMode", OptionValue::Bool(false))]);
    assert_eq!(state("codex", &models, MODEL, &picked, true).model_options_for_dispatch, Some(picked));
}

#[test]
fn drops_selections_for_descriptors_the_model_does_not_declare() {
    let models = model_with(vec![boolean("thinking", None)]);
    let picked = selections(&[("effort", s("max")), ("thinking", OptionValue::Bool(false))]);
    let result = state("codex", &models, MODEL, &picked, true);
    assert_eq!(result.prompt_effort, None);
    assert_eq!(result.model_options_for_dispatch, Some(selections(&[("thinking", OptionValue::Bool(false))])));
}

#[test]
fn derives_prompt_effort_from_the_first_select_and_preserves_the_others() {
    let models = model_with(vec![
        select("effort", &[("high", "High", true)], &[]),
        select("contextWindow", &[("200k", "200k", true), ("1m", "1M", false)], &[]),
        select("agent", &[("build", "Build", true), ("plan", "Plan", false)], &[]),
    ]);
    let picked = selections(&[("agent", s("plan"))]);
    let result = state("codex", &models, MODEL, &picked, true);
    assert_eq!(result.prompt_effort.as_deref(), Some("high"));
    assert_eq!(result.model_options_for_dispatch, Some(picked));
}

#[test]
fn drops_the_plan_agent_when_legacy_plan_mode_is_disabled() {
    let models = model_with(vec![select("agent", &[("build", "Build", true), ("plan", "Plan", false)], &[])]);
    let result = state("codex", &models, MODEL, &selections(&[("agent", s("plan"))]), false);
    assert_eq!(result.model_options_for_dispatch, Some(selections(&[("agent", s("build"))])));

    let only_plan = model_with(vec![select("agent", &[("plan", "Plan", true)], &[])]);
    let result = state("codex", &only_plan, MODEL, &selections(&[("agent", s("plan"))]), false);
    assert_eq!(result.prompt_effort, None);
    assert_eq!(result.model_options_for_dispatch, None);

    let fallback = model_with(vec![select("agent", &[("plan", "Plan", true), ("research", "Research", false)], &[])]);
    assert_eq!(state("codex", &fallback, MODEL, &[], false).model_options_for_dispatch, None);
}

#[test]
fn returns_no_dispatch_options_when_the_model_declares_no_descriptors() {
    let models = model_with(vec![]);
    let result = state("codex", &models, MODEL, &selections(&[("anything", s("value"))]), true);
    assert_eq!((result.prompt_effort, result.model_options_for_dispatch), (None, None));
}

#[test]
fn preserves_explicit_options_when_an_opencode_model_is_absent_from_the_catalog() {
    let catalog = vec![ProviderModel {
        capabilities: Some(ModelCapabilities::default()),
        ..ProviderModel::simple("opencode/big-pickle", "Big Pickle")
    }];
    let picked = selections(&[("variant", s("max")), ("agent", s("build"))]);
    assert_eq!(
        state("opencode", &catalog, "opencode/kimi-k3", &picked, false).model_options_for_dispatch,
        Some(picked.clone())
    );
    assert_eq!(state("opencode", &[], "opencode/kimi-k3", &picked, false).model_options_for_dispatch, Some(picked));
    let with_plan = selections(&[("variant", s("max")), ("agent", s("plan"))]);
    assert_eq!(
        state("opencode", &[], "opencode/kimi-k3", &with_plan, false).model_options_for_dispatch,
        Some(selections(&[("variant", s("max"))]))
    );
}

#[test]
fn does_not_preserve_unknown_options_for_other_providers() {
    for provider in ["codex", "claudeAgent", "cursor", "grok"] {
        let models = model_with(vec![]);
        let result = state(provider, &models, "missing-model", &selections(&[("unknown", s("value"))]), true);
        assert_eq!(result.model_options_for_dispatch, None, "{provider}");
    }
}

#[test]
fn validates_options_for_a_known_model_selected_through_a_legacy_alias() {
    let models = vec![ProviderModel {
        aliases: vec!["legacy-test-model".into()],
        capabilities: Some(ModelCapabilities {
            option_descriptors: vec![select("effort", &[("low", "Low", false), ("high", "High", true)], &[])],
        }),
        ..ProviderModel::simple("test-model", "Test Model")
    }];
    let picked = selections(&[("effort", s("low")), ("unknown", s("value"))]);
    let result = state("claudeAgent", &models, "legacy-test-model", &picked, false);
    assert_eq!(result.model_options_for_dispatch, Some(selections(&[("effort", s("low"))])));
}

#[test]
fn adds_the_ultrathink_glow_only_for_prompt_injected_descriptors() {
    let models = model_with(vec![select(
        "effort",
        &[("medium", "Medium", false), ("high", "High", true), ("ultrathink", "Ultrathink", false)],
        &["ultrathink"],
    )]);
    let result = get_composer_provider_state(&ComposerProviderStateInput {
        provider: "codex",
        model: MODEL,
        models: &models,
        prompt_injection_state: get_composer_prompt_injection_state("Ultrathink:\nInvestigate this failure"),
        model_options: &selections(&[("effort", s("medium"))]),
        plan_mode_enabled: true,
    });
    assert_eq!(result.prompt_effort.as_deref(), Some("medium"));
    assert!(result.ultrathink_active);
    let plain = model_with(vec![select("effort", &[("high", "High", true)], &[])]);
    let result = get_composer_provider_state(&ComposerProviderStateInput {
        provider: "codex",
        model: MODEL,
        models: &plain,
        prompt_injection_state: ComposerPromptInjectionState::Ultrathink,
        model_options: &[],
        plan_mode_enabled: true,
    });
    assert!(!result.ultrathink_active);
}

#[test]
fn defaults_fast_mode_to_false_unless_the_user_selected_it() {
    let models = model_with(vec![boolean("fastMode", Some(true))]);
    assert_eq!(
        state("cursor", &models, MODEL, &[], true).model_options_for_dispatch,
        Some(selections(&[("fastMode", OptionValue::Bool(false))]))
    );
    for value in [true, false] {
        let picked = selections(&[("fastMode", OptionValue::Bool(value))]);
        assert_eq!(state("cursor", &models, MODEL, &picked, true).model_options_for_dispatch, Some(picked));
    }
}

#[test]
fn implicit_fast_mode_default_needs_a_fast_mode_descriptor() {
    let fast = ModelCapabilities { option_descriptors: vec![boolean("fastMode", Some(true))] };
    assert_eq!(
        with_implicit_fast_mode_default(&fast, &[]),
        Some(selections(&[("fastMode", OptionValue::Bool(false))]))
    );
    let chosen = selections(&[("fastMode", OptionValue::Bool(true))]);
    assert_eq!(with_implicit_fast_mode_default(&fast, &chosen), Some(chosen));
    let thinking = ModelCapabilities { option_descriptors: vec![boolean("thinking", Some(true))] };
    assert_eq!(with_implicit_fast_mode_default(&thinking, &[]), None);
}

#[test]
fn traits_fast_mode_resolves_to_normal_without_a_user_selection() {
    let models = model_with(vec![boolean("fastMode", Some(true))]);
    let caps = get_provider_model_capabilities(&models, Some(MODEL), "cursor", true);
    let resolved = with_implicit_fast_mode_default(&caps, &[]).unwrap();
    let descriptors = get_provider_option_descriptors(&caps, &resolved);
    assert!(matches!(&descriptors[0], OptionDescriptor::Boolean { current_value: Some(false), .. }));
}

// ── buildTraitsTriggerDisplay ──────────────────────────────────────────────

fn select_current(id: &str, options: &[(&str, &str)], current: &str) -> OptionDescriptor {
    OptionDescriptor::Select {
        id: id.into(),
        label: id.into(),
        description: None,
        options: options
            .iter()
            .map(|(id, label)| OptionChoice { id: (*id).into(), label: (*label).into(), description: None, is_default: false })
            .collect(),
        current_value: Some(current.into()),
        prompt_injected_values: vec![],
    }
}

fn effort() -> OptionDescriptor {
    select_current("reasoningEffort", &[("high", "High"), ("max", "Max")], "high")
}

fn context_window() -> OptionDescriptor {
    select_current("contextWindow", &[("200k", "200k"), ("1m", "1M")], "1m")
}

fn fast_mode(value: bool) -> OptionDescriptor {
    OptionDescriptor::Boolean { id: "fastMode".into(), label: "Fast Mode".into(), description: None, current_value: Some(value) }
}

fn service_tier(current: &str) -> OptionDescriptor {
    let mut descriptor = select_current(
        "serviceTier",
        &[("default", "Standard"), ("priority", "Fast"), ("ultrafast", "Ultrafast"), ("flex", "Flex")],
        current,
    );
    if let OptionDescriptor::Select { options, .. } = &mut descriptor {
        options[0].is_default = true;
    }
    descriptor
}

fn display_for(provider: &str, descriptors: &[OptionDescriptor], primary: Option<&str>, ultrathink: bool) -> String {
    build_traits_trigger_display(&TraitsTriggerInput {
        provider,
        descriptors,
        primary_select_descriptor_id: primary,
        ultrathink_prompt_controlled: ultrathink,
        model_selection: None,
        reported_model_selection: None,
    })
}

fn display(descriptors: &[OptionDescriptor]) -> String {
    display_for("codex", descriptors, Some("reasoningEffort"), false)
}

fn renamed(descriptor: OptionDescriptor, new_id: &str) -> OptionDescriptor {
    match descriptor {
        OptionDescriptor::Select { label, description, options, current_value, prompt_injected_values, .. } => {
            OptionDescriptor::Select { id: new_id.into(), label, description, options, current_value, prompt_injected_values }
        }
        other => other,
    }
}

#[test]
fn omits_fast_mode_from_the_label_when_it_is_off_and_pairs_it_with_reasoning() {
    assert_eq!(display(&[effort(), fast_mode(false), context_window()]), "High · 1M");
    assert_eq!(display(&[effort(), fast_mode(true), context_window()]), "High Fast · 1M");
    assert_eq!(display(&[effort(), context_window(), fast_mode(true)]), "High Fast · 1M");
}

#[test]
fn pairs_fast_with_any_reasoning_descriptor_even_when_context_is_primary() {
    for id in ["reasoningEffort", "reasoning", "effort", "variant", "thinking"] {
        for speed in [fast_mode(true), service_tier("priority")] {
            let descriptors = [
                select_current("profile", &[("balanced", "Balanced")], "balanced"),
                context_window(),
                renamed(effort(), id),
                speed,
            ];
            assert_eq!(display_for("codex", &descriptors, Some("profile"), false), "Balanced · 1M · High Fast", "{id}");
        }
        let descriptors = [context_window(), renamed(effort(), id), service_tier("ultrafast")];
        assert_eq!(display_for("codex", &descriptors, Some("contextWindow"), false), "1M · High Ultrafast");
    }
}

#[test]
fn pairs_cursor_fast_with_reasoning_rather_than_the_thinking_toggle() {
    let thinking = OptionDescriptor::Boolean { id: "thinking".into(), label: "Thinking".into(), description: None, current_value: Some(true) };
    let descriptors = [renamed(effort(), "reasoning"), fast_mode(true), thinking];
    assert_eq!(display_for("cursor", &descriptors, Some("reasoning"), false), "High Fast · Thinking On");
}

#[test]
fn keeps_boolean_thinking_independent_of_fast() {
    for value in [true, false] {
        let thinking = OptionDescriptor::Boolean { id: "thinking".into(), label: "Thinking".into(), description: None, current_value: Some(value) };
        let label = format!("Thinking {}", if value { "On" } else { "Off" });
        assert_eq!(display(&[thinking.clone(), fast_mode(true)]), format!("{label} · Fast"));
        assert_eq!(
            display(&[thinking.clone(), renamed(effort(), "reasoning"), fast_mode(true)]),
            format!("{label} · High Fast")
        );
        assert_eq!(display(&[thinking, fast_mode(false)]), label);
    }
}

#[test]
fn keeps_speed_separate_without_a_reasoning_descriptor() {
    assert_eq!(display(&[context_window(), fast_mode(true)]), "1M · Fast");
}

#[test]
fn preserves_reasoning_labels_with_fast_across_harnesses() {
    for label in ["Low", "Medium", "High", "Extra High", "Max", "Ultra"] {
        for provider in ["codex", "claudeAgent", "cursor"] {
            let descriptor = select_current("reasoningEffort", &[("effort", label)], "effort");
            assert_eq!(
                display_for(provider, &[descriptor, fast_mode(true)], Some("reasoningEffort"), false),
                format!("{label} Fast")
            );
        }
    }
}

#[test]
fn treats_codex_service_tiers_as_fast_mode_states() {
    assert_eq!(display(&[effort(), service_tier("default")]), "High");
    assert_eq!(display(&[effort(), service_tier("priority")]), "High Fast");
    assert_eq!(display(&[effort(), service_tier("ultrafast")]), "High Ultrafast");
    assert_eq!(display(&[effort(), service_tier("flex")]), "High · Flex");
    let mut without_priority = service_tier("ultrafast");
    if let OptionDescriptor::Select { options, .. } = &mut without_priority {
        options.retain(|option| option.id != "priority");
    }
    assert_eq!(display(&[effort(), without_priority]), "High Ultrafast");
}

#[test]
fn keeps_standard_as_text_for_models_without_speed_tiers() {
    let mut non_speed = service_tier("default");
    if let OptionDescriptor::Select { options, .. } = &mut non_speed {
        options.retain(|option| option.id == "default" || option.id == "flex");
    }
    assert_eq!(display(&[effort(), non_speed.clone()]), "High · Standard");
    assert_eq!(display(&[non_speed]), "Standard");
}

#[test]
fn keeps_the_service_tier_readable_when_it_is_the_only_trait() {
    assert_eq!(display(&[service_tier("default")]), "Standard");
    assert_eq!(display(&[service_tier("priority")]), "Fast");
    assert_eq!(display(&[service_tier("ultrafast")]), "Ultrafast");
}

#[test]
fn falls_back_to_text_when_fast_mode_is_the_only_trait() {
    assert_eq!(display(&[fast_mode(true)]), "Fast");
    assert_eq!(display(&[fast_mode(false)]), "Normal");
    let thinking = OptionDescriptor::Boolean { id: "thinking".into(), label: "Thinking".into(), description: None, current_value: Some(true) };
    assert_eq!(display(&[effort(), thinking]), "High · Thinking On");
    assert_eq!(display(&[effort(), context_window()]), "High · 1M");
}

#[test]
fn stays_blank_when_descriptors_resolve_to_no_label_and_there_is_no_fast_mode() {
    let unresolved = OptionDescriptor::Select {
        id: "effort".into(),
        label: "effort".into(),
        description: None,
        options: vec![
            OptionChoice { id: "low".into(), label: "Low".into(), description: None, is_default: false },
            OptionChoice { id: "high".into(), label: "High".into(), description: None, is_default: false },
        ],
        current_value: None,
        prompt_injected_values: vec![],
    };
    assert_eq!(display(&[unresolved]), "");
}

#[test]
fn renders_prompt_controlled_ultrathink_with_fast() {
    assert_eq!(
        display_for("codex", &[effort(), fast_mode(true)], Some("reasoningEffort"), true),
        "Ultrathink Fast"
    );
}

#[test]
fn builds_read_only_descriptors_for_unavailable_models() {
    let descriptors = build_unavailable_model_option_descriptors(&selections(&[
        ("variant", s("max")),
        ("agent", s("build")),
        ("fastMode", OptionValue::Bool(true)),
    ]));
    assert_eq!(descriptors[0].label(), "Reasoning");
    assert_eq!(descriptors[1].label(), "Agent");
    assert_eq!(descriptors[2].label(), "Fast Mode");
    assert!(matches!(&descriptors[2], OptionDescriptor::Boolean { current_value: Some(true), .. }));
}

#[test]
fn shows_unknown_until_a_matching_provider_report_provides_default() {
    let descriptor = select_current("variant", &[("none", "None"), ("thinking", "Thinking")], "");
    let selection = SelectionContext { instance_id: "opencode", model: "ling", options: &[] };
    let build = |reported: Option<&SelectionContext<'_>>| {
        build_traits_trigger_display(&TraitsTriggerInput {
            provider: "opencode",
            descriptors: std::slice::from_ref(&descriptor),
            primary_select_descriptor_id: Some("variant"),
            ultrathink_prompt_controlled: false,
            model_selection: Some(&selection),
            reported_model_selection: reported,
        })
    };
    assert_eq!(build(None), "Unknown");
    let reported_options = [OptionSelection::string("variant", "default")];
    let reported = SelectionContext { instance_id: "opencode", model: "ling", options: &reported_options };
    assert_eq!(build(Some(&reported)), "Default");
}

// ── Traits menu behaviour ──────────────────────────────────────────────────

#[test]
fn picking_a_prompt_injected_value_rewrites_the_prompt() {
    let models = model_with(vec![select(
        "effort",
        &[("medium", "Medium", false), ("high", "High", true), ("ultrathink", "Ultrathink", false)],
        &["ultrathink"],
    )]);
    let section = traits_section(&TraitsInput {
        provider: "claudeAgent",
        models: &models,
        model: Some(MODEL),
        prompt: "",
        model_options: &[],
        allow_prompt_injected_effort: true,
        plan_mode_enabled: true,
    });
    assert!(section.has_any_controls);
    assert_eq!(
        handle_traits_select_change(&section, "effort", "ultrathink", ""),
        TraitsSelectOutcome::PromptChange("Ultrathink:\n".into())
    );
    assert_eq!(
        handle_traits_select_change(&section, "effort", "ultrathink", "fix it"),
        TraitsSelectOutcome::PromptChange("Ultrathink:\nfix it".into())
    );
    let outcome = handle_traits_select_change(&section, "effort", "medium", "fix it");
    assert_eq!(
        outcome,
        TraitsSelectOutcome::OptionsChange {
            selections: Some(selections(&[("effort", s("medium"))])),
            prompt: None
        }
    );
}

#[test]
fn ultrathink_prompt_control_strips_the_prefix_when_another_effort_is_picked() {
    let models = model_with(vec![select(
        "effort",
        &[("medium", "Medium", false), ("high", "High", true), ("ultrathink", "Ultrathink", false)],
        &["ultrathink"],
    )]);
    let section = traits_section(&TraitsInput {
        provider: "claudeAgent",
        models: &models,
        model: Some(MODEL),
        prompt: "Ultrathink:\nfix it",
        model_options: &[],
        allow_prompt_injected_effort: true,
        plan_mode_enabled: true,
    });
    assert!(section.ultrathink_prompt_controlled);
    assert!(!section.ultrathink_in_body_text);
    assert_eq!(section.effort.as_deref(), Some("ultrathink"));
    let TraitsSelectOutcome::OptionsChange { prompt, .. } =
        handle_traits_select_change(&section, "effort", "medium", "Ultrathink:\nfix it")
    else {
        panic!("options change");
    };
    assert_eq!(prompt.as_deref(), Some("fix it"));
    // "ultrathink" in the body itself locks the option.
    let locked = traits_section(&TraitsInput { prompt: "Ultrathink:\nplease ultrathink about it", ..TraitsInput {
        provider: "claudeAgent",
        models: &models,
        model: Some(MODEL),
        prompt: "",
        model_options: &[],
        allow_prompt_injected_effort: true,
        plan_mode_enabled: true,
    }});
    assert!(locked.ultrathink_in_body_text);
    assert_eq!(
        handle_traits_select_change(&locked, "effort", "medium", "Ultrathink:\nplease ultrathink about it"),
        TraitsSelectOutcome::None
    );
}

#[test]
fn traits_hide_for_models_without_descriptors() {
    let models = model_with(vec![]);
    assert!(!should_render_traits_controls(&TraitsInput {
        provider: "codex",
        models: &models,
        model: Some(MODEL),
        prompt: "",
        model_options: &[],
        allow_prompt_injected_effort: true,
        plan_mode_enabled: true,
    }));
}

#[test]
fn resolves_models_by_slug_name_alias_and_provider_alias() {
    let models = vec![
        ProviderModel { aliases: vec!["Old-Name".into()], ..ProviderModel::simple("gpt-5.4", "GPT 5.4") },
        ProviderModel::simple("other", "Other Model"),
    ];
    assert_eq!(resolve_selectable_model("codex", Some("gpt-5.4"), &models).as_deref(), Some("gpt-5.4"));
    assert_eq!(resolve_selectable_model("codex", Some("other model"), &models).as_deref(), Some("other"));
    assert_eq!(resolve_selectable_model("codex", Some("old-name"), &models).as_deref(), Some("gpt-5.4"));
    assert_eq!(resolve_selectable_model("codex", Some("5.4"), &models).as_deref(), Some("gpt-5.4"));
    assert_eq!(resolve_selectable_model("codex", Some("nope"), &models), None);
    assert_eq!(resolve_selectable_model("codex", None, &models), None);
}
