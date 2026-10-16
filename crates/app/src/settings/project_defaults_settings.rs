//! Port of upstream/apps/web/src/components/settings/ProjectDefaultsSettings.tsx.
//!
//! Rows for the settings a project may override. The same rows edit the
//! environment defaults at an environment scope and project overrides at a
//! project or checkout scope; `SettingsView::update` routes the write.
//!
//! Not ported (PORTING.md Deviations): the `integrations` category (agent
//! browser access; the browser is cut) and the `t3.json` tier of the effective
//! value (`resolveProjectSettings(target.settings, null, null, null)`: the
//! built-ins `local` and `recursive` fill a null value here).

use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    div, rems,
};
use serde_json::{Value, json};

use super::controls::SelectWidth;
use super::layout::SettingsSection;
use super::rows::{BoolRow, Json};
use super::view::{SettingsView, Which};
use crate::components::branch_toolbar::branch_toolbar_logic::{
    EnvMode, resolve_env_mode_label, worktree_submodules_label,
};
use crate::pull_request::pull_request_detail_logic::PULL_REQUEST_MERGE_METHOD_LABELS;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Icon, IconName};

/// `ProjectSettingsCategory`, without `integrations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectDefaultsCategory {
    General,
    /// The Projects page shortcut: the new-thread defaults people change most.
    Project,
    SourceControl,
}

/// `runtimeModeConfig`: value, label, description.
pub const RUNTIME_MODES: [(&str, &str, &str); 4] = [
    (
        "approval-required",
        "Supervised",
        "Ask before commands and file changes.",
    ),
    (
        "auto-accept-edits",
        "Auto-accept edits",
        "Auto-approve edits, ask before other actions.",
    ),
    (
        "auto",
        "Auto",
        "Supported providers approve routine actions; others still ask.",
    ),
    (
        "full-access",
        "Full access",
        "Allow commands and edits without prompts.",
    ),
];

fn runtime_mode_icon(value: &str) -> IconName {
    match value {
        "approval-required" => IconName::Lock,
        "auto-accept-edits" => IconName::PenLine,
        "auto" => IconName::Sparkles,
        _ => IconName::LockOpen,
    }
}

/// The section title and anchor id of a category.
pub fn category_heading(category: ProjectDefaultsCategory) -> (&'static str, &'static str) {
    match category {
        ProjectDefaultsCategory::General | ProjectDefaultsCategory::Project => {
            ("project-defaults", "New threads")
        }
        ProjectDefaultsCategory::SourceControl => ("source-control-defaults", "Repositories"),
    }
}

/// The description a row shows at a project scope or at the defaults.
pub fn scoped_description(
    project_scope: bool,
    project: &'static str,
    defaults: &'static str,
) -> &'static str {
    if project_scope { project } else { defaults }
}

/// The label of `value` for `defaultThreadEnvMode`, with the built-in `local`
/// standing in for a null value.
pub fn env_mode_label(value: &Value) -> &'static str {
    match value.as_str() {
        Some("worktree") => resolve_env_mode_label(EnvMode::Worktree),
        _ => resolve_env_mode_label(EnvMode::Local),
    }
}

/// The label of `value` for `worktreeSubmodules`, with the built-in
/// `recursive` standing in for a null value.
pub fn submodules_label(value: &Value) -> &'static str {
    use turin_contracts::environment::WorktreeSubmodules as W;
    match value.as_str() {
        Some("top-level") => worktree_submodules_label(W::TopLevel),
        Some("none") => worktree_submodules_label(W::None),
        _ => worktree_submodules_label(W::Recursive),
    }
}

/// The label of `value` for `pullRequestMergeMethod` (null is "Last selected").
pub fn merge_method_label(value: &Value) -> &'static str {
    let Some(text) = value.as_str() else {
        return "Last selected";
    };
    PULL_REQUEST_MERGE_METHOD_LABELS
        .iter()
        .find(|(method, _)| {
            serde_json::to_value(method)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .as_deref()
                == Some(text)
        })
        .map(|(_, label)| *label)
        .unwrap_or("Last selected")
}

