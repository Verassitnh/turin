//! The `@` / `/` / `$` command menu: trigger tracking, item lists, keyboard
//! highlight and selection (ChatComposer.tsx `composerMenuItems` /
//! `onSelectComposerItem`, ComposerCommandMenu.tsx, useComposerTriggerState).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, MouseMoveEvent, ParentElement as _, StatefulInteractiveElement as _, Styled as _,
    Window, div, rems,
};

use crate::components::chat::composer_pending_panels::{BannerVariant, banner_surface};
use crate::composer::command_items::{
    ComposerCommandItem, PathKind, search_slash_command_items,
    slash_command_items_for_prompt_position,
};
use crate::composer::editor_state::EditorSnapshot;
use crate::composer::file_link::serialize_composer_file_link;
use crate::composer::js_string::utf16_len;
use crate::composer::logic::{
    ComposerSlashCommand, ComposerTrigger, ComposerTriggerKind, collapse_expanded_composer_cursor,
    detect_composer_trigger,
};
use crate::composer::menu_highlight::resolve_composer_menu_active_item_id;
use crate::composer::skills::{
    ProviderSkillSourceKind, format_provider_skill_display_name,
    get_provider_skills_for_slash_menu, get_provider_slash_commands_for_slash_menu,
    resolve_provider_skill_source_kind, search_provider_skills,
};
use crate::composer::view_model::{ComposerViewModel, InteractionMode};
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::prompt_editor::CommandKey;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_XS};
use crate::ui::{Icon, IconName};

/// Which trigger the user dismissed with Escape, until the text moves on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DismissedTrigger {
    pub kind: ComposerTriggerKind,
    pub range_start: usize,
}

#[derive(Default)]
pub struct MenuState {
    pub trigger: Option<ComposerTrigger>,
    pub items: Vec<ComposerCommandItem>,
    pub highlighted: Option<String>,
    pub highlighted_search_key: Option<String>,
    pub dismissed: Option<DismissedTrigger>,
}

impl MenuState {
    fn search_key(&self) -> Option<String> {
        self.trigger
            .as_ref()
            .map(|trigger| format!("{:?}:{}", trigger.kind, trigger.query))
    }

    pub fn is_open(&self) -> bool {
        self.trigger.is_some()
    }

