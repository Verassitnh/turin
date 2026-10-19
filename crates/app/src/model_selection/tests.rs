//! Port of upstream/apps/web/src/modelSelection.test.ts.
//!
//! Not ported here: the cases that run `deriveEffectiveComposerModelState`
//! (composerDraftStore.ts), which has no Rust counterpart yet. The half of those
//! cases that belongs to this module (which options an instance offers and which
//! model a selection resolves to) is covered by the cases below.

use serde_json::{Value, json};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::model::ANTIGRAVITY_DEFAULT_MODEL;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceId,
};
use turin_contracts::settings::{ProviderModelPreferencesEntry, UnifiedSettings};
use turin_provider_core::server_provider::ServerProvider;

use super::*;

struct ProviderSpec<'a> {
    driver: Option<&'a str>,
    instance_id: &'a str,
    models: &'a [&'a str],
    supports_text_generation: Option<bool>,
}

fn spec<'a>(instance_id: &'a str, models: &'a [&'a str]) -> ProviderSpec<'a> {
    ProviderSpec {
        driver: None,
        instance_id,
        models,
        supports_text_generation: None,
    }
}

fn provider_json(input: &ProviderSpec<'_>) -> Value {
    let driver = input
        .driver
        .unwrap_or(if input.instance_id.starts_with("claude_") {
            "claudeAgent"
        } else {
            "codex"
        });
    let mut value = json!({
        "instanceId": input.instance_id,
        "driver": driver,
        "enabled": true,
        "installed": true,
        "version": null,
        "status": "ready",
        "auth": {"status": "authenticated"},
        "checkedAt": "2026-01-01T00:00:00.000Z",
        "models": input.models.iter().map(|slug| json!({
            "slug": slug, "name": slug, "isCustom": false, "capabilities": {},
        })).collect::<Vec<_>>(),
        "slashCommands": [],
        "skills": [],
    });
    if let Some(supports) = input.supports_text_generation {
        value["supportsTextGeneration"] = json!(supports);
    }
    value
}

fn provider(input: ProviderSpec<'_>) -> ServerProvider {
    serde_json::from_value(provider_json(&input)).expect("server provider")
}

fn with_driver<'a>(
    driver: &'a str,
    instance_id: &'a str,
    models: &'a [&'a str],
) -> ProviderSpec<'a> {
    ProviderSpec {
        driver: Some(driver),
        ..spec(instance_id, models)
    }
}

fn instance_config(driver: &str, custom_models: Value) -> ProviderInstanceConfig {
    ProviderInstanceConfig {
        driver: ProviderDriverKind::new(driver).expect("driver"),
        display_name: None,
        accent_color: None,
        environment: None,
        enabled: None,
        config: Some(json!({ "customModels": custom_models })),
    }
}

fn instance_id(id: &str) -> ProviderInstanceId {
    ProviderInstanceId::new(id).expect("instance id")
}

fn settings_with_provider_instances() -> UnifiedSettings {
    let mut settings = UnifiedSettings::default();
    settings.server.provider_instances.insert(
        instance_id("claudeAgent"),
        instance_config("claudeAgent", json!([])),
    );
    settings.server.provider_instances.insert(
        instance_id("claude_openrouter"),
        instance_config("claudeAgent", json!(["openai/gpt-5.5"])),
    );
    settings
}

fn preferences(settings: &mut UnifiedSettings, id: &str, hidden: &[&str], order: &[&str]) {
    settings
        .server
        .provider_model_preferences
        .get_or_insert_default()
        .insert(
            instance_id(id),
            ProviderModelPreferencesEntry {
                hidden_models: hidden.iter().map(|slug| (*slug).to_owned()).collect(),
                model_order: order.iter().map(|slug| (*slug).to_owned()).collect(),
            },
        );
}

fn entries(providers: &[ServerProvider]) -> Vec<ProviderInstanceEntry> {
    provider_entries(providers)
}

fn entry_of(providers: &[ServerProvider], id: &str) -> ProviderInstanceEntry {
    entries(providers)
        .into_iter()
        .find(|entry| entry.instance_id == id)
        .expect("entry")
}

fn slugs(options: &[AppModelOption]) -> Vec<&str> {
    options.iter().map(|option| option.slug.as_str()).collect()
}

