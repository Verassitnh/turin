//! Port of upstream/apps/web/src/components/settings/SettingInheritance.tsx
//! and upstream/apps/web/src/components/settings/SettingInheritance.test.ts.
//!
//! A small indicator beside a row's title that opens a top-down view of where
//! the setting's value comes from on each selected target. It sits inline so
//! narrowing to a project does not add a caption line to every row.
//!
//! Not ported (PORTING.md Deviations): the `t3.json` layer
//! (`PROJECT_FILE_BACKED_SETTINGS`, cut with `t3ProjectFile`); the environment
//! machine icon in the section heading (one local environment).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, div, px, rems,
};
use serde_json::Value;
use turin_contracts::settings::default_server_settings;

use super::controls::{PopupAlign, toggle_menu, with_popup};
use super::scope::OverridingProject;
use super::view::SettingsView;
use crate::components::branch_toolbar::branch_toolbar_logic::{
    resolve_env_mode_label, worktree_submodules_label,
};
use crate::pull_request::pull_request_detail_logic::PULL_REQUEST_MERGE_METHOD_LABELS;
use crate::settings_logic::scoped_settings::{ScopedSettingsTarget, is_project_scoped_setting_key};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};
use turin_shared::project_settings::ProjectSettingSource;

/// One step of the resolution chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritanceLayer {
    pub key: InheritanceLayerKey,
    pub label: String,
    pub value: String,
    pub effective: bool,
    pub set: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InheritanceLayerKey {
    Project,
    Environment,
    BuiltIn,
}

/// `SettingInheritanceState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingInheritanceState {
    Default,
    Environment,
    Inherited,
    Overridden,
    Mixed,
}

fn writing_style_label(mode: &str) -> Option<&'static str> {
    match mode {
        "repo_conventions" => Some("Repository conventions"),
        "conventional_commits" => Some("Conventional Commits"),
        "custom" => Some("Custom instructions"),
        _ => None,
    }
}

/// Human labels for the values the chain can show; falls back to a type summary.
pub fn format_value(key: &str, value: &Value) -> String {
    match value {
        Value::Null => match key {
            "pullRequestMergeMethod" => "Last selected",
            "sidebarAutoSettleAfterDays" => "Never",
            "defaultModelSelection" => "Automatic",
            "sourceControlWriterModelSelection" => "Text generation model",
            "reviewGuideModelSelection" => "Source control writer model",
            "defaultThreadEnvMode" | "worktreeSubmodules" => "Inherit",
            _ => "Not set",
        }
        .to_owned(),
        Value::Bool(value) => if *value { "On" } else { "Off" }.to_owned(),
        Value::Number(number) => {
            if key == "sidebarAutoSettleAfterDays" {
                let days = number.as_f64().unwrap_or(0.);
                let count = if days.fract() == 0. {
                    format!("{}", days as i64)
                } else {
                    number.to_string()
                };
                format!("{count} {}", if days == 1. { "day" } else { "days" })
            } else {
                number.to_string()
            }
        }
        Value::String(text) => {
            let labelled = match key {
                "defaultThreadEnvMode" => match text.as_str() {
                    "local" => Some(resolve_env_mode_label(
                        crate::components::branch_toolbar::branch_toolbar_logic::EnvMode::Local,
                    )),
                    "worktree" => Some(resolve_env_mode_label(
                        crate::components::branch_toolbar::branch_toolbar_logic::EnvMode::Worktree,
                    )),
                    _ => None,
                },
                "worktreeSubmodules" => {
                    use turin_contracts::environment::WorktreeSubmodules as W;
                    match text.as_str() {
                        "recursive" => Some(worktree_submodules_label(W::Recursive)),
                        "top-level" => Some(worktree_submodules_label(W::TopLevel)),
                        "none" => Some(worktree_submodules_label(W::None)),
                        _ => None,
                    }
                }
                "pullRequestMergeMethod" => PULL_REQUEST_MERGE_METHOD_LABELS
                    .iter()
                    .find(|(method, _)| {
                        serde_json::to_value(method)
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .as_deref()
                            == Some(text.as_str())
                    })
                    .map(|(_, label)| *label),
                _ => None,
            };
            match labelled {
                Some(label) => label.to_owned(),
                None if text.is_empty() => "Empty".to_owned(),
                None => text.clone(),
            }
        }
        Value::Array(items) => format!(
            "{} {}",
            items.len(),
            if items.len() == 1 { "item" } else { "items" }
        ),
        Value::Object(object) => {
            if let Some(Value::String(model)) = object.get("model") {
                return model.clone();
            }
            if let Some(Value::String(mode)) = object.get("mode") {
                return writing_style_label(mode)
                    .map(str::to_owned)
                    .unwrap_or_else(|| mode.clone());
            }
            "Custom".to_owned()
        }
    }
}

