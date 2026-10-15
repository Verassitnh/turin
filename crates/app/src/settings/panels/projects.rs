//! Port of ProjectsSettings.tsx / ProjectSettingsPanel.tsx (overview, new
//! thread defaults, actions, danger) and ProjectActionsSettings.tsx /
//! ProjectActionsList.tsx for the single local environment.
//!
//! Not ported (PORTING.md Deviations): the settings-scope picker (this page
//! has its own project picker), per-project overrides of the defaults (rows
//! edit the environment defaults), the project icon / favicon pickers, the
//! checkout list and per-checkout removal, importing actions from `t3.json`
//! and the full action editor (name, command and setup toggle only).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px, rems,
};
use serde_json::{Value, json};
use turin_contracts::project::ProjectScript;

use super::super::controls::{FieldSize, SelectWidth, switch, text_field};
use super::super::dialog::{ConfirmSpec, Dialog, dialog_footer, dialog_frame, dialog_header, dialog_panel};
use super::super::layout::{SettingsRow, SettingsSection};
use super::super::project_scripts::{command_for_project_script, next_project_script_id};
use super::super::project_settings_logic::project_group_title_needs_update;
use super::super::search::searchable_setting;
use super::super::view::SettingsView;
use crate::components::project_monogram::ProjectMonogram;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_SM, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

