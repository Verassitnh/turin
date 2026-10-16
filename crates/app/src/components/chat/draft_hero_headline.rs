//! Port of upstream/apps/web/src/components/chat/DraftHeroHeadline.tsx
//!
//! The line above an empty draft's composer: "What should we build in
//! {project}?" with the project as an inline picker, "{project} to start"
//! when none is chosen yet, or "What should we work on?" for a draft that
//! lives outside any project, with "or start without a project" as the way
//! into that home.
//!
//! Upstream derives the picker's entries inside the component from the
//! entity stores. Here the host builds a [`DraftHeroModel`] (the entries,
//! the active group and the scratch state) and answers three intents through
//! [`DraftHeroActions`]: choose a project, add one, start without one. The
//! rules that stay in the component (the picker items, the filter, the
//! heading text, which variant shows) are the methods of the model.
//!
//! Not ported: `tracking-tight` on the heading, the `data-popup-open` decoration
//! on the open trigger (the decoration follows hover), and the
//! `latestTargetRef` guard (the host serialises its own project changes).

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AppContext as _, Focusable as _};
use gpui_kit::{
    Bounds, Context, CursorStyle, Div, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, point, px, rems, size,
};
use turin_contracts::environment::EnvironmentMachineKind;

use crate::components::branch_toolbar::env_selectors::environment_machine_icon;
use crate::components::project_favicon::ProjectFavicon;
use crate::composer::views::chrome::{BoundsCell, floating, track_bounds};
use crate::state::models::EnvironmentProject;
use crate::theme::ActiveAppTheme as _;
use crate::theme::AppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_SM;
use crate::ui::combobox::{combobox_contains, combobox_empty};
use crate::ui::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::ui::{Icon, IconName, Tooltip};

/// Picker value for the "Add project" row; real entries are keyed by logical
/// project key.
pub const ADD_PROJECT_VALUE: &str = "add-project";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerItem {
    pub value: String,
    pub label: String,
}

/// `ProjectEnvironmentBadge`: the machine icon of a row whose group has a
/// member on another environment, with the environment names as its tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftHeroEnvironmentBadge {
    pub machine: EnvironmentMachineKind,
    pub description: String,
}

/// One picker row (`menuEntries`).
#[derive(Clone, Debug)]
pub struct DraftHeroProject {
    pub project_key: String,
    pub display_name: String,
    /// The target project, for its favicon.
    pub project: Option<EnvironmentProject>,
    pub environment_badge: Option<DraftHeroEnvironmentBadge>,
}

#[derive(Clone, Debug, Default)]
pub struct DraftHeroModel {
    /// The picker rows without the project that hosts threads without a
    /// project: that one is reached through the line under the headline.
    pub menu_entries: Vec<DraftHeroProject>,
    /// `projectPickerEntries.length > 0`.
    pub can_choose_project: bool,
    /// The logical key of the active project's group; empty without one.
    pub active_project_key: String,
    /// The active group's display name.
    pub active_group_name: Option<String>,
    /// `activeProjectTitle`.
    pub active_project_title: Option<String>,
    pub is_scratch_draft: bool,
    /// `scratchWorkspaceRoot !== null`: threads can skip a project.
    pub scratch_available: bool,
    /// `projectGroupsSpanEnvironments`.
    pub show_project_environments: bool,
    /// `shortcutLabelForCommand(keybindings, "chat.newWithoutProject")`.
    pub no_project_shortcut: Option<String>,
}

/// Which sentence the heading is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeroHeading {
    /// "What should we work on?"
    Scratch,
    /// "What should we build in {project}?"
    BuildIn,
    /// "{project} to start"
    ChooseToStart,
    /// "Add a project to start"
    AddProject,
}

impl DraftHeroModel {
    /// `activeProjectGroup?.displayName ?? activeProjectTitle`.
    pub fn active_project_display_name(&self) -> Option<&str> {
        self.active_group_name
            .as_deref()
            .or(self.active_project_title.as_deref())
    }

