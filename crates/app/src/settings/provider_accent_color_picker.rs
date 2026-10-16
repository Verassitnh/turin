//! Port of upstream/apps/web/src/components/settings/ProviderAccentColorPicker.tsx.
//!
//! A swatch button opening a popover with a saturation/value plane, a hue
//! slider and a hex field; "Clear color" when a color is set. With a commit
//! delay the new color shows at once and is saved after the pointer settles.
//!
//! Deviation: the picker is always the inline layout's swatch; the stacked
//! layout (caption and description) has no caller in Turin.

use std::collections::HashMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::controls::{FieldSize, PopupAlign, text_field, toggle_menu, with_popup};
use super::view::SettingsView;
use crate::lib_color::{HsvColor, hex_to_hsv, hsv_to_hex};
use crate::state::helpers::provider_instance_display::normalize_provider_accent_color;
use crate::theme::ActiveAppTheme;
use crate::theme::color::{color, with_alpha};
use crate::theme::tokens::{self, RADIUS_LG, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

pub const FALLBACK_ACCENT_COLOR: &str = "#2563eb";

/// `/^#[\da-f]{6}$/i`.
pub fn is_hex_color(value: &str) -> bool {
    value
        .strip_prefix('#')
        .is_some_and(|hex| hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Per-picker state: the HSV the panel edits, the hex draft being typed, the
/// optimistic value and the commit waiting for its delay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccentPickerState {
    pub hsv: Option<HsvColor>,
    pub hex_draft: Option<String>,
    /// `optimisticValue`; `None` follows the prop.
    pub optimistic: Option<String>,
    /// `pendingCommitRef`.
    pub pending_commit: Option<String>,
    /// Bumped by each delayed commit so a stale timer does nothing.
    pub generation: u64,
}

pub type AccentPickerStates = HashMap<String, AccentPickerState>;

impl SettingsView {
    fn accent_state(&mut self, id: &str) -> &mut AccentPickerState {
        self.panels
            .providers
            .accent
            .entry(id.to_owned())
            .or_default()
    }

    /// `commitAccentColor`.
    fn commit_accent_color(
        &mut self,
        id: &str,
        value: &str,
        commit_delay_ms: u64,
        on_commit: std::rc::Rc<dyn Fn(&mut Self, String, &mut Context<Self>)>,
        cx: &mut Context<Self>,
    ) {
        let normalized = normalize_provider_accent_color(Some(value)).unwrap_or_default();
        self.accent_state(id).optimistic = Some(normalized.clone());
        if commit_delay_ms == 0 {
            self.accent_state(id).pending_commit = None;
            on_commit(self, normalized, cx);
            cx.notify();
            return;
        }
        let generation = {
            let state = self.accent_state(id);
            state.pending_commit = Some(normalized);
            state.generation += 1;
            state.generation
        };
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(commit_delay_ms))
                .await;
            this.update(cx, |this, cx| {
                let state = this.accent_state(&id);
                if state.generation != generation {
                    return;
                }
                if let Some(pending) = state.pending_commit.take() {
                    on_commit(this, pending, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// `ProviderAccentColorPicker layout="inline"`.
    pub(super) fn provider_accent_color_picker(
        &mut self,
        id: &str,
        display_name: &str,
        value: Option<&str>,
        commit_delay_ms: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let _ = window;
        let theme = cx.app_theme().clone();
        let on_commit: std::rc::Rc<dyn Fn(&mut Self, String, &mut Context<Self>)> =
            std::rc::Rc::new(on_commit);
        let state = self.accent_state(id).clone();
        // The prop wins once nothing is waiting to commit.
        let shown = if state.pending_commit.is_some() {
            state.optimistic.clone()
        } else {
            value.map(str::to_owned)
        };
        let normalized = normalize_provider_accent_color(shown.as_deref());
        let menu_id = SharedString::from(format!("accent-{id}"));
        let open = self.menus.is_open(&menu_id);
        let tooltip_label = format!(
            "{} accent color for {display_name}",
            if normalized.is_some() {
                "Change"
            } else {
                "Add"
            }
        );
        let initial = normalized
            .clone()
            .unwrap_or_else(|| FALLBACK_ACCENT_COLOR.to_owned());
        let open_id = id.to_owned();
        let open_initial = initial.clone();
        let toggle = toggle_menu(cx, |this| &mut this.menus, menu_id.clone());
        let trigger = div()
            .id(SharedString::from(format!("accent-trigger-{id}")))
            .flex()
            .size_7()
            .items_center()
            .justify_center()
            .rounded(tokens::RADIUS_MD)
            .cursor_pointer()
            .when_some(normalized.as_deref(), |this, hex| this.bg(color(hex)))
            .when(normalized.is_none(), |this| {
                this.hover(|style| style.bg(theme.accent))
            })
            .tooltip({
                let tooltip = Tooltip::new(tooltip_label);
                move |window, cx| tooltip.clone().build(window, cx)
            })
            .on_click(cx.listener(move |this, event, window, cx| {
                // Each opening edits from the committed color.
                let state = this.accent_state(&open_id);
                state.hsv = Some(hex_to_hsv(&open_initial));
                state.hex_draft = None;
                toggle(event, window, cx);
            }))
            .when(normalized.is_none(), |this| {
                this.child(
                    Icon::new(IconName::Plus)
                        .size(rems(1.))
                        .color(theme.muted_foreground),
                )
            });

        let popup = open.then(|| {
            let hsv = self
                .accent_state(id)
                .hsv
                .unwrap_or_else(|| hex_to_hsv(&initial));
            let hex_text = self
                .accent_state(id)
                .hex_draft
                .clone()
                .unwrap_or_else(|| hsv_to_hex(hsv.h, hsv.s, hsv.v));
            let plane_id = id.to_owned();
            let plane_commit = on_commit.clone();
            let plane = self.color_saturation_value_plane(cx, &format!("accent-{id}"), hsv, {
                move |this, next, cx| {
                    this.accent_state(&plane_id).hsv = Some(next);
                    this.accent_state(&plane_id).hex_draft = None;
                    let hex = hsv_to_hex(next.h, next.s, next.v);
                    this.commit_accent_color(
                        &plane_id,
                        &hex,
                        commit_delay_ms,
                        plane_commit.clone(),
                        cx,
                    );
                }
            });
            let hue_id = id.to_owned();
            let hue_commit = on_commit.clone();
            let hue =
                self.color_hue_slider(cx, &format!("accent-{id}"), hsv.h, move |this, h, cx| {
                    let current = this.accent_state(&hue_id).hsv.unwrap_or(hsv);
                    let next = HsvColor { h, ..current };
                    this.accent_state(&hue_id).hsv = Some(next);
                    this.accent_state(&hue_id).hex_draft = None;
                    let hex = hsv_to_hex(next.h, next.s, next.v);
                    this.commit_accent_color(
                        &hue_id,
                        &hex,
                        commit_delay_ms,
                        hue_commit.clone(),
                        cx,
                    );
                });
            let hex_id = id.to_owned();
            let hex_commit = on_commit.clone();
            let input = self.input_live(
                &format!("accent-hex-{id}"),
                &hex_text,
                "",
                window,
                cx,
                move |this, text, _, cx| {
                    this.accent_state(&hex_id).hex_draft = Some(text.clone());
                    if is_hex_color(&text) {
                        this.accent_state(&hex_id).hsv = Some(hex_to_hsv(&text));
                        this.commit_accent_color(
                            &hex_id,
                            &text,
                            commit_delay_ms,
                            hex_commit.clone(),
                            cx,
                        );
                    }
                    cx.notify();
                },
            );
            let clear_id = id.to_owned();
            let clear_commit = on_commit.clone();
            let clear = normalized.is_some().then(|| {
                div()
                    .border_t_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .p_1()
                    .child(
                        Button::new(SharedString::from(format!("accent-clear-{id}")))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::Compact)
                            .icon(IconName::X)
                            .icon_size(rems(0.875))
                            .label("Clear color")
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menus.close();
                                this.commit_accent_color(
                                    &clear_id,
                                    "",
                                    0,
                                    clear_commit.clone(),
                                    cx,
                                );
                            })),
                    )
            });
            div()
                .id(SharedString::from(format!("accent-popup-{id}")))
                .occlude()
                .w(rems(14.))
                .overflow_hidden()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                .bg(theme.popover)
                .shadow(tokens::shadow_popover(theme.is_dark()))
                .text_color(theme.popover_foreground)
                .child(plane)
                .child(
                    div().flex().flex_col().gap_3().p_3().child(hue).child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_size(TEXT_XS.size)
                            .child(text_field(&input, None, FieldSize::Sm, true, cx)),
                    ),
                )
                .children(clear)
                .into_any_element()
        });
        with_popup(trigger, popup, PopupAlign::Start, px(28.)).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_six_digit_hex_while_typing() {
        assert!(is_hex_color("#2563eb"));
        assert!(is_hex_color("#2563EB"));
        assert!(!is_hex_color("#2563e"));
        assert!(!is_hex_color("2563eb"));
        assert!(!is_hex_color("#2563eg"));
        assert!(!is_hex_color("#2563ebb"));
    }

    #[test]
    fn opens_from_the_fallback_when_no_color_is_set() {
        assert_eq!(FALLBACK_ACCENT_COLOR, "#2563eb");
        let hsv = hex_to_hsv(FALLBACK_ACCENT_COLOR);
        assert_eq!(hsv_to_hex(hsv.h, hsv.s, hsv.v), FALLBACK_ACCENT_COLOR);
    }
}
