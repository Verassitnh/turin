//! Ports of ModelPickerContent.test.ts plus list-building cases from the
//! component's memo chain.

use super::*;
use crate::composer::catalog::{AuthStatus, ProviderStatus};

fn entry(status: ProviderStatus, driver: &str) -> ProviderInstanceEntry {
    let mut entry = ProviderInstanceEntry::new(&format!("{driver}_work"), driver, driver);
    entry.status = status;
    entry
}

fn model(slug: &str, name: &str) -> ProviderModel {
    ProviderModel::simple(slug, name)
}

fn unavailable(slug: &str) -> ProviderModel {
    ProviderModel { is_unavailable: true, ..model(slug, slug) }
}

#[test]
fn never_offers_the_internal_antigravity_default_marker_as_a_model() {
    for status in [ProviderStatus::Ready, ProviderStatus::Error] {
        let provider = entry(status, "antigravity");
        let option = unavailable(ANTIGRAVITY_DEFAULT_MODEL);
        assert!(!should_include_model_picker_option(&provider, &option, &provider.instance_id, ANTIGRAVITY_DEFAULT_MODEL));
    }
}

#[test]
fn keeps_only_the_active_synthetic_row_when_the_provider_is_not_ready() {
    for (driver, status) in [
        ("opencode", ProviderStatus::Error),
        ("opencode", ProviderStatus::Warning),
        ("antigravity", ProviderStatus::Error),
        ("antigravity", ProviderStatus::Warning),
    ] {
        let provider = entry(status, driver);
        let active = "missing-model";
        let id = provider.instance_id.clone();
        assert!(should_include_model_picker_option(&provider, &unavailable(active), &id, active));
        assert!(!should_include_model_picker_option(&provider, &model("stale/model", "Stale model"), &id, active));
        assert!(!should_include_model_picker_option(&provider, &unavailable("other/missing"), &id, active));
        assert!(!should_include_model_picker_option(&provider, &unavailable(active), &format!("{driver}_personal"), active));
    }
}

#[test]
fn follows_the_catalog_default_for_the_marker_but_keeps_an_explicit_native_model() {
    let previous = vec![
        ProviderModel { aliases: vec![ANTIGRAVITY_DEFAULT_MODEL.into()], ..model("gemini-fast", "Gemini Fast") },
        model("gemini-pro", "Gemini Pro"),
    ];
    let next = vec![
        model("gemini-fast", "Gemini Fast"),
        ProviderModel { aliases: vec![ANTIGRAVITY_DEFAULT_MODEL.into()], ..model("gemini-pro", "Gemini Pro") },
    ];
    let pick = |model_name: &str, options: &[ProviderModel]| {
        resolve_model_picker_selected_model(Some("antigravity"), model_name, options).map(|m| m.slug.clone())
    };
    assert_eq!(pick(ANTIGRAVITY_DEFAULT_MODEL, &previous).as_deref(), Some("gemini-fast"));
    assert_eq!(pick(ANTIGRAVITY_DEFAULT_MODEL, &next).as_deref(), Some("gemini-pro"));
    assert_eq!(pick("gemini-fast", &next).as_deref(), Some("gemini-fast"));
    // Does not guess the default from the first model in a catalog.
    assert_eq!(pick(ANTIGRAVITY_DEFAULT_MODEL, &[model("gemini-fast", "Gemini Fast")]), None);
}

#[test]
fn offers_setup_per_driver_capability() {
    let available = model("gemini-3.1-pro", "Gemini 3.1 Pro");
    assert!(should_offer_model_picker_setup(&entry(ProviderStatus::Error, "antigravity"), &[]));
    let mut signed_out = entry(ProviderStatus::Ready, "antigravity");
    signed_out.auth_status = AuthStatus::Unauthenticated;
    assert!(should_offer_model_picker_setup(&signed_out, std::slice::from_ref(&available)));
    assert!(should_offer_model_picker_setup(
        &entry(ProviderStatus::Ready, "antigravity"),
        &[ProviderModel { is_unavailable: true, ..available.clone() }]
    ));
    assert!(!should_offer_model_picker_setup(&entry(ProviderStatus::Ready, "antigravity"), &[available]));
    let mut disabled = entry(ProviderStatus::Error, "antigravity");
    disabled.enabled = false;
    assert!(!should_offer_model_picker_setup(&disabled, &[]));
    assert!(!should_offer_model_picker_setup(&entry(ProviderStatus::Error, "codex"), &[]));
    let mut custom = entry(ProviderStatus::Error, "custom_driver");
    custom.can_setup = true;
    assert!(should_offer_model_picker_setup(&custom, &[]));
}