    /// `activeProjectTitle !== null`.
    pub fn has_resolved_project(&self) -> bool {
        self.active_project_title.is_some()
    }

    pub fn heading(&self) -> HeroHeading {
        if self.is_scratch_draft {
            HeroHeading::Scratch
        } else if self.has_resolved_project() {
            HeroHeading::BuildIn
        } else if self.can_choose_project {
            HeroHeading::ChooseToStart
        } else {
            HeroHeading::AddProject
        }
    }

    /// The heading's accessible name: a full sentence, never the widget
    /// state of the picker inside it. GPUI has no accessibility tree yet.
    #[allow(dead_code)]
    pub fn heading_label(&self) -> String {
        match self.heading() {
            HeroHeading::Scratch => "What should we work on?".to_owned(),
            HeroHeading::BuildIn => format!(
                "What should we build in {}?",
                self.active_project_display_name().unwrap_or_default()
            ),
            HeroHeading::ChooseToStart => format!(
                "{} to start",
                self.active_project_display_name()
                    .unwrap_or("Choose a project")
            ),
            HeroHeading::AddProject => "Add a project to start".to_owned(),
        }
    }

    /// `pickerItems`: the projects, then the "Add project" action.
    pub fn picker_items(&self) -> Vec<PickerItem> {
        self.menu_entries
            .iter()
            .map(|entry| PickerItem {
                value: entry.project_key.clone(),
                label: entry.display_name.clone(),
            })
            .chain(std::iter::once(PickerItem {
                value: ADD_PROJECT_VALUE.to_owned(),
                label: "Add project".to_owned(),
            }))
            .collect()
    }

    /// `filteredPickerItems`: "Add project" is an action, not a project, so
    /// it only trails the unfiltered list.
    pub fn filtered_picker_items(&self, query: &str) -> Vec<PickerItem> {
        let query = turin_contracts::base_schemas::js_trim(query);
        let items = self.picker_items();
        if query.is_empty() {
            return items;
        }
        items
            .into_iter()
            .filter(|item| item.value != ADD_PROJECT_VALUE && combobox_contains(&item.label, query))
            .collect()
    }

    /// `selectedPickerItem`: the row Base UI marks selected; the popup
    /// highlights by keyboard position instead.
    #[allow(dead_code)]
    pub fn selected_picker_item(&self) -> Option<PickerItem> {
        self.picker_items()
            .into_iter()
            .find(|item| item.value == self.active_project_key)
    }

    /// The trigger's text.
    pub fn trigger_text(&self) -> String {
        if self.is_scratch_draft {
            "No project".to_owned()
        } else {
            self.active_project_display_name()
                .unwrap_or("Choose a project")
                .to_owned()
        }
    }

    /// The trigger's tooltip: the full name, for a real project.
    pub fn trigger_tooltip(&self) -> Option<String> {
        self.active_project_display_name()
            .filter(|_| !self.is_scratch_draft)
            .map(str::to_owned)
    }

    /// `orStartWithoutProject` is shown.
    pub fn shows_start_without_project(&self) -> bool {
        self.scratch_available
            && !self.is_scratch_draft
            && (self.has_resolved_project() || self.can_choose_project)
    }
}

type OnKey = Rc<dyn Fn(&str, &mut Window, &mut gpui_kit::App)>;
type OnIntent = Rc<dyn Fn(&mut Window, &mut gpui_kit::App)>;

/// What the host does when the headline asks.
#[derive(Clone)]
pub struct DraftHeroActions {
    /// A project row was chosen, by its logical project key.
    pub select_project: OnKey,
    /// `openCommandPalette({ open: "add-project" })`.
    pub add_project: OnIntent,
    /// `startScratch()`; true once the draft moved to the no-project home.
    pub start_scratch: Rc<dyn Fn(&mut Window, &mut gpui_kit::App) -> bool>,
}

pub struct DraftHeroHeadline {
    model: DraftHeroModel,
    actions: DraftHeroActions,
    open: bool,
    highlighted: usize,
    query: String,
    search: Entity<InputState>,
    trigger_bounds: BoundsCell,
    _subscriptions: Vec<Subscription>,
}

