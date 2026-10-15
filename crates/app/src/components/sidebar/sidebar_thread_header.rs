//! Port of upstream/apps/web/src/components/sidebar/SidebarThreadHeader.tsx
//! and the project-scope combobox Sidebar.tsx passes into it.
//!
//! One row: the search field spanning it, then the project scope, add
//! project and new thread icon buttons (`SidebarHeaderIconButton`). The
//! scope popup is a searchable list of project groups; right-click (or the
//! gear) on a project opens its settings.

use gpui_kit::component::Selectable;
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, CursorStyle, ElementId, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::sidebar_logic::{
    ProjectScopeItem, SidebarProjectScopeMenuAction, filter_sidebar_project_scope_items,
    reduce_sidebar_project_scope_menu_state,
};
use super::{Sidebar, SidebarEvent};
use crate::components::project_favicon::ProjectFavicon;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, RADIUS_SM, TEXT_SM};
use crate::ui::sidebar::{SidebarInput, SidebarMenuButton, SidebarMenuButtonSize};
use crate::ui::{Icon, IconName, Tooltip};

/// `SidebarHeaderIconButton`: `SidebarMenuButton size="icon"` at `size-7`
/// with a tooltip. Selectable so it can trigger a popover.
#[derive(IntoElement)]
pub struct SidebarHeaderIconButton {
    id: ElementId,
    icon: Option<IconName>,
    leading: Option<AnyElement>,
    tooltip: Tooltip,
    disabled: bool,
    selected: bool,
    on_click: Option<crate::ui::sidebar::ClickHandler>,
}

impl SidebarHeaderIconButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<Tooltip>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            leading: None,
            tooltip: label.into(),
            disabled: false,
            selected: false,
            on_click: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A project favicon in place of the icon (scoped project).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Selectable for SidebarHeaderIconButton {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for SidebarHeaderIconButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut button = SidebarMenuButton::new(self.id)
            .size(SidebarMenuButtonSize::Icon)
            .box_size(rems(1.75))
            .active(self.selected)
            .disabled(self.disabled)
            .tooltip(self.tooltip);
        if let Some(icon) = self.icon {
            button = button.icon(icon);
        }
        if let Some(leading) = self.leading {
            button = button.leading(leading);
        }
        if let Some(handler) = self.on_click {
            button = button.on_click(move |event, window, cx| handler(event, window, cx));
        }
        button
    }
}

