//! The drawers attached to the composer: the notice stack
//! (ComposerBannerStack.tsx), queued runs (QueuedRunsControl.tsx), the stash
//! tab and menu (ComposerStashBadge/Menu.tsx), the plan follow-up banner and
//! the context window meter (ContextWindowMeter.tsx).

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    PathBuilder, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, point, px, rems,
};

use crate::components::chat::composer_pending_panels::{BannerVariant as SurfaceVariant, attachment_overlap, banner_surface};
use crate::composer::banner_stack::order_banners;
use crate::composer::context_meter::{
    format_context_window_compaction_message, format_context_window_cost, format_percentage,
};
use crate::composer::draft_store::PromptStashEntry;
use crate::composer::primary_actions::format_context_window_tokens;
use crate::composer::stash::{missing_image_count, relative_time_label, stash_entry_snippet};
use crate::composer::view_model::{BannerItem, BannerVariant, ComposerViewModel, QueueState};
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::chrome::{floating, glass_surface};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_2XL, TEXT_2XS, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

fn surface_variant(variant: BannerVariant) -> SurfaceVariant {
    match variant {
        BannerVariant::Default => SurfaceVariant::Default,
        BannerVariant::Error => SurfaceVariant::Error,
        BannerVariant::Info => SurfaceVariant::Info,
        BannerVariant::Success => SurfaceVariant::Success,
        BannerVariant::Warning => SurfaceVariant::Warning,
    }
}

/// `ComposerBanner.Row`: icon column, content, actions.
fn banner_row(icon: Option<AnyElement>, content: AnyElement, actions: Vec<AnyElement>) -> gpui_kit::Div {
    div()
        .flex()
        .w_full()
        .min_w_0()
        .min_h(rems(1.5))
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .flex()
                .flex_none()
                .w(rems(1.5))
                .items_center()
                .justify_center()
                .children(icon),
        )
        .child(div().flex().min_w_0().flex_1().items_center().gap_1().child(content))
        .when(!actions.is_empty(), |this| {
            this.child(div().flex().flex_shrink_0().items_center().justify_end().gap_1().children(actions))
        })
}

fn small_icon(name: IconName, theme: &AppTheme) -> AnyElement {
    Icon::new(name).size(rems(0.75)).color(theme.muted_foreground).into_any_element()
}

fn count_label(count: usize, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .min_w(rems(1.5))
        .flex_none()
        .justify_center()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .child(count.to_string())
        .into_any_element()
}