fn resolve(
    id: &str,
    settings: &UnifiedSettings,
    providers: &[ServerProvider],
    selected: &str,
    preserve_unavailable_selection: bool,
) -> Option<String> {
    resolve_app_model_selection_for_instance(
        id,
        settings,
        providers,
        Some(selected),
        ModelResolutionOptions {
            preserve_unavailable_selection,
        },
    )
}

fn model_selection(id: &str, model: &str) -> ModelSelection {
    ModelSelection {
        instance_id: instance_id(id),
        model: TrimmedNonEmptyString::new(model).expect("model"),
        options: None,
    }
}

#[test]
fn preserves_server_provided_legacy_model_metadata() {
    let mut value = provider_json(&spec("claudeAgent", &["claude-opus-4-8"]));
    value["models"][0]["isLegacy"] = json!(true);
    let providers = vec![serde_json::from_value::<ServerProvider>(value).expect("provider")];
    let stock = &entries(&providers)[0];

    assert!(
        get_app_model_options_for_instance(&settings_with_provider_instances(), stock, None)[0]
            .is_legacy
    );
}

#[test]
fn keeps_custom_models_on_the_provider_instance_that_declared_them() {
    let providers = vec![
        provider(spec("claudeAgent", &["claude-sonnet-4-6"])),
        provider(spec("claude_openrouter", &["claude-sonnet-4-6"])),
    ];
    let stock = entry_of(&providers, "claudeAgent");
    let openrouter = entry_of(&providers, "claude_openrouter");
    let settings = settings_with_provider_instances();

    assert!(
        !slugs(&get_app_model_options_for_instance(&settings, &stock, None))
            .contains(&"openai/gpt-5.5")
    );
    assert!(
        slugs(&get_app_model_options_for_instance(
            &settings,
            &openrouter,
            None
        ))
        .contains(&"openai/gpt-5.5")
    );
}

#[test]
fn resolves_a_custom_slug_against_the_selected_custom_instance() {
    let providers = vec![
        provider(with_driver("claudeAgent", "claudeAgent", &[])),
        provider(with_driver("claudeAgent", "claude_openrouter", &[])),
    ];

    assert_eq!(
        resolve(
            "claude_openrouter",
            &settings_with_provider_instances(),
            &providers,
            "openai/gpt-5.5",
            false
        ),
        Some("openai/gpt-5.5".to_owned())
    );
}

#[test]
fn preserves_a_custom_slug_that_collides_with_a_provider_alias() {
    let providers = vec![provider(with_driver(
        "claudeAgent",
        "claude_openrouter",
        &["claude-opus-4-8"],
    ))];
    let mut settings = settings_with_provider_instances();
    settings.server.provider_instances.insert(
        instance_id("claude_openrouter"),
        instance_config("claudeAgent", json!(["opus"])),
    );
    let openrouter = &entries(&providers)[0];

    assert_eq!(
        slugs(&get_app_model_options_for_instance(
            &settings, openrouter, None
        )),
        vec!["claude-opus-4-8", "opus"]
    );
    assert_eq!(
        resolve("claude_openrouter", &settings, &providers, "opus", false),
        Some("opus".to_owned())
    );
}

#[test]
fn includes_grok_custom_models_from_the_selected_provider_instance() {
    let providers = vec![provider(with_driver("grok", "grok", &[]))];
    let mut settings = settings_with_provider_instances();
    settings.server.provider_instances.insert(
        instance_id("grok"),
        instance_config("grok", json!(["grok-test-custom-model"])),
    );
    let grok = entry_of(&providers, "grok");

    assert!(
        slugs(&get_app_model_options_for_instance(&settings, &grok, None))
            .contains(&"grok-test-custom-model")
    );
}

#[test]
fn does_not_inject_an_unknown_selected_slug_into_the_stock_instance_list() {
    let providers = vec![
        provider(spec("claudeAgent", &["claude-sonnet-4-6"])),
        provider(spec("claude_openrouter", &["claude-sonnet-4-6"])),
    ];
    let stock = entry_of(&providers, "claudeAgent");

    assert!(
        !slugs(&get_app_model_options_for_instance(
            &settings_with_provider_instances(),
            &stock,
            None
        ))
        .contains(&"openai/gpt-5.5")
    );
}