#[derive(Default)]
pub struct ProjectsState {
    pub selected: Option<String>,
    pub script: ScriptDraft,
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
        self.vm(cx).server.default_project_scripts.clone()
    }

    pub(super) fn render_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let json = self.json(cx);
        let Some(project) = self
            .panels
            .projects
            .selected
            .as_ref()
            .and_then(|id| vm.projects.iter().find(|p| &p.id == id))
            .or_else(|| vm.projects.first())
            .cloned()
        else {
            return vec![div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_8()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child("Add a project from the sidebar to configure it here.")
                .into_any_element()];
        };

        // Project picker (the scope picker of upstream's settings header).
        let current = project.id.clone();
        let picker = self.select_control(
            cx,
            "settings-project",
            project.display_name.clone().into(),
            vm.projects
                .iter()
                .map(|p| (json!(p.id), SharedString::from(p.display_name.clone()), p.id == current))
                .collect(),
            SelectWidth::Fixed(14.),
            false,
            |this, value, _, cx| {
                this.panels.projects.selected = value.as_str().map(str::to_owned);
                cx.notify();
            },
        );
        let picker_row = SettingsRow::new("Project")
            .description("Choose a project to manage its name, icon, checkouts and actions.")
            .control(picker);

        // Overview.
        let name_id = project.id.clone();
        let name_input = self.input(
            &format!("project-name-{}", project.id),
            &project.display_name,
            "",
            window,
            cx,
            move |this, text, _, cx| {
                let title = text.trim().to_owned();
                if title.is_empty() {
                    return;
                }
                let current = this
                    .vm(cx)
                    .projects
                    .iter()
                    .find(|p| p.id == name_id)
                    .map(|p| p.display_name.clone())
                    .unwrap_or_default();
                if project_group_title_needs_update(&[current.as_str()], &title, true) {
                    let source = this.store.read(cx).source();
                    source.rename_project(&name_id, &title, cx);
                }
            },
        );
        let (overview_id, _) = searchable_setting("project-overview");
        let overview = SettingsSection::new("Project")
            .id(overview_id)
            .hide_title()
            .child(
                SettingsRow::new("Name")
                    .description("The shared name for this project group in the sidebar and thread lists.")
                    .control(div().w(rems(16.)).child(text_field(&name_input, None, FieldSize::Sm, false, cx))),
            )
            .child(
                SettingsRow::new("Project icon")
                    .description(format!("{} · Automatic", project.monogram))
                    .control(
                        ProjectMonogram::new(project.monogram.clone(), gpui_kit::rgb(project.favicon_color).into())
                            .size(rems(1.5)),
                    ),
            );

        // New threads (category "project": model + workspace).
        let new_threads = SettingsSection::new("New threads")
            .id("project-defaults")
            .child(self.default_model_row(&json, "Model for new threads in this project.", cx))
            .child(self.workspace_row(&json, "Where new threads in this project start.", cx));

        let actions = self.project_actions_section(cx);

        // Danger.
        let remove_id = project.id.clone();
        let remove_name = project.display_name.clone();
        let remove_root = project.workspace_root.clone();
        let danger = SettingsSection::new("Danger").child(
            SettingsRow::new("Remove project")
                .description("Deletes the project entry and its threads. Files on disk are not touched.")
                .control(
                    Button::new("remove-project")
                        .variant(ButtonVariant::DestructiveOutline)
                        .size(ButtonSize::Sm)
                        .icon(IconName::Trash)
                        .label("Remove project")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let id = remove_id.clone();
                            this.dialog = Some(Dialog::Confirm(ConfirmSpec {
                                title: format!("Remove project \"{remove_name}\"?").into(),
                                description: format!(
                                    "Path: {remove_root}\nThis permanently clears any archived conversation history. This removes only the project entry, not the files on disk. This action cannot be undone."
                                )
                                .into(),
                                confirm_label: "Remove project".into(),
                                destructive: true,
                                on_confirm: std::rc::Rc::new(move |this, _, cx| {
                                    let source = this.store.read(cx).source();
                                    source.remove_project(&id, cx);
                                    this.panels.projects.selected = None;
                                }),
                            }));
                            cx.notify();
                        })),
                ),
        );
        vec![
            SettingsSection::new("Project picker").hide_title().child(picker_row).into_any_element(),
            overview.into_any_element(),
            new_threads.into_any_element(),
            actions,
            danger.into_any_element(),
        ]
    }

    fn project_actions_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let scripts = self.project_scripts(cx);
        let mac = vm.is_mac();
        let add = Button::new("add-project-action")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Xs)
            .icon(IconName::Plus)
            .icon_size(rems(0.875))
            .label("Add action")
            .on_click(cx.listener(|this, _, window, cx| this.open_script_editor(None, window, cx)));
        let (actions_id, _) = searchable_setting("project-actions");
        let mut section = SettingsSection::new("Actions").id(actions_id).child(
            SettingsRow::new("Actions")
                .description("Commands that run in this project's checkout or its worktree, with optional shortcuts.")
                .control(add),
        );
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
            let shortcut = vm.keybindings.rules.iter().find(|rule| rule.command.as_str() == command).map(|rule| {
                super::keybindings::keybinding_pill(
                    &super::super::keybindings_logic::shortcut_to_keybinding_input(&rule.shortcut),
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
                .child(Icon::new(script_icon(&icon)).size(rems(1.)).color(theme.muted_foreground))
                .child(div().min_w_0().truncate().child(script.name.as_str().to_owned()))
                .when(script.run_on_worktree_create, |this| this.child(tag("setup")))
                .when(script.run_on_settle == Some(true), |this| this.child(tag("on settle")));
            let edit_id = id.clone();
            let name = script.name.as_str().to_owned();
            section = section.child(
                SettingsRow::new(title)
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
                            .children(shortcut)
                            .child(
                                Button::new(SharedString::from(format!("edit-action-{id}")))
                                    .variant(ButtonVariant::GhostMuted)
                                    .size(ButtonSize::IconXs)
                                    .icon(IconName::Settings2)
                                    .icon_size(rems(0.875))
                                    .tooltip(Tooltip::new(format!("Edit {name}")))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_script_editor(Some(edit_id.clone()), window, cx)
                                    })),
                            ),
                    ),
            );
        }
        section.into_any_element()
    }

    fn open_script_editor(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let scripts = self.project_scripts(cx);
        let existing = id.as_ref().and_then(|id| scripts.iter().find(|s| s.id.as_str() == id));
        let (name, command, setup) = existing
            .map(|s| (s.name.as_str().to_owned(), s.command.as_str().to_owned(), s.run_on_worktree_create))
            .unwrap_or_default();
        self.panels.projects.script = ScriptDraft { id, run_on_worktree_create: setup };
        for (key, placeholder, value) in [("script-name", "Test", name), ("script-command", "bun test", command)] {
            let state = self.input(key, &value, placeholder, window, cx, |_, _, _, _| {});
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.dialog = Some(Dialog::ScriptEditor);
        cx.notify();
    }

    fn save_scripts(&mut self, scripts: Vec<ProjectScript>, cx: &mut Context<Self>) {
        let value = serde_json::to_value(scripts).unwrap_or(Value::Null);
        self.update_server(json!({ "defaultProjectScripts": value }), cx);
    }

    /// `ProjectScriptEditorDialog`, reduced to name, command and the setup toggle.
    pub(in crate::settings) fn render_script_editor_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let draft = self.panels.projects.script.clone();
        let editing = draft.id.is_some();
        let name = self.input("script-name", "", "Test", window, cx, |_, _, _, _| {});
        let command = self.input("script-command", "", "bun test", window, cx, |_, _, _, _| {});
        let read = |this: &Self, key: &str, cx: &gpui_kit::App| {
            this.inputs
                .get(&SharedString::from(key.to_owned()))
                .map(|state| state.read(cx).value().trim().to_owned())
                .unwrap_or_default()
        };
        let can_save = !read(self, "script-name", cx).is_empty() && !read(self, "script-command", cx).is_empty();
        let label = |text: &'static str| {
            div().text_size(TEXT_SM.size).font_weight(FontWeight::MEDIUM).text_color(theme.foreground).child(text)
        };
        let entity = cx.entity();
        let setup_switch = switch("script-setup", draft.run_on_worktree_create, false, cx, move |next, _, cx| {
            entity.update(cx, |this, cx| {
                this.panels.projects.script.run_on_worktree_create = next;
                cx.notify();
            });
        });
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
                    let remaining: Vec<ProjectScript> =
                        this.project_scripts(cx).into_iter().filter(|s| s.id.as_str() != delete_id).collect();
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
                .label(if editing { "Save changes" } else { "Add action" })
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
                            let ids: Vec<String> = scripts.iter().map(|s| s.id.as_str().to_owned()).collect();
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
