//! Port of upstream/apps/web/src/components/settings/ProjectsSettings.tsx,
//! upstream/apps/web/src/components/settings/ProjectSettingsPanel.tsx,
//! upstream/apps/web/src/components/settings/ProjectSettingsPanel.logic.ts (in
//! `project_settings_logic.rs`),
//! upstream/apps/web/src/components/settings/ProjectActionsSettings.tsx and
//! upstream/apps/web/src/components/settings/ProjectActionsList.tsx.
//!
//! The Projects page follows the settings scope: it shows the selected
//! project's name, icon, new-thread defaults, actions, checkouts and the
//! danger zone, or asks for a project to be picked.
//!
//! Not ported (PORTING.md Deviations): importing actions from `t3.json`
//! (`t3ProjectFile` is cut), the full `ProjectScriptEditorDialog` (name,
//! command and the setup toggle only), the remembered-members follow when a
//! grouping change replaces the project key, and the composer draft cleanup
//! after removal (the chat store owns it).
//!
//! Port of upstream/apps/web/src/components/settings/ProjectSettingsPanel.tsx.
//! Port of upstream/apps/web/src/components/settings/ProjectActionsSettings.tsx.
//! Port of upstream/apps/web/src/components/settings/ProjectActionsList.tsx.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px, rems,
};
use serde_json::{Value, json};
use turin_contracts::project::ProjectScript;

use super::super::alert::{AlertVariant, alert};
use super::super::controls::{FieldSize, switch, text_field};
use super::super::dialog::{
    ConfirmSpec, Dialog, dialog_footer, dialog_frame, dialog_header, dialog_panel,
};
use super::super::layout::{SettingsRow, SettingsSection, setting_reset_button};
use super::super::project_defaults_settings::ProjectDefaultsCategory;
use super::super::project_scripts::{command_for_project_script, next_project_script_id};
use super::super::project_settings_logic::{
    project_group_title_needs_update, project_icon_description, project_remove_copy,
    project_title_warning,
};
use super::super::scope::ScopeNoticeTarget;
use super::super::search::searchable_setting;
use super::super::view::{SettingsToastKind, SettingsView, Which};
use crate::components::project_favicon::ProjectFavicon;
use crate::settings_logic::settings_scope::SettingsScopeKind;
use crate::state::models::EnvironmentProject;
use crate::state::sidebar_project_grouping::{SidebarProjectGroupMember, SidebarProjectSnapshot};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_SM, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

type Member = SidebarProjectGroupMember<EnvironmentProject>;
type Group = SidebarProjectSnapshot<EnvironmentProject>;

#[derive(Default)]
pub struct ProjectsState {
    pub script: ScriptDraft,
    /// `ProjectIconPickerDialog`.
    pub icon_picker: Option<super::super::project_icon_picker_dialog::IconPicker>,
    /// `ProjectFaviconPickerDialog`.
    pub favicon_picker: Option<super::super::project_favicon_picker_dialog::FaviconPicker>,
}

#[derive(Default, Clone)]
pub struct ScriptDraft {
    /// `None` adds a new action.
    pub id: Option<String>,
    pub run_on_worktree_create: bool,
}

fn script_icon(icon: &str) -> IconName {
    match icon {
        "test" => IconName::FlaskConical,
        "lint" => IconName::ListChecks,
        "configure" => IconName::Wrench,
        "build" => IconName::Hammer,
        "debug" => IconName::Bug,
        _ => IconName::Play,
    }
}

impl SettingsView {
    fn project_scripts(&self, cx: &gpui_kit::App) -> Vec<ProjectScript> {
        self.scope_context(cx)
            .settings()
            .map(|settings| settings.default_project_scripts.clone())
            .unwrap_or_else(|| self.vm(cx).server.default_project_scripts.clone())
    }

    /// `ProjectsSettings`.
    pub(super) fn render_projects(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let context = self.scope_context(cx);
        let message = |text: String| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_8()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        match &context.scope.kind {
            SettingsScopeKind::Project { group, .. }
            | SettingsScopeKind::Checkout { group, .. } => {
                let group = group.clone();
                let members = context.scope.members.clone();
                self.project_detail(group, members, window, cx)
            }
            SettingsScopeKind::Unavailable {
                message: reason, ..
            } => {
                if context.groups.is_empty() {
                    vec![message(
                        "Add a project from the sidebar to configure it here.".into(),
                    )]
                } else {
                    vec![message(reason.clone())]
                }
            }
            _ => vec![self.settings_scope_notice(
                "Choose a project to manage its name, icon, checkouts and actions.",
                ScopeNoticeTarget::Project,
                cx,
            )],
        }
    }

