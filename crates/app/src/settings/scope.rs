//! Port of upstream/apps/web/src/components/settings/SettingsScopeContext.tsx,
//! upstream/apps/web/src/components/settings/SettingsScopeSentence.tsx,
//! upstream/apps/web/src/components/settings/SettingsScopeNotice.tsx,
//! upstream/apps/web/src/components/settings/useScopedSettings.ts and
//! upstream/apps/web/src/components/settings/useSettingsProjectGroups.ts.
//!
//! The settings scope is two axes: an environment (`machine`) and a project
//! (`project`, optionally one `checkout`). [`ScopeContext`] resolves the current
//! selection against the view model's projects with `settings_logic` (R3),
//! and derives the targets whose effective values the rows display. Writes
//! plan through `plan_scoped_settings_patch`, so a project selection writes
//! project overrides and an environment selection writes the environment.
//!
//! Deviations (PORTING.md): Turin has one local environment, so the context
//! holds that environment only. The scope lives in the view (upstream keeps it
//! in the URL search). `useMemberProjectFiles` (t3.json layers) is cut with
//! `t3ProjectFile`.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rems,
};
use serde_json::Value;
use turin_contracts::settings::ServerSettings;

use super::controls::{PopupAlign, SelectItem, popup_list, toggle_menu, with_popup};
use super::search::{SettingsPath, SettingsScopeKind as SearchScopeKind};
use super::view::{SettingsRoute, SettingsToastKind, SettingsView};
use super::view_model::SettingsViewModel;
use crate::components::project_favicon::ProjectFavicon;
use crate::settings_logic::scoped_settings::{
    ProjectOverrideEntry, ScopedServerWrite, ScopedSettingsEnvironment, ScopedSettingsPlan,
    ScopedSettingsServerConfig, ScopedSettingsTarget, list_project_overrides,
    plan_project_overrides_clear, plan_scoped_settings_clear, plan_scoped_settings_patch,
    resolve_scoped_settings_targets, select_scoped_settings_environments,
};
use crate::settings_logic::settings_scope::{
    ResolvedSettingsScope, SettingsScopeEnvironment, SettingsScopeKind, SettingsScopeSearch,
    resolve_settings_scope,
};
use crate::settings_logic::settings_scope_axis::{
    ALL_ENVIRONMENTS_VALUE, ALL_PROJECTS_VALUE, ScopeEnvironment, environment_axis_value,
    project_axis_value, select_environment_axis, select_project_axis,
    settings_scope_environment_label,
};
use crate::state::contracts::EnvironmentId;
use crate::state::helpers::usage_access::EnvironmentConnectionPhase;
use crate::state::models::EnvironmentProject;
use crate::state::project_grouping::select_project_grouping_settings;
use crate::state::sidebar_project_grouping::{
    SidebarProjectSnapshot, build_sidebar_project_snapshots,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_BASE, TEXT_SM};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// The one environment's id (`EnvironmentId` of the local server).
pub const LOCAL_ENVIRONMENT_ID: &str = "local";

/// Pages whose every row is saved on this client; they have no scope to pick
/// (`SETTINGS_DEVICE_ONLY_PATHS`; snap-shot and connections are cut).
pub fn is_device_only_route(route: SettingsRoute) -> bool {
    matches!(route, SettingsRoute::Section(SettingsPath::Appearance))
}

/// A project whose own value hides the environment's (`SettingOverridingProject`).
#[derive(Debug, Clone, PartialEq)]
pub struct OverridingProject {
    pub project_id: String,
    pub project_key: String,
    pub label: String,
    /// The override, formatted.
    pub value: String,
}

/// `useResolvedSettingsScope`'s result.
pub struct ScopeContext {
    pub search: SettingsScopeSearch,
    pub groups: Vec<SidebarProjectSnapshot<EnvironmentProject>>,
    pub scope: ResolvedSettingsScope<EnvironmentProject>,
    pub environments: Vec<ScopedSettingsEnvironment>,
    pub scope_environments: Vec<SettingsScopeEnvironment>,
    pub targets: Vec<ScopedSettingsTarget>,
    /// The representative target supplies display values.
    pub target: Option<ScopedSettingsTarget>,
}

