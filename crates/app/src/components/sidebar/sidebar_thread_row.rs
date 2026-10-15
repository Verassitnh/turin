//! `SidebarThreadRow` from upstream/apps/web/src/components/Sidebar.tsx:
//! the card row (active, pinned, working: project line, title, branch/diff
//! line) and the slim row (snoozed, settled), with the status slot, hover
//! actions, pin marker, drag destination badge and jump hint.
//!
//! Hover is tracked per row on the sidebar (`group-hover/sidebar-row`).
//! Files dropped on rows, per-thread unsent-draft markers and terminal
//! status are not ported (composer/terminal tasks).

use std::rc::Rc;

use gpui_kit::component::Selectable;
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, CursorStyle, ElementId, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::runtime::thread_settled::{can_snooze, snooze_wake_label, thread_woke_at};
use super::runtime::thread_sort::resolve_settled_thread_timestamp;
use super::sidebar_gestures::SweepAction;
use super::sidebar_logic::{
    SidebarDropVerb, SidebarSection, SidebarThreadStatus, SidebarV2TopStatusKind,
    format_working_duration_label, has_unseen_completion, resolve_sidebar_thread_status,
    resolve_sidebar_v2_top_status, resolve_thread_last_visited_at, resolve_working_started_at,
    should_recede_sidebar_thread, working_label,
};
use super::sidebar_snooze::resolve_snooze_presets_now;
use super::thread_hover_card::SidebarThreadTooltip;
use super::thread_status_indicators::{
    ThreadPullRequestBadge, ThreadWorktreeIndicator, current_link, format_diff_count,
};
use super::view_model::ThreadRef;
use super::{Sidebar, SidebarEvent, jump_shortcut_label};
use crate::components::drag_region::no_drag;
use crate::components::project_favicon::ProjectFavicon;
use crate::state::models::{
    EnvironmentThreadShell, SettledOverride, resolve_thread_provider_stack,
};
use crate::theme::color::{hex, with_alpha};
use crate::theme::tokens::{
    self, RADIUS_MD, RADIUS_SM, SIDEBAR_CONTENT_INSET, SIDEBAR_ROW_CONTENT_INSET, TEXT_2XS,
    TEXT_3XS, TEXT_SM, TEXT_XS,
};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::timestamp_format::{format_relative_time_label, now_iso, now_ms, parse_timestamp_ms};
use crate::ui::{Icon, IconName, Tooltip};

/// `card` (78px content + 2px block padding) or `slim` (36px).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowVariant {
    Card,
    Slim,
}

/// Card height including its `py-0.5`, and the slim/header heights, in rems.
pub const CARD_ROW_HEIGHT: f32 = 4.875 + 0.25;
pub const SLIM_ROW_HEIGHT: f32 = 2.25;

/// `compactSidebarTimeLabel`.
fn compact_sidebar_time_label(label: &str) -> String {
    if label == "just now" {
        return "now".into();
    }
    label.strip_suffix(" ago").unwrap_or(label).to_string()
}

/// `threadTimeLabel`: last user message, else last update.
pub fn thread_time_label(thread: &EnvironmentThreadShell) -> String {
    let timestamp = thread
        .latest_user_message_at
        .as_deref()
        .unwrap_or(&thread.updated_at);
    compact_sidebar_time_label(&format_relative_time_label(timestamp, now_ms()))
}

/// `settledTimeLabel`: matches the settled sort key.
fn settled_time_label(thread: &EnvironmentThreadShell) -> String {
    resolve_settled_thread_timestamp(thread)
        .map(|timestamp| {
            compact_sidebar_time_label(&format_relative_time_label(&timestamp, now_ms()))
        })
        .unwrap_or_default()
}

/// What a row's status slot shows (`topStatus`).
struct TopStatus {
    label: &'static str,
    icon: Option<IconName>,
    color: Hsla,
    kind: SidebarV2TopStatusKind,
}