#[test]
fn adjacent_provider_wraps_through_favorites_and_ready_instances() {
    let codex = entry(ProviderStatus::Ready, "codex");
    let claude = entry(ProviderStatus::Ready, "claudeAgent");
    let broken = entry(ProviderStatus::Error, "opencode");
    let entries = vec![codex.clone(), broken.clone(), claude.clone()];
    let instance = |entry: &ProviderInstanceEntry| PickerSelection::Instance(entry.instance_id.clone());
    let step = |selected: PickerSelection, direction, disabled: Option<&HashSet<String>>, unavailable: Option<&HashSet<String>>| {
        adjacent_model_picker_provider(&entries, &selected, direction, disabled, unavailable)
    };
    assert_eq!(step(instance(&codex), 1, None, None), instance(&claude));
    assert_eq!(step(PickerSelection::Favorites, -1, None, None), instance(&claude));
    assert_eq!(step(instance(&claude), 1, None, None), PickerSelection::Favorites);
    let disabled = HashSet::from([claude.instance_id.clone()]);
    assert_eq!(step(instance(&codex), 1, Some(&disabled), None), PickerSelection::Favorites);
    let unavailable = HashSet::from([broken.instance_id.clone()]);
    assert_eq!(step(instance(&codex), 1, None, Some(&unavailable)), instance(&broken));
    assert_eq!(
        adjacent_model_picker_provider(&[], &instance(&codex), -1, None, None),
        PickerSelection::Favorites
    );
    assert_eq!(step(instance(&broken), 1, None, None), PickerSelection::Favorites);
    assert_eq!(step(instance(&broken), -1, None, None), instance(&claude));
}

fn catalog() -> Vec<ProviderInstanceEntry> {
    let mut codex = ProviderInstanceEntry::new("codex", "codex", "Codex");
    codex.models = vec![model("gpt-5.4", "GPT 5.4"), model("gpt-5.3", "GPT 5.3"), ProviderModel { is_legacy: true, ..model("gpt-4", "GPT 4") }];
    let mut claude = ProviderInstanceEntry::new("claudeAgent", "claudeAgent", "Claude");
    claude.models = vec![model("opus", "Claude Opus"), model("sonnet", "Claude Sonnet")];
    vec![codex, claude]
}

fn input<'a>(
    entries: &'a [ProviderInstanceEntry],
    favorites: &'a HashSet<String>,
    selected: &'a PickerSelection,
    expanded: &'a HashSet<String>,
    query: &'a str,
) -> ModelPickerInput<'a> {
    ModelPickerInput {
        instance_entries: entries,
        active_instance_id: "codex",
        model: "gpt-5.4",
        locked_provider: None,
        locked_continuation_group_key: None,
        favorites,
        search_query: query,
        selected,
        expanded_legacy_instances: expanded,
        on_open_provider_setup: false,
    }
}

fn slugs(list: &ModelPickerList) -> Vec<String> {
    list.rows
        .iter()
        .map(|row| match row {
            PickerRow::Model(item) => item.model.slug.clone(),
            PickerRow::LegacySection { count, expanded, .. } => format!("legacy:{count}:{expanded}"),
        })
        .collect()
}

