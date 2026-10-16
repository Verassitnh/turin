//! Port of upstream/apps/web/src/components/chat/CompactComposerControlsMenu.tsx
//!
//! The `...` control that holds what the footer has no room for. The footer
//! measures its blocks (`footer_layout`), moves the trailing ones out while
//! the row is too narrow, and this menu lists them: the reasoning traits, the
//! Chat/Plan mode, and always the Access (runtime) mode.
//!
//! Upstream keeps the menu mounted out of flow while every block fits and
//! closes an open menu when its trigger hides; here the trigger is only drawn
//! while something is hidden, and the layer closes when nothing is.

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, rems,
};

use crate::composer::view_model::{ComposerViewModel, InteractionMode};
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::chrome::{
    ControlSize, composer_control, floating, glass_surface, menu_row, track_bounds,
};
use crate::composer::views::footer::RUNTIME_MODES;
use crate::theme::ActiveAppTheme as _;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Icon, IconName, Tooltip};

/// The footer blocks that moved into the menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HiddenControls {
    pub traits: bool,
    pub mode: bool,
}

/// `aria-label` of the trigger.
pub const COMPACT_CONTROLS_LABEL: &str = "More composer controls";

/// `data-composer-shortcut`: the commands the menu answers.
pub fn compact_controls_shortcut(has_traits: bool) -> &'static str {
    if has_traits {
        "composer.mode composer.effort"
    } else {
        "composer.mode"
    }
}

impl ChatComposer {
    /// The trigger: a `ComposerControl` holding an ellipsis.
    pub(super) fn render_compact_controls_trigger(
        &mut self,
        size: ControlSize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.layer == Some(Layer::CompactControls);
        let bounds = self.trigger_bounds.compact.clone();
        composer_control("composer-compact-controls", size, open, cx)
            .child(track_bounds(&bounds))
            .child(
                Icon::new(IconName::Ellipsis)
                    .size(if size == ControlSize::Xs {
                        rems(0.75)
                    } else {
                        rems(1.)
                    })
                    .color(cx.app_theme().secondary_label),
            )
            .tooltip(|window, cx| Tooltip::new(COMPACT_CONTROLS_LABEL).build(window, cx))
            .on_click(
                cx.listener(|this, _, _, cx| this.set_layer(Some(Layer::CompactControls), cx)),
            )
            .into_any_element()
    }

    /// `MenuPopup align="start"`.
    pub(super) fn compact_controls_menu(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let hidden = self.footer_hidden_controls();
        let theme = cx.app_theme().clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let header = |text: &'static str| {
            div()
                .px_2()
                .py(rems(0.375))
                .font_weight(FontWeight::MEDIUM)
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let divider = || div().my_1().h(px(1.)).bg(theme.border).into_any_element();
        if hidden.traits
            && let Some(section) = self.traits_section_for(vm, cx)
        {
            rows.extend(self.traits_menu_rows(vm, &section, cx));
            rows.push(divider());
        }
        if hidden.mode && vm.settings.plan_mode_enabled {
            rows.push(header("Mode"));
            for (index, (value, label)) in [
                (InteractionMode::Default, "Chat"),
                (InteractionMode::Plan, "Plan"),
            ]
            .into_iter()
            .enumerate()
            {
                let selected = vm.interaction_mode == value;
                rows.push(
                    radio_row(("compact-mode", index), selected, label, cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // `value === props.interactionMode` is a no-op.
                            if !selected {
                                this.toggle_interaction_mode(cx);
                            }
                            this.close_layer(window, cx);
                        }))
                        .into_any_element(),
                );
            }
            rows.push(divider());
        }
        rows.push(header("Access"));
        for (index, option) in RUNTIME_MODES.iter().enumerate() {
            let mode = option.mode;
            let selected = vm.runtime_mode == mode;
            rows.push(
                radio_row(("compact-access", index), selected, option.label, cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !selected && let Some(source) = this.source(cx) {
                            source.set_runtime_mode(mode, cx);
                        }
                        this.close_layer(window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let _ = window;
        Some(floating(
            self.trigger_bounds.compact.get(),
            true,
            false,
            glass_surface(cx)
                .min_w(rems(10.))
                .p_1()
                .flex()
                .flex_col()
                .children(rows),
        ))
    }
}

/// `MenuRadioItem`: `grid-cols-[1rem_minmax(0,1fr)]`, the check in the first
/// column when selected.
fn radio_row(
    id: (&'static str, usize),
    selected: bool,
    label: &'static str,
    cx: &mut Context<ChatComposer>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.app_theme().clone();
    menu_row(id, false, cx)
        .gap_2()
        .child(
            div()
                .flex()
                .flex_none()
                .w(rems(1.))
                .items_center()
                .justify_center()
                .when(selected, |this| {
                    this.child(
                        Icon::new(IconName::Check)
                            .size(rems(1.))
                            .color(theme.foreground),
                    )
                }),
        )
        .child(div().min_w_0().flex_1().child(label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortcut_names_effort_only_when_traits_are_in_the_menu() {
        assert_eq!(compact_controls_shortcut(false), "composer.mode");
        assert_eq!(
            compact_controls_shortcut(true),
            "composer.mode composer.effort"
        );
    }
}