fn top_status(
    kind: SidebarV2TopStatusKind,
    thread: &EnvironmentThreadShell,
    theme: &AppTheme,
) -> TopStatus {
    let scope = theme.sidebar_scope();
    let dark = theme.is_dark();
    let (label, icon, color) = match kind {
        // No shimmer: a forever-animating label is noise (upstream note).
        SidebarV2TopStatusKind::Working => (
            working_label(thread),
            Some(IconName::CircleDashed),
            theme.info,
        ),
        SidebarV2TopStatusKind::Waiting => ("Waiting", None, scope.muted_foreground),
        SidebarV2TopStatusKind::Approval => (
            "Approval",
            Some(IconName::ShieldQuestionMark),
            theme.warning_foreground,
        ),
        // `text-indigo-600 dark:text-indigo-300`.
        SidebarV2TopStatusKind::Input => (
            "Input",
            Some(IconName::MessageCircleQuestionMark),
            hex(if dark { 0xa3b3ff } else { 0x4f39f6 }).into(),
        ),
        SidebarV2TopStatusKind::Limited => ("Limited", Some(IconName::CircleAlert), theme.warning),
        SidebarV2TopStatusKind::Failed => ("Failed", Some(IconName::CircleAlert), theme.error),
        SidebarV2TopStatusKind::Woke => ("Woke", Some(IconName::AlarmClock), theme.warning),
        SidebarV2TopStatusKind::Done => ("Done", Some(IconName::CircleCheck), theme.success),
    };
    TopStatus {
        label,
        icon,
        color,
        kind,
    }
}

/// `dropVerbBadge`: the drop's verb on the lifted row (and sweeps).
fn drop_verb_badge(verb: SidebarDropVerb, theme: &AppTheme) -> AnyElement {
    let (icon, label) = match verb {
        SidebarDropVerb::Pin => (IconName::Pin, "Pin"),
        SidebarDropVerb::Unpin => (IconName::PinOff, "Unpin"),
        SidebarDropVerb::Settle => (IconName::CircleCheck, "Settle"),
        SidebarDropVerb::Unsettle => (IconName::Undo2, "Un-settle"),
        SidebarDropVerb::Wake => (IconName::AlarmClockOff, "Wake"),
    };
    div()
        .ml_auto()
        .flex()
        .flex_shrink_0()
        .h_5()
        .items_center()
        .gap_1()
        .rounded(RADIUS_SM)
        .border_1()
        .border_color(with_alpha(theme.primary, 0.4))
        .bg(with_alpha(theme.primary, 0.1))
        .px(rems(0.375))
        .text_size(TEXT_2XS.size)
        .line_height(TEXT_2XS.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.primary)
        .child(Icon::new(icon).size(rems(0.75)).color(theme.primary))
        .child(label)
        .into_any_element()
}

/// `JumpHintBadge`: floats at the row's right edge while ⌘ is held.
fn jump_hint_badge(label: String, theme: &AppTheme) -> AnyElement {
    div()
        .absolute()
        .right(rems(0.375))
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .child(
            div()
                .flex()
                .h_5()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(with_alpha(theme.border, 0.8))
                .bg(with_alpha(theme.background, 0.95))
                .px(rems(0.375))
                .shadow(tokens::shadow_sm(0.05))
                .font_family(theme.fonts.mono.clone())
                .text_size(TEXT_3XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(label),
        )
        .into_any_element()
}

/// A Popover trigger that remembers the open state (Base UI `MenuTrigger`).
#[derive(IntoElement)]
pub(crate) struct SnoozeTrigger {
    id: ElementId,
    open: bool,
    color: Hsla,
    hover: Hsla,
}

impl Selectable for SnoozeTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for SnoozeTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let hover = self.hover;
        div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .h_full()
            .items_center()
            .gap(px(2.))
            .rounded(RADIUS_MD)
            .px(rems(0.375))
            .cursor(CursorStyle::PointingHand)
            .text_size(TEXT_XS.size)
            .text_color(if self.open { hover } else { self.color })
            .hover(move |style| style.text_color(hover))
            .child(Icon::new(IconName::Clock).size(rems(0.75)))
            .tooltip(|window, cx| Tooltip::new("Snooze thread").build(window, cx))
    }
}

/// The hover action buttons (`Settle`, un-settle, wake): a click applies
/// the action; a press that moves sweeps it across neighbouring rows.
fn action_button(
    id: impl Into<ElementId>,
    content: AnyElement,
    tooltip: Option<&'static str>,
    color: Hsla,
    hover: Hsla,
    on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id.into())
        .flex()
        .h_full()
        .items_center()
        .gap_1()
        .rounded(RADIUS_MD)
        .px(rems(0.375))
        .cursor(CursorStyle::PointingHand)
        .text_size(TEXT_XS.size)
        .text_color(color)
        .hover(move |style| style.text_color(hover))
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            on_down(event, window, cx);
        })
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .when_some(tooltip, |this, tooltip| {
            this.tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
        })
        .child(content)
        .into_any_element()
}