impl DraftHeroHeadline {
    pub fn new(
        model: DraftHeroModel,
        actions: DraftHeroActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        let subscriptions = vec![cx.subscribe_in(
            &search,
            window,
            |this, state, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.query = state.read(cx).value().to_string();
                    this.highlighted = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.choose_highlighted(window, cx),
                _ => {}
            },
        )];
        Self {
            model,
            actions,
            open: false,
            highlighted: 0,
            query: String::new(),
            search,
            trigger_bounds: Rc::new(Cell::new(Bounds::default())),
            _subscriptions: subscriptions,
        }
    }

    #[allow(dead_code)]
    pub fn set_model(&mut self, model: DraftHeroModel, cx: &mut Context<Self>) {
        self.model = model;
        cx.notify();
    }

    /// `onOpenChange`: opening or closing resets the query.
    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open = open;
        self.query.clear();
        self.highlighted = 0;
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        if open {
            let focus = self.search.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn choose(&mut self, item: &PickerItem, window: &mut Window, cx: &mut Context<Self>) {
        self.set_open(false, window, cx);
        if item.value == ADD_PROJECT_VALUE {
            (self.actions.add_project)(window, cx);
        } else if item.value != self.model.active_project_key {
            (self.actions.select_project)(&item.value, window, cx);
        }
    }

    fn choose_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.model.filtered_picker_items(&self.query);
        if let Some(item) = items.get(self.highlighted).cloned() {
            self.choose(&item, window, cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.model.filtered_picker_items(&self.query).len();
        match event.keystroke.key.as_str() {
            "escape" => self.set_open(false, window, cx),
            "down" if count > 0 => {
                self.highlighted = (self.highlighted + 1) % count;
                cx.notify();
            }
            "up" if count > 0 => {
                self.highlighted = (self.highlighted + count - 1) % count;
                cx.notify();
            }
            _ => {}
        }
    }

    fn picker(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.app_theme().clone();
        let open = self.open;
        let bounds = self.trigger_bounds.clone();
        let tooltip = self.model.trigger_tooltip();
        let trigger = picker_trigger(
            "draft-project-trigger",
            self.model.trigger_text(),
            open,
            &theme,
        )
        .child(track_bounds(&bounds))
        .on_click(cx.listener(|this, _, window, cx| {
            let open = !this.open;
            this.set_open(open, window, cx);
        }))
        .when_some(tooltip.filter(|_| !open), |this, text| {
            this.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
        });
        let mut root = div().relative().flex().max_w(rems(16.)).child(trigger);
        if open {
            root = root
                .child(
                    div()
                        .id("draft-project-backdrop")
                        .absolute()
                        .top(px(-4000.))
                        .left(px(-4000.))
                        .w(px(8000.))
                        .h(px(8000.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.set_open(false, window, cx)),
                        ),
                )
                .child(self.popup(&theme, cx));
        }
        root.into_any_element()
    }

    /// `ComboboxPopup align="center" className="w-72 overflow-hidden"`.
    fn popup(
        &mut self,
        theme: &crate::theme::AppTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let items = self.model.filtered_picker_items(&self.query);
        let rows: Vec<gpui_kit::AnyElement> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let entry = self
                    .model
                    .menu_entries
                    .iter()
                    .find(|entry| entry.project_key == item.value);
                let leading = if item.value == ADD_PROJECT_VALUE {
                    Some(
                        Icon::new(IconName::FolderPlus)
                            .size(rems(1.))
                            .into_any_element(),
                    )
                } else {
                    entry
                        .and_then(|entry| entry.project.as_ref())
                        .map(|project| {
                            ProjectFavicon::new(project)
                                .size(rems(1.))
                                .into_any_element()
                        })
                };
                let badge = entry
                    .filter(|_| self.model.show_project_environments)
                    .and_then(|entry| entry.environment_badge.clone());
                let picked = item.clone();
                let muted = theme.muted_foreground;
                div()
                    .id(("draft-project-item", index))
                    .cursor(CursorStyle::PointingHand)
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlighted != index {
                            this.highlighted = index;
                            cx.notify();
                        }
                    }))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.choose(&picked, window, cx)),
                    )
                    .child(project_row(
                        item.label.clone(),
                        index == self.highlighted,
                        leading,
                        badge.map(|badge| {
                            let description = badge.description.clone();
                            div()
                                .id(("draft-project-badge", index))
                                .ml_auto()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .text_color(muted)
                                .child(environment_machine_icon(badge.machine).size(rems(0.875)))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(description.clone()).build(window, cx)
                                })
                                .into_any_element()
                        }),
                        theme,
                    ))
                    .into_any_element()
            })
            .collect();
        let bounds = self.trigger_bounds.get();
        // `align="center"` on a `w-72` popup.
        let anchor = Bounds::new(
            point(
                bounds.center().x - rems(18.).to_pixels(px(16.)) / 2.,
                bounds.top(),
            ),
            size(px(0.), bounds.size.height),
        );
        let content =
            div()
                .w(rems(18.))
                .overflow_hidden()
                .rounded(crate::theme::tokens::RADIUS_LG)
                .border_1()
                .border_color(dropdown_glass_border(theme))
                .bg(dropdown_glass_background(theme))
                .text_color(theme.foreground)
                .shadow(crate::theme::tokens::shadow_popover(theme.is_dark()))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_key(event, window, cx)
                }))
                .child(search_frame(
                    Input::new(&self.search)
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .text_size(TEXT_SM.size),
                    theme,
                ))
                .child(
                    div()
                        .id("draft-project-list")
                        .max_h(rems(18.))
                        .overflow_y_scroll()
                        .p_1()
                        .children(rows)
                        .when(items.is_empty(), |this| {
                            this.child(combobox_empty("No matching projects.", cx))
                        }),
                );
        floating(anchor, false, false, content)
    }
}