impl ScopeContext {
    pub fn is_project_scope(&self) -> bool {
        matches!(
            self.scope.kind,
            SettingsScopeKind::Project { .. } | SettingsScopeKind::Checkout { .. }
        )
    }

    /// The scope kind in the search catalog's terms.
    pub fn search_kind(&self) -> SearchScopeKind {
        match self.scope.kind {
            SettingsScopeKind::All => SearchScopeKind::All,
            SettingsScopeKind::Environment { .. } => SearchScopeKind::Environment,
            SettingsScopeKind::Project { .. } => SearchScopeKind::Project,
            SettingsScopeKind::Checkout { .. } => SearchScopeKind::Checkout,
            SettingsScopeKind::Unavailable { .. } => SearchScopeKind::Unavailable,
        }
    }

    pub fn settings(&self) -> Option<&ServerSettings> {
        self.target.as_ref().map(|target| &target.settings)
    }
}

/// `useSettingsProjectGroups`: the sidebar's logical projects, sorted by name.
pub fn settings_project_groups(
    vm: &SettingsViewModel,
) -> Vec<SidebarProjectSnapshot<EnvironmentProject>> {
    let environment_id = EnvironmentId::new(LOCAL_ENVIRONMENT_ID).expect("environment id");
    let label = vm.environment.label.clone();
    let mut groups = build_sidebar_project_snapshots(
        &vm.environment_projects,
        &select_project_grouping_settings(&vm.client),
        false,
        Some(&environment_id),
        |_| Some(label.clone()),
    );
    groups.sort_by(|a, b| {
        crate::state::support::collate::locale_compare(&a.display_name, &b.display_name)
    });
    groups
}

/// The local environment as the scoped-settings planner sees it.
pub fn scoped_environments(vm: &SettingsViewModel) -> Vec<ScopedSettingsEnvironment> {
    vec![ScopedSettingsEnvironment {
        environment_id: EnvironmentId::new(LOCAL_ENVIRONMENT_ID).expect("environment id"),
        label: vm.environment.label.clone(),
        connection_phase: EnvironmentConnectionPhase::Connected,
        server_config: Some(ScopedSettingsServerConfig {
            settings: vm.server.clone(),
            project_settings_overrides_capable: true,
        }),
    }]
}

/// The text generation fallback a project resolves with.
pub fn text_generation_fallback() -> turin_shared::project_settings::TextGenerationFallback<'static>
{
    &|instances, enabled, default_model| {
        turin_shared::server_settings::fallback_text_generation_model_selection(
            instances,
            |id, _driver| enabled(id),
            true,
            default_model,
        )
    }
}

/// Resolves `search` against the view model.
pub fn resolve_scope_context(vm: &SettingsViewModel, search: &SettingsScopeSearch) -> ScopeContext {
    let groups = settings_project_groups(vm);
    let environments = scoped_environments(vm);
    let scope_environments: Vec<SettingsScopeEnvironment> = environments
        .iter()
        .map(|environment| SettingsScopeEnvironment {
            environment_id: environment.environment_id.clone(),
            label: environment.label.clone(),
        })
        .collect();
    let scope = resolve_settings_scope(search, &groups, &scope_environments);
    let selected = select_scoped_settings_environments(&scope, &environments, None);
    let connected: Vec<ScopedSettingsEnvironment> = selected
        .connected_environments
        .iter()
        .map(|environment| (*environment).clone())
        .collect();
    let representative = selected
        .environment
        .map(|environment| environment.environment_id.clone());
    let targets = resolve_scoped_settings_targets(&scope, &connected, text_generation_fallback());
    let target = targets
        .iter()
        .find(|candidate| Some(&candidate.environment_id) == representative.as_ref())
        .or_else(|| targets.first())
        .cloned();
    ScopeContext {
        search: search.clone(),
        groups,
        scope,
        environments,
        scope_environments,
        targets,
        target,
    }
}