/// Everything a row reads, resolved once per frame.
pub(crate) struct RowInput<'a> {
    pub thread: &'a EnvironmentThreadShell,
    pub section: SidebarSection,
    pub is_active: bool,
    pub jump_index: Option<usize>,
    /// The lifted row of a drag (and its current verb).
    pub drag_lifted: bool,
    pub drop_verb: Option<SidebarDropVerb>,
    pub drag_over_pinned: bool,
    pub sweep_action: Option<SweepAction>,
    /// A ghost copy (exit fade): never interactive.
    pub ghost: bool,
}

impl Sidebar {
    pub(crate) fn render_thread_row(
        &mut self,
        input: RowInput<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let thread = input.thread;
        let key = thread.key();
        let snapshot = self.snapshot(cx);
        let project_key = thread.project_key();
        let project = snapshot.project(&project_key).cloned();
        let project_display_name: Option<SharedString> = snapshot
            .project_group_for(&project_key)
            .map(|group| group.display_name.clone().into());
        let capabilities = snapshot.capabilities(&thread.environment_id);
        let settlement_supported = capabilities.is_some_and(|c| c.thread_settlement);
        let snooze_supported = capabilities.is_some_and(|c| c.thread_snooze);
        let pinning_supported = capabilities.is_some_and(|c| c.thread_pinning);
        let local_visited = snapshot.local_last_visited_at.get(&key).cloned();
        let tooltip_card = SidebarThreadTooltip::from_thread(thread, snapshot);

        let variant = match input.section {
            SidebarSection::Active | SidebarSection::Pinned | SidebarSection::Working => {
                RowVariant::Card
            }
            _ => RowVariant::Slim,
        };
        let variant_action = match input.section {
            SidebarSection::Snoozed => SweepAction::Unsnooze,
            SidebarSection::Settled => SweepAction::Unsettle,
            _ => SweepAction::Settle,
        };
        let now = now_iso();
        let last_visited_at = resolve_thread_last_visited_at(
            thread
                .last_visited_at
                .as_ref()
                .map(|value| value.as_deref()),
            local_visited.as_deref(),
        );
        // Never-visited counts as read.
        let is_unread =
            has_unseen_completion(thread.latest_run.as_ref(), last_visited_at.as_deref());
        let status = resolve_sidebar_thread_status(thread);
        let woke_at = thread_woke_at(thread, &now);
        // An unparseable visit counts as never-visited.
        let is_woke = woke_at
            .as_deref()
            .and_then(parse_timestamp_ms)
            .is_some_and(|woke| {
                last_visited_at
                    .as_deref()
                    .and_then(parse_timestamp_ms)
                    .is_none_or(|visited| visited < woke)
            })
            && thread.settled_override != Some(SettledOverride::Settled);
        let is_selected = self.selection.contains(&key);
        let is_hovered = !input.ghost
            && self.sweep.is_none()
            && self.hovered_row.as_deref() == Some(key.as_str());
        let snooze_menu_open = self.snooze_menu_for.as_deref() == Some(key.as_str());
        let show_actions = is_hovered || snooze_menu_open;
        let should_recede =
            should_recede_sidebar_thread(status, is_unread, is_woke, input.is_active, is_selected);
        let top = resolve_sidebar_v2_top_status(status, is_unread, is_woke)
            .map(|kind| top_status(kind, thread, &theme));
        let is_renaming = self.rename.as_ref().is_some_and(|rename| rename.key == key);
        let is_regenerating = thread.title_regeneration.is_some();
        let show_snooze = snooze_supported && can_snooze(thread, &now);
        let weak = cx.entity().downgrade();
        let thread_ref = ThreadRef::of(thread);

        // ── Surface ────────────────────────────────────────────────────
        let surface_bg: Option<Hsla> = if input.drag_lifted || input.is_active {
            Some(scope.sidebar_row_active)
        } else if is_selected || input.sweep_action.is_some() {
            Some(scope.sidebar_row_selected)
        } else if is_hovered {
            Some(scope.sidebar_row_hover)
        } else {
            None
        };
        // Settled rows dim every descendant at rest (`text-secondary-label/70`).
        let settled_rest = variant_action == SweepAction::Unsettle && !is_hovered;
        let secondary: Hsla = if settled_rest {
            with_alpha(theme.secondary_label, 0.7).into()
        } else {
            theme.secondary_label
        };
        let muted_40: Hsla = with_alpha(scope.muted_foreground, 0.4).into();

        // ── Title ──────────────────────────────────────────────────────
        let title_color: Hsla = match variant {
            RowVariant::Card => {
                if should_recede {
                    theme.secondary_label
                } else if is_unread || is_woke || status == SidebarThreadStatus::Input {
                    scope.foreground
                } else if status == SidebarThreadStatus::Failed {
                    with_alpha(scope.foreground, 0.95).into()
                } else {
                    with_alpha(scope.foreground, 0.9).into()
                }
            }
            RowVariant::Slim => {
                if is_hovered {
                    scope.foreground
                } else if should_recede {
                    with_alpha(theme.secondary_label, 0.7).into()
                } else if input.is_active || is_woke || status == SidebarThreadStatus::Input {
                    scope.foreground
                } else if is_unread {
                    scope.muted_foreground
                } else {
                    with_alpha(theme.secondary_label, 0.7).into()
                }
            }
        };
        let title: AnyElement = match (&self.rename, is_renaming) {
            (Some(rename), true) => div()
                .flex_1()
                .min_w_0()
                .rounded(RADIUS_SM)
                .border_1()
                .border_color(scope.foreground)
                .bg(scope.card)
                .px_1()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(scope.card_foreground)
                .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.cancel_rename(cx);
                    }
                }))
                .child(
                    gpui_kit::component::input::Input::new(&rename.input)
                        .appearance(false)
                        .p_0(),
                )
                .into_any_element(),
            _ => div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .font_weight(if should_recede {
                    FontWeight::NORMAL
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(title_color)
                .when(is_regenerating, |this| this.opacity(0.55))
                .child(thread.title.clone())
                .into_any_element(),
        };

        // ── Pin marker (unpin quick action once pinning is supported) ──
        let show_pin = thread.pinned_at.is_some()
            && (!input.drag_lifted || (input.drag_over_pinned && input.drop_verb.is_none()));
        let pin_marker = show_pin.then(|| {
            let color: Hsla = with_alpha(scope.muted_foreground, 0.65).into();
            if pinning_supported && !input.drag_lifted && !input.ghost {
                let weak = weak.clone();
                let thread_ref = thread_ref.clone();
                div()
                    .id(SharedString::from(format!("unpin-{key}")))
                    .group(SharedString::from(format!("unpin-{key}")))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .cursor(CursorStyle::PointingHand)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let thread_ref = thread_ref.clone();
                        weak.update(cx, |this, cx| this.attempt_unpin(thread_ref, window, cx))
                            .ok();
                    })
                    .tooltip(|window, cx| Tooltip::new("Unpin thread").build(window, cx))
                    // Pin marks the state at rest; hover swaps in pin-off.
                    .child({
                        let group = SharedString::from(format!("unpin-{key}"));
                        div()
                            .relative()
                            .size(rems(0.75))
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .group_hover(group.clone(), |style| style.opacity(0.))
                                    .child(Icon::new(IconName::Pin).size(rems(0.75)).color(color)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .opacity(0.)
                                    .group_hover(group, |style| style.opacity(1.))
                                    .child(
                                        Icon::new(IconName::PinOff)
                                            .size(rems(0.75))
                                            .color(scope.foreground),
                                    ),
                            )
                    })
                    .into_any_element()
            } else {
                Icon::new(IconName::Pin)
                    .size(rems(0.75))
                    .color(color)
                    .into_any_element()
            }
        });

        // ── PR badge ───────────────────────────────────────────────────
        let visible_prs: Vec<_> = thread
            .pull_requests
            .iter()
            .filter(|link| !link.dismissed)
            .cloned()
            .collect();
        let pr_badge = current_link(&thread.pull_requests).cloned().map(|link| {
            let others = visible_prs.len().saturating_sub(1);
            let weak = weak.clone();
            let key = key.clone();
            ThreadPullRequestBadge::new(SharedString::from(format!("pr-{key}")), link, others)
                .on_open_list(move |_, _, cx| {
                    let key = key.clone();
                    weak.update(cx, |this, cx| {
                        if this.route_thread_key() != Some(key.as_str()) {
                            this.navigate_to_thread(&key, cx);
                        }
                        cx.emit(SidebarEvent::OpenPullRequests(key));
                    })
                    .ok();
                })
                .into_any_element()
        });

        // ── Woke pill (an action: dismisses the notification) ──────────
        // Card pills use `text-warning`; the slim row uses `text-warning-foreground`
        // (upstream Sidebar.tsx slim Woke pill). Both underline on hover.
        let woke_button = |size: gpui_kit::Rems,
                           color: Hsla,
                           weak: gpui_kit::WeakEntity<Sidebar>| {
            let thread_ref = thread_ref.clone();
            let woke = woke_at.clone().unwrap_or_default();
            div()
                .id(SharedString::from(format!("woke-{key}")))
                .flex()
                .items_center()
                .gap_1()
                .cursor(CursorStyle::PointingHand)
                .font_weight(FontWeight::MEDIUM)
                .text_size(TEXT_XS.size)
                .text_color(color)
                .hover(|style| style.underline())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let thread_ref = thread_ref.clone();
                    let woke = woke.clone();
                    weak.update(cx, |this, cx| this.acknowledge_woke(thread_ref, woke, cx))
                        .ok();
                })
                .tooltip(|window, cx| Tooltip::new("Dismiss Woke notification").build(window, cx))
                .child(Icon::new(IconName::AlarmClock).size(size).color(color))
                .child("Woke")
                .into_any_element()
        };

        // ── Action buttons ─────────────────────────────────────────────
        let sweep_down = {
            let weak = weak.clone();
            let key = key.clone();
            move |event: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                let key = key.clone();
                weak.update(cx, |this, cx| {
                    this.start_action_sweep(&key, variant_action, event, cx)
                })
                .ok();
            }
        };
        let action_click = {
            let weak = weak.clone();
            let thread_ref = thread_ref.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let thread_ref = thread_ref.clone();
                weak.update(cx, |this, cx| {
                    if this.consume_sweep_click() {
                        return;
                    }
                    match variant_action {
                        SweepAction::Settle => this.attempt_settle(thread_ref, None, window, cx),
                        SweepAction::Unsettle => this.attempt_unsettle(thread_ref, window, cx),
                        SweepAction::Unsnooze => this.attempt_unsnooze(thread_ref, window, cx),
                    }
                })
                .ok();
            }
        };
        let muted = scope.muted_foreground;
        let fg = scope.foreground;

        let destination = if input.drag_lifted {
            input.drop_verb
        } else {
            input.sweep_action.map(|action| match action {
                SweepAction::Settle => SidebarDropVerb::Settle,
                SweepAction::Unsettle => SidebarDropVerb::Unsettle,
                SweepAction::Unsnooze => SidebarDropVerb::Wake,
            })
        };

        let jump_badge = (self.show_jump_hints() && !input.ghost)
            .then_some(input.jump_index)
            .flatten()
            .and_then(jump_shortcut_label)
            .map(|label| jump_hint_badge(label, &theme));

        let content: AnyElement = match variant {
            RowVariant::Slim => {
                let favicon = project.as_ref().map(|project| {
                    let dim = !is_hovered
                        && (!input.is_active || variant_action == SweepAction::Unsettle);
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .when(dim, |this| this.opacity(0.4))
                        .child(ProjectFavicon::new(project).grayscale(dim))
                });
                let time_text: AnyElement = if variant_action == SweepAction::Unsnooze
                    && let Some(until) = &thread.snoozed_until
                {
                    // Snoozed rows show when they come back.
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.info_foreground)
                        .child(snooze_wake_label(until, &now))
                        .into_any_element()
                } else if is_woke {
                    woke_button(rems(0.75), theme.warning_foreground, weak.clone())
                } else {
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(secondary)
                        .child(if variant_action == SweepAction::Unsettle {
                            settled_time_label(thread)
                        } else {
                            thread_time_label(thread)
                        })
                        .into_any_element()
                };
                let action: Option<AnyElement> = match variant_action {
                    SweepAction::Unsnooze if snooze_supported => Some(action_button(
                        SharedString::from(format!("wake-{key}")),
                        Icon::new(IconName::AlarmClockOff)
                            .size(rems(0.75))
                            .into_any_element(),
                        None,
                        muted,
                        fg,
                        sweep_down.clone(),
                        action_click.clone(),
                    )),
                    SweepAction::Unsettle if settlement_supported => Some(action_button(
                        SharedString::from(format!("unsettle-{key}")),
                        Icon::new(IconName::Undo2)
                            .size(rems(0.875))
                            .into_any_element(),
                        Some("Un-settle thread"),
                        muted,
                        fg,
                        sweep_down.clone(),
                        action_click.clone(),
                    )),
                    SweepAction::Settle if settlement_supported => Some(action_button(
                        SharedString::from(format!("settle-{key}")),
                        Icon::new(IconName::Check)
                            .size(rems(0.75))
                            .into_any_element(),
                        None,
                        muted,
                        fg,
                        sweep_down.clone(),
                        action_click.clone(),
                    )),
                    _ => None,
                };
                let slot = if input.drag_lifted {
                    destination.map(|verb| drop_verb_badge(verb, &theme))
                } else if input.sweep_action.is_some() {
                    None
                } else {
                    Some(
                        div()
                            .relative()
                            .ml_auto()
                            .flex()
                            .h_6()
                            .min_w_8()
                            .flex_shrink_0()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .when(!show_actions || is_woke, |this| this.child(time_text))
                            .when(show_actions && !input.ghost, |this| this.children(action))
                            .into_any_element(),
                    )
                };
                div()
                    .flex()
                    .h_9()
                    .items_center()
                    .gap(rems(0.625))
                    .px(rems(0.625))
                    .children(favicon)
                    .child(title)
                    .children(pin_marker)
                    .children(pr_badge)
                    .children(slot)
                    .when(input.sweep_action.is_some(), |this| {
                        this.children(destination.map(|verb| drop_verb_badge(verb, &theme)))
                    })
                    .into_any_element()
            }
            RowVariant::Card => {
                let status_label: AnyElement = match &top {
                    Some(top) if top.kind == SidebarV2TopStatusKind::Woke => {
                        woke_button(rems(1.), theme.warning, weak.clone())
                    }
                    Some(top) => div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(top.color)
                        .children(top.icon.map(|icon| Icon::new(icon).color(top.color)))
                        .child(top.label)
                        .when(status == SidebarThreadStatus::Working, |this| {
                            let duration = resolve_working_started_at(thread)
                                .and_then(|started| parse_timestamp_ms(&started))
                                .map(|started| {
                                    format_working_duration_label((now_ms() - started) as f64)
                                });
                            this.children(duration)
                        })
                        .into_any_element(),
                    None => div()
                        .text_color(secondary)
                        .child(thread_time_label(thread))
                        .into_any_element(),
                };
                let is_woke_status = top
                    .as_ref()
                    .is_some_and(|top| top.kind == SidebarV2TopStatusKind::Woke);
                let snooze_button = (show_snooze && !input.ghost)
                    .then(|| self.render_snooze_menu(&key, thread_ref.clone(), cx));
                let settle_button = (settlement_supported && !input.ghost).then(|| {
                    action_button(
                        SharedString::from(format!("settle-{key}")),
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(Icon::new(IconName::Check).size(rems(0.875)))
                            .child("Settle")
                            .into_any_element(),
                        Some("Settle thread"),
                        muted,
                        fg,
                        sweep_down.clone(),
                        action_click.clone(),
                    )
                });
                let slot: Option<AnyElement> = if input.drag_lifted {
                    destination.map(|verb| drop_verb_badge(verb, &theme))
                } else if input.sweep_action.is_some() {
                    None
                } else {
                    Some(
                        div()
                            .relative()
                            .ml_auto()
                            .flex()
                            .h_5()
                            .min_w_8()
                            .flex_shrink_0()
                            .items_center()
                            .justify_end()
                            .text_size(TEXT_XS.size)
                            .when(!show_actions || is_woke_status, |this| {
                                this.child(status_label)
                            })
                            .when(show_actions, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .h_full()
                                        .items_stretch()
                                        .mr(px(-4.))
                                        .children(snooze_button)
                                        .children(settle_button),
                                )
                            })
                            .into_any_element(),
                    )
                };
                let diff = thread.latest_run_diff.map(|diff| {
                    div()
                        .flex_shrink_0()
                        .font_family(theme.fonts.mono.clone())
                        .child(
                            gpui_kit::StyledText::new(format!(
                                "+{} \u{2212}{}",
                                format_diff_count(diff.insertions),
                                format_diff_count(diff.deletions)
                            ))
                            .with_highlights({
                                let plus = format_diff_count(diff.insertions).len() + 1;
                                vec![
                                    (
                                        0..plus,
                                        gpui_kit::HighlightStyle {
                                            color: Some(theme.diff_addition_foreground),
                                            ..Default::default()
                                        },
                                    ),
                                    (
                                        plus + 1
                                            ..plus
                                                + 1
                                                + "\u{2212}".len()
                                                + format_diff_count(diff.deletions).len(),
                                        gpui_kit::HighlightStyle {
                                            color: Some(theme.diff_deletion_foreground),
                                            ..Default::default()
                                        },
                                    ),
                                ]
                            }),
                        )
                });
                let branch_line = div()
                    .mt(rems(0.125))
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .text_color(secondary)
                    .map(|this| match &thread.branch {
                        // Always the branch: the row's most stable identifier.
                        Some(branch) => this
                            .when(
                                thread
                                    .worktree_path
                                    .as_deref()
                                    .is_some_and(|p| !p.trim().is_empty()),
                                |this| {
                                    this.child(ThreadWorktreeIndicator::new(
                                        SharedString::from(format!("worktree-{key}")),
                                        thread.worktree_path.clone(),
                                        thread.branch.clone(),
                                    ))
                                },
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .text_color(muted_40)
                                    .child(branch.clone()),
                            ),
                        None => this.child(div().flex_1()),
                    })
                    .children(pr_badge)
                    .children(diff)
                    .child(self.render_provider_stack(thread, &theme, cx));
                div()
                    .h(rems(4.875))
                    .px(SIDEBAR_ROW_CONTENT_INSET)
                    .py(SIDEBAR_CONTENT_INSET)
                    .child(
                        div()
                            .flex()
                            .h_5()
                            .min_w_0()
                            .items_center()
                            .gap(rems(0.375))
                            .children(project.as_ref().map(ProjectFavicon::new))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(if should_recede {
                                        FontWeight::NORMAL
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .text_color(theme.secondary_label)
                                    .children(project_display_name.clone()),
                            )
                            .children(pin_marker)
                            .children(slot)
                            .when(input.sweep_action.is_some(), |this| {
                                this.children(destination.map(|verb| drop_verb_badge(verb, &theme)))
                            }),
                    )
                    .child(div().mt_1().flex().min_w_0().child(title))
                    .child(branch_line)
                    .into_any_element()
            }
        };

        let row_id = SharedString::from(format!("row-{key}"));
        let surface = div()
            .id(row_id)
            .relative()
            .w_full()
            .overflow_hidden()
            .rounded(RADIUS_MD)
            .cursor(CursorStyle::PointingHand)
            .text_color(if should_recede && !is_hovered {
                with_alpha(scope.sidebar_muted_foreground, 0.75).into()
            } else {
                scope.sidebar_foreground
            })
            .when_some(surface_bg, |this, bg| this.bg(bg))
            .when(input.drag_lifted, |this| {
                this.bg(scope.sidebar_row_active)
                    .shadow(tokens::shadow_lg(0.1))
            })
            // Background work fades as a whole row.
            .when(
                should_recede
                    && status == SidebarThreadStatus::Working
                    && !is_hovered
                    && !input.drag_lifted,
                |this| this.opacity(0.7),
            )
            .child(content)
            .children(jump_badge);
        if input.ghost {
            return surface.into_any_element();
        }
        let hover_key = key.clone();
        let click_key = key.clone();
        let down_key = key.clone();
        let surface = surface
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    if this.hovered_row.as_deref() != Some(hover_key.as_str()) {
                        this.hovered_row = Some(hover_key.clone());
                        cx.notify();
                    }
                } else if this.hovered_row.as_deref() == Some(hover_key.as_str()) {
                    this.hovered_row = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.press_row(&down_key, event, cx);
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.handle_row_click(&click_key, event, window, cx);
            }));
        let surface = if input.drag_lifted || self.drag.is_some() || snooze_menu_open {
            surface.into_any_element()
        } else {
            surface
                .tooltip(move |window, cx| tooltip_card.clone().build(window, cx))
                .into_any_element()
        };
        let _ = window;
        gpui_kit::component::menu::ContextMenuExt::context_menu(
            div().w_full().child(surface),
            Sidebar::thread_context_menu(weak, key),
        )
        .into_any_element()
    }

    /// `SidebarProviderStack`: earlier owners dim, the current one last.
    fn render_provider_stack(
        &self,
        thread: &EnvironmentThreadShell,
        theme: &AppTheme,
        cx: &App,
    ) -> AnyElement {
        let snapshot = self.snapshot(cx);
        let stack = resolve_thread_provider_stack(thread);
        let last = stack.len().saturating_sub(1);
        let icons: Vec<AnyElement> = stack
            .iter()
            .enumerate()
            .filter(|(_, id)| snapshot.provider_entry(id).is_some())
            .map(|(index, _)| {
                let current = index == last;
                Icon::new(IconName::Bot)
                    .size(rems(if current { 0.875 } else { 0.75 }))
                    .color(
                        with_alpha(
                            theme.sidebar_scope().foreground,
                            if current { 0.6 } else { 0.35 },
                        )
                        .into(),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .ml_auto()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(0.375))
            .children(icons)
            .into_any_element()
    }

    /// `SnoozeMenuButton`: the clock opening the preset menu.
    fn render_snooze_menu(
        &mut self,
        key: &str,
        thread: ThreadRef,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let weak = cx.entity().downgrade();
        let format = self.snapshot(cx).settings.timestamp_format;
        let open_key = key.to_string();
        let thread = Rc::new(thread);
        Popover::new(SharedString::from(format!("snooze-menu-{key}")))
            .anchor(gpui_kit::Anchor::TopRight)
            .appearance(false)
            .trigger(SnoozeTrigger {
                id: SharedString::from(format!("snooze-{key}")).into(),
                open: false,
                color: scope.muted_foreground,
                hover: scope.foreground,
            })
            .on_open_change({
                let weak = weak.clone();
                move |open, _, cx| {
                    let key = open_key.clone();
                    let open = *open;
                    weak.update(cx, |this, cx| {
                        this.snooze_menu_for = open.then_some(key);
                        cx.notify();
                    })
                    .ok();
                }
            })
            .content(move |_, window, cx| {
                // Presets resolve at open time so "In 1 hour" is relative to the click.
                let presets = resolve_snooze_presets_now(format);
                let theme = cx.app_theme().clone();
                let row = |id: String, label: String, when: Option<String>| {
                    div()
                        .id(SharedString::from(id))
                        .flex()
                        .h_8()
                        .items_center()
                        .gap_2()
                        .rounded(RADIUS_SM)
                        .px_2()
                        .cursor(CursorStyle::PointingHand)
                        .text_size(TEXT_SM.size)
                        .text_color(theme.popover_foreground)
                        .hover(|style| style.bg(theme.accent))
                        .child(div().flex_1().child(label))
                        .children(when.map(|when| {
                            div()
                                .ml_auto()
                                .pl_4()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(TEXT_XS.size)
                                .text_color(theme.secondary_label)
                                .child(when)
                        }))
                };
                let _ = window;
                div()
                    .min_w(px(220.))
                    .p_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                    .bg(crate::ui::tooltip::dropdown_glass_background(&theme))
                    .shadow(tokens::shadow_popover(theme.is_dark()))
                    .children(presets.into_iter().map(|preset| {
                        let weak = weak.clone();
                        let thread = thread.clone();
                        let until = preset.snoozed_until.clone();
                        row(
                            format!("snooze-{}", preset.id.as_str()),
                            preset.label.to_string(),
                            Some(preset.when_label.clone()),
                        )
                        .on_click(cx.listener(
                            move |state, _, window, cx| {
                                state.dismiss(window, cx);
                                let thread = (*thread).clone();
                                let until = until.clone();
                                weak.update(cx, |this, cx| {
                                    this.attempt_snooze(thread, until, None, window, cx)
                                })
                                .ok();
                            },
                        ))
                    }))
                    .child(div().my_1().h(px(1.)).bg(theme.border))
                    .child({
                        let weak = weak.clone();
                        let thread = thread.clone();
                        row("snooze-custom".into(), "Custom…".into(), None).on_click(cx.listener(
                            move |state, _, window, cx| {
                                state.dismiss(window, cx);
                                let thread = (*thread).clone();
                                weak.update(cx, |this, cx| {
                                    this.open_custom_snooze(vec![thread], window, cx)
                                })
                                .ok();
                            },
                        ))
                    })
            })
            .into_any_element()
    }

    /// Row click: Cmd toggles selection, Shift extends it, a double click
    /// renames, a plain click navigates (never twice for one double click).
    pub(crate) fn handle_row_click(
        &mut self,
        key: &str,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.consume_drag_click() {
            return;
        }
        let modifiers = event.modifiers();
        if modifiers.platform {
            self.selection.toggle_thread(key);
            cx.notify();
            return;
        }
        if modifiers.shift {
            let (_, layout) = self.compute_sections(cx);
            self.selection.range_select_to(key, &layout.ordered_keys);
            cx.notify();
            return;
        }
        if event.click_count() == 2 {
            if !modifiers.alt && !modifiers.control {
                self.start_rename(key, window, cx);
            }
            return;
        }
        if super::sidebar_logic::is_trailing_double_click(event.click_count()) {
            return;
        }
        self.navigate_to_thread(key, cx);
    }
}