#[test]
fn hides_server_models_from_the_instance_option_list() {
    let providers = vec![provider(spec(
        "claudeAgent",
        &["claude-opus-4-6", "claude-sonnet-4-6"],
    ))];
    let mut settings = settings_with_provider_instances();
    preferences(&mut settings, "claudeAgent", &["claude-opus-4-6"], &[]);
    let stock = entry_of(&providers, "claudeAgent");

    assert_eq!(
        slugs(&get_app_model_options_for_instance(&settings, &stock, None)),
        vec!["claude-sonnet-4-6"]
    );
}

#[test]
fn drops_server_reported_custom_rows_that_are_no_longer_in_settings() {
    let mut value = provider_json(&spec("claude_openrouter", &["claude-sonnet-4-6"]));
    value["models"].as_array_mut().expect("models").push(json!({
        "slug": "removed/custom", "name": "removed/custom", "isCustom": true, "capabilities": {},
    }));
    let providers = vec![serde_json::from_value::<ServerProvider>(value).expect("provider")];
    let openrouter = &entries(&providers)[0];

    assert_eq!(
        slugs(&get_app_model_options_for_instance(
            &settings_with_provider_instances(),
            openrouter,
            None
        )),
        vec!["claude-sonnet-4-6", "openai/gpt-5.5"]
    );
}

#[test]
fn applies_persisted_per_instance_model_ordering() {
    let providers = vec![provider(spec(
        "claudeAgent",
        &["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"],
    ))];
    let mut settings = settings_with_provider_instances();
    preferences(
        &mut settings,
        "claudeAgent",
        &[],
        &["claude-haiku-4-5", "claude-opus-4-6"],
    );
    let stock = entry_of(&providers, "claudeAgent");

    assert_eq!(
        slugs(&get_app_model_options_for_instance(&settings, &stock, None)),
        vec!["claude-haiku-4-5", "claude-opus-4-6", "claude-sonnet-4-6"]
    );
}

#[test]
fn falls_back_when_the_selected_model_is_hidden() {
    let providers = vec![provider(spec(
        "claudeAgent",
        &["claude-opus-4-6", "claude-sonnet-4-6"],
    ))];
    let mut settings = settings_with_provider_instances();
    preferences(&mut settings, "claudeAgent", &["claude-opus-4-6"], &[]);

    assert_eq!(
        resolve(
            "claudeAgent",
            &settings,
            &providers,
            "claude-opus-4-6",
            false
        ),
        Some("claude-sonnet-4-6".to_owned())
    );
    assert_eq!(
        resolve(
            "claudeAgent",
            &settings,
            &providers,
            "claude-opus-4-6",
            true
        ),
        Some("claude-sonnet-4-6".to_owned())
    );
}

#[test]
fn falls_back_instead_of_resolving_a_custom_slug_against_the_wrong_instance() {
    let providers = vec![
        provider(spec("claudeAgent", &["claude-sonnet-4-6"])),
        provider(spec("claude_openrouter", &["claude-sonnet-4-6"])),
    ];

    assert_eq!(
        resolve(
            "claudeAgent",
            &settings_with_provider_instances(),
            &providers,
            "openai/gpt-5.5",
            false
        ),
        Some("claude-sonnet-4-6".to_owned())
    );
}

/// `describe.each` over the dynamic-catalogue drivers.
const CATALOG_GAP_DRIVERS: [(&str, &str, &str); 2] = [
    ("opencode", "opencode/big-pickle", "opencode/kimi-k3"),
    ("antigravity", "gemini-3.1-pro", "gemini-3.1-pro-high"),
];

#[test]
fn catalog_gaps_preserve_a_selected_model_when_a_refresh_no_longer_contains_it() {
    for (driver, available, missing) in CATALOG_GAP_DRIVERS {
        let providers = vec![provider(with_driver(driver, driver, &[available]))];
        let settings = settings_with_provider_instances();

        assert_eq!(
            resolve(driver, &settings, &providers, missing, true),
            Some(missing.to_owned()),
            "{driver}"
        );
        assert_eq!(
            resolve(driver, &settings, &providers, missing, false),
            Some(available.to_owned()),
            "{driver}"
        );
    }
}