impl SettingsView {
    /// `ProjectDefaultsSettings category`.
    pub(super) fn project_defaults_section(
        &mut self,
        category: ProjectDefaultsCategory,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let context = self.scope_context(cx);
        let project_scope = context.is_project_scope();
        let unavailable = context.targets.is_empty();
        let json = self.json(cx);
        let (id, title) = category_heading(category);
        let rows: Vec<AnyElement> = match category {
            ProjectDefaultsCategory::Project => vec![
                self.default_model_row(&json, project_scope, unavailable, cx),
                self.workspace_row(&json, project_scope, unavailable, cx),
            ],
            ProjectDefaultsCategory::General => vec![
                self.default_model_row(&json, project_scope, unavailable, cx),
                self.permissions_row(&json, project_scope, cx),
                self.workspace_row(&json, project_scope, unavailable, cx),
                self.submodules_row(&json, project_scope, unavailable, cx),
            ],
            ProjectDefaultsCategory::SourceControl => vec![
                self.auto_pull_row(&json, project_scope, cx),
                self.agent_credits_row(&json, cx),
                self.merge_method_row(&json, project_scope, cx),
            ],
        };
        SettingsSection::new(title)
            .id(id)
            .children(rows)
            .into_any_element()
    }

    /// `workspaceRow`.
    fn workspace_row(
        &mut self,
        json: &Json,
        project_scope: bool,
        unavailable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = json.get(Which::Server, "defaultThreadEnvMode").clone();
        let row = Self::base_row("new-threads").description(scoped_description(
            project_scope,
            "Where new threads in this project start.",
            "Where new threads start. Projects and their t3.json can override it.",
        ));
        let (row, mixed) = self.scoped_row(
            cx,
            json,
            row,
            "new-threads",
            "defaultThreadEnvMode",
            "default workspace",
            "Reset to default",
        );
        let selected = if current.is_null() {
            "local".to_owned()
        } else {
            current.as_str().unwrap_or("local").to_owned()
        };
        let label: SharedString = if mixed {
            "Mixed"
        } else if unavailable {
            "Unavailable"
        } else {
            env_mode_label(&current)
        }
        .into();
        let control = self.select_control(
            cx,
            "default-workspace",
            label,
            vec![
                (
                    json!("local"),
                    resolve_env_mode_label(EnvMode::Local).into(),
                    !mixed && selected == "local",
                ),
                (
                    json!("worktree"),
                    resolve_env_mode_label(EnvMode::Worktree).into(),
                    !mixed && selected == "worktree",
                ),
            ],
            SelectWidth::Auto,
            false,
            |this, value, _, cx| {
                this.update(Which::Server, json!({ "defaultThreadEnvMode": value }), cx)
            },
        );
        row.control(control).into_any_element()
    }