impl SettingsView {
    /// The cached [`ScopeContext`] for the current view model and selection.
    pub(super) fn scope_context(&self, cx: &gpui_kit::App) -> Rc<ScopeContext> {
        let vm = self.vm(cx);
        let key = Rc::as_ptr(&vm) as usize;
        if let Some((cached_key, cached_search, context)) = &*self.scope_cache.borrow()
            && *cached_key == key
            && *cached_search == self.scope_search
        {
            return context.clone();
        }
        let context = Rc::new(resolve_scope_context(&vm, &self.scope_search));
        *self.scope_cache.borrow_mut() = Some((key, self.scope_search.clone(), context.clone()));
        context
    }

    /// `selectScope`: the chosen target, back at the top of the page.
    pub(super) fn select_scope(&mut self, next: SettingsScopeSearch, cx: &mut Context<Self>) {
        self.scope_search = next;
        self.menus.close();
        cx.notify();
    }

    /// Jumps to one project, keeping the environment axis (`open()` of an
    /// overriding project).
    pub(super) fn select_project_scope(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        let mut next = SettingsScopeSearch {
            project,
            ..Default::default()
        };
        if let Some(machine) = &self.scope_search.machine {
            next.machine = Some(machine.clone());
        }
        self.select_scope(next, cx);
    }

    /// Runs a plan (`useRunScopedPlan`).
    pub(super) fn run_scoped_plan(&self, plan: ScopedSettingsPlan, cx: &mut gpui_kit::App) {
        if let Some(reason) = plan.unavailable_reason {
            self.toast(SettingsToastKind::Warning, "Setting not saved", reason, cx);
            return;
        }
        let source = self.store.read(cx).source();
        if plan.has_client_write {
            source.update_client(plan.client_patch, cx);
        }
        for ScopedServerWrite { patch, .. } in plan.server_writes {
            if let Ok(value) = serde_json::to_value(&patch) {
                source.update_server(value, cx);
            }
        }
    }

    /// `useUpdateScopedSettings`: a patch at the current scope.
    pub(super) fn update_scoped(&self, patch: Value, cx: &mut gpui_kit::App) {
        let context = self.scope_context(cx);
        let plan = plan_scoped_settings_patch(
            &context.scope,
            &context.environments,
            &patch,
            text_generation_fallback(),
        );
        self.run_scoped_plan(plan, cx);
    }

    /// `useClearScopedSettings`: drop the project overrides for `keys`.
    pub(super) fn clear_scoped_settings(&self, keys: &[&str], cx: &mut gpui_kit::App) {
        let context = self.scope_context(cx);
        if !context.is_project_scope() {
            return;
        }
        let plan = plan_scoped_settings_clear(&context.scope, &context.environments, keys);
        self.run_scoped_plan(plan, cx);
    }

    /// `useClearProjectOverrides`: clear `keys` on specific project entries, from
    /// an environment scope's chain popover. Entries are `(project id, key)`.
    pub(super) fn clear_project_overrides(
        &self,
        entries: Vec<(String, String)>,
        cx: &mut gpui_kit::App,
    ) {
        let context = self.scope_context(cx);
        let keys: Vec<&str> = entries.iter().map(|(_, key)| key.as_str()).collect();
        let projects: Vec<ProjectOverrideEntry> = entries
            .iter()
            .filter_map(|(project_id, _)| {
                Some(ProjectOverrideEntry {
                    environment_id: EnvironmentId::new(LOCAL_ENVIRONMENT_ID).ok()?,
                    project_id: turin_contracts::base_schemas::ProjectId::new(project_id).ok()?,
                })
            })
            .collect();
        let plan = plan_project_overrides_clear(&context.environments, &projects, &keys);
        self.run_scoped_plan(plan, cx);
    }