#[test]
fn catalog_gaps_add_the_selected_missing_model_as_an_unavailable_option() {
    for (driver, available, missing) in CATALOG_GAP_DRIVERS {
        let providers = vec![provider(with_driver(driver, driver, &[available]))];
        let entry = &entries(&providers)[0];
        let options = get_app_model_options_for_instance(
            &settings_with_provider_instances(),
            entry,
            Some(missing),
        );

        assert_eq!(options.len(), 2, "{driver}");
        assert_eq!(options[0].slug, available);
        assert_eq!(options[1].slug, missing);
        assert_eq!(options[1].name, missing);
        assert!(options[1].is_unavailable);
    }
}

#[test]
fn catalog_gaps_keep_a_missing_option_scoped_to_the_selected_instance() {
    for (driver, _, missing) in CATALOG_GAP_DRIVERS {
        let selected_instance_id = format!("{driver}_work");
        let other_instance_id = format!("{driver}_personal");
        let providers = vec![
            provider(with_driver(driver, &selected_instance_id, &[])),
            provider(with_driver(driver, &other_instance_id, &[])),
        ];
        let options = get_custom_model_options_by_instance(
            &settings_with_provider_instances(),
            &providers,
            Some(&selected_instance_id),
            Some(missing),
        );

        let selected = &options[&selected_instance_id];
        assert_eq!(selected.len(), 1, "{driver}");
        assert_eq!(selected[0].slug, missing);
        assert!(selected[0].is_unavailable);
        assert!(options[&other_instance_id].is_empty(), "{driver}");
    }
}

#[test]
fn catalog_gaps_replace_the_unavailable_marker_with_catalog_metadata_after_recovery() {
    for (driver, available, missing) in CATALOG_GAP_DRIVERS {
        let pending = vec![provider(with_driver(driver, driver, &[available]))];
        let recovered = vec![provider(with_driver(driver, driver, &[available, missing]))];
        let settings = settings_with_provider_instances();

        let before =
            get_app_model_options_for_instance(&settings, &entries(&pending)[0], Some(missing));
        assert_eq!(
            before
                .iter()
                .find(|option| option.slug == missing)
                .map(|option| option.is_unavailable),
            Some(true),
            "{driver}"
        );
        let after =
            get_app_model_options_for_instance(&settings, &entries(&recovered)[0], Some(missing));
        assert_eq!(
            after
                .iter()
                .find(|option| option.slug == missing)
                .map(|option| option.is_unavailable),
            Some(false),
            "{driver}"
        );
        assert_eq!(
            resolve(driver, &settings, &recovered, missing, true),
            Some(missing.to_owned())
        );
    }
}

#[test]
fn catalog_gaps_do_not_resurrect_a_hidden_model_when_the_raw_catalog_omits_it() {
    for (driver, _, missing) in CATALOG_GAP_DRIVERS {
        let mut settings = settings_with_provider_instances();
        preferences(&mut settings, driver, &[missing], &[]);
        let providers = vec![provider(with_driver(driver, driver, &[]))];
        let entry = &entries(&providers)[0];

        assert_eq!(
            get_app_model_options_for_instance(&settings, entry, Some(missing)),
            Vec::new(),
            "{driver}"
        );
        assert_eq!(
            resolve(driver, &settings, &providers, missing, true),
            None,
            "{driver}"
        );
    }
}

#[test]
fn does_not_add_unavailable_options_for_other_providers() {
    let providers = vec![provider(with_driver("codex", "codex", &["gpt-5.6-sol"]))];
    let entry = &entries(&providers)[0];
    let settings = settings_with_provider_instances();

    assert_eq!(
        slugs(&get_app_model_options_for_instance(
            &settings,
            entry,
            Some("gpt-missing")
        )),
        vec!["gpt-5.6-sol"]
    );
    assert_eq!(
        resolve("codex", &settings, &providers, "gpt-missing", true),
        Some("gpt-5.6-sol".to_owned())
    );
}

#[test]
fn offers_only_account_catalog_models_for_antigravity_despite_custom_model_settings() {
    let mut settings = UnifiedSettings::default();
    settings.server.provider_instances.insert(
        instance_id("antigravity"),
        instance_config("antigravity", json!(["api-only-model"])),
    );
    settings.server.provider_instances.insert(
        instance_id("antigravity_work"),
        instance_config("antigravity", json!(["unknown-model"])),
    );
    let native_model = "gemini-3.1-pro";
    let providers = vec![
        provider(with_driver("antigravity", "antigravity", &[native_model])),
        provider(with_driver(
            "antigravity",
            "antigravity_work",
            &[native_model],
        )),
    ];

    for entry in entries(&providers) {
        assert_eq!(
            slugs(&get_app_model_options_for_instance(&settings, &entry, None)),
            vec![native_model],
            "{}",
            entry.instance_id
        );
    }
}