    /// The `defaultRuntimeMode` row.
    fn permissions_row(
        &mut self,
        json: &Json,
        project_scope: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let runtime_mode = json.str(Which::Server, "defaultRuntimeMode").to_owned();
        let row = Self::base_row("default-permissions").description(scoped_description(
            project_scope,
            "Permissions for new threads in this project.",
            "Default permissions for new threads. Projects can override them.",
        ));
        let (row, mixed) = self.scoped_row(
            cx,
            json,
            row,
            "default-permissions",
            "defaultRuntimeMode",
            "default permissions",
            "Reset to default",
        );
        let current = RUNTIME_MODES
            .iter()
            .find(|(value, _, _)| *value == runtime_mode);
        let trigger_label = div()
            .flex()
            .items_center()
            .gap(rems(0.375))
            .children((!mixed).then(|| {
                Icon::new(runtime_mode_icon(&runtime_mode))
                    .size(rems(0.875))
                    .color(theme.muted_foreground)
            }))
            .child(if mixed {
                "Mixed"
            } else {
                current.map(|(_, label, _)| *label).unwrap_or("")
            })
            .into_any_element();
        let control = self.select_control_rich(
            cx,
            "default-permissions",
            trigger_label,
            RUNTIME_MODES
                .iter()
                .map(|(value, label, description)| {
                    let item = div()
                        .min_w(rems(16.))
                        .flex()
                        .flex_col()
                        .gap(rems(0.125))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .font_weight(FontWeight::MEDIUM)
                                .child(
                                    Icon::new(runtime_mode_icon(value))
                                        .size(rems(0.875))
                                        .color(theme.muted_foreground),
                                )
                                .child(*label),
                        )
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .line_height(rems(1.))
                                .text_color(theme.muted_foreground)
                                .child(*description),
                        )
                        .into_any_element();
                    (json!(value), item, !mixed && *value == runtime_mode)
                })
                .collect(),
            SelectWidth::Auto,
            |this, value, _, cx| {
                this.update(Which::Server, json!({ "defaultRuntimeMode": value }), cx)
            },
        );
        row.control(control).into_any_element()
    }

    /// The `worktreeSubmodules` row.
    fn submodules_row(
        &mut self,
        json: &Json,
        project_scope: bool,
        unavailable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = json.get(Which::Server, "worktreeSubmodules").clone();
        let row = Self::base_row("worktree-submodules").description(scoped_description(
            project_scope,
            "How new worktrees in this project populate git submodules.",
            "How new worktrees populate git submodules. Projects and their t3.json can override it.",
        ));
        // At the defaults the arrow restores the null (inherit) value.
        let (row, mixed) = self.scoped_row(
            cx,
            json,
            row,
            "worktree-submodules",
            "worktreeSubmodules",
            "worktree submodules",
            "Reset to default",
        );
        let row = if project_scope || current.is_null() {
            row
        } else {
            let action = self.cb(cx, |this, _, cx| {
                this.update(
                    Which::Server,
                    json!({ "worktreeSubmodules": Value::Null }),
                    cx,
                )
            });
            row.reset_action(Some(
                super::layout::setting_reset_button(
                    "reset-worktree-submodules",
                    "Reset to default",
                    move |w, c| action(w, c),
                )
                .into_any_element(),
            ))
        };
        let selected = current.as_str().unwrap_or("recursive").to_owned();
        let label: SharedString = if mixed {
            "Mixed"
        } else if unavailable {
            "Unavailable"
        } else {
            submodules_label(&current)
        }
        .into();
        let options = [
            ("recursive", submodules_label(&json!("recursive"))),
            ("top-level", submodules_label(&json!("top-level"))),
            ("none", submodules_label(&json!("none"))),
        ];
        let control = self.select_control(
            cx,
            "worktree-submodules",
            label,
            options
                .iter()
                .map(|(value, label)| {
                    (
                        json!(value),
                        SharedString::from(*label),
                        !mixed && selected == *value,
                    )
                })
                .collect(),
            SelectWidth::Auto,
            false,
            |this, value, _, cx| {
                this.update(Which::Server, json!({ "worktreeSubmodules": value }), cx)
            },
        );
        row.control(control).into_any_element()
    }

    /// The `defaultAutoPull` row.
    fn auto_pull_row(
        &mut self,
        json: &Json,
        project_scope: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.bool_row_tip(
            cx,
            json,
            BoolRow {
                search_id: "automatic-pull",
                title: Some("Automatically pull"),
                description: scoped_description(
                    project_scope,
                    "Keeps this project's default branch current when the checkout has no local changes or commits.",
                    "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.",
                )
                .into(),
                which: Which::Server,
                key: "defaultAutoPull",
                label: "Default automatic pull",
                reset_label: "default automatic pull",
            },
            "Reset automatic pull to off",
        )
    }

    /// The `removeAgentCreditsOnMerge` row.
    fn agent_credits_row(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        self.bool_row_tip(
            cx,
            json,
            BoolRow {
                search_id: "remove-agent-credits-on-merge",
                title: None,
                description: "Remove recognized agent credit lines from GitHub merge and squash messages, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.".into(),
                which: Which::Server,
                key: "removeAgentCreditsOnMerge",
                label: "Remove agent credits when merging",
                reset_label: "agent credit removal",
            },
            "Keep agent credits",
        )
    }

    /// The `pullRequestMergeMethod` row (null is "Last selected").
    fn merge_method_row(
        &mut self,
        json: &Json,
        project_scope: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = json.get(Which::Server, "pullRequestMergeMethod").clone();
        let row = Self::base_row("pull-request-merge-method").description(scoped_description(
            project_scope,
            "Pull requests in this project start with this method.",
            "Pull requests start with this method. Last selected reuses whatever you chose most recently on this device.",
        ));
        let (row, mixed) = self.scoped_row(
            cx,
            json,
            row,
            "pull-request-merge-method",
            "pullRequestMergeMethod",
            "default merge method",
            "Reset to last selected",
        );
        let selected = current.as_str().unwrap_or("last").to_owned();
        let label: SharedString = if mixed {
            "Mixed"
        } else {
            merge_method_label(&current)
        }
        .into();
        let mut options: Vec<(Value, SharedString, bool)> = vec![(
            json!("last"),
            "Last selected".into(),
            !mixed && selected == "last",
        )];
        for (method, name) in PULL_REQUEST_MERGE_METHOD_LABELS {
            let value = serde_json::to_value(method).unwrap_or(Value::Null);
            let is_selected = !mixed && value.as_str() == Some(selected.as_str());
            options.push((value, SharedString::from(name), is_selected));
        }
        let control = self.select_control(
            cx,
            "pull-request-merge-method",
            label,
            options,
            SelectWidth::Auto,
            false,
            |this, value, _, cx| {
                let next = match value.as_str() {
                    Some("last") | None => Value::Null,
                    Some(other) => json!(other),
                };
                this.update(Which::Server, json!({ "pullRequestMergeMethod": next }), cx);
            },
        );
        row.control(control).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_null_values_with_the_built_ins() {
        assert_eq!(env_mode_label(&Value::Null), "Current checkout");
        assert_eq!(env_mode_label(&json!("worktree")), "New worktree");
        assert_eq!(submodules_label(&Value::Null), "Recursive");
        assert_eq!(submodules_label(&json!("top-level")), "Top level only");
        assert_eq!(submodules_label(&json!("none")), "Skip");
    }

    #[test]
    fn labels_the_merge_method_with_last_selected_for_null() {
        assert_eq!(merge_method_label(&Value::Null), "Last selected");
        assert_eq!(merge_method_label(&json!("squash")), "Squash and merge");
        assert_eq!(merge_method_label(&json!("merge")), "Merge");
        assert_eq!(merge_method_label(&json!("rebase")), "Rebase and merge");
    }

    #[test]
    fn describes_a_row_by_its_scope() {
        assert_eq!(scoped_description(true, "a", "b"), "a");
        assert_eq!(scoped_description(false, "a", "b"), "b");
    }

    #[test]
    fn names_each_category_section() {
        assert_eq!(
            category_heading(ProjectDefaultsCategory::General),
            ("project-defaults", "New threads")
        );
        assert_eq!(
            category_heading(ProjectDefaultsCategory::Project),
            ("project-defaults", "New threads")
        );
        assert_eq!(
            category_heading(ProjectDefaultsCategory::SourceControl),
            ("source-control-defaults", "Repositories")
        );
    }

    #[test]
    fn lists_the_runtime_modes_in_upstream_order() {
        let values: Vec<&str> = RUNTIME_MODES.iter().map(|(value, _, _)| *value).collect();
        assert_eq!(
            values,
            [
                "approval-required",
                "auto-accept-edits",
                "auto",
                "full-access"
            ]
        );
    }
}