    /// The projects whose own value hides the environment's for `keys`
    /// (`overridingProjects`).
    pub(super) fn overriding_projects(
        &self,
        keys: &[&'static str],
        cx: &gpui_kit::App,
    ) -> Vec<OverridingProject> {
        let context = self.scope_context(cx);
        if context.is_project_scope() {
            return Vec::new();
        }
        let Some(key) = keys.first().copied() else {
            return Vec::new();
        };
        let overrides = &context
            .environments
            .first()
            .and_then(|environment| environment.server_config.as_ref())
            .map(|config| config.settings.project_settings_overrides.clone())
            .unwrap_or_default();
        list_project_overrides(&context.environments, keys)
            .into_iter()
            .filter_map(|entry| {
                let group = context.groups.iter().find(|group| {
                    group
                        .member_projects
                        .iter()
                        .any(|member| member.project.id == entry.project_id)
                })?;
                let value = overrides
                    .get(&entry.project_id)
                    .and_then(|stored| serde_json::to_value(stored).ok())
                    .and_then(|stored| stored.get(key).cloned())
                    .map(|value| super::setting_inheritance::format_value(key, &value))
                    .unwrap_or_default();
                Some(OverridingProject {
                    project_id: entry.project_id.as_str().to_owned(),
                    project_key: group.project_key.clone(),
                    label: group.display_name.clone(),
                    value,
                })
            })
            .collect()
    }

    // ── SettingsScopeSentence ────────────────────────────────────────────

    /// "Applying settings for <project> on <environment>".
    pub(super) fn settings_scope_sentence(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if is_device_only_route(self.route) {
            return None;
        }
        let theme = cx.app_theme().clone();
        let context = self.scope_context(cx);
        let machine_selected = context.search.machine.is_some()
            || matches!(context.scope.kind, SettingsScopeKind::Checkout { .. });
        let project_menu = self.project_scope_menu(&context, cx);
        let environment_menu = self.environment_scope_menu(&context, cx);
        Some(
            div()
                .flex()
                .flex_wrap()
                .min_w_0()
                .items_center()
                .gap_x(rems(0.375))
                .gap_y_1()
                .px_4()
                .text_size(TEXT_BASE.size)
                .line_height(TEXT_BASE.line_height)
                .text_color(theme.muted_foreground)
                // Each connective stays with its picker so a wrap never strands "on".
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(rems(0.375))
                        .child(div().flex_shrink_0().child("Applying settings for"))
                        .child(project_menu),
                )
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(rems(0.375))
                        .child(div().flex_shrink_0().child(if machine_selected {
                            "on"
                        } else {
                            "across"
                        }))
                        .child(environment_menu),
                )
                .into_any_element(),
        )
    }

    /// `ScopeMenu`'s trigger: the picker `InlineButton` with an icon, label
    /// and chevron.
    fn scope_menu(
        &mut self,
        id: &str,
        aria_label: &str,
        icon: Option<AnyElement>,
        label: String,
        items: Vec<SelectItem>,
        on_pick: Rc<dyn Fn(SharedString, &mut gpui_kit::Window, &mut gpui_kit::App)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let menu_id = SharedString::from(id.to_owned());
        let open = self.menus.is_open(&menu_id);
        let trigger = div()
            .id(SharedString::from(format!("{id}-trigger")))
            .flex()
            .min_w_0()
            .max_w(rems(18.))
            .items_center()
            .gap(rems(0.375))
            .cursor_pointer()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.foreground)
            .underline()
            .text_decoration_color(if open {
                theme.foreground
            } else {
                with_alpha(theme.foreground, 0.3).into()
            })
            .hover(|style| style.text_decoration_color(theme.foreground))
            .on_click(toggle_menu(cx, |this| &mut this.menus, menu_id.clone()))
            .children(icon)
            .child(div().min_w_0().truncate().child(label))
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(rems(0.875))
                    .color(theme.muted_foreground),
            );
        let _ = aria_label;
        let popup = open.then(|| {
            popup_list(
                SharedString::from(format!("{id}-popup")),
                items,
                Some(rems(12.).to_pixels(px(16.))),
                cx,
                on_pick,
            )
            .into_any_element()
        });
        with_popup(
            trigger,
            popup,
            PopupAlign::Start,
            rems(1.5).to_pixels(px(16.)),
        )
        .into_any_element()
    }

    fn project_scope_menu(&mut self, context: &ScopeContext, cx: &mut Context<Self>) -> AnyElement {
        let selected = context
            .groups
            .iter()
            .find(|group| Some(&group.project_key) == context.search.project.as_ref());
        let icon = selected.map(|group| {
            div()
                .flex_shrink_0()
                .child(ProjectFavicon::new(&group.representative).size(rems(0.875)))
                .into_any_element()
        });
        let label = selected
            .map(|group| group.display_name.clone())
            .unwrap_or_else(|| {
                if context.search.project.is_some() {
                    "Unavailable project".to_owned()
                } else {
                    "All projects".to_owned()
                }
            });
        let current = project_axis_value(&context.search);
        let mut items = vec![
            SelectItem::new(ALL_PROJECTS_VALUE, "All projects")
                .selected(current == ALL_PROJECTS_VALUE),
            SelectItem::separator(),
        ];
        for group in &context.groups {
            items.push(
                SelectItem::new(
                    SharedString::from(group.project_key.clone()),
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(ProjectFavicon::new(&group.representative).size(rems(0.875)))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .child(group.display_name.clone()),
                        ),
                )
                .selected(current == group.project_key),
            );
        }
        let entity = cx.entity().downgrade();
        let search = context.search.clone();
        let on_pick = Rc::new(
            move |value: SharedString, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
                let next = select_project_axis(&search, value.as_ref());
                entity
                    .update(cx, |this, cx| this.select_scope(next, cx))
                    .ok();
            },
        );
        self.scope_menu(
            "scope-project",
            "Project scope",
            icon,
            label,
            items,
            on_pick,
            cx,
        )
    }

    fn environment_scope_menu(
        &mut self,
        context: &ScopeContext,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let environments: Vec<ScopeEnvironment> = context
            .scope_environments
            .iter()
            .map(|environment| ScopeEnvironment {
                environment_id: environment.environment_id.clone(),
                label: environment.label.clone(),
                display_url: None,
            })
            .collect();
        let resolved_environment = match &context.scope.kind {
            SettingsScopeKind::Checkout { environment_id, .. } => Some(environment_id.as_str()),
            _ => None,
        };
        let current = environment_axis_value(&context.search, resolved_environment);
        let selected = environments
            .iter()
            .find(|environment| environment.environment_id.as_str() == current);
        let label = match selected {
            Some(environment) => settings_scope_environment_label(environment, &environments),
            None if current != ALL_ENVIRONMENTS_VALUE => "Unavailable environment".to_owned(),
            None => "All environments".to_owned(),
        };
        let mut items = vec![
            SelectItem::new(
                ALL_ENVIRONMENTS_VALUE,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::Layers).size(rems(0.875)))
                    .child("All environments"),
            )
            .selected(current == ALL_ENVIRONMENTS_VALUE),
            SelectItem::separator(),
        ];
        for environment in &environments {
            items.push(
                SelectItem::new(
                    SharedString::from(environment.environment_id.as_str().to_owned()),
                    settings_scope_environment_label(environment, &environments),
                )
                .selected(current == environment.environment_id.as_str()),
            );
        }
        let entity = cx.entity().downgrade();
        let search = context.search.clone();
        let on_pick = Rc::new(
            move |value: SharedString, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
                let next = select_environment_axis(&search, value.as_ref());
                entity
                    .update(cx, |this, cx| this.select_scope(next, cx))
                    .ok();
            },
        );
        self.scope_menu(
            "scope-environment",
            "Environment scope",
            None,
            label,
            items,
            on_pick,
            cx,
        )
    }

    // ── SettingsScopeNotice ──────────────────────────────────────────────

    /// Offers an explicit target change when a category has no settings at this
    /// scope. `target` is where the choices lead.
    pub(super) fn settings_scope_notice(
        &mut self,
        message: &str,
        target: ScopeNoticeTarget,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let context = self.scope_context(cx);
        let choices: Vec<(String, SettingsScopeSearch)> = match target {
            ScopeNoticeTarget::Checkout => context
                .groups
                .iter()
                .filter(|group| {
                    context
                        .search
                        .project
                        .as_ref()
                        .is_none_or(|project| &group.project_key == project)
                })
                .flat_map(|group| {
                    group.member_projects.iter().map(|member| {
                        (
                            format!(
                                "{} · {} · {}",
                                group.display_name,
                                member.environment_label.as_deref().unwrap_or("Environment"),
                                member.project.workspace_root
                            ),
                            SettingsScopeSearch {
                                project: Some(group.project_key.clone()),
                                machine: Some(member.project.environment_id.as_str().to_owned()),
                                checkout: Some(member.physical_project_key.clone()),
                            },
                        )
                    })
                })
                .collect(),
            ScopeNoticeTarget::Project => context
                .groups
                .iter()
                .map(|group| {
                    (
                        group.display_name.clone(),
                        SettingsScopeSearch {
                            project: Some(group.project_key.clone()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ScopeNoticeTarget::Environment => context
                .scope_environments
                .iter()
                .map(|environment| {
                    (
                        environment.label.clone(),
                        SettingsScopeSearch {
                            machine: Some(environment.environment_id.as_str().to_owned()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ScopeNoticeTarget::All => {
                vec![(
                    "Open all environments".to_owned(),
                    SettingsScopeSearch::default(),
                )]
            }
        };
        let mut actions = div().flex().flex_wrap().gap_2();
        for (index, (label, search)) in choices.into_iter().enumerate() {
            actions = actions.child(
                Button::new(SharedString::from(format!("scope-notice-{index}")))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::SmMultiline)
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_scope(search.clone(), cx);
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(crate::theme::tokens::RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(with_alpha(theme.card, 0.4))
            .px_4()
            .py_3()
            .text_size(TEXT_SM.size)
            .text_color(theme.foreground)
            .child(div().child(message.to_owned()))
            .child(actions)
            .into_any_element()
    }
}

/// `SettingsScopeNotice target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeNoticeTarget {
    Environment,
    All,
    Project,
    Checkout,
}

/// The reason a page shows a message instead of its content
/// (`SettingsScopeBoundary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeBoundary {
    /// Render the page.
    Content,
    /// An unavailable selection: the message, with the sentence kept so the
    /// selection can be changed back.
    Unavailable(String),
    /// The selected environment is not connected.
    Reconnect(String),
}

/// `SettingsScopeBoundary`'s page decision.
pub fn scope_boundary(
    route: SettingsRoute,
    scope_kind: &SettingsScopeKind<EnvironmentProject>,
    scope_label: &str,
    connected_environments: usize,
) -> ScopeBoundary {
    // Device-local pages ignore the scope entirely; the project page follows
    // remembered members while a grouping change replaces its URL key.
    if is_device_only_route(route) || route == SettingsRoute::Section(SettingsPath::Projects) {
        return ScopeBoundary::Content;
    }
    if let SettingsScopeKind::Unavailable { message, .. } = scope_kind {
        return ScopeBoundary::Unavailable(message.clone());
    }
    if matches!(scope_kind, SettingsScopeKind::Environment { .. }) && connected_environments == 0 {
        return ScopeBoundary::Reconnect(format!(
            "Reconnect {scope_label} to change its settings."
        ));
    }
    ScopeBoundary::Content
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::mock_source::MockSettingsSource;

    fn vm() -> SettingsViewModel {
        MockSettingsSource::view_model_for_tests()
    }

    #[test]
    fn resolves_all_projects_to_the_environment_defaults() {
        let vm = vm();
        let context = resolve_scope_context(&vm, &SettingsScopeSearch::default());
        assert!(matches!(context.scope.kind, SettingsScopeKind::All));
        assert!(!context.is_project_scope());
        let target = context.target.expect("a representative target");
        assert_eq!(target.project_id, None);
        assert_eq!(target.label, vm.environment.label);
    }

    #[test]
    fn resolves_a_project_to_its_checkout_targets() {
        let vm = vm();
        let groups = settings_project_groups(&vm);
        let first = groups.first().expect("a project");
        let search = SettingsScopeSearch {
            project: Some(first.project_key.clone()),
            ..Default::default()
        };
        let context = resolve_scope_context(&vm, &search);
        assert!(context.is_project_scope());
        assert!(matches!(context.search_kind(), SearchScopeKind::Project));
        assert!(
            context
                .targets
                .iter()
                .all(|target| target.project_id.is_some())
        );
    }

    #[test]
    fn an_unknown_project_is_unavailable_never_all() {
        let vm = vm();
        let search = SettingsScopeSearch {
            project: Some("gone".into()),
            ..Default::default()
        };
        let context = resolve_scope_context(&vm, &search);
        assert!(matches!(
            context.scope.kind,
            SettingsScopeKind::Unavailable { .. }
        ));
        assert!(context.targets.is_empty());
    }

    #[test]
    fn plans_a_project_write_as_an_override_and_an_environment_write_as_the_environment() {
        let vm = vm();
        let groups = settings_project_groups(&vm);
        let project_key = groups[0].project_key.clone();
        let project = resolve_scope_context(
            &vm,
            &SettingsScopeSearch {
                project: Some(project_key),
                ..Default::default()
            },
        );
        let plan = plan_scoped_settings_patch(
            &project.scope,
            &project.environments,
            &serde_json::json!({ "defaultAutoPull": true }),
            text_generation_fallback(),
        );
        assert_eq!(plan.unavailable_reason, None);
        assert_eq!(plan.server_writes.len(), 1);
        assert!(
            plan.server_writes[0]
                .patch
                .project_settings_overrides
                .is_some()
        );

        let environment = resolve_scope_context(&vm, &SettingsScopeSearch::default());
        let plan = plan_scoped_settings_patch(
            &environment.scope,
            &environment.environments,
            &serde_json::json!({ "defaultAutoPull": true }),
            text_generation_fallback(),
        );
        assert_eq!(plan.server_writes.len(), 1);
        assert!(
            plan.server_writes[0]
                .patch
                .project_settings_overrides
                .is_none()
        );
        assert_eq!(plan.server_writes[0].patch.default_auto_pull, Some(true));
    }

    #[test]
    fn rejects_an_environment_wide_key_at_project_scope() {
        let vm = vm();
        let groups = settings_project_groups(&vm);
        let project = resolve_scope_context(
            &vm,
            &SettingsScopeSearch {
                project: Some(groups[0].project_key.clone()),
                ..Default::default()
            },
        );
        let plan = plan_scoped_settings_patch(
            &project.scope,
            &project.environments,
            &serde_json::json!({ "providerHealthRefreshInterval": 5 }),
            text_generation_fallback(),
        );
        assert!(plan.server_writes.is_empty());
        assert!(plan.unavailable_reason.is_some());
    }

    #[test]
    fn device_only_pages_have_no_scope_and_the_project_page_skips_the_boundary() {
        assert!(is_device_only_route(SettingsRoute::Section(
            SettingsPath::Appearance
        )));
        assert!(!is_device_only_route(SettingsRoute::Section(
            SettingsPath::General
        )));
        let unavailable = SettingsScopeKind::<EnvironmentProject>::Unavailable {
            reason: crate::settings_logic::settings_scope::UnavailableReason::ProjectMissing,
            message: "This project is no longer available.".into(),
        };
        assert_eq!(
            scope_boundary(
                SettingsRoute::Section(SettingsPath::General),
                &unavailable,
                "x",
                0
            ),
            ScopeBoundary::Unavailable("This project is no longer available.".into())
        );
        assert_eq!(
            scope_boundary(
                SettingsRoute::Section(SettingsPath::Projects),
                &unavailable,
                "x",
                0
            ),
            ScopeBoundary::Content
        );
        assert_eq!(
            scope_boundary(
                SettingsRoute::Section(SettingsPath::Appearance),
                &unavailable,
                "x",
                0
            ),
            ScopeBoundary::Content
        );
    }
}