fn field(settings: &impl serde::Serialize, key: &str) -> Value {
    serde_json::to_value(settings)
        .ok()
        .and_then(|value| value.get(key).cloned())
        .unwrap_or(Value::Null)
}

/// The layers a setting resolves through for one target, top-down: the project
/// override when the target is a project, the environment's value, and the
/// built-in default. The first layer that is set wins. Same order as
/// `resolveProjectSettings`.
pub fn setting_inheritance_layers(
    target: &ScopedSettingsTarget,
    environment_settings: &turin_contracts::settings::ServerSettings,
    key: &str,
) -> Vec<InheritanceLayer> {
    let environment_value = field(environment_settings, key);
    let default_value = field(&default_server_settings(), key);
    let source = if is_project_scoped_setting_key(key) {
        target
            .sources
            .get(key)
            .copied()
            .unwrap_or(ProjectSettingSource::Environment)
    } else {
        ProjectSettingSource::Environment
    };
    let environment_set = environment_value != default_value;
    let mut layers = Vec::new();
    if target.project_id.is_some() && is_project_scoped_setting_key(key) {
        let is_project = source == ProjectSettingSource::Project;
        layers.push(InheritanceLayer {
            key: InheritanceLayerKey::Project,
            label: "Project".to_owned(),
            value: if is_project {
                format_value(key, &field(&target.settings, key))
            } else {
                "Inherits".to_owned()
            },
            effective: is_project,
            set: is_project,
        });
    }
    layers.push(InheritanceLayer {
        key: InheritanceLayerKey::Environment,
        label: target.label.clone(),
        value: if environment_set {
            format_value(key, &environment_value)
        } else {
            "Inherits".to_owned()
        },
        effective: source == ProjectSettingSource::Environment && environment_set,
        set: environment_set,
    });
    layers.push(InheritanceLayer {
        key: InheritanceLayerKey::BuiltIn,
        label: "Default".to_owned(),
        value: format_value(key, &default_value),
        effective: source == ProjectSettingSource::Environment && !environment_set,
        set: true,
    });
    layers
}

