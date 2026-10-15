//! Port of KeybindingsSettings.tsx: the keybindings editor (search, per-command
//! rows with key recorder and `when` builder, add / reset / remove).
//!
//! The recorder captures a chord from a key press, as upstream does while
//! recording; typing a chord as text (`onChange` on the field) is not
//! supported, the field is read-only apart from recording.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px, rems,
};
use turin_contracts::keybindings::KeybindingWhenNode as Node;

use super::super::controls::{
    FieldSize, PopupAlign, SelectItem, SelectSize, SelectWidth, popup_list, text_field, with_popup,
};
use super::super::keybindings_logic::{
    KeybindingGroup, KeybindingRow, KeybindingSource, KeyboardEvent, WhenExpressionDraft,
    build_keybinding_command_options, build_keybinding_rows, build_when_variable_options,
    command_label, group_keybinding_rows, is_known_when_variable, keybinding_conflict_labels,
    keybinding_from_keyboard_event, parse_when_expression_draft, unknown_when_variables,
    when_ast_to_expression, when_node_remove_label,
};
use super::super::layout::{SettingsRow, SettingsSection, settings_group};
use super::super::search::{keybinding_search_anchor_id, searchable_setting};
use super::super::view::SettingsView;
use super::super::view_model::{RemoveKeybinding, UpsertKeybinding};
use super::super::when_tree::{
    BooleanOperator, build_when_expression_group, condition_parts, default_when_condition,
    default_when_group, flatten_when_children, node_operator, set_condition_identifier,
    set_condition_negated,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_MD, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Badge, BadgeSize, BadgeVariant, Button, ButtonSize, ButtonVariant, Icon, IconName, Kbd,
    KbdGroup, Tooltip,
};

/// Draft state for one binding being edited (`KeybindingRowDraftState`).
#[derive(Clone, Debug)]
pub struct RowDraft {
    pub key: String,
    pub when: Option<Node>,
    /// The text of the expression field.
    pub when_text: String,
    pub when_valid: bool,
}

impl RowDraft {
    fn from_row(row: &KeybindingRow) -> Self {
        Self {
            key: row.key.clone(),
            when: row.binding.when_ast.clone(),
            when_text: row.when.clone(),
            when_valid: true,
        }
    }

    fn empty() -> Self {
        Self {
            key: String::new(),
            when: None,
            when_text: String::new(),
            when_valid: true,
        }
    }
}

#[derive(Default)]
pub struct KeybindingsState {
    pub query: String,
    pub drafts: HashMap<String, RowDraft>,
    /// The row (id) or `"new"` whose recorder is listening.
    pub capture: Option<String>,
    pub adding: bool,
    pub new_command: String,
    pub new_draft: Option<RowDraft>,
}

/// Where a `when` builder writes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WhenTarget {
    Row(String),
    New,
}

impl WhenTarget {
    fn key(&self) -> String {
        match self {
            Self::Row(id) => format!("row:{id}"),
            Self::New => "new".to_owned(),
        }
    }
}

type NodeChange = Rc<dyn Fn(&mut SettingsView, Node, &mut Context<SettingsView>)>;
type NodeRemove = Rc<dyn Fn(&mut SettingsView, &mut Context<SettingsView>)>;

/// `KeybindingPill`.
fn keybinding_pill(value: &str, mac: bool) -> AnyElement {
    let parts: Vec<String> = value
        .split('+')
        .map(|part| match part {
            "mod" => (if mac { "⌘" } else { "Ctrl" }).to_owned(),
            "shift" => "⇧".to_owned(),
            "alt" => (if mac { "⌥" } else { "Alt" }).to_owned(),
            "ctrl" => "⌃".to_owned(),
            other if other.chars().count() == 1 => other.to_uppercase(),
            other => other.to_owned(),
        })
        .collect();
    let mut group = KbdGroup::new();
    for part in parts {
        group = group.child(Kbd::new(part));
    }
    group.into_any_element()
}

fn warning_icon(label: String, cx: &gpui_kit::App) -> AnyElement {
    let theme = cx.app_theme();
    div()
        .id(SharedString::from(format!("warn-{label}")))
        .flex()
        .size_5()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_sm()
        .text_color(theme.warning)
        .hover(|style| style.bg(with_alpha(theme.warning, 0.1)))
        .tooltip({
            let tooltip = Tooltip::new(label);
            move |window, cx| tooltip.clone().build(window, cx)
        })
        .child(Icon::new(IconName::TriangleAlert).size(rems(0.875)).color(theme.warning))
        .into_any_element()
}

fn conflict_warning(labels: &[String], cx: &gpui_kit::App) -> Option<AnyElement> {
    if labels.is_empty() {
        return None;
    }
    let description = if labels.len() == 1 {
        format!("Conflicts with {}.", labels[0])
    } else {
        format!(
            "Conflicts with {}{}.",
            labels.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
            if labels.len() > 3 { ", and more" } else { "" }
        )
    };
    Some(warning_icon(
        format!("{description} The most recent matching binding wins when both conditions can apply."),
        cx,
    ))
}

fn as_handler<F>(handler: &Rc<F>) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static
where
    F: Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
    let handler = handler.clone();
    move |event, window, cx| handler(event, window, cx)
}