    /// `ProjectDetail`.
    fn project_detail(
        &mut self,
        group: Group,
        members: Vec<Member>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let has_other_members = members.len() < group.member_projects.len();
        let ids: Vec<String> = members
            .iter()
            .map(|member| member.project.id.as_str().to_owned())
            .collect();
        let Some(representative) = members.first().map(|member| member.project.clone()) else {
            return Vec::new();
        };
        let has_multiple_checkouts = members.len() > 1;

        let info = alert(
            &theme,
            AlertVariant::Info,
            Some(IconName::Info),
            vec![div()
                .child("Can't find a setting? Keep this project picked above and hop to any other settings page.")
                .into_any_element()],
            None,
        );

        // Overview.
        let name_ids = ids.clone();
        let titles: Vec<String> = members
            .iter()
            .map(|member| member.project.title.clone())
            .collect();
        let display_name = group.display_name.clone();
        let name_input = self.input(
            &format!("project-name-{}:{}", group.project_key, group.display_name),
            &group.display_name,
            "",
            window,
            cx,
            move |this, text, _, cx| {
                let was_edited = text != display_name;
                this.rename_project_group(&name_ids, &titles, &text, was_edited, cx);
            },
        );
        let (overview_id, _) = searchable_setting("project-overview");
        let icon_set = members.iter().any(|member| {
            member.project.favicon_path.is_some() || member.project.project_icon.is_some()
        });
        let icon_reset = icon_set.then(|| {
            let reset_ids = ids.clone();
            let action = self.cb(cx, move |this, _, cx| {
                this.set_project_icon(reset_ids.clone(), None, None, cx)
            });
            setting_reset_button("reset-project-icon", "Reset to default", move |w, c| {
                action(w, c)
            })
            .into_any_element()
        });
        let picker_ids = ids.clone();
        let picker_name = representative.title.clone();
        let picker_icon = representative.project_icon.clone();
        let file_ids = ids.clone();
        let file_name = group.display_name.clone();
        let file_cwd = representative.workspace_root.clone();
        let overview = SettingsSection::new("Project")
            .id(overview_id)
            .hide_title()
            .child(
                SettingsRow::new("Name")
                    .description(
                        "The shared name for this project group in the sidebar and thread lists.",
                    )
                    .control(div().w(rems(16.)).child(text_field(
                        &name_input,
                        None,
                        FieldSize::Sm,
                        false,
                        cx,
                    ))),
            )
            .child(
                SettingsRow::new("Project icon")
                    .description(project_icon_description(
                        representative.project_icon.as_ref(),
                        representative.favicon_path.as_deref(),
                    ))
                    .reset_action(icon_reset)
                    .control(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ProjectFavicon::new(&representative).size(rems(1.5)))
                            .child(
                                Button::new("choose-project-icon")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .label("Choose icon")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_project_icon_picker(
                                            picker_ids.clone(),
                                            picker_name.clone(),
                                            picker_icon.clone(),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("choose-project-icon-file")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .label("Choose file")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_project_favicon_picker(
                                            file_ids.clone(),
                                            file_name.clone(),
                                            file_cwd.clone(),
                                            true,
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            );

        let defaults = self.project_defaults_section(ProjectDefaultsCategory::Project, cx);
        let actions = self.project_actions_section(cx);

        let checkouts = has_multiple_checkouts.then(|| {
            let mut section = SettingsSection::new("Checkouts");
            for member in &members {
                let remove_member = member.clone();
                let remove_group = group.clone();
                section = section.child(
                    SettingsRow::new(
                        member
                            .environment_label
                            .clone()
                            .unwrap_or_else(|| "Environment".to_owned()),
                    )
                    .description(member.project.workspace_root.clone())
                    .control(
                        Button::new(SharedString::from(format!(
                            "remove-checkout-{}",
                            member.project.id.as_str()
                        )))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_project_members(
                                vec![remove_member.clone()],
                                remove_group.clone(),
                                has_other_members,
                                cx,
                            );
                        })),
                    ),
                );
            }
            section.into_any_element()
        });

        // Danger.
        let copy = project_remove_copy(has_other_members, members.len());
        let danger_members = members.clone();
        let danger_group = group.clone();
        let danger = SettingsSection::new("Danger").child(
            SettingsRow::new(copy.title)
                .description(copy.description)
                .control(
                    Button::new("remove-project")
                        .variant(ButtonVariant::DestructiveOutline)
                        .size(ButtonSize::Sm)
                        .icon(IconName::Trash)
                        .label(copy.button)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_project_members(
                                danger_members.clone(),
                                danger_group.clone(),
                                has_other_members,
                                cx,
                            );
                        })),
                ),
        );

        let mut sections = vec![info, overview.into_any_element(), defaults, actions];
        sections.extend(checkouts);
        sections.push(danger.into_any_element());
        sections
    }

    /// `renameGroup`: a group-level edit fans out to every member.
    fn rename_project_group(
        &mut self,
        ids: &[String],
        member_titles: &[String],
        text: &str,
        was_edited: bool,
        cx: &mut Context<Self>,
    ) {
        let title = text.trim().to_owned();
        if let Some(warning) = project_title_warning(&title) {
            self.toast(SettingsToastKind::Warning, warning, "", cx);
            return;
        }
        let titles: Vec<&str> = member_titles.iter().map(String::as_str).collect();
        if !project_group_title_needs_update(&titles, &title, was_edited) {
            return;
        }
        let source = self.store.read(cx).source();
        for id in ids {
            source.rename_project(id, &title, cx);
        }
    }

    /// `removeMembers`: confirm, then remove each member.
    fn remove_project_members(
        &mut self,
        members: Vec<Member>,
        group: Group,
        has_other_members: bool,
        cx: &mut Context<Self>,
    ) {
        let vm = self.vm(cx);
        let thread_count: usize = members
            .iter()
            .map(|member| {
                vm.project_thread_counts
                    .get(member.project.id.as_str())
                    .copied()
                    .unwrap_or(0)
            })
            .sum();
        let is_whole_group = members.len() == group.member_projects.len();
        let single = (members.len() == 1).then(|| members[0].clone());
        let lines = super::super::project_settings_logic::project_removal_confirmation(
            has_other_members || !is_whole_group,
            single
                .as_ref()
                .map(|member| member.project.title.as_str())
                .unwrap_or(group.display_name.as_str()),
            thread_count,
            single.as_ref().map(|member| {
                (
                    member.project.workspace_root.as_str(),
                    member.environment_label.as_deref(),
                )
            }),
            members.len(),
            is_whole_group && !has_other_members,
        );
        let (title, description) = lines
            .split_first()
            .map(|(first, rest)| (first.clone(), rest.join("\n")))
            .unwrap_or_default();
        let ids: Vec<String> = members
            .iter()
            .map(|member| member.project.id.as_str().to_owned())
            .collect();
        let leaves_page = is_whole_group && !has_other_members;
        self.dialog = Some(Dialog::Confirm(ConfirmSpec {
            title: title.into(),
            description: description.into(),
            confirm_label: if members.len() == 1 && has_other_members {
                "Remove checkout".into()
            } else {
                "Remove project".into()
            },
            destructive: true,
            on_confirm: std::rc::Rc::new(move |this, _, cx| {
                let source = this.store.read(cx).source();
                for id in &ids {
                    source.remove_project(id, cx);
                }
                if leaves_page {
                    this.select_scope(Default::default(), cx);
                }
            }),
        }));
        cx.notify();
    }

    /// `ProjectActionsSettings`: the actions of the selected scope.
    fn project_actions_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let context = self.scope_context(cx);
        let scripts = self.project_scripts(cx);
        let mixed = context.targets.iter().any(|target| {
            serde_json::to_value(&target.settings.default_project_scripts).ok()
                != serde_json::to_value(&scripts).ok()
        });
        let mac = vm.is_mac();
        let add = Button::new("add-project-action")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Xs)
            .icon(IconName::Plus)
            .icon_size(rems(0.875))
            .label("Add action")
            .disabled(context.targets.is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.open_script_editor(None, window, cx)));
        let (actions_id, _) = searchable_setting("project-actions");
        let header_row = SettingsRow::new("Actions")
            .description("Commands that run in this project's checkout or its worktree, with optional shortcuts.")
            .control(div().flex().flex_wrap().items_center().gap(rems(0.375)).child(add));
        let (header_row, scoped_reset, _) = self.decorate_server_row(
            cx,
            header_row,
            "project-actions",
            &["defaultProjectScripts"],
            "project actions",
        );
        let header_row = match scoped_reset {
            Some(reset) => header_row.reset_action(reset),
            None => header_row,
        };
        let mut section = SettingsSection::new("Actions")
            .id(actions_id)
            .child(header_row);
        if mixed {
            section = section.child(
                SettingsRow::new("Different actions across environments").description(
                    "Choose one environment to edit its list. Adding an action here adds it on every selected environment.",
                ),
            );
            return section.into_any_element();
        }
        if scripts.is_empty() {
            section = section.child(
                div()
                    .px_4()
                    .py_2()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("No actions configured."),
            );
        }
        for script in scripts {
            let id = script.id.as_str().to_owned();
            let command = command_for_project_script(&id);
            // `shortcutLabelForCommand`: the last rule for the command wins.
            let shortcut = vm
                .keybindings
                .rules
                .iter()
                .rev()
                .find(|rule| rule.command.as_str() == command)
                .map(|rule| {
                    crate::components::command_palette::shortcut_labels::format_shortcut_label(
                        &rule.shortcut,
                        mac,
                    )
                });
            let icon = serde_json::to_value(script.icon)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let tag = |label: &'static str| {
                div()
                    .flex_shrink_0()
                    .rounded(RADIUS_SM)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .px(px(6.))
                    .py(px(1.))
                    .text_size(TEXT_2XS.size)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme.muted_foreground)
                    .child(label)
            };
            let title = div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(script_icon(&icon))
                        .size(rems(1.))
                        .color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(script.name.as_str().to_owned()),
                )
                .when(script.run_on_worktree_create, |this| {
                    this.child(tag("setup"))
                })
                .when(script.run_on_settle == Some(true), |this| {
                    this.child(tag("on settle"))
                })
                .when(script.preview_url.is_some(), |this| {
                    this.child(tag("preview · desktop only"))
                });
            let edit_id = id.clone();
            let name = script.name.as_str().to_owned();
            let group_name = SharedString::from(format!("action-row-{id}"));
            section = section.child(
                div().group(group_name.clone()).child(
                    SettingsRow::new(title)
                        .compact()
                        .description(
                            div()
                                .max_w_full()
                                .truncate()
                                .font_family(theme.fonts.mono.clone())
                                .child(script.command.as_str().to_owned()),
                        )
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .children(shortcut.map(|label| {
                                    div()
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(label)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_shrink_0()
                                        .opacity(0.)
                                        .group_hover(group_name, |style| style.opacity(1.))
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "edit-action-{id}"
                                            )))
                                            .variant(ButtonVariant::GhostMuted)
                                            .size(ButtonSize::IconXs)
                                            .icon(IconName::Settings2)
                                            .icon_size(rems(0.875))
                                            .tooltip(Tooltip::new(format!("Edit {name}")))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_script_editor(
                                                    Some(edit_id.clone()),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                        ),
                                ),
                        ),
                ),
            );
        }
        section.into_any_element()
    }

    fn open_script_editor(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scripts = self.project_scripts(cx);
        let existing = id
            .as_ref()
            .and_then(|id| scripts.iter().find(|s| s.id.as_str() == id));
        let (name, command, setup) = existing
            .map(|s| {
                (
                    s.name.as_str().to_owned(),
                    s.command.as_str().to_owned(),
                    s.run_on_worktree_create,
                )
            })
            .unwrap_or_default();
        self.panels.projects.script = ScriptDraft {
            id,
            run_on_worktree_create: setup,
        };
        for (key, placeholder, value) in [
            ("script-name", "Test", name),
            ("script-command", "bun test", command),
        ] {
            let state = self.input(key, &value, placeholder, window, cx, |_, _, _, _| {});
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.dialog = Some(Dialog::ScriptEditor);
        cx.notify();
    }

    fn save_scripts(&mut self, scripts: Vec<ProjectScript>, cx: &mut Context<Self>) {
        let value = serde_json::to_value(scripts).unwrap_or(Value::Null);
        self.update(Which::Server, json!({ "defaultProjectScripts": value }), cx);
    }

    /// `ProjectScriptEditorDialog`, reduced to name, command and the setup toggle.
    pub(in crate::settings) fn render_script_editor_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let draft = self.panels.projects.script.clone();
        let editing = draft.id.is_some();
        let name = self.input("script-name", "", "Test", window, cx, |_, _, _, _| {});
        let command = self.input(
            "script-command",
            "",
            "bun test",
            window,
            cx,
            |_, _, _, _| {},
        );
        let read = |this: &Self, key: &str, cx: &gpui_kit::App| {
            this.inputs
                .get(&SharedString::from(key.to_owned()))
                .map(|state| state.read(cx).value().trim().to_owned())
                .unwrap_or_default()
        };
        let can_save = !read(self, "script-name", cx).is_empty()
            && !read(self, "script-command", cx).is_empty();
        let label = |text: &'static str| {
            div()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(text)
        };
        let entity = cx.entity();
        let setup_switch = switch(
            "script-setup",
            draft.run_on_worktree_create,
            false,
            cx,
            move |next, _, cx| {
                entity.update(cx, |this, cx| {
                    this.panels.projects.script.run_on_worktree_create = next;
                    cx.notify();
                });
            },
        );
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().flex().flex_col().gap(px(6.)).child(label("Name")).child(text_field(&name, None, FieldSize::Default, false, cx)))
            .child(div().flex().flex_col().gap(px(6.)).child(label("Command")).child(text_field(&command, None, FieldSize::Default, true, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div().flex().flex_col().child(label("Run automatically on worktree creation")).child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child("Use it for setup commands such as installing dependencies."),
                        ),
                    )
                    .child(setup_switch),
            );
        let close = |this: &mut Self, _: &mut Window, cx: &mut Context<Self>| {
            this.dialog = None;
            cx.notify();
        };
        let dismiss = self.cb(cx, move |this, window, cx| close(this, window, cx));
        let delete = editing.then(|| {
            let delete_id = draft.id.clone().unwrap_or_default();
            Button::new("script-delete")
                .variant(ButtonVariant::DestructiveOutline)
                .label("Delete")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let remaining: Vec<ProjectScript> = this
                        .project_scripts(cx)
                        .into_iter()
                        .filter(|s| s.id.as_str() != delete_id)
                        .collect();
                    this.save_scripts(remaining, cx);
                    this.dialog = None;
                    cx.notify();
                }))
                .into_any_element()
        });
        let mut footer: Vec<AnyElement> = delete.into_iter().collect();
        footer.push(
            Button::new("script-cancel")
                .variant(ButtonVariant::Outline)
                .label("Cancel")
                .on_click(cx.listener(move |this, _, window, cx| close(this, window, cx)))
                .into_any_element(),
        );
        footer.push(
            Button::new("script-save")
                .label(if editing {
                    "Save changes"
                } else {
                    "Add action"
                })
                .disabled(!can_save)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let name = read(this, "script-name", cx);
                    let command = read(this, "script-command", cx);
                    if name.is_empty() || command.is_empty() {
                        return;
                    }
                    let draft = this.panels.projects.script.clone();
                    let mut scripts = this.project_scripts(cx);
                    match &draft.id {
                        Some(id) => {
                            if let Some(script) = scripts.iter_mut().find(|s| s.id.as_str() == id)
                                && let (Ok(name), Ok(command)) = (
                                    turin_contracts::TrimmedNonEmptyString::new(&name),
                                    turin_contracts::TrimmedNonEmptyString::new(&command),
                                )
                            {
                                script.name = name;
                                script.command = command;
                                script.run_on_worktree_create = draft.run_on_worktree_create;
                            }
                        }
                        None => {
                            let ids: Vec<String> =
                                scripts.iter().map(|s| s.id.as_str().to_owned()).collect();
                            let id = next_project_script_id(&name, &ids);
                            if let Ok(script) = serde_json::from_value::<ProjectScript>(json!({
                                "id": id,
                                "name": name,
                                "command": command,
                                "icon": "play",
                                "runOnWorktreeCreate": draft.run_on_worktree_create,
                            })) {
                                scripts.push(script);
                            }
                        }
                    }
                    this.save_scripts(scripts, cx);
                    this.dialog = None;
                    cx.notify();
                }))
                .into_any_element(),
        );
        dialog_frame(
            &theme,
            rems(28.),
            vec![
                dialog_header(
                    if editing { "Edit action" } else { "Add action" },
                    Some("Actions run in this project's checkout or its worktree.".into()),
                    &theme,
                ),
                dialog_panel("script-body", body),
                dialog_footer(&theme, footer),
            ],
            dismiss,
        )
    }
}
