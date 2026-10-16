//! Upstream has no test file for DraftHeroHeadline; these lock the rules the
//! component states in code: the picker items, the filter, the heading text
//! and which variant shows.

use super::*;

fn entry(key: &str, name: &str) -> DraftHeroProject {
    DraftHeroProject {
        project_key: key.to_owned(),
        display_name: name.to_owned(),
        project: None,
        environment_badge: None,
    }
}

fn model() -> DraftHeroModel {
    DraftHeroModel {
        menu_entries: vec![entry("t3", "t3code"), entry("web", "Marketing site")],
        can_choose_project: true,
        active_project_key: "t3".to_owned(),
        active_group_name: Some("t3code".to_owned()),
        active_project_title: Some("t3code".to_owned()),
        scratch_available: true,
        ..Default::default()
    }
}

#[test]
fn the_add_project_row_trails_the_projects() {
    let values: Vec<String> = model()
        .picker_items()
        .into_iter()
        .map(|item| item.value)
        .collect();
    assert_eq!(values, ["t3", "web", ADD_PROJECT_VALUE]);
}

#[test]
fn filtering_drops_the_add_project_action() {
    let model = model();
    let labels = |query: &str| -> Vec<String> {
        model
            .filtered_picker_items(query)
            .into_iter()
            .map(|item| item.label)
            .collect()
    };
    assert_eq!(labels(""), ["t3code", "Marketing site", "Add project"]);
    assert_eq!(labels("   "), ["t3code", "Marketing site", "Add project"]);
    assert_eq!(labels("MARKET"), ["Marketing site"]);
    // "Add project" matches the text but is an action, not a project.
    assert!(labels("add").is_empty());
    assert!(labels("zzz").is_empty());
}

#[test]
fn the_selected_item_is_the_active_group() {
    assert_eq!(
        model().selected_picker_item().map(|item| item.value),
        Some("t3".to_owned())
    );
    let mut none = model();
    none.active_project_key = String::new();
    assert_eq!(none.selected_picker_item(), None);
}

#[test]
fn the_heading_is_a_full_sentence() {
    let mut model = model();
    assert_eq!(model.heading(), HeroHeading::BuildIn);
    assert_eq!(model.heading_label(), "What should we build in t3code?");

    model.is_scratch_draft = true;
    assert_eq!(model.heading(), HeroHeading::Scratch);
    assert_eq!(model.heading_label(), "What should we work on?");

    model.is_scratch_draft = false;
    model.active_project_title = None;
    model.active_group_name = None;
    assert_eq!(model.heading(), HeroHeading::ChooseToStart);
    assert_eq!(model.heading_label(), "Choose a project to start");

    model.can_choose_project = false;
    assert_eq!(model.heading(), HeroHeading::AddProject);
    assert_eq!(model.heading_label(), "Add a project to start");
}

#[test]
fn the_group_name_wins_over_the_project_title() {
    let mut model = model();
    model.active_group_name = Some("Group".to_owned());
    model.active_project_title = Some("Title".to_owned());
    assert_eq!(model.active_project_display_name(), Some("Group"));
    model.active_group_name = None;
    assert_eq!(model.active_project_display_name(), Some("Title"));
}

#[test]
fn the_trigger_reads_no_project_for_a_scratch_draft() {
    let mut model = model();
    assert_eq!(model.trigger_text(), "t3code");
    assert_eq!(model.trigger_tooltip().as_deref(), Some("t3code"));
    model.is_scratch_draft = true;
    assert_eq!(model.trigger_text(), "No project");
    assert_eq!(model.trigger_tooltip(), None);
    model.is_scratch_draft = false;
    model.active_group_name = None;
    model.active_project_title = None;
    assert_eq!(model.trigger_text(), "Choose a project");
    assert_eq!(model.trigger_tooltip(), None);
}

#[test]
fn the_way_out_of_a_project_needs_somewhere_to_land() {
    let mut model = model();
    assert!(model.shows_start_without_project());
    model.scratch_available = false;
    assert!(!model.shows_start_without_project());
    model.scratch_available = true;
    model.is_scratch_draft = true;
    assert!(!model.shows_start_without_project());
    model.is_scratch_draft = false;
    model.can_choose_project = false;
    model.active_project_title = None;
    assert!(!model.shows_start_without_project());
}