/// `InlineButton tone="picker"`: `gap-1.5 text-foreground underline
/// decoration-foreground/30 decoration-dotted underline-offset-4
/// hover:decoration-foreground hover:decoration-solid`, `max-w-64` in the
/// headline. GPUI underlines are solid, so the dotted decoration is a solid
/// 30% rule that goes full on hover or while the popup is open.
fn picker_trigger(
    id: impl Into<ElementId>,
    text: String,
    open: bool,
    theme: &AppTheme,
) -> Stateful<Div> {
    let foreground = theme.foreground;
    div()
        .id(id)
        .relative()
        .flex()
        .flex_shrink_0()
        .min_w_0()
        .max_w(rems(16.))
        .items_center()
        .gap(rems(0.375))
        .font_weight(FontWeight::MEDIUM)
        .cursor(CursorStyle::PointingHand)
        .text_color(foreground)
        .underline()
        .text_decoration_color(with_alpha(foreground, if open { 1. } else { 0.3 }))
        .hover(move |style| style.text_decoration_color(foreground))
        .child(div().min_w_0().truncate().child(text))
}

/// `ComboboxSearchInput`: `px-3 pt-2.5`, a bottom rule, a search icon.
fn search_frame(input: Input, theme: &AppTheme) -> gpui_kit::AnyElement {
    div()
        .px_3()
        .pt(px(10.))
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(with_alpha(theme.border, 0.7))
                .pb(px(6.))
                .pl(px(20.))
                .h(px(26.))
                .text_size(TEXT_SM.size)
                .text_color(theme.foreground)
                .child(
                    div().absolute().left_0().top(px(6.)).child(
                        Icon::new(IconName::Search)
                            .size(rems(1.))
                            .color(with_alpha(theme.muted_foreground, 0.55).into()),
                    ),
                )
                .child(input),
        )
        .into_any_element()
}