#[test]
fn resolves_the_antigravity_default_marker_without_creating_an_unavailable_model() {
    let native_model = "gemini-3.1-pro";
    let mut value = provider_json(&with_driver(
        "antigravity",
        "antigravity_work",
        &[native_model],
    ));
    value["models"][0]["isDefault"] = json!(true);
    value["models"][0]["aliases"] = json!([ANTIGRAVITY_DEFAULT_MODEL]);
    let live_provider = serde_json::from_value::<ServerProvider>(value).expect("provider");
    let settings = settings_with_provider_instances();
    let live = vec![live_provider];

    assert_eq!(
        slugs(&get_app_model_options_for_instance(
            &settings,
            &entries(&live)[0],
            Some(ANTIGRAVITY_DEFAULT_MODEL)
        )),
        vec![native_model]
    );
    assert_eq!(
        resolve(
            "antigravity_work",
            &settings,
            &live,
            ANTIGRAVITY_DEFAULT_MODEL,
            true
        ),
        Some(native_model.to_owned())
    );

    let mut hidden_settings = settings.clone();
    preferences(
        &mut hidden_settings,
        "antigravity_work",
        &[native_model],
        &[],
    );
    assert_eq!(
        resolve(
            "antigravity_work",
            &hidden_settings,
            &live,
            ANTIGRAVITY_DEFAULT_MODEL,
            true
        ),
        None
    );

    let empty = vec![provider(with_driver(
        "antigravity",
        "antigravity_work",
        &[],
    ))];
    assert_eq!(
        get_app_model_options_for_instance(
            &settings,
            &entries(&empty)[0],
            Some(ANTIGRAVITY_DEFAULT_MODEL)
        ),
        Vec::new()
    );
}

#[test]
fn preserves_custom_provider_instances_in_settings_model_selection() {
    let providers = vec![
        provider(spec("claudeAgent", &["claude-sonnet-4-6"])),
        provider(spec("claude_openrouter", &["claude-sonnet-4-6"])),
    ];
    let mut settings = settings_with_provider_instances();
    settings.server.text_generation_model_selection =
        model_selection("claude_openrouter", "openai/gpt-5.5");

    assert_eq!(
        resolve_app_model_selection_state(&settings, &providers),
        AppModelSelection::create("claude_openrouter", "openai/gpt-5.5", None)
    );
}

#[test]
fn self_heals_a_persisted_selection_pointing_at_a_text_generation_incapable_instance() {
    let providers = vec![
        provider(ProviderSpec {
            driver: Some("acpRegistry"),
            supports_text_generation: Some(false),
            ..spec("acp_gemini", &["default"])
        }),
        provider(spec("codex", &["gpt-5.6-luna"])),
    ];
    let mut settings = settings_with_provider_instances();
    settings.server.text_generation_model_selection = model_selection("acp_gemini", "default");

    assert_eq!(
        resolve_app_model_selection_state(&settings, &providers).instance_id,
        "codex"
    );
}

#[test]
fn does_not_select_a_provider_that_cannot_generate_system_text() {
    let unsupported = provider(ProviderSpec {
        driver: Some("antigravity"),
        supports_text_generation: Some(false),
        ..spec("antigravity", &["gemini-3.1-pro"])
    });
    let supported = provider(spec("codex", &["gpt-5.6-sol"]));
    let mut settings = settings_with_provider_instances();
    settings.server.text_generation_model_selection =
        model_selection("antigravity", "gemini-3.1-pro");

    assert_eq!(
        resolve_app_model_selection_state(&settings, &[unsupported.clone(), supported]),
        AppModelSelection::create("codex", "gpt-5.6-sol", None)
    );
    assert_eq!(
        resolve_app_model_selection_state(&settings, &[unsupported]),
        no_provider_model_selection()
    );
}

#[test]
fn resolve_app_model_selection_falls_back_to_the_server_default() {
    let providers = entries(&[provider(with_driver(
        "claudeAgent",
        "claudeAgent",
        &["claude-sonnet-4-6"],
    ))]);
    let settings = settings_with_provider_instances();

    assert_eq!(
        resolve_app_model_selection("claudeAgent", &settings, &providers, Some("missing")),
        "claude-sonnet-4-6"
    );
    assert_eq!(
        resolve_app_model_selection("claudeAgent", &settings, &providers, Some("sonnet")),
        "claude-sonnet-4-6"
    );
}