    fn active_item(&self) -> Option<&ComposerCommandItem> {
        let ids: Vec<&str> = self.items.iter().map(ComposerCommandItem::id).collect();
        let key = self.search_key();
        let id = resolve_composer_menu_active_item_id(
            &ids,
            self.highlighted.as_deref(),
            key.as_deref(),
            self.highlighted_search_key.as_deref(),
        )?;
        self.items.iter().find(|item| item.id() == id)
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `composerMenuItems`.
fn build_menu_items(vm: &ComposerViewModel, trigger: &ComposerTrigger) -> Vec<ComposerCommandItem> {
    let entry = vm.selected_entry();
    let provider = vm.selected_instance_id.clone();
    let skills = entry.map(|entry| entry.skills.clone()).unwrap_or_default();
    let commands = entry
        .map(|entry| entry.slash_commands.clone())
        .unwrap_or_default();
    match trigger.kind {
        ComposerTriggerKind::Path => {
            let query = trigger.query.to_lowercase();
            let threads = if query.is_empty() {
                Vec::new()
            } else {
                vm.threads
                    .iter()
                    .filter(|thread| thread.title.to_lowercase().contains(&query))
                    .take(3)
                    .map(|thread| ComposerCommandItem::Thread {
                        id: format!("thread:{}", thread.thread_id),
                        thread_id: thread.thread_id.clone(),
                        label: thread.title.clone(),
                        description: "Thread".to_owned(),
                    })
                    .collect()
            };
            let paths = vm
                .path_entries
                .iter()
                .filter(|entry| query.is_empty() || entry.path.to_lowercase().contains(&query))
                .take(40)
                .map(|entry| ComposerCommandItem::Path {
                    id: format!(
                        "path:{}:{}",
                        if entry.kind == PathKind::Directory {
                            "directory"
                        } else {
                            "file"
                        },
                        entry.path
                    ),
                    path: entry.path.clone(),
                    path_kind: entry.kind,
                    label: basename(&entry.path).to_owned(),
                    description: entry
                        .path
                        .rsplit_once('/')
                        .map(|(dir, _)| dir.to_owned())
                        .unwrap_or_default(),
                });
            threads.into_iter().chain(paths).collect()
        }
        ComposerTriggerKind::SlashCommand => {
            let mut items = vec![ComposerCommandItem::SlashCommand {
                id: "slash:model".into(),
                command: ComposerSlashCommand::Model,
                label: "/model".into(),
                description: "Switch response model for this thread".into(),
            }];
            if vm.settings.plan_mode_enabled {
                items.push(ComposerCommandItem::SlashCommand {
                    id: "slash:plan".into(),
                    command: ComposerSlashCommand::Plan,
                    label: "/plan".into(),
                    description: "Switch this thread into plan mode".into(),
                });
                items.push(ComposerCommandItem::SlashCommand {
                    id: "slash:default".into(),
                    command: ComposerSlashCommand::Default,
                    label: "/default".into(),
                    description: "Switch this thread back to normal build mode".into(),
                });
            }
            let menu_skills =
                get_provider_skills_for_slash_menu(&skills, vm.settings.show_skills_in_slash_menu);
            items.extend(
                get_provider_slash_commands_for_slash_menu(&commands, &menu_skills)
                    .into_iter()
                    .map(|command| ComposerCommandItem::ProviderSlashCommand {
                        id: format!("provider-slash-command:{provider}:{}", command.name),
                        provider: provider.clone(),
                        label: format!("/{}", command.name),
                        description: command
                            .description
                            .clone()
                            .or_else(|| command.input_hint.clone())
                            .unwrap_or_else(|| "Run provider command".into()),
                        command,
                    }),
            );
            items.extend(menu_skills.into_iter().map(|skill| {
                ComposerCommandItem::Skill {
                    id: format!("skill:{provider}:{}", skill.name),
                    provider: provider.clone(),
                    label: format!("/skill:{}", skill.name),
                    description: skill
                        .short_description
                        .clone()
                        .or_else(|| skill.description.clone())
                        .or_else(|| skill.scope.as_ref().map(|scope| format!("{scope} skill")))
                        .unwrap_or_default(),
                    skill,
                }
            }));
            let items = slash_command_items_for_prompt_position(&items, trigger.range_start == 0);
            search_slash_command_items(&items, trigger.query.trim().to_lowercase().as_str())
        }
        ComposerTriggerKind::Skill => search_provider_skills(&skills, &trigger.query, 20)
            .into_iter()
            .map(|skill| ComposerCommandItem::Skill {
                id: format!("skill:{provider}:{}", skill.name),
                provider: provider.clone(),
                label: format_provider_skill_display_name(
                    &skill.name,
                    skill.display_name.as_deref(),
                ),
                description: skill
                    .short_description
                    .clone()
                    .or_else(|| skill.description.clone())
                    .or_else(|| skill.scope.as_ref().map(|scope| format!("{scope} skill")))
                    .unwrap_or_else(|| "Run provider skill".into()),
                skill,
            })
            .collect(),
        ComposerTriggerKind::PullRequest => vm
            .pull_requests
            .iter()
            .filter(|pr| {
                trigger.query.is_empty()
                    || pr.number.to_string().starts_with(&trigger.query)
                    || pr
                        .title
                        .to_lowercase()
                        .contains(&trigger.query.to_lowercase())
            })
            .take(12)
            .map(|pr| ComposerCommandItem::PullRequest {
                id: format!("pull-request:{}", pr.number),
                label: format!("#{}", pr.number),
                description: pr.title.clone(),
                pull_request: pr.clone(),
            })
            .collect(),
    }
}

impl ChatComposer {
    /// Re-detect the trigger under the caret and rebuild its items.
    pub(super) fn after_prompt_changed(
        &mut self,
        snapshot: &EditorSnapshot,
        cx: &mut Context<Self>,
    ) {
        let detected = detect_composer_trigger(&snapshot.value, snapshot.expanded_cursor);
        if let Some(dismissed) = &self.menu.dismissed {
            let same = detected.as_ref().is_some_and(|t| {
                t.kind == dismissed.kind && t.range_start == dismissed.range_start
            });
            if !same {
                self.menu.dismissed = None;
            }
        }
        let visible = detected.filter(|trigger| {
            !self
                .menu
                .dismissed
                .as_ref()
                .is_some_and(|d| d.kind == trigger.kind && d.range_start == trigger.range_start)
        });
        self.menu.trigger = visible;
        self.refresh_menu_items(cx);
        let query = self
            .menu
            .trigger
            .as_ref()
            .filter(|t| t.kind == ComposerTriggerKind::Path)
            .map(|t| t.query.clone());
        if let (Some(query), Some(source)) = (query, self.source(cx)) {
            source.search_paths(&query, cx);
        }
        let path_active = self
            .menu
            .trigger
            .as_ref()
            .is_some_and(|t| t.kind == ComposerTriggerKind::Path);
        self.editor
            .update(cx, |editor, _| editor.set_path_query_active(path_active));
    }

    pub(super) fn refresh_menu_items(&mut self, cx: &mut Context<Self>) {
        let vm = self.view_model(cx);
        self.menu.items = match &self.menu.trigger {
            Some(trigger) => build_menu_items(&vm, trigger),
            None => Vec::new(),
        };
    }

    fn nudge_menu_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.menu.items.is_empty() {
            return;
        }
        let current = self.menu.active_item().map(|item| item.id().to_owned());
        let index = current.and_then(|id| self.menu.items.iter().position(|item| item.id() == id));
        let len = self.menu.items.len() as i32;
        let base = match index {
            Some(index) => index as i32,
            None => {
                if down {
                    -1
                } else {
                    0
                }
            }
        };
        let next = (base + if down { 1 } else { -1 }).rem_euclid(len) as usize;
        self.menu.highlighted = Some(self.menu.items[next].id().to_owned());
        self.menu.highlighted_search_key = self.menu.search_key();
        cx.notify();
    }

    /// The menu's share of `onComposerCommandKey`.
    pub(super) fn on_menu_key(
        &mut self,
        snapshot: &EditorSnapshot,
        key: &CommandKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(trigger) = self.menu.trigger.clone() else {
            return false;
        };
        if key.is_composing {
            return false;
        }
        let plain = !key.alt && !key.ctrl && !key.meta;
        match key.key.as_str() {
            "Escape" => {
                self.menu.dismissed = Some(DismissedTrigger {
                    kind: trigger.kind,
                    range_start: trigger.range_start,
                });
                self.menu.trigger = None;
                self.menu.items.clear();
                cx.notify();
                true
            }
            "ArrowDown" | "ArrowUp" if plain && !key.shift && !self.menu.items.is_empty() => {
                self.nudge_menu_highlight(key.key == "ArrowDown", cx);
                true
            }
            "Enter" | "Tab" if plain && !key.shift => {
                let Some(item) = self.menu.active_item().cloned() else {
                    return false;
                };
                let weak = cx.weak_entity();
                let value = snapshot.value.clone();
                window.defer(cx, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        this.select_menu_item(&item, &trigger, &value, window, cx)
                    })
                    .ok();
                });
                true
            }
            _ => false,
        }
    }

    /// Replace `range` of the markdown prompt and put the caret after it.
    #[allow(clippy::too_many_arguments)]
    fn replace_prompt_range(
        &mut self,
        value: &str,
        start: usize,
        end: usize,
        replacement: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let units: Vec<u16> = value.encode_utf16().collect();
        let start = start.min(units.len());
        let end = end.clamp(start, units.len());
        // A trailing space in the replacement swallows the one already there.
        let end = if replacement.ends_with(' ') && units.get(end) == Some(&(b' ' as u16)) {
            end + 1
        } else {
            end
        };
        self.apply_prompt_replacement(value, start, end, replacement, focus, window, cx);
    }

    /// `applyPromptReplacement`: swap a markdown range and put the caret after it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_prompt_replacement(
        &mut self,
        value: &str,
        start: usize,
        end: usize,
        replacement: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let units: Vec<u16> = value.encode_utf16().collect();
        let start = start.min(units.len());
        let end = end.clamp(start, units.len());
        let mut next = String::from_utf16_lossy(&units[..start]);
        next.push_str(replacement);
        next.push_str(&String::from_utf16_lossy(&units[end..]));
        let cursor =
            collapse_expanded_composer_cursor(&next, start + utf16_len(replacement), false);
        self.editor
            .update(cx, |editor, cx| editor.set_value(&next, cursor, cx));
        self.menu.highlighted = None;
        if focus {
            self.focus_editor_at(cursor, window, cx);
        }
    }

    fn focus_editor_at(&self, cursor: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.focus_at(cursor, window, cx));
    }

    /// `onSelectComposerItem`.
    pub(super) fn select_menu_item(
        &mut self,
        item: &ComposerCommandItem,
        trigger: &ComposerTrigger,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (start, end) = (trigger.range_start, trigger.range_end);
        match item {
            ComposerCommandItem::Path { path, .. } => {
                let replacement = format!("{} ", serialize_composer_file_link(path));
                self.replace_prompt_range(value, start, end, &replacement, true, window, cx);
            }
            ComposerCommandItem::SlashCommand { command, .. } => match command {
                ComposerSlashCommand::Model => {
                    self.replace_prompt_range(value, start, end, "", false, window, cx);
                    self.open_model_picker(window, cx);
                }
                ComposerSlashCommand::Plan | ComposerSlashCommand::Default => {
                    if !self.view_model(cx).settings.plan_mode_enabled {
                        return;
                    }
                    let mode = if *command == ComposerSlashCommand::Plan {
                        InteractionMode::Plan
                    } else {
                        InteractionMode::Default
                    };
                    if let Some(source) = self.source(cx) {
                        source.set_interaction_mode(mode, cx);
                    }
                    self.replace_prompt_range(value, start, end, "", true, window, cx);
                }
            },
            ComposerCommandItem::ProviderSlashCommand { command, .. } => {
                let replacement = format!("/{} ", command.name);
                self.replace_prompt_range(value, start, end, &replacement, true, window, cx);
            }
            ComposerCommandItem::Skill { skill, .. } => {
                let replacement = format!("${} ", skill.name);
                self.replace_prompt_range(value, start, end, &replacement, true, window, cx);
            }
            // Thread and pull-request context references are cut: the label
            // lands as plain text.
            ComposerCommandItem::Thread { label, .. } => {
                let replacement = format!("{label} ");
                self.replace_prompt_range(value, start, end, &replacement, true, window, cx);
            }
            ComposerCommandItem::PullRequest { label, .. } => {
                let replacement = format!("{label} ");
                self.replace_prompt_range(value, start, end, &replacement, true, window, cx);
            }
        }
        let _ = Layer::ModelPicker;
    }

    /// `ComposerCommandMenu`: the drawer above the composer.
    pub(super) fn render_menu_drawer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let trigger = self.menu.trigger.clone()?;
        let theme = cx.app_theme().clone();
        let vm = self.view_model(cx);
        let active_id = self.menu.active_item().map(|item| item.id().to_owned());
        let body = if self.menu.items.is_empty() {
            let text = if vm.path_search_pending && trigger.kind == ComposerTriggerKind::Path {
                "Searching workspace files..."
            } else {
                match trigger.kind {
                    ComposerTriggerKind::Skill => {
                        "No skills found. Try / to browse provider commands."
                    }
                    ComposerTriggerKind::Path => "No matching files or folders.",
                    ComposerTriggerKind::PullRequest => "Finding pull request...",
                    ComposerTriggerKind::SlashCommand => "No matching command.",
                }
            };
            div()
                .px(rems(1.25))
                .pt(rems(0.875))
                .pb(rems(1.75))
                .text_size(TEXT_XS.size)
                .text_color(theme.secondary_label)
                .child(text)
                .into_any_element()
        } else {
            let rows: Vec<AnyElement> = self
                .menu
                .items
                .clone()
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    self.menu_row_element(
                        item,
                        index,
                        active_id.as_deref() == Some(item.id()),
                        &trigger,
                        cx,
                    )
                })
                .collect();
            div()
                .id("composer-command-list")
                .flex()
                .flex_col()
                .max_h(rems(18.))
                .overflow_y_scroll()
                .p_1()
                .children(rows)
                .into_any_element()
        };
        Some(banner_surface(
            BannerVariant::Default,
            false,
            body,
            window,
            &theme,
        ))
    }

    fn menu_row_element(
        &mut self,
        item: &ComposerCommandItem,
        index: usize,
        active: bool,
        trigger: &ComposerTrigger,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let id = item.id().to_owned();
        let selected = item.clone();
        let selected_trigger = trigger.clone();
        let icon = match item {
            ComposerCommandItem::Path { path_kind, .. } => Some(
                Icon::new(if *path_kind == PathKind::Directory {
                    IconName::Folder
                } else {
                    IconName::File
                })
                .color(theme.muted_foreground),
            ),
            ComposerCommandItem::Thread { .. } => {
                Some(Icon::new(IconName::MessagesSquare).color(theme.secondary_label))
            }
            ComposerCommandItem::PullRequest { .. } => {
                Some(Icon::new(IconName::GitPullRequest).color(theme.secondary_label))
            }
            _ => None,
        };
        let label = match (trigger.kind, item) {
            (ComposerTriggerKind::SlashCommand, ComposerCommandItem::Skill { skill, .. }) => {
                format!(
                    "/skill:{}",
                    format_provider_skill_display_name(&skill.name, skill.display_name.as_deref())
                )
            }
            _ => item.label().to_owned(),
        };
        let badge = match item {
            ComposerCommandItem::Skill { skill, .. } => {
                Some(resolve_provider_skill_source_kind(skill))
            }
            _ => None,
        };
        let show_skill_suffix = trigger.kind == ComposerTriggerKind::Skill;
        div()
            .id(("composer-menu-item", index))
            .flex()
            .min_h(rems(2.))
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(RADIUS_LG)
            .cursor(CursorStyle::PointingHand)
            .when(active, |this| {
                this.bg(theme.accent).text_color(theme.accent_foreground)
            })
            .when(!active, |this| {
                this.hover(|style| style.bg(with_alpha(theme.accent, 0.5)))
            })
            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                if this.menu.highlighted.as_deref() != Some(id.as_str()) {
                    this.menu.highlighted = Some(id.clone());
                    this.menu.highlighted_search_key = this.menu.search_key();
                    cx.notify();
                }
            }))
            // Keep the editor focused through the click.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _, window, cx| {
                let value = this.prompt(cx);
                this.select_menu_item(&selected, &selected_trigger, &value, window, cx);
            }))
            .children(icon)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .max_w(gpui_kit::relative(0.45))
                            .flex_shrink_0()
                            .truncate()
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.secondary_label)
                            .child(item.description().to_owned()),
                    )
                    .when_some(badge, |this, kind| {
                        this.child(
                            div()
                                .ml_auto()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap(rems(0.25))
                                .rounded(rems(0.375))
                                .bg(theme.secondary)
                                .px(rems(0.375))
                                .text_size(rems(0.6875))
                                .text_color(theme.secondary_foreground)
                                .child(Icon::new(source_icon(kind)).size(rems(0.75)))
                                .child(format!(
                                    "{}{}",
                                    kind.label(),
                                    if show_skill_suffix { " Skill" } else { "" }
                                )),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn source_icon(kind: ProviderSkillSourceKind) -> IconName {
    match kind {
        ProviderSkillSourceKind::App => IconName::Blocks,
        ProviderSkillSourceKind::Repo | ProviderSkillSourceKind::Project => IconName::Folder,
        ProviderSkillSourceKind::Personal => IconName::UserRound,
        ProviderSkillSourceKind::System => IconName::Settings,
        ProviderSkillSourceKind::Other => IconName::Package,
    }
}