/// `ComboboxItem hideIndicator`: `min-h-7 gap-2 rounded-sm px-2 py-1 text-sm`.
fn project_row(
    label: String,
    highlighted: bool,
    leading: Option<gpui_kit::AnyElement>,
    trailing: Option<gpui_kit::AnyElement>,
    theme: &AppTheme,
) -> Div {
    div()
        .flex()
        .min_h(px(28.))
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(crate::theme::tokens::RADIUS_SM)
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(if highlighted {
            theme.accent_foreground
        } else {
            theme.foreground
        })
        .when(highlighted, |this| this.bg(theme.accent))
        .children(leading)
        .child(div().min_w_0().flex_1().truncate().child(label))
        .children(trailing)
}

impl Render for DraftHeroHeadline {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let model = self.model.clone();
        let text_color = theme.foreground;
        // `text-2xl sm:text-3xl font-normal text-foreground`.
        let heading = div()
            .flex()
            .w_full()
            .items_baseline()
            .justify_center()
            .text_size(rems(1.875))
            .line_height(rems(2.25))
            .font_weight(FontWeight::NORMAL)
            .text_color(text_color)
            .whitespace_nowrap();
        let heading = match model.heading() {
            HeroHeading::Scratch => heading.child("What should we work on?"),
            HeroHeading::BuildIn => heading
                .child("What should we build in\u{a0}")
                .child(self.picker(cx))
                .child("?"),
            HeroHeading::ChooseToStart => heading.child(self.picker(cx)).child("\u{a0}to start"),
            HeroHeading::AddProject => heading.child("Add a project to start"),
        };
        let subline = model.scratch_available.then(|| {
            let content = if model.is_scratch_draft {
                Some(self.picker(cx))
            } else if model.shows_start_without_project() {
                let shortcut = model.no_project_shortcut.clone();
                let actions = self.actions.clone();
                let theme = theme.clone();
                Some(
                    crate::components::usage::usage_ui::inline_button(
                        "draft-start-without-project",
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(move |_, _, window, cx| {
                        (actions.start_scratch)(window, cx);
                    }))
                    .when_some(shortcut, |this, shortcut| {
                        this.tooltip(move |window, cx| {
                            Tooltip::new(shortcut.clone()).build(window, cx)
                        })
                    })
                    .child("or start without a project")
                    .into_any_element(),
                )
            } else {
                None
            };
            // `mt-2 flex h-6 items-center text-sm`, reserved so the heading
            // does not move.
            div()
                .mt_2()
                .flex()
                .h(rems(1.5))
                .items_center()
                .text_size(TEXT_SM.size)
                .children(content)
        });
        div()
            .mx_auto()
            .flex()
            .flex_col()
            .items_center()
            .w_full()
            .max_w(rems(64.))
            .child(heading)
            .children(subline)
    }
}

/// The mock project catalog behind the headline while the chat column has no
/// draft routing: three projects, the second one active.
pub fn demo_draft_hero_model() -> DraftHeroModel {
    use crate::state::contracts::{EnvironmentId, ProjectId};
    let project = |id: &str, title: &str| EnvironmentProject {
        environment_id: EnvironmentId::new("local").expect("environment id"),
        id: ProjectId::new(id).expect("project id"),
        title: title.to_owned(),
        workspace_root: format!("/Users/me/{title}"),
        favicon_path: None,
        project_icon: None,
        created_at: None,
        updated_at: None,
        is_scratch: false,
        source: None,
    };
    let entry = |id: &str, title: &str| DraftHeroProject {
        project_key: id.to_owned(),
        display_name: title.to_owned(),
        project: Some(project(id, title)),
        environment_badge: None,
    };
    DraftHeroModel {
        menu_entries: vec![
            entry("t3code", "t3code"),
            entry("turin", "turin"),
            entry("website", "website"),
        ],
        can_choose_project: true,
        active_project_key: "turin".to_owned(),
        active_group_name: Some("turin".to_owned()),
        active_project_title: Some("turin".to_owned()),
        is_scratch_draft: false,
        scratch_available: true,
        show_project_environments: false,
        no_project_shortcut: Some("\u{21e7}\u{2318}N".to_owned()),
    }
}

#[cfg(test)]
#[path = "draft_hero_headline_tests.rs"]
mod tests;