impl SettingsView {
    /// `<SettingInheritance …/>`: the layers icon and its popover.
    pub(super) fn setting_inheritance(
        &mut self,
        row_id: &str,
        state: SettingInheritanceState,
        summary: &str,
        targets: &[ScopedSettingsTarget],
        environment_settings: &turin_contracts::settings::ServerSettings,
        keys: &[&'static str],
        overriding: Vec<OverridingProject>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let key = *keys.first()?;
        if targets.is_empty() {
            return None;
        }
        let theme = cx.app_theme().clone();
        let override_summary = if overriding.is_empty() {
            summary.to_owned()
        } else {
            format!(
                "{summary} · {} project {}",
                overriding.len(),
                if overriding.len() == 1 {
                    "override"
                } else {
                    "overrides"
                }
            )
        };
        let menu_id = SharedString::from(format!("inheritance-{row_id}"));
        let open = self.menus.is_open(&menu_id);
        let icon_color = match state {
            SettingInheritanceState::Overridden => theme.primary,
            SettingInheritanceState::Mixed => theme.warning,
            _ => theme.muted_foreground,
        };
        let trigger = Button::new(SharedString::from(format!("inheritance-trigger-{row_id}")))
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconMicro)
            .tooltip(Tooltip::new(override_summary))
            .child(
                Icon::new(IconName::Layers)
                    .size(rems(0.75))
                    .color(icon_color),
            )
            .on_click(toggle_menu(cx, |this| &mut this.menus, menu_id.clone()));

        let popup = open.then(|| {
            let mut sections = div().flex().flex_col();
            for (index, target) in targets.iter().enumerate() {
                let layers = setting_inheritance_layers(target, environment_settings, key);
                let mut list = div().flex().flex_col();
                for layer in layers {
                    let name = if layer.key == InheritanceLayerKey::Environment {
                        "Environment".to_owned()
                    } else {
                        layer.label.clone()
                    };
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .rounded(RADIUS_MD)
                            .px_2()
                            .py_1()
                            .text_size(TEXT_SM.size)
                            .when(layer.effective, |this| {
                                this.bg(with_alpha(theme.foreground, 0.06))
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .when(layer.effective, |this| {
                                        this.font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                    })
                                    .when(!layer.effective, |this| {
                                        this.text_color(theme.muted_foreground)
                                    })
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(rems(0.375))
                                    .text_color(if layer.effective {
                                        theme.foreground
                                    } else if layer.set {
                                        theme.muted_foreground
                                    } else {
                                        with_alpha(theme.muted_foreground, 0.6).into()
                                    })
                                    .child(
                                        div().max_w(rems(8.)).truncate().child(layer.value.clone()),
                                    )
                                    .child(if layer.effective {
                                        Icon::new(IconName::Check)
                                            .size(rems(0.875))
                                            .color(theme.primary)
                                            .into_any_element()
                                    } else {
                                        div().size(rems(0.875)).into_any_element()
                                    }),
                            ),
                    );
                }
                let overriding_here: Vec<&OverridingProject> = overriding.iter().collect();
                let mut section = div()
                    .px_3()
                    .py(rems(0.625))
                    .when(index > 0, |this| {
                        this.border_t_1()
                            .border_color(with_alpha(theme.border, 0.6))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.375))
                            .pb(rems(0.375))
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child(div().min_w_0().truncate().child(target.label.clone())),
                    )
                    .child(list);
                if !overriding_here.is_empty() {
                    let mut block = div()
                        .mt_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(with_alpha(theme.border, 0.6))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .px_2()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child("Overridden by")
                                .child({
                                    let entries: Vec<(String, String)> = overriding_here
                                        .iter()
                                        .map(|project| (project.project_id.clone(), key.to_owned()))
                                        .collect();
                                    let clear_label = if overriding_here.len() == 1 {
                                        "Reset it"
                                    } else {
                                        "Reset all"
                                    };
                                    Button::new(SharedString::from(format!(
                                        "inheritance-reset-overrides-{row_id}"
                                    )))
                                    .variant(ButtonVariant::Link)
                                    .size(ButtonSize::Compact)
                                    .label(clear_label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.clear_project_overrides(entries.clone(), cx);
                                    }))
                                }),
                        );
                    for project in overriding_here {
                        let project_key = project.project_key.clone();
                        block = block.child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .px_2()
                                .py_1()
                                .text_size(TEXT_SM.size)
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "inheritance-open-{row_id}-{}",
                                        project.project_id
                                    )))
                                    .variant(ButtonVariant::Link)
                                    .size(ButtonSize::Compact)
                                    .label(project.label.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_project_scope(Some(project_key.clone()), cx);
                                    })),
                                )
                                .child(
                                    div()
                                        .max_w(rems(8.))
                                        .truncate()
                                        .text_color(theme.muted_foreground)
                                        .child(project.value.clone()),
                                ),
                        );
                    }
                    section = section.child(block);
                }
                sections = sections.child(section);
            }
            div()
                .id(SharedString::from(format!("inheritance-popup-{row_id}")))
                .occlude()
                .w(rems(20.))
                .rounded(tokens::RADIUS_LG)
                .border_1()
                .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                .bg(theme.popover)
                .shadow(tokens::shadow_popover(theme.is_dark()))
                .text_color(theme.popover_foreground)
                .child(sections)
                .into_any_element()
        });
        Some(with_popup(trigger, popup, PopupAlign::Start, px(20.)).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::base_schemas::ProjectId;
    use turin_contracts::settings::{
        ProjectSettingsOverrides, ServerSettings, default_server_settings,
    };
    use turin_shared::project_settings::resolve_project_settings;

    use super::*;

    fn target(settings: &ServerSettings, project: Option<&str>) -> ScopedSettingsTarget {
        let project_id = project.map(|id| ProjectId::new(id).expect("project id"));
        let resolved =
            resolve_project_settings(settings, project_id.as_ref(), None, &|_, _, _| None);
        let sources: HashMap<&'static str, ProjectSettingSource> =
            turin_contracts::settings::PROJECT_SCOPED_SERVER_SETTING_KEYS
                .iter()
                .map(|key| (*key, resolved.sources.get(key)))
                .collect();
        ScopedSettingsTarget {
            environment_id: crate::state::contracts::EnvironmentId::new("laptop").expect("env id"),
            label: "Laptop".to_owned(),
            project_id,
            settings: resolved.settings,
            sources,
        }
    }

    fn rows(layers: &[InheritanceLayer]) -> Vec<(String, String, bool)> {
        layers
            .iter()
            .map(|layer| (layer.label.clone(), layer.value.clone(), layer.effective))
            .collect()
    }

    #[test]
    fn marks_the_built_in_default_effective_when_nothing_is_set() {
        let settings = default_server_settings();
        let layers =
            setting_inheritance_layers(&target(&settings, None), &settings, "defaultAutoPull");
        assert_eq!(
            rows(&layers),
            vec![
                ("Laptop".to_owned(), "Inherits".to_owned(), false),
                ("Default".to_owned(), "Off".to_owned(), true),
            ]
        );
    }

    #[test]
    fn walks_project_override_environment_value_then_built_in_default() {
        let mut settings = default_server_settings();
        settings.default_auto_pull = true;
        settings.project_settings_overrides.insert(
            ProjectId::new("project").unwrap(),
            ProjectSettingsOverrides {
                default_auto_pull: Some(false),
                ..Default::default()
            },
        );
        let layers = setting_inheritance_layers(
            &target(&settings, Some("project")),
            &settings,
            "defaultAutoPull",
        );
        assert_eq!(
            rows(&layers),
            vec![
                ("Project".to_owned(), "Off".to_owned(), true),
                ("Laptop".to_owned(), "On".to_owned(), false),
                ("Default".to_owned(), "Off".to_owned(), false),
            ]
        );
        let inherited = setting_inheritance_layers(
            &target(&settings, Some("other")),
            &settings,
            "defaultAutoPull",
        );
        assert_eq!(
            inherited
                .iter()
                .map(|layer| (layer.value.clone(), layer.effective))
                .collect::<Vec<_>>(),
            vec![
                ("Inherits".to_owned(), false),
                ("On".to_owned(), true),
                ("Off".to_owned(), false),
            ]
        );
    }

    #[test]
    fn labels_values_the_way_the_pickers_do() {
        assert_eq!(
            format_value("defaultThreadEnvMode", &json!("worktree")),
            "New worktree"
        );
        assert_eq!(
            format_value("defaultThreadEnvMode", &json!("local")),
            "Current checkout"
        );
        assert_eq!(
            format_value("worktreeSubmodules", &json!("top-level")),
            "Top level only"
        );
        assert_eq!(
            format_value("pullRequestMergeMethod", &json!("squash")),
            "Squash and merge"
        );
        assert_eq!(
            format_value("pullRequestMergeMethod", &Value::Null),
            "Last selected"
        );
        assert_eq!(
            format_value("sidebarAutoSettleAfterDays", &json!(1)),
            "1 day"
        );
        assert_eq!(
            format_value("sidebarAutoSettleAfterDays", &json!(7)),
            "7 days"
        );
        assert_eq!(
            format_value("sidebarAutoSettleAfterDays", &Value::Null),
            "Never"
        );
        assert_eq!(
            format_value("defaultModelSelection", &Value::Null),
            "Automatic"
        );
        assert_eq!(
            format_value(
                "defaultModelSelection",
                &json!({"instanceId": "codex", "model": "gpt-6-sol"})
            ),
            "gpt-6-sol"
        );
        assert_eq!(
            format_value(
                "sourceControlWritingStyle",
                &json!({"mode": "conventional_commits"})
            ),
            "Conventional Commits"
        );
        assert_eq!(
            format_value("defaultProjectScripts", &json!([1, 2])),
            "2 items"
        );
        assert_eq!(format_value("defaultProjectScripts", &json!([1])), "1 item");
        assert_eq!(format_value("branchNamePrefix", &json!("")), "Empty");
        assert_eq!(format_value("something", &json!({"a": 1})), "Custom");
        assert_eq!(format_value("something", &Value::Null), "Not set");
    }
}