#[test]
fn normalizes_custom_model_entries_by_length_duplicates_built_ins_and_count() {
    let definition = |slug: &str| CustomModelDefinition {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        capabilities: None,
    };
    let long = "x".repeat(MAX_CUSTOM_MODEL_LENGTH + 1);
    let exactly_max = "y".repeat(MAX_CUSTOM_MODEL_LENGTH);
    let built_in = HashSet::from(["built-in"]);
    let models = vec![
        definition(&long),
        definition("built-in"),
        definition("a"),
        definition("a"),
        definition(&exactly_max),
    ];

    let normalized = normalize_custom_model_entries(&models, &built_in);
    assert_eq!(
        normalized
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["a", exactly_max.as_str()]
    );

    let many: Vec<_> = (0..40).map(|n| definition(&format!("m{n}"))).collect();
    assert_eq!(
        normalize_custom_model_entries(&many, &HashSet::new()).len(),
        32
    );
}

mod opencode_plan_agent {
    use super::*;

    fn opencode() -> ServerProvider {
        let model = "opencode/gpt-5.4";
        let mut value = provider_json(&with_driver("opencode", "opencode", &[]));
        value["models"] = json!([{
            "slug": model, "name": model, "isCustom": false,
            "capabilities": {"optionDescriptors": [{
                "id": "agent", "label": "Agent", "type": "select",
                "options": [
                    {"id": "build", "label": "Build", "isDefault": true},
                    {"id": "plan", "label": "Plan"},
                ],
                "currentValue": "build",
            }]},
        }]);
        serde_json::from_value(value).expect("provider")
    }

    #[test]
    fn keeps_a_stored_plan_agent_this_device_did_not_pick() {
        let mut settings = UnifiedSettings::default();
        settings.client.plan_mode_enabled = false;
        settings.server.text_generation_model_selection = ModelSelection {
            options: Some(vec![
                serde_json::from_value(json!({"id": "agent", "value": "plan"}))
                    .expect("option selection"),
            ]),
            ..model_selection("opencode", "opencode/gpt-5.4")
        };

        assert_eq!(
            resolve_app_model_selection_state(&settings, &[opencode()]),
            AppModelSelection {
                instance_id: "opencode".to_owned(),
                model: "opencode/gpt-5.4".to_owned(),
                options: vec![OptionSelection::string("agent", "plan")],
            }
        );
    }

    #[test]
    fn selects_plan_agent_reads_the_agent_option_only() {
        assert!(selects_plan_agent(Some(&[OptionSelection::string(
            "agent", "plan"
        )])));
        assert!(!selects_plan_agent(Some(&[OptionSelection::string(
            "agent", "build"
        )])));
        assert!(!selects_plan_agent(Some(&[OptionSelection::string(
            "variant", "plan"
        )])));
        assert!(!selects_plan_agent(None));
    }
}

#[test]
fn applies_app_options_to_an_entry_and_keeps_each_models_capabilities() {
    let mut value = provider_json(&spec("claude_openrouter", &["a", "b"]));
    value["models"][0]["capabilities"] = json!({"optionDescriptors": [{
        "id": "effort", "label": "Effort", "type": "select",
        "options": [{"id": "low", "label": "Low"}],
    }]});
    let providers = vec![serde_json::from_value::<ServerProvider>(value).expect("provider")];
    let mut settings = settings_with_provider_instances();
    preferences(&mut settings, "claude_openrouter", &["b"], &[]);
    let mut entry = entries(&providers).remove(0);

    apply_app_model_options(&settings, &mut entry, None);

    // `b` is hidden, the settings' custom model joins, `a` keeps its descriptors.
    assert_eq!(
        entry
            .models
            .iter()
            .map(|model| (model.slug.as_str(), model.is_custom))
            .collect::<Vec<_>>(),
        vec![("a", false), ("openai/gpt-5.5", true)]
    );
    assert_eq!(
        entry.models[0]
            .capabilities
            .as_ref()
            .map(|caps| caps.option_descriptors.len()),
        Some(1)
    );
    assert!(entry.models[1].capabilities.is_none());
}
