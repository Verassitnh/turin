//! Port of upstream/apps/web/src/components/chat/ProviderModelPicker.tsx
//!
//! The picker's trigger content: the instance icon (with the account badge
//! when several instances share a provider) or the overlapped icons of a
//! multi-model selection, the model label with its tooltip, the
//! "Unavailable" badge and the chevron. What the label reads comes from
//! `composer::provider_model_picker`. The popover, its page-scroll lock and
//! `ChatGptSharingControl` are the composer's model-picker layer
//! (`views/model_picker_view.rs`): GPUI popovers do not scroll the page, and
//! the sharing control is not ported.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, rems,
};

use crate::composer::provider_model_picker::{ProviderModelTrigger, SelectedModelLabel};
use crate::composer::views::chrome::{ControlSize, control_chevron};
use crate::composer::views::footer::provider_mark;
use crate::state::helpers::provider_instance_display::provider_instance_initials;
use crate::theme::ActiveAppTheme;
use crate::theme::color::{color, with_alpha};
use crate::theme::tokens::{TEXT_3XS, TEXT_5XS};
use crate::ui::{Badge, BadgeSize, BadgeVariant, Tooltip};

/// The instance icon at `size-4`, with the account badge when asked:
/// `h-3 min-w-3 px-0.5 text-5xs` at the bottom right (`-0.125rem` offsets).
fn instance_icon(
    entry: &crate::composer::catalog::ProviderInstanceEntry,
    show_badge: bool,
    size: ControlSize,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let accent = entry
        .accent_color
        .as_deref()
        .filter(|value| value.starts_with('#') && value.len() == 7)
        .map(color);
    div()
        .relative()
        .flex()
        .flex_shrink_0()
        .size(rems(1.))
        .items_center()
        .justify_center()
        .child(provider_mark(
            &entry.driver_kind,
            rems(1.),
            // Collapsed (`xs`) controls tint the glyph with the muted foreground.
            if size == ControlSize::Xs {
                with_alpha(theme.muted_foreground, 0.7).into()
            } else {
                crate::components::provider_instance_icon::provider_icon_color(
                    &entry.driver_kind,
                    cx,
                )
            },
        ))
        .when(show_badge, |this| {
            this.child(
                div()
                    .absolute()
                    .right(rems(-0.125))
                    .bottom(rems(-0.125))
                    .flex()
                    .h(rems(0.75))
                    .min_w(rems(0.75))
                    .px(rems(0.125))
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.popover)
                    .bg(accent.unwrap_or(theme.card))
                    .text_color(if accent.is_some() {
                        gpui_kit::white()
                    } else {
                        theme.muted_foreground
                    })
                    .text_size(TEXT_5XS.size)
                    .line_height(gpui_kit::relative(1.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(provider_instance_initials(&entry.display_name)),
            )
        })
        .into_any_element()
}

/// The overlapped icons of a multi-model selection (`-space-x-1`, at most
/// three, then a `+N` disc).
fn selected_icons(selected: &[SelectedModelLabel], cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    let ring = theme.background;
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .children(
            selected
                .iter()
                .take(3)
                .enumerate()
                .filter_map(|(index, selection)| {
                    let entry = selection.entry.as_ref()?;
                    Some(
                        div()
                            .when(index > 0, |this| this.ml(px(-4.)))
                            .size(rems(1.))
                            .rounded_full()
                            .bg(ring)
                            .border_2()
                            .border_color(ring)
                            .child(provider_mark(
                                &entry.driver_kind,
                                rems(1.),
                                crate::components::provider_instance_icon::provider_icon_color(
                                    &entry.driver_kind,
                                    cx,
                                ),
                            )),
                    )
                }),
        )
        .when(selected.len() > 3, |this| {
            this.child(
                div()
                    .ml(px(-4.))
                    .flex()
                    .size(rems(1.))
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(ring)
                    .border_2()
                    .border_color(ring)
                    .text_size(TEXT_3XS.size)
                    .child(format!("+{}", selected.len() - 3)),
            )
        })
        .into_any_element()
}

/// The children of the model trigger control, in order: the icon-and-label
/// group, then the chevron. `has_label` is false when a caller wants the
/// aggregate `triggerLabel` instead of an icon (settings rows).
pub fn provider_model_trigger_children(
    trigger: &ProviderModelTrigger,
    size: ControlSize,
    show_icon: bool,
    cx: &App,
) -> Vec<AnyElement> {
    let gap = if size == ControlSize::Xs {
        px(4.)
    } else {
        px(6.)
    };
    let icon = if !show_icon {
        None
    } else if let Some(selected) = &trigger.selected {
        Some(selected_icons(selected, cx))
    } else {
        trigger
            .active_entry
            .as_ref()
            .map(|entry| instance_icon(entry, trigger.show_instance_badge, size, cx))
    };
    let tooltip = trigger.tooltip.clone();
    let label = div()
        .id("composer-model-picker-label")
        .min_w_0()
        .flex_1()
        .overflow_hidden()
        .truncate()
        .child(trigger.text.clone())
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
    let group = div()
        .flex()
        .min_w_0()
        .flex_1()
        .items_center()
        .gap(gap)
        .children(icon)
        .child(label)
        .when(trigger.show_unavailable_badge, |this| {
            this.child(
                Badge::label("Unavailable")
                    .variant(BadgeVariant::Outline)
                    .size(BadgeSize::Sm),
            )
        })
        .into_any_element();
    vec![group, control_chevron(size, cx).into_any_element()]
}