fn gpui_to_keyboard_event(event: &KeyDownEvent) -> KeyboardEvent {
    let keystroke = &event.keystroke;
    let key = match keystroke.key.as_str() {
        "space" => " ".to_owned(),
        "up" => "arrowup".to_owned(),
        "down" => "arrowdown".to_owned(),
        "left" => "arrowleft".to_owned(),
        "right" => "arrowright".to_owned(),
        other => other.to_owned(),
    };
    KeyboardEvent {
        key,
        code: String::new(),
        meta_key: keystroke.modifiers.platform,
        ctrl_key: keystroke.modifiers.control,
        alt_key: keystroke.modifiers.alt,
        shift_key: keystroke.modifiers.shift,
    }
}

impl SettingsView {
    pub(super) fn render_keybindings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let vm = self.vm(cx);
        let query = self.panels.keybindings.query.clone();
        let rows = build_keybinding_rows(&vm.keybindings.rules, &query);
        let groups = group_keybinding_rows(&rows);
        let command_options = build_keybinding_command_options(&vm.keybindings.rules);
        let when_variables = build_when_variable_options();
        // Settings search jumps to a command, so only its first row anchors.
        let mut anchors: HashMap<String, String> = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        for row in &rows {
            if seen.insert(row.command.clone()) {
                anchors.insert(row.id.clone(), keybinding_search_anchor_id(&row.command));
            }
        }
        // Forget drafts of rows that no longer exist (ids carry the index).
        let live: std::collections::HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        self.panels.keybindings.drafts.retain(|id, _| live.contains(id.as_str()));
        let mac = vm.is_mac();

        let toolbar = self.keybindings_toolbar(&vm.keybindings.config_path, window, cx);
        let (_, title) = searchable_setting("keybindings");
        let (id, _) = searchable_setting("keybindings");

        let mut sections: Vec<AnyElement> = vec![
            SettingsSection::new(title).id(id).child(toolbar).into_any_element(),
        ];