impl ChatComposer {
    /// Drawers above the composer, top to bottom.
    pub(super) fn render_drawers(&mut self, vm: &Arc<ComposerViewModel>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let mut drawers: Vec<AnyElement> = Vec::new();
        let ordered: Vec<BannerItem> = order_banners(&vm.banners).into_iter().cloned().collect();
        let (front, stacked) = match ordered.split_first() {
            Some((front, rest)) => (Some(front.clone()), rest.to_vec()),
            None => (None, Vec::new()),
        };

        // Stash tab, top right.
        let stash_count = self.drafts.stash_entries().len();
        if stash_count > 0 && self.layer != Some(Layer::Stash) {
            drawers.push(self.stash_tab(stash_count, window, cx));
        }
        if self.layer == Some(Layer::Stash) {
            drawers.push(self.stash_menu(window, cx));
        }

        // Stacked notices peek behind the front banner.
        if !stacked.is_empty() {
            if self.banner_stack_expanded {
                for item in &stacked {
                    drawers.push(
                        div()
                            .pb_2()
                            .child(self.banner_surface_for(item, false, window, cx))
                            .into_any_element(),
                    );
                }
            } else {
                let variant = stacked[0].variant;
                let outline = match variant {
                    BannerVariant::Error => with_alpha(theme.destructive, 0.24),
                    BannerVariant::Warning => with_alpha(theme.warning, 0.24),
                    _ if theme.is_dark() => with_alpha(crate::theme::tokens::tw::WHITE, 0.05),
                    _ => with_alpha(theme.foreground, 0.08),
                };
                let surface = if theme.is_dark() { theme.surface_raised } else { theme.card };
                drawers.push(
                    div()
                        .id("composer-banner-peek")
                        .relative()
                        .h(px(12.))
                        .mx_auto()
                        .w(gpui_kit::relative(0.96))
                        .rounded_t(RADIUS_2XL)
                        .border_1()
                        .border_b_0()
                        .border_color(outline)
                        .bg(surface)
                        .cursor(CursorStyle::PointingHand)
                        .tooltip(|window, cx| Tooltip::new("Show other notices").build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.banner_stack_expanded = true;
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }

        if let Some(queue) = vm.queue.as_ref().filter(|queue| !queue.queued.is_empty()) {
            drawers.push(self.queue_drawer(queue, window, cx));
        }
        if let Some(front) = front {
            drawers.push(self.banner_surface_for(&front, true, window, cx));
        }
        if vm.show_plan_follow_up_prompt && vm.pending_answer.is_none() && !vm.has_pending_approval {
            let title = vm.plan_follow_up_title.clone();
            let content = div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child("Plan ready"))
                .when_some(title, |this, title| {
                    this.child(div().min_w_0().flex_1().truncate().text_color(with_alpha(theme.foreground, 0.85)).child(title))
                })
                .into_any_element();
            drawers.push(banner_surface(
                SurfaceVariant::Info,
                false,
                banner_row(None, content, Vec::new()).into_any_element(),
                window,
                &theme,
            ));
        }
        drawers
    }

    /// `ComposerBannerStackAlert`.
    fn banner_surface_for(&mut self, item: &BannerItem, attached: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let title = item.title.clone();
        let description = item.description.clone();
        let content = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .whitespace_nowrap()
            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(title))
            .when_some(description, |this, description| {
                this.child(div().min_w_0().flex_1().truncate().text_color(theme.muted_foreground).child(description))
            })
            .into_any_element();
        let mut actions: Vec<AnyElement> = Vec::new();
        for (index, (label, action_id)) in item.actions.iter().enumerate() {
            let banner_id = item.id.clone();
            let action_id = action_id.clone();
            actions.push(
                Button::new(("banner-action", index))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Xs)
                    .label(label.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(source) = this.source(cx) {
                            source.banner_action(&banner_id, &action_id, cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        if item.dismissible {
            let id = item.id.clone();
            actions.push(
                Button::new(("banner-dismiss", item.id.len()))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::X)
                    .tooltip(Tooltip::new(item.dismiss_label.clone().unwrap_or_else(|| "Dismiss warning".to_owned())))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(source) = this.source(cx) {
                            source.dismiss_banner(&id, cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        let icon = Icon::new(item.icon).size(rems(0.75)).color(theme.muted_foreground).into_any_element();
        let row = banner_row(Some(icon), content, actions).into_any_element();
        let surface = banner_surface(surface_variant(item.variant), false, row, window, &theme);
        if attached {
            div()
                .id("composer-banner-front")
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if !*hovered && this.banner_stack_expanded {
                        this.banner_stack_expanded = false;
                        cx.notify();
                    }
                }))
                .child(surface)
                .into_any_element()
        } else {
            surface
        }
    }

    // ----- Queue -----------------------------------------------------------

    fn queue_drawer(&mut self, queue: &QueueState, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let expanded = self.queue_expanded;
        let count = queue.queued.len();
        let header = div()
            .id("composer-queue-header")
            .cursor(CursorStyle::PointingHand)
            .rounded(px(6.))
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue_expanded = !this.queue_expanded;
                cx.notify();
            }))
            .child(banner_row(
                Some(small_icon(IconName::ListOrdered, &theme)),
                div().text_color(theme.muted_foreground).child("Queued").into_any_element(),
                vec![
                    count_label(count, &theme),
                    div()
                        .flex()
                        .size(rems(1.5))
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronUp })
                                .size(rems(0.875))
                                .color(theme.muted_foreground),
                        )
                        .into_any_element(),
                ],
            ));
        let editing = queue.editing_run_id.clone();
        let rows: Vec<AnyElement> = queue
            .queued
            .iter()
            .enumerate()
            .map(|(index, run)| {
                let is_editing = editing.as_deref() == Some(run.run_id.as_str());
                let preview = run.text.split_whitespace().collect::<Vec<_>>().join(" ");
                let run_id = run.run_id.clone();
                let steer_id = run.run_id.clone();
                let remove_id = run.run_id.clone();
                let pending = run.pending;
                let actions: Vec<AnyElement> = if is_editing {
                    vec![
                        Button::new(("queue-cancel-edit", index))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Xs)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(source) = this.source(cx) {
                                    source.cancel_queued_edit(cx);
                                }
                            }))
                            .into_any_element(),
                    ]
                } else {
                    vec![
                        Button::new(("queue-edit", index))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Pencil)
                            .disabled(pending)
                            .tooltip(Tooltip::new("Edit in the composer"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(source) = this.source(cx) {
                                    source.edit_queued_run(&run_id, cx);
                                }
                            }))
                            .into_any_element(),
                        Button::new(("queue-steer", index))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::Xs)
                            .icon(IconName::CornerUpRight)
                            .label("Steer")
                            .disabled(pending)
                            .tooltip(Tooltip::new("Send as a steer instead"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(source) = this.source(cx) {
                                    source.promote_queued_run(&steer_id, cx);
                                }
                            }))
                            .into_any_element(),
                        Button::new(("queue-remove", index))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::X)
                            .disabled(pending)
                            .tooltip(Tooltip::new("Remove from queue"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(source) = this.source(cx) {
                                    source.cancel_queued_run(&remove_id, cx);
                                }
                            }))
                            .into_any_element(),
                    ]
                };
                let icon = if pending {
                    small_icon(IconName::Clock3, &theme)
                } else {
                    Icon::new(IconName::GripVertical).size(rems(0.75)).color(with_alpha(theme.muted_foreground, 0.7).into()).into_any_element()
                };
                let text = if preview.is_empty() {
                    let images = run.image_names.len();
                    let files = run.file_names.len();
                    format!("({} attachment{})", images + files, if images + files == 1 { "" } else { "s" })
                } else {
                    preview
                };
                div()
                    .rounded(px(4.))
                    .when(is_editing, |this| this.bg(theme.accent).text_color(theme.accent_foreground))
                    .child(banner_row(
                        Some(icon),
                        div().min_w_0().truncate().text_color(with_alpha(theme.foreground, 0.85)).child(text).into_any_element(),
                        actions,
                    ))
                    .into_any_element()
            })
            .collect();
        let body = div()
            .flex()
            .flex_col()
            .child(header)
            .when(expanded, |this| {
                this.child(div().id("composer-queue-list").flex().flex_col().max_h(rems(8.)).overflow_y_scroll().gap(px(1.)).children(rows))
            })
            .into_any_element();
        banner_surface(SurfaceVariant::Default, false, body, window, &theme)
    }

    // ----- Stash -----------------------------------------------------------

    fn stash_tab(&mut self, count: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let bounds = self.trigger_bounds.stash.clone();
        let content = div()
            .id("composer-stash-badge")
            .relative()
            .cursor(CursorStyle::PointingHand)
            .text_color(theme.muted_foreground)
            .hover(|style| style.text_color(theme.foreground))
            .on_click(cx.listener(|this, _, _, cx| this.set_layer(Some(Layer::Stash), cx)))
            .child(super::chrome::track_bounds(&bounds))
            .child(banner_row(
                Some(small_icon(IconName::Bookmark, &theme)),
                div().child("Stash").into_any_element(),
                vec![count_label(count, &theme)],
            ))
            .into_any_element();
        div()
            .flex()
            .justify_end()
            .child(banner_surface(SurfaceVariant::Default, false, content, window, &theme))
            .into_any_element()
    }

    fn stash_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entries: Vec<PromptStashEntry> = self.drafts.stash_entries().to_vec();
        let highlighted = self.stash_highlight.clone().or_else(|| entries.first().map(|entry| entry.id.clone()));
        let header = div()
            .id("composer-stash-header")
            .cursor(CursorStyle::PointingHand)
            .rounded(px(6.))
            .on_click(cx.listener(|this, _, window, cx| this.close_layer(window, cx)))
            .child(banner_row(
                Some(small_icon(IconName::Bookmark, &theme)),
                div().text_color(theme.muted_foreground).child("Stash").into_any_element(),
                vec![
                    count_label(entries.len(), &theme),
                    div()
                        .flex()
                        .size(rems(1.5))
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::ChevronDown).size(rems(0.875)).color(theme.muted_foreground))
                        .into_any_element(),
                ],
            ));
        let rows: Vec<AnyElement> = if entries.is_empty() {
            vec![banner_row(
                None,
                div().text_color(theme.muted_foreground).child("Nothing stashed yet.").into_any_element(),
                Vec::new(),
            )
            .into_any_element()]
        } else {
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let id = entry.id.clone();
                    let hover_id = entry.id.clone();
                    let delete_id = entry.id.clone();
                    let active = highlighted.as_deref() == Some(entry.id.as_str());
                    let mut actions: Vec<AnyElement> = Vec::new();
                    if entry.pending_image_count > 0 {
                        actions.push(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "saving {} image{}…",
                                    entry.pending_image_count,
                                    if entry.pending_image_count == 1 { "" } else { "s" }
                                ))
                                .into_any_element(),
                        );
                    } else if missing_image_count(entry) > 0 {
                        let missing = missing_image_count(entry);
                        actions.push(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.warning_foreground)
                                .child(format!("{missing} image{} dropped", if missing == 1 { "" } else { "s" }))
                                .into_any_element(),
                        );
                    }
                    if !entry.files.is_empty() {
                        actions.push(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap_1()
                                .text_color(theme.muted_foreground)
                                .child(Icon::new(IconName::File).size(rems(0.75)))
                                .child(entry.files.len().to_string())
                                .into_any_element(),
                        );
                    }
                    actions.push(
                        div()
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(relative_time_label(&entry.created_at))
                            .into_any_element(),
                    );
                    actions.push(
                        Button::new(("stash-delete", index))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::X)
                            .tooltip(Tooltip::new("Delete stashed prompt"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete_stash_entry(&delete_id, cx);
                            }))
                            .into_any_element(),
                    );
                    div()
                        .id(("stash-entry", index))
                        .rounded(px(4.))
                        .cursor(CursorStyle::PointingHand)
                        .when(active, |this| this.bg(theme.accent).text_color(theme.accent_foreground))
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.stash_highlight.as_deref() != Some(hover_id.as_str()) {
                                this.stash_highlight = Some(hover_id.clone());
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| this.restore_stash_entry(&id, window, cx)))
                        .child(banner_row(
                            Some(small_icon(IconName::File, &theme)),
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(with_alpha(theme.foreground, 0.8))
                                .child(stash_entry_snippet(entry))
                                .into_any_element(),
                            actions,
                        ))
                        .into_any_element()
                })
                .collect()
        };
        let body = div()
            .flex()
            .flex_col()
            .child(header)
            .child(div().id("composer-stash-list").flex().flex_col().max_h(rems(14.)).overflow_y_scroll().gap(px(1.)).children(rows))
            .into_any_element();
        banner_surface(SurfaceVariant::Default, false, body, window, &theme)
    }

    // ----- Context window meter ---------------------------------------------

    pub(super) fn render_context_meter(&mut self, vm: &Arc<ComposerViewModel>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let usage = vm.context_window.clone();
        let Some(usage) = usage else {
            return vm.reserve_context_window_meter.then(|| div().flex_shrink_0().size(rems(1.75)).into_any_element());
        };
        if !vm.settings.context_window_meter_enabled {
            return None;
        }
        let percentage = usage.used_percentage();
        let normalized = percentage.unwrap_or(0.).clamp(0., 100.) as f32;
        let overloaded = normalized > 90.;
        let ring = if overloaded { theme.error } else { with_alpha(theme.muted_foreground, 0.72).into() };
        let track = with_alpha(theme.muted_foreground, 0.24);
        let bounds = self.trigger_bounds.meter.clone();
        let label = match (usage.max_tokens, format_percentage(percentage)) {
            (Some(_), Some(used)) => format!("Context window {used} used"),
            _ => format!("Context window {} tokens used", format_context_window_tokens(Some(usage.used_tokens))),
        };
        let open = self.layer == Some(Layer::ContextMeter);
        Some(
            div()
                .id("composer-context-meter")
                .relative()
                .flex()
                .flex_shrink_0()
                .size(rems(1.75))
                .items_center()
                .justify_center()
                .rounded(crate::theme::tokens::RADIUS_MD)
                .cursor(CursorStyle::PointingHand)
                .when(open, |this| this.bg(with_alpha(theme.foreground, 0.06)))
                .hover(|style| style.bg(with_alpha(theme.foreground, 0.06)))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_click(cx.listener(|this, _, _, cx| this.set_layer(Some(Layer::ContextMeter), cx)))
                .child(super::chrome::track_bounds(&bounds))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            // `r=9.75` in a 24 box, 3 wide: a 20px ring at 2.5px.
                            let size = px(20.);
                            let center = point(bounds.center().x, bounds.center().y);
                            let radius = size * (9.75 / 24.);
                            let width = size * (3. / 24.);
                            let mut stroke = |from: f32, to: f32, color: gpui_kit::Hsla| {
                                let steps = (((to - from).abs() * 64.).ceil() as usize).max(2);
                                let mut builder = PathBuilder::stroke(width);
                                for step in 0..=steps {
                                    let t = from + (to - from) * step as f32 / steps as f32;
                                    let angle = t * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                                    let at = point(center.x + radius * angle.cos(), center.y + radius * angle.sin());
                                    if step == 0 {
                                        builder.move_to(at);
                                    } else {
                                        builder.line_to(at);
                                    }
                                }
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, color);
                                }
                            };
                            stroke(0., 1., track.into());
                            if normalized > 0. {
                                stroke(0., normalized / 100., ring);
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .into_any_element(),
        )
    }

    pub(super) fn context_meter_popover(&mut self, vm: &Arc<ComposerViewModel>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let usage = vm.context_window.clone()?;
        let percentage = usage.used_percentage();
        let normalized = percentage.unwrap_or(0.).clamp(0., 100.) as f32;
        let ring = if normalized > 90. { theme.error } else { with_alpha(theme.muted_foreground, 0.72).into() };
        let used = format_percentage(percentage);
        let total_processed = usage.total_processed_tokens.filter(|tokens| *tokens > 0.);
        let heading = match (usage.max_tokens, used.clone()) {
            (Some(max), Some(used)) => format!(
                "{used} · {}/{}",
                format_context_window_tokens(Some(usage.used_tokens)),
                format_context_window_tokens(Some(max))
            ),
            _ => format_context_window_tokens(Some(usage.used_tokens)),
        };
        let stat = |label: &'static str, value: String| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .text_size(TEXT_2XS.size)
                .child(div().text_color(theme.secondary_label).child(label))
                .child(div().font_weight(FontWeight::MEDIUM).text_color(theme.secondary_label).child(value))
        };
        let can_compact = vm.selected_entry().is_some_and(|entry| entry.slash_commands.iter().any(|c| c.name == "compact"));
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .w(rems(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().font_weight(FontWeight::MEDIUM).text_size(TEXT_XS.size).text_color(theme.muted_foreground).child("Context Window"))
                    .child(div().text_size(TEXT_2XS.size).text_color(theme.secondary_label).child(heading)),
            )
            .when(usage.max_tokens.is_some(), |this| {
                this.child(
                    div().h(px(6.)).w_full().overflow_hidden().rounded_full().bg(with_alpha(theme.muted, 0.6)).child(
                        div().h_full().rounded_full().w(gpui_kit::relative(normalized / 100.)).bg(ring),
                    ),
                )
            })
            .when_some(total_processed, |this, tokens| this.child(stat("Total processed", format_context_window_tokens(Some(tokens)))))
            .when_some(usage.cost.clone(), |this, cost| this.child(stat("Cost", format_context_window_cost(&cost))))
            .when(usage.compacts_automatically, |this| {
                this.child(
                    div()
                        .mt_1()
                        .text_size(TEXT_2XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.secondary_label)
                        .child(format_context_window_compaction_message(
                            vm.active_thread_model_display_name.as_deref(),
                            usage.auto_compact_threshold,
                        )),
                )
            })
            .when(can_compact, |this| {
                this.child(
                    Button::new("compact-context")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Xs)
                        .icon(IconName::Minimize2)
                        .label("Compact context")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(source) = this.source(cx) {
                                source.compact_context(cx);
                            }
                            this.close_layer(window, cx);
                        })),
                )
            });
        let _ = attachment_overlap;
        Some(floating(self.trigger_bounds.meter.get(), true, true, glass_surface(cx).child(body)))
    }
}