#[test]
fn lists_one_instance_with_a_collapsed_legacy_section() {
    let entries = catalog();
    let favorites = HashSet::new();
    let expanded = HashSet::new();
    let selected = PickerSelection::Instance("codex".into());
    let list = compute_model_picker_list(&input(&entries, &favorites, &selected, &expanded, ""));
    assert_eq!(slugs(&list), ["gpt-5.4", "gpt-5.3", "legacy:1:false"]);
    assert!(list.show_sidebar);
    assert_eq!(list.active_model_key, Some(model_picker_model_key("codex", "gpt-5.4")));
    let expanded = HashSet::from(["codex".to_owned()]);
    let list = compute_model_picker_list(&input(&entries, &favorites, &selected, &expanded, ""));
    assert_eq!(slugs(&list), ["gpt-5.4", "gpt-5.3", "legacy:1:true", "gpt-4"]);
}

#[test]
fn favorites_group_first_within_an_instance_and_form_their_own_list() {
    let entries = catalog();
    let favorites = HashSet::from([provider_model_key("codex", "gpt-5.3"), provider_model_key("claudeAgent", "sonnet")]);
    let expanded = HashSet::new();
    let selected = PickerSelection::Instance("codex".into());
    let list = compute_model_picker_list(&input(&entries, &favorites, &selected, &expanded, ""));
    assert_eq!(slugs(&list)[..2], ["gpt-5.3", "gpt-5.4"]);
    let favorites_view = PickerSelection::Favorites;
    let list = compute_model_picker_list(&input(&entries, &favorites, &favorites_view, &expanded, ""));
    assert_eq!(slugs(&list), ["gpt-5.3", "sonnet"]);
}

#[test]
fn searching_ranks_across_instances_and_hides_the_sidebar() {
    let entries = catalog();
    let favorites = HashSet::new();
    let expanded = HashSet::new();
    let selected = PickerSelection::Instance("codex".into());
    let list = compute_model_picker_list(&input(&entries, &favorites, &selected, &expanded, "opus"));
    assert_eq!(slugs(&list), ["opus"]);
    assert!(list.is_searching && !list.show_sidebar);
    let list = compute_model_picker_list(&input(&entries, &favorites, &selected, &expanded, "zzzz"));
    assert!(list.rows.is_empty());
}

#[test]
fn a_locked_provider_limits_results_and_disables_other_rail_entries() {
    let entries = catalog();
    let favorites = HashSet::new();
    let expanded = HashSet::new();
    let selected = PickerSelection::Instance("codex".into());
    let mut locked = input(&entries, &favorites, &selected, &expanded, "");
    locked.locked_provider = Some("codex");
    let list = compute_model_picker_list(&locked);
    assert_eq!(list.locked_disabled_instance_ids, Some(HashSet::from(["claudeAgent".to_owned()])));
    assert_eq!(list.sidebar_entries[0].instance_id, "codex");
    let mut locked_search = input(&entries, &favorites, &selected, &expanded, "claude");
    locked_search.locked_provider = Some("codex");
    assert!(compute_model_picker_list(&locked_search).rows.is_empty());
}

#[test]
fn initial_selection_prefers_favorites_unless_locked_or_empty() {
    let entries = catalog();
    let none = HashSet::new();
    let favorites = HashSet::from([provider_model_key("codex", "gpt-5.3")]);
    let expanded = HashSet::new();
    let selected = PickerSelection::Favorites;
    assert_eq!(
        initial_picker_selection(&input(&entries, &none, &selected, &expanded, "")),
        PickerSelection::Instance("codex".into())
    );
    assert_eq!(initial_picker_selection(&input(&entries, &favorites, &selected, &expanded, "")), PickerSelection::Favorites);
    let mut locked = input(&entries, &favorites, &selected, &expanded, "");
    locked.locked_provider = Some("codex");
    assert_eq!(initial_picker_selection(&locked), PickerSelection::Instance("codex".into()));
}

#[test]
fn model_picks_resolve_through_the_driver() {
    let entries = catalog();
    assert_eq!(resolve_model_pick(&entries, "codex", "gpt 5.4").as_deref(), Some("gpt-5.4"));
    assert_eq!(resolve_model_pick(&entries, "nope", "x"), None);
}