        let mut body: Vec<AnyElement> = Vec::new();
        if self.panels.keybindings.adding {
            let new_row = self.new_keybinding_row(&rows, &command_options, &when_variables, mac, window, cx);
            body.push(settings_group(vec![new_row], cx).into_any_element());
        }
        if groups.is_empty() {
            let theme = cx.app_theme().clone();
            body.push(
                settings_group(
                    vec![div()
                        .px_4()
                        .py(rems(3.))
                        .text_center()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.muted_foreground)
                        .child("No keybindings match your search.")
                        .into_any_element()],
                    cx,
                )
                .into_any_element(),
            );
        } else {
            for group in &groups {
                body.push(self.keybindings_group(group, &rows, &anchors, &when_variables, mac, window, cx));
            }
        }
        sections.push(div().flex().flex_col().gap_8().children(body).into_any_element());
        sections
    }

    fn keybindings_toolbar(&mut self, config_path: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let query = self.panels.keybindings.query.clone();
        let input = self.input_live("keybindings-search", &query, "Search keybindings", window, cx, |this, text, _, cx| {
            this.panels.keybindings.query = text;
            cx.notify();
        });
        let search = div().flex_1().min_w(rems(12.)).child(super::super::controls::text_field_leading(
            &input,
            None,
            FieldSize::Sm,
            false,
            Some(
                div()
                    .flex()
                    .flex_shrink_0()
                    .child(Icon::new(IconName::Search).size(rems(0.875)).color(theme.muted_foreground))
                    .into_any_element(),
            ),
            cx,
        ));
        let adding = self.panels.keybindings.adding;
        let add = Button::new("add-keybinding")
            .variant(ButtonVariant::Outline)
            .icon(IconName::Plus)
            .label("Add keybinding")
            .disabled(adding)
            .on_click(cx.listener(|this, _, _, cx| {
                this.panels.keybindings.adding = true;
                this.panels.keybindings.new_draft = Some(RowDraft::empty());
                this.panels.keybindings.new_command.clear();
                cx.notify();
            }));
        let open_file = Button::new("open-keybindings-file")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Icon)
            .icon(IconName::FileBraces)
            .disabled(config_path.is_empty())
            .tooltip(Tooltip::new("Open keybindings.json"))
            .on_click(cx.listener(|this, _, _, cx| {
                let source = this.store.read(cx).source();
                source.open_keybindings_file(cx);
            }));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .child(search)
            .child(add)
            .child(open_file)
            .into_any_element()
    }

    /// Demo hook: the first row's id.
    pub(in super::super) fn first_keybinding_row_id(&self, cx: &gpui_kit::App) -> Option<String> {
        let vm = self.vm(cx);
        build_keybinding_rows(&vm.keybindings.rules, "").first().map(|row| row.id.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn keybindings_group(
        &mut self,
        group: &KeybindingGroup,
        all_rows: &[KeybindingRow],
        anchors: &HashMap<String, String>,
        variables: &[String],
        mac: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut section = SettingsSection::new(group.title).id(format!("keybindings-{}", group.id));
        for row in &group.rows {
            section = section.child(self.keybinding_row(row, all_rows, anchors.get(&row.id).cloned(), variables, mac, window, cx));
        }
        section.into_any_element()
    }

    fn draft_for(&mut self, row: &KeybindingRow) -> RowDraft {
        self.panels
            .keybindings
            .drafts
            .entry(row.id.clone())
            .or_insert_with(|| RowDraft::from_row(row))
            .clone()
    }

    pub(super) fn draft_mut(&mut self, target: &WhenTarget) -> Option<&mut RowDraft> {
        match target {
            WhenTarget::Row(id) => self.panels.keybindings.drafts.get_mut(id),
            WhenTarget::New => self.panels.keybindings.new_draft.as_mut(),
        }
    }

    /// `updateExpressionValue`: the tree changed; keep the text field in step.
    fn set_when_value(&mut self, target: &WhenTarget, value: Option<Node>, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft_mut(target) {
            draft.when_text = when_ast_to_expression(value.as_ref());
            draft.when = value;
            draft.when_valid = true;
        }
        cx.notify();
    }

    fn start_capture(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.panels.keybindings.capture = Some(id);
        self.menus.close();
        window.focus(&self.capture_focus, cx);
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn keybinding_row(
        &mut self,
        row: &KeybindingRow,
        all_rows: &[KeybindingRow],
        anchor: Option<String>,
        variables: &[String],
        mac: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let draft = self.draft_for(row);
        let when_expr = when_ast_to_expression(draft.when.as_ref());
        let is_dirty = draft.key != row.key || when_expr != row.when;
        let conflicts = keybinding_conflict_labels(all_rows, &row.id, &draft.key, &when_expr);
        let recording = self.panels.keybindings.capture.as_deref() == Some(row.id.as_str());
        let label = command_label(&row.command);
        let target = WhenTarget::Row(row.id.clone());

        // Title: label + source badge, tooltip names the command id.
        let title = div()
            .id(SharedString::from(format!("kb-title-{}", row.id)))
            .flex()
            .items_center()
            .gap_2()
            .child(label.clone())
            .when(row.source != KeybindingSource::Default, |this| {
                this.child(
                    Badge::label(row.source.label())
                        .variant(BadgeVariant::Outline)
                        .size(BadgeSize::Sm),
                )
            })
            .tooltip({
                let tooltip = Tooltip::new(row.command.clone());
                move |window, cx| tooltip.clone().build(window, cx)
            });

        let when_control = self.when_clause_control(&target, &label, &when_expr, draft.when.as_ref(), variables, window, cx);
        let description = div()
            .flex()
            .h_6()
            .items_center()
            .gap(rems(0.375))
            .child(
                div()
                    .text_size(TEXT_XS.size)
                    .line_height(gpui_kit::relative(1.))
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child("When"),
            )
            .child(when_control);

        // Controls: conflict warning, hover menu, key control.
        let row_group: SharedString = format!("kb-row-{}", row.id).into();
        let can_reset = row.source == KeybindingSource::Custom && row.default_key.is_some();
        let can_remove = row.source != KeybindingSource::Default;
        let menu = (can_reset || can_remove).then(|| self.row_menu(row, can_reset, can_remove, &row_group, cx));

        let key_control = self.key_control(row, &draft, is_dirty, recording, mac, &label, cx);
        let control = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(rems(0.375))
            .children(conflict_warning(&conflicts, cx))
            .children(menu)
            .child(key_control);

        let mut settings_row = SettingsRow::new(title).description(description).control(control);
        if let Some(anchor) = anchor {
            settings_row = settings_row.id(anchor);
        }
        div().group(row_group).child(settings_row).into_any_element()
    }

    fn row_menu(
        &mut self,
        row: &KeybindingRow,
        can_reset: bool,
        can_remove: bool,
        row_group: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let menu_id = SharedString::from(format!("kb-menu-{}", row.id));
        let open = self.menus.is_open(&menu_id);
        let toggle_id = menu_id.clone();
        let trigger = Button::new(SharedString::from(format!("kb-menu-trigger-{}", row.id)))
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconSm)
            .icon(IconName::Ellipsis)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menus.toggle(toggle_id.clone());
                cx.notify();
            }));
        let popup_id = SharedString::from(format!("kb-menu-popup-{}", row.id));
        let popup = open.then(|| {
            let mut items = Vec::new();
            if can_reset {
                items.push(SelectItem::new("reset", "Reset to default"));
            }
            if can_remove {
                items.push(SelectItem::new(
                    "remove",
                    div().text_color(theme.destructive).child("Remove"),
                ));
            }
            let entity = cx.entity();
            let row = row.clone();
            let pick: Rc<dyn Fn(SharedString, &mut Window, &mut gpui_kit::App)> =
                Rc::new(move |item, _, cx| {
                    let row = row.clone();
                    entity.update(cx, |this, cx| {
                        this.menus.close();
                        match item.as_ref() {
                            "reset" => this.reset_keybinding(&row, cx),
                            "remove" => this.remove_keybinding(&row, cx),
                            _ => {}
                        }
                        cx.notify();
                    });
                });
            popup_list(
                popup_id,
                items,
                Some(rems(9.).to_pixels(px(16.))),
                cx,
                pick,
            )
            .into_any_element()
        });
        // Hidden until the row is hovered or the menu is open.
        let hover_group = row_group.clone();
        div()
            .flex()
            .items_center()
            .opacity(if open { 1. } else { 0. })
            .group_hover(hover_group, |style| style.opacity(1.))
            .child(with_popup(trigger, popup, PopupAlign::End, rems(1.75).to_pixels(px(16.))))
            .into_any_element()
    }

    fn reset_keybinding(&mut self, row: &KeybindingRow, cx: &mut Context<Self>) {
        let Some(default_key) = row.default_key.clone() else {
            return;
        };
        self.save_keybinding(
            UpsertKeybinding {
                command: row.command.clone(),
                key: default_key,
                when: (!row.default_when.trim().is_empty()).then(|| row.default_when.clone()),
                replaces: Some(RemoveKeybinding {
                    command: row.command.clone(),
                    key: row.key.clone(),
                    when: (!row.when.trim().is_empty()).then(|| row.when.clone()),
                }),
            },
            cx,
        );
    }

    fn remove_keybinding(&mut self, row: &KeybindingRow, cx: &mut Context<Self>) {
        let source = self.store.read(cx).source();
        source.remove_keybinding(
            RemoveKeybinding {
                command: row.command.clone(),
                key: row.key.clone(),
                when: (!row.when.trim().is_empty()).then(|| row.when.clone()),
            },
            cx,
        );
    }

    fn save_keybinding(&mut self, input: UpsertKeybinding, cx: &mut Context<Self>) {
        let source = self.store.read(cx).source();
        let input = UpsertKeybinding {
            key: input.key.trim().to_owned(),
            when: input.when.map(|when| when.trim().to_owned()).filter(|when| !when.is_empty()),
            ..input
        };
        source.upsert_keybinding(input, cx);
        self.panels.keybindings.adding = false;
        self.panels.keybindings.capture = None;
        cx.notify();
    }

    /// `KeybindingKeyControl`: Save, then the shortcut pill or the recorder field.
    #[allow(clippy::too_many_arguments)]
    fn key_control(
        &mut self,
        row: &KeybindingRow,
        draft: &RowDraft,
        is_dirty: bool,
        recording: bool,
        mac: bool,
        label: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let show_pill = !recording && draft.key == row.key && !row.key.is_empty() && !is_dirty;
        let save = is_dirty.then(|| {
            let row = row.clone();
            let draft = draft.clone();
            Button::new(SharedString::from(format!("kb-save-{}", row.id)))
                .size(ButtonSize::Sm)
                .label("Save")
                .disabled(draft.key.trim().is_empty() || !draft.when_valid)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let when = when_ast_to_expression(draft.when.as_ref());
                    this.save_keybinding(
                        UpsertKeybinding {
                            command: row.command.clone(),
                            key: draft.key.clone(),
                            when: Some(when),
                            replaces: Some(RemoveKeybinding {
                                command: row.command.clone(),
                                key: row.key.clone(),
                                when: (!row.when.trim().is_empty()).then(|| row.when.clone()),
                            }),
                        },
                        cx,
                    );
                }))
                .into_any_element()
        });
        let field: AnyElement = if show_pill {
            let id = row.id.clone();
            div()
                .id(SharedString::from(format!("kb-pill-{}", row.id)))
                .flex()
                .h_7()
                .items_center()
                .mr(px(-6.))
                .px(px(6.))
                .rounded(RADIUS_MD)
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .cursor(CursorStyle::PointingHand)
                .hover(|style| style.border_color(with_alpha(theme.border, 0.7)).bg(theme.accent))
                .tooltip({
                    let tooltip = Tooltip::new(format!("Edit shortcut for {label}"));
                    move |window, cx| tooltip.clone().build(window, cx)
                })
                .on_click(cx.listener(move |this, _, window, cx| this.start_capture(id.clone(), window, cx)))
                .child(keybinding_pill(&row.key, mac))
                .into_any_element()
        } else {
            self.recorder_field(&row.id, &draft.key, &row.key, recording, &theme, cx)
        };
        div()
            .flex()
            .items_center()
            .gap(rems(0.375))
            .children(save)
            .child(field)
            .into_any_element()
    }

    /// The shortcut capture field (`<Input data-keybinding-capture size="sm" font="mono" className="w-44">`).
    fn recorder_field(
        &mut self,
        id: &str,
        draft_key: &str,
        original_key: &str,
        recording: bool,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dark = theme.is_dark();
        let id_owned = id.to_owned();
        let original = original_key.to_owned();
        let text: AnyElement = if recording {
            div().text_color(theme.placeholder).child("Press shortcut").into_any_element()
        } else if draft_key.is_empty() {
            div().text_color(theme.placeholder).child("Unassigned").into_any_element()
        } else {
            div().child(draft_key.to_owned()).into_any_element()
        };
        let mut field = div()
            .id(SharedString::from(format!("kb-recorder-{id}")))
            .flex()
            .items_center()
            .w(rems(11.))
            .h(px(28.))
            .px(px(9.))
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(if recording { theme.ring } else { theme.input })
            .bg(if dark { with_alpha(theme.input, 0.32).into() } else { theme.background })
            .font_family(theme.fonts.mono.clone())
            .text_size(TEXT_SM.size)
            .text_color(theme.foreground)
            .cursor(CursorStyle::IBeam)
            .child(text);
        if recording {
            field = field.track_focus(&self.capture_focus).on_key_down(cx.listener(
                move |this, event: &KeyDownEvent, _, cx| {
                    let key = event.keystroke.key.as_str();
                    cx.stop_propagation();
                    if key == "escape" {
                        let restore = if id_owned == "new" { String::new() } else { original.clone() };
                        this.set_draft_key(&id_owned, restore);
                        this.panels.keybindings.capture = None;
                        cx.notify();
                        return;
                    }
                    let keyboard_event = gpui_to_keyboard_event(event);
                    let platform = this.vm(cx).platform.clone();
                    if let Some(next) = keybinding_from_keyboard_event(&keyboard_event, &platform) {
                        this.set_draft_key(&id_owned, next);
                        this.panels.keybindings.capture = None;
                        cx.notify();
                    }
                },
            ));
        } else {
            let id_owned = id.to_owned();
            field = field.on_click(cx.listener(move |this, _, window, cx| {
                this.start_capture(id_owned.clone(), window, cx)
            }));
        }
        field.into_any_element()
    }

    fn set_draft_key(&mut self, id: &str, key: String) {
        if id == "new" {
            if let Some(draft) = self.panels.keybindings.new_draft.as_mut() {
                draft.key = key;
            }
        } else if let Some(draft) = self.panels.keybindings.drafts.get_mut(id) {
            draft.key = key;
        }
    }

    // ── when clause ─────────────────────────────────────────────────────

    /// `WhenClauseControl`: the inline trigger plus its builder popover.
    #[allow(clippy::too_many_arguments)]
    fn when_clause_control(
        &mut self,
        target: &WhenTarget,
        label: &str,
        expression: &str,
        value: Option<&Node>,
        variables: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let menu_id = SharedString::from(format!("when-{}", target.key()));
        let open = self.menus.is_open(&menu_id);
        let toggle_id = menu_id.clone();
        let trigger = div()
            .id(SharedString::from(format!("when-trigger-{}", target.key())))
            .flex()
            .h(rems(1.25))
            .min_w_0()
            .items_center()
            .gap(rems(0.25))
            .px(px(5.))
            .rounded_sm()
            .text_size(TEXT_2XS.size)
            .font_weight(FontWeight::MEDIUM)
            .cursor(CursorStyle::PointingHand)
            .text_color(if expression.is_empty() { theme.muted_foreground } else { theme.foreground })
            .when(open, |this| this.bg(theme.accent))
            .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
            .tooltip({
                let tooltip = Tooltip::new(format!("Edit when clause for {label}"));
                move |window, cx| tooltip.clone().build(window, cx)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menus.toggle(toggle_id.clone());
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.fonts.mono.clone())
                    .child(if expression.is_empty() { "Always".to_owned() } else { expression.to_owned() }),
            )
            .child(
                div().opacity(0.6).child(
                    Icon::new(IconName::ChevronDown).size(rems(0.875)).color(theme.foreground),
                ),
            );
        let popup = open.then(|| self.when_builder(target, value, variables, window, cx));
        div()
            .min_w_0()
            .child(with_popup(trigger, popup, PopupAlign::Start, rems(1.25).to_pixels(px(16.))))
            .into_any_element()
    }

    /// `WhenExpressionBuilder` in its popover surface.
    fn when_builder(
        &mut self,
        target: &WhenTarget,
        value: Option<&Node>,
        variables: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let expression = when_ast_to_expression(value);
        let text = self
            .draft_mut(target)
            .map(|draft| draft.when_text.clone())
            .unwrap_or_else(|| expression.clone());
        let parsed = parse_when_expression_draft(&text);
        let parse_error = match &parsed {
            WhenExpressionDraft::Err { message } => Some(message.clone()),
            _ => None,
        };
        let unknown: Vec<String> = match &parsed {
            WhenExpressionDraft::Ok(node) => unknown_when_variables(node.as_ref()),
            _ => Vec::new(),
        };

        let expr_target = target.clone();
        let input = self.input_live(
            &format!("when-expr-{}", target.key()),
            &text,
            "Always",
            window,
            cx,
            move |this, text, _, cx| {
                let result = parse_when_expression_draft(&text);
                if let Some(draft) = this.draft_mut(&expr_target) {
                    draft.when_text = text;
                    match result {
                        WhenExpressionDraft::Ok(value) => {
                            draft.when = value;
                            draft.when_valid = true;
                        }
                        WhenExpressionDraft::Err { .. } => draft.when_valid = false,
                    }
                }
                cx.notify();
            },
        );

        let root_condition = {
            let target = target.clone();
            let current = value.cloned();
            Rc::new(cx.listener(move |this, _: &gpui_kit::ClickEvent, _, cx| {
                let next = match &current {
                    None => default_when_condition(),
                    Some(value) => Node::And {
                        left: Box::new(value.clone()),
                        right: Box::new(default_when_condition()),
                    },
                };
                this.set_when_value(&target, Some(next), cx);
            }))
        };
        let root_group = {
            let target = target.clone();
            let current = value.cloned();
            Rc::new(cx.listener(move |this, _: &gpui_kit::ClickEvent, _, cx| {
                let group = default_when_group(BooleanOperator::Or);
                let next = match &current {
                    None => group,
                    Some(value) => Node::And {
                        left: Box::new(value.clone()),
                        right: Box::new(group),
                    },
                };
                this.set_when_value(&target, Some(next), cx);
            }))
        };

        let plus_button = |id: &str, label: &'static str, variant: ButtonVariant| {
            Button::new(SharedString::from(format!("{id}-{}", target.key())))
                .variant(variant)
                .size(ButtonSize::Compact)
                .icon(IconName::Plus)
                .icon_size(rems(0.875))
                .label(label)
        };

        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .text_size(TEXT_SM.size)
                    .font_weight(FontWeight::MEDIUM)
                    .child("When"),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .child(plus_button("when-add-condition", "Condition", ButtonVariant::Outline).on_click(as_handler(&root_condition)))
                    .child(plus_button("when-add-group", "Group", ButtonVariant::Outline).on_click(as_handler(&root_group))),
            );

        let expression_field = {
            let mut field = div().relative().child(text_field(&input, None, FieldSize::Sm, true, cx));
            if !unknown.is_empty() {
                field = field.child(
                    div()
                        .absolute()
                        .right(px(6.))
                        .top(px(4.))
                        .child(warning_icon(
                            if unknown.len() == 1 {
                                format!("Unknown condition: {}", unknown[0])
                            } else {
                                format!("Unknown conditions: {}", unknown.join(", "))
                            },
                            cx,
                        )),
                );
            }
            div()
                .flex()
                .flex_col()
                .gap(rems(0.375))
                .child(field)
                .children(parse_error.clone().map(|message| {
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.375))
                        .text_size(TEXT_2XS.size)
                        .text_color(theme.destructive)
                        .child(Icon::new(IconName::CircleX).size(rems(0.875)).color(theme.destructive))
                        .child(message)
                }))
        };

        let editor: AnyElement = match value {
            Some(node) => {
                let change_target = target.clone();
                let on_change: NodeChange = Rc::new(move |this, next, cx| {
                    this.set_when_value(&change_target, Some(next), cx)
                });
                let remove_target = target.clone();
                let on_remove: NodeRemove = Rc::new(move |this, cx| this.set_when_value(&remove_target, None, cx));
                self.when_node_editor(target, node, variables, 0, "r".to_owned(), on_change, Some(on_remove), cx)
            }
            None => div()
                .rounded_md()
                .border_1()
                .border_color(with_alpha(theme.border, 0.8))
                .bg(with_alpha(theme.muted, 0.15))
                .p_3()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(plus_button("when-empty-condition", "Condition", ButtonVariant::Default).on_click(as_handler(&root_condition)))
                        .child(plus_button("when-empty-group", "Group", ButtonVariant::Outline).on_click(as_handler(&root_group))),
                )
                .into_any_element(),
        };
        let editor = div()
            .relative()
            .child(editor)
            .when(parse_error.is_some(), |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_lg()
                        .border_1()
                        .border_color(with_alpha(theme.destructive, 0.3))
                        .bg(with_alpha(theme.background, 0.75))
                        .p_4()
                        .text_center()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.destructive)
                        .child("Fix the expression above to continue editing visually."),
                )
            });

        let surface = div()
            .id(SharedString::from(format!("when-builder-{}", target.key())))
            .occlude()
            .flex()
            .flex_col()
            .gap_3()
            .w(rems(34.))
            .max_w(window.viewport_size().width - px(32.))
            .p_4()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
            .bg(theme.popover)
            .shadow(tokens::shadow_popover(theme.is_dark()))
            .text_color(theme.popover_foreground)
            .child(header)
            .child(expression_field)
            .child(editor);
        surface.into_any_element()
    }

    /// `WhenExpressionNodeEditor`.
    #[allow(clippy::too_many_arguments)]
    fn when_node_editor(
        &mut self,
        target: &WhenTarget,
        node: &Node,
        variables: &[String],
        depth: usize,
        path: String,
        on_change: NodeChange,
        on_remove: Option<NodeRemove>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let remove_button = |this_path: &str, node: &Node, on_remove: &NodeRemove, ml_auto: bool, cx: &mut Context<Self>| {
            let on_remove = on_remove.clone();
            let label = when_node_remove_label(node, depth);
            div()
                .when(ml_auto, |this| this.ml_auto())
                .child(
                    Button::new(SharedString::from(format!("when-remove-{}-{this_path}", target.key())))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::IconSm)
                        .icon(IconName::Minus)
                        .icon_size(rems(0.875))
                        .tooltip(Tooltip::new(label))
                        .on_click(cx.listener(move |this, _, _, cx| on_remove(this, cx))),
                )
                .into_any_element()
        };
        let not_toggle = |id: String, pressed: bool, on_toggle: Rc<dyn Fn(&mut SettingsView, bool, &mut Context<SettingsView>)>, cx: &mut Context<Self>| {
            Button::new(SharedString::from(id))
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Compact)
                .label("Not")
                .pressed(pressed)
                .min_w(rems(2.5))
                .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, !pressed, cx)))
        };

        // A condition: `[Not] [variable ▾] [−]`.
        if let Some((identifier, negated)) = condition_parts(node) {
            let unknown = !is_known_when_variable(&identifier);
            let node_for_negate = node.clone();
            let change = on_change.clone();
            let toggle = not_toggle(
                format!("when-not-{}-{path}", target.key()),
                negated,
                Rc::new(move |this, pressed, cx| change(this, set_condition_negated(&node_for_negate, pressed), cx)),
                cx,
            );
            let mut options: Vec<String> = variables.to_vec();
            if !options.iter().any(|option| *option == identifier) {
                options.insert(0, identifier.clone());
            }
            let node_for_ident = node.clone();
            let change = on_change.clone();
            let select = self.select_control_sized(
                cx,
                &format!("when-var-{}-{path}", target.key()),
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(identifier.clone())
                    .when(unknown, |this| {
                        this.child(Icon::new(IconName::TriangleAlert).size(rems(0.875)).color(theme.warning))
                    })
                    .into_any_element(),
                options
                    .iter()
                    .map(|option| (serde_json::json!(option), div().child(option.clone()).into_any_element(), *option == identifier))
                    .collect(),
                SelectWidth::Full,
                SelectSize::Compact,
                false,
                move |this, value, _, cx| {
                    if let Some(name) = value.as_str() {
                        change(this, set_condition_identifier(&node_for_ident, name), cx);
                    }
                },
            );
            return div()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(with_alpha(theme.border, 0.7))
                .bg(with_alpha(theme.background, 0.6))
                .px_2()
                .py_2()
                .child(toggle)
                .child(div().min_w_0().flex_1().child(select))
                .children(on_remove.as_ref().map(|remove| remove_button(&path, node, remove, false, cx)))
                .into_any_element();
        }

        let connector = |depth: usize, theme: &AppTheme| -> Hsla {
            if depth == 0 {
                theme.border
            } else {
                with_alpha(theme.border, 0.7).into()
            }
        };

        // `not (group)`.
        if let Node::Not { node: inner } = node {
            let node_clone = node.clone();
            let inner_clone = (**inner).clone();
            let change = on_change.clone();
            let toggle = not_toggle(
                format!("when-not-{}-{path}", target.key()),
                true,
                Rc::new(move |this, pressed, cx| {
                    change(this, if pressed { node_clone.clone() } else { inner_clone.clone() }, cx)
                }),
                cx,
            );
            let change = on_change.clone();
            let child_change: NodeChange = Rc::new(move |this, next, cx| {
                change(this, Node::Not { node: Box::new(next) }, cx)
            });
            let child = self.when_node_editor(target, inner, variables, depth + 1, format!("{path}n"), child_change, None, cx);
            let line = with_alpha(theme.border, 0.7);
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(with_alpha(theme.border, 0.7))
                .bg(if depth > 0 { with_alpha(theme.background, 0.5) } else { with_alpha(theme.muted, 0.2) })
                .p_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(toggle)
                        .children(on_remove.as_ref().map(|remove| remove_button(&path, node, remove, true, cx))),
                )
                .child(
                    div()
                        .relative()
                        .pl_4()
                        .child(div().absolute().top_0().bottom_0().left(px(6.)).w(px(1.)).bg(line))
                        .child(div().absolute().top_4().left(px(6.)).h(px(1.)).w(px(10.)).bg(line))
                        .child(child),
                )
                .into_any_element();
        }

        // and / or group.
        let operator = node_operator(node);
        let children = flatten_when_children(node, operator);
        let line = connector(depth, &theme);

        let operator_select = {
            let children = children.clone();
            let change = on_change.clone();
            self.select_control_sized(
                cx,
                &format!("when-op-{}-{path}", target.key()),
                div().child(operator.label()).into_any_element(),
                [BooleanOperator::And, BooleanOperator::Or]
                    .into_iter()
                    .map(|op| (serde_json::json!(op.label()), div().child(op.label()).into_any_element(), op == operator))
                    .collect(),
                SelectWidth::Fixed(6.),
                SelectSize::Compact,
                false,
                move |this, value, _, cx| {
                    let next = if value == serde_json::json!("or") { BooleanOperator::Or } else { BooleanOperator::And };
                    if next == operator {
                        return;
                    }
                    if let Some(node) = build_when_expression_group(&children, next) {
                        change(this, node, cx);
                    }
                },
            )
        };
        let add_condition = {
            let children = children.clone();
            let change = on_change.clone();
            Button::new(SharedString::from(format!("when-add-c-{}-{path}", target.key())))
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Compact)
                .icon(IconName::Plus)
                .icon_size(rems(0.875))
                .label("Condition")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut next = children.clone();
                    next.push(default_when_condition());
                    if let Some(node) = build_when_expression_group(&next, operator) {
                        change(this, node, cx);
                    }
                }))
        };
        let add_group = {
            let children = children.clone();
            let change = on_change.clone();
            Button::new(SharedString::from(format!("when-add-g-{}-{path}", target.key())))
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Compact)
                .icon(IconName::Plus)
                .icon_size(rems(0.875))
                .label("Group")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let nested = if operator == BooleanOperator::And { BooleanOperator::Or } else { BooleanOperator::And };
                    let mut next = children.clone();
                    next.push(default_when_group(nested));
                    if let Some(node) = build_when_expression_group(&next, operator) {
                        change(this, node, cx);
                    }
                }))
        };

        let mut child_elements: Vec<AnyElement> = Vec::new();
        for (index, child) in children.iter().enumerate() {
            let siblings = children.clone();
            let change = on_change.clone();
            let child_change: NodeChange = Rc::new(move |this, next, cx| {
                let mut updated = siblings.clone();
                updated[index] = next;
                if let Some(node) = build_when_expression_group(&updated, operator) {
                    change(this, node, cx);
                }
            });
            let siblings = children.clone();
            let change = on_change.clone();
            let child_remove: NodeRemove = Rc::new(move |this, cx| {
                let mut remaining = siblings.clone();
                remaining.remove(index);
                match build_when_expression_group(&remaining, operator) {
                    Some(node) => change(this, node, cx),
                    None => change(this, default_when_condition(), cx),
                }
            });
            let editor = self.when_node_editor(
                target,
                child,
                variables,
                depth + 1,
                format!("{path}{index}"),
                child_change,
                Some(child_remove),
                cx,
            );
            child_elements.push(
                div()
                    .relative()
                    .pl_4()
                    .child(div().absolute().top_0().bottom_0().left(px(6.)).w(px(1.)).bg(line))
                    .child(div().absolute().top_4().left(px(6.)).h(px(1.)).w(px(10.)).bg(line))
                    .child(editor)
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(if depth > 0 { with_alpha(theme.border, 0.7) } else { with_alpha(theme.border, 0.6) })
            .bg(if depth > 0 { with_alpha(theme.background, 0.55) } else { with_alpha(theme.muted, 0.1) })
            .p_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(operator_select)
                    .child(add_condition)
                    .child(add_group)
                    .children(on_remove.as_ref().map(|remove| remove_button(&path, node, remove, true, cx))),
            )
            .child(div().flex().flex_col().gap_2().children(child_elements))
            .into_any_element()
    }

    // ── new keybinding row ──────────────────────────────────────────────

    fn new_keybinding_row(
        &mut self,
        all_rows: &[KeybindingRow],
        command_options: &[String],
        variables: &[String],
        mac: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let draft = self
            .panels
            .keybindings
            .new_draft
            .get_or_insert_with(RowDraft::empty)
            .clone();
        let command = self.panels.keybindings.new_command.clone();
        let when_expr = when_ast_to_expression(draft.when.as_ref());
        let conflicts = keybinding_conflict_labels(all_rows, "new", &draft.key, &when_expr);
        let recording = self.panels.keybindings.capture.as_deref() == Some("new");
        let label_text = if command.is_empty() { "new keybinding".to_owned() } else { command_label(&command) };
        let can_save = !command.is_empty() && !draft.key.trim().is_empty() && draft.when_valid;
        let _ = mac;

        let when_control = self.when_clause_control(&WhenTarget::New, &label_text, &when_expr, draft.when.as_ref(), variables, window, cx);
        let description = div()
            .flex()
            .h_6()
            .items_center()
            .gap(rems(0.375))
            .child(
                div()
                    .text_size(TEXT_XS.size)
                    .line_height(gpui_kit::relative(1.))
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child("When"),
            )
            .child(when_control);

        let command_select = self.select_control_sized(
            cx,
            "kb-new-command",
            if command.is_empty() {
                div().text_color(theme.placeholder).child("Command").into_any_element()
            } else {
                div().child(command_label(&command)).into_any_element()
            },
            command_options
                .iter()
                .map(|option| (serde_json::json!(option), div().child(command_label(option)).into_any_element(), *option == command))
                .collect(),
            SelectWidth::Fixed(14.),
            SelectSize::Sm,
            false,
            |this, value, _, cx| {
                if let Some(command) = value.as_str() {
                    this.panels.keybindings.new_command = command.to_owned();
                    cx.notify();
                }
            },
        );
        let key_field = self.recorder_field("new", &draft.key, "", recording, &theme, cx);
        let save = {
            let draft = draft.clone();
            let command = command.clone();
            Button::new("kb-new-save")
                .size(ButtonSize::Sm)
                .label("Save")
                .disabled(!can_save)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let when = when_ast_to_expression(draft.when.as_ref());
                    this.save_keybinding(
                        UpsertKeybinding {
                            command: command.clone(),
                            key: draft.key.clone(),
                            when: Some(when),
                            replaces: None,
                        },
                        cx,
                    );
                }))
        };
        let cancel = Button::new("kb-new-cancel")
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconSm)
            .icon(IconName::X)
            .icon_size(rems(0.875))
            .tooltip(Tooltip::new("Cancel"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.panels.keybindings.adding = false;
                this.panels.keybindings.capture = None;
                cx.notify();
            }));
        let control = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(command_select)
            .children(conflict_warning(&conflicts, cx))
            .child(key_field)
            .child(save)
            .child(cancel);
        div()
            .bg(with_alpha(theme.muted, 0.15))
            .child(SettingsRow::new("New keybinding").description(description).control(control))
            .into_any_element()
    }
}

#[allow(dead_code)]
fn _unused(_: ElementId) {}