impl Sidebar {
    pub(crate) fn render_thread_header(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let snapshot = self.snapshot(cx).clone();
        let has_projects = !snapshot.project_groups.is_empty();
        let searching = self.is_searching();
        let multi_project = snapshot.project_groups.len() > 1;
        let new_thread_label: SharedString = "New thread (⌘N)".into();
        let new_thread_tooltip = if multi_project {
            Tooltip::new(format!(
                "{new_thread_label}\nNew thread in current project: Shift+click (⇧⌘N)"
            ))
        } else {
            Tooltip::new(new_thread_label)
        };

        let search = div()
            .id("sidebar-search")
            .flex()
            .h_8()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap_2()
            .rounded(RADIUS_MD)
            .px_2()
            .py(rems(0.375))
            .text_size(TEXT_SM.size)
            .font_weight(FontWeight::MEDIUM)
            .text_color(scope.sidebar_muted_foreground)
            .cursor(CursorStyle::IBeam)
            .hover(move |style| {
                style
                    .bg(scope.sidebar_row_hover)
                    .text_color(scope.sidebar_foreground)
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.search_key_down(event, window, cx);
            }))
            .child(Icon::new(IconName::Search).color(theme.sidebar_icon_color))
            .child(SidebarInput::new(&self.search_input))
            .when(searching, |this| {
                this.child(
                    div()
                        .id("sidebar-search-clear")
                        .flex()
                        .flex_shrink_0()
                        .size_5()
                        .items_center()
                        .justify_center()
                        .rounded(RADIUS_SM)
                        .cursor(CursorStyle::PointingHand)
                        .text_color(theme.muted_foreground)
                        .hover(move |style| {
                            style
                                .bg(scope.sidebar_row_hover)
                                .text_color(scope.foreground)
                        })
                        .tooltip(|window, cx| Tooltip::new("Clear thread search").build(window, cx))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear_search(window, cx);
                            this.search_input
                                .update(cx, |input, cx| input.focus(window, cx));
                        }))
                        .child(Icon::new(IconName::X).size(rems(0.75))),
                )
            });

        let weak = cx.entity().downgrade();
        let actions = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .when(has_projects, |this| {
                this.child(self.render_project_scope(cx)).child(
                    SidebarHeaderIconButton::new("sidebar-add-project", "Add project")
                        .icon(IconName::FolderPlus)
                        .on_click({
                            let weak = weak.clone();
                            move |_, _, cx| {
                                weak.update(cx, |_, cx| cx.emit(SidebarEvent::AddProject))
                                    .ok();
                            }
                        }),
                )
            })
            .child(
                SidebarHeaderIconButton::new("sidebar-new-thread", new_thread_tooltip)
                    .icon(IconName::SquarePen)
                    .disabled(snapshot.projects.is_empty())
                    .on_click(move |event, _, cx| {
                        let shift = event.modifiers().shift;
                        weak.update(cx, |this, cx| this.handle_new_thread(shift, cx))
                            .ok();
                    }),
            );

        div()
            .flex()
            .items_center()
            .gap_1()
            .child(search)
            .child(actions)
            .into_any_element()
    }

    /// The project-scope combobox: the trigger shows the scoped project's
    /// favicon; the popup searches project groups.
    fn render_project_scope(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self.snapshot(cx).clone();
        let scoped = snapshot
            .project_scope_key
            .as_ref()
            .and_then(|key| {
                snapshot
                    .project_groups
                    .iter()
                    .find(|group| &group.project_key == key)
            })
            .cloned();
        let trigger_label = match &scoped {
            Some(group) => format!("Filter threads by project: {}", group.display_name),
            None => "Filter threads by project".to_string(),
        };
        let mut trigger = SidebarHeaderIconButton::new("sidebar-project-scope", trigger_label);
        trigger = match scoped
            .as_ref()
            .and_then(|group| group.member_project_keys.first())
            .and_then(|key| snapshot.project(key))
        {
            Some(project) => trigger.leading(ProjectFavicon::new(project)),
            None => trigger.icon(IconName::ListFilter),
        };
        let weak = cx.entity().downgrade();
        let scope_input = self.scope_input.clone();
        let open_weak = weak.clone();
        Popover::new("sidebar-project-scope-popover")
            .appearance(false)
            .trigger(trigger)
            .open(self.scope_menu.open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                open_weak
                    .update(cx, |this, cx| {
                        this.scope_menu = reduce_sidebar_project_scope_menu_state(
                            &this.scope_menu,
                            SidebarProjectScopeMenuAction::OpenChanged(open),
                        );
                        this.scope_input.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                            if open {
                                input.focus(window, cx);
                            }
                        });
                        cx.notify();
                    })
                    .ok();
            })
            .content(move |_, _, cx| {
                let Some(sidebar) = weak.upgrade() else {
                    return div().into_any_element();
                };
                let theme = cx.app_theme().clone();
                let (snapshot, query) = {
                    let sidebar = sidebar.read(cx);
                    (sidebar.snapshot(cx).clone(), sidebar.scope_menu.query.clone())
                };
                let mut items = vec![ProjectScopeItem {
                    value: "all".into(),
                    label: "All projects".into(),
                }];
                items.extend(snapshot.project_groups.iter().map(|group| ProjectScopeItem {
                    value: group.project_key.clone(),
                    label: group.display_name.clone(),
                }));
                let filtered = filter_sidebar_project_scope_items(&items, &query, |item, query| {
                    item.label.to_lowercase().contains(&query.to_lowercase())
                });
                let selected = snapshot.project_scope_key.clone().unwrap_or_else(|| "all".into());
                let rows: Vec<AnyElement> = filtered
                    .iter()
                    .map(|item| {
                        let project = snapshot
                            .project_groups
                            .iter()
                            .find(|group| group.project_key == item.value)
                            .and_then(|group| group.member_project_keys.first())
                            .and_then(|key| snapshot.project(key))
                            .cloned();
                        let value = item.value.clone();
                        let settings_value = item.value.clone();
                        let menu_value = item.value.clone();
                        let pick = weak.clone();
                        let settings = weak.clone();
                        let context = weak.clone();
                        let is_selected = selected == item.value;
                        let is_project = item.value != "all";
                        div()
                            .id(SharedString::from(format!("scope-{}", item.value)))
                            .group(SharedString::from(format!("scope-{}", item.value)))
                            .flex()
                            .h_8()
                            .items_center()
                            .gap_2()
                            .rounded(RADIUS_SM)
                            .px_2()
                            .cursor(CursorStyle::PointingHand)
                            .text_size(TEXT_SM.size)
                            .text_color(theme.popover_foreground)
                            .when(is_selected, |this| this.bg(theme.accent))
                            .hover(|style| style.bg(theme.accent))
                            .on_click(move |_, window, cx| {
                                let value = value.clone();
                                pick.update(cx, |this, cx| {
                                    this.set_project_scope((value != "all").then_some(value), cx);
                                    this.scope_menu = reduce_sidebar_project_scope_menu_state(
                                        &this.scope_menu,
                                        SidebarProjectScopeMenuAction::OpenChanged(false),
                                    );
                                    let _ = window;
                                    cx.notify();
                                })
                                .ok();
                            })
                            .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                                if menu_value == "all" {
                                    return;
                                }
                                let value = menu_value.clone();
                                context
                                    .update(cx, |this, cx| {
                                        this.scope_menu = reduce_sidebar_project_scope_menu_state(
                                            &this.scope_menu,
                                            SidebarProjectScopeMenuAction::ProjectSettingsOpened,
                                        );
                                        cx.emit(SidebarEvent::OpenProjectSettings(value));
                                        cx.notify();
                                    })
                                    .ok();
                            })
                            .child(match &project {
                                Some(project) => ProjectFavicon::new(project).into_any_element(),
                                None => Icon::new(IconName::Folder).color(theme.icon_muted).into_any_element(),
                            })
                            .child(div().min_w_0().flex_1().truncate().child(item.label.clone()))
                            .when(is_project, |this| {
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("scope-settings-{settings_value}")))
                                        .flex()
                                        .size_6()
                                        .items_center()
                                        .justify_center()
                                        .rounded(RADIUS_SM)
                                        .text_color(theme.muted_foreground)
                                        .hover(|style| style.text_color(theme.foreground))
                                        .tooltip({
                                            let label = format!("Project settings for {}", item.label);
                                            move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                                        })
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            let value = settings_value.clone();
                                            settings
                                                .update(cx, |this, cx| {
                                                    this.scope_menu = reduce_sidebar_project_scope_menu_state(
                                                        &this.scope_menu,
                                                        SidebarProjectScopeMenuAction::ProjectSettingsOpened,
                                                    );
                                                    cx.emit(SidebarEvent::OpenProjectSettings(value));
                                                    cx.notify();
                                                })
                                                .ok();
                                        })
                                        .child(Icon::new(IconName::Settings).size(rems(0.875))),
                                )
                            })
                            .into_any_element()
                    })
                    .collect();
                div()
                    .w(px(240.))
                    .max_w(rems(18.))
                    .p_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                    .bg(crate::ui::tooltip::dropdown_glass_background(&theme))
                    .shadow(crate::theme::tokens::shadow_popover(theme.is_dark()))
                    .child(
                        div()
                            .flex()
                            .h_8()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .mb_1()
                            .border_b_1()
                            .border_color(with_alpha(theme.border, 0.6))
                            .child(Icon::new(IconName::Search).size(rems(0.875)).color(theme.icon_muted))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(TEXT_SM.size)
                                    .child(gpui_kit::component::input::Input::new(&scope_input).appearance(false).p_0()),
                            ),
                    )
                    .map(|this| {
                        if rows.is_empty() {
                            this.child(
                                div()
                                    .px_2()
                                    .py_4()
                                    .flex()
                                    .justify_center()
                                    .text_size(TEXT_SM.size)
                                    .text_color(theme.muted_foreground)
                                    .child("No matching projects."),
                            )
                        } else {
                            this.children(rows)
                        }
                    })
                    .into_any_element()
            })
            .into_any_element()
    }
}
