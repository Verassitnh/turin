//! Port of upstream/apps/web/src/components/settings/ExpandableText.tsx

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, rems,
};

use super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant};

/// Whether upstream shows the expand toggle (`text.length > 180 || includes newline`).
pub fn can_expand(text: &str) -> bool {
    text.len() > 180 || text.contains('\n')
}

impl SettingsView {
    /// Long error text clamped to three lines with a toggle to reveal the rest.
    pub(super) fn expandable_text(
        &mut self,
        id: &str,
        text: &str,
        expand_label: &str,
        mono: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme();
        let expandable = can_expand(text);
        let expanded = self.panels.expanded_text.contains(id);
        let collapsed = expandable && !expanded;
        let toggle_id = id.to_owned();
        div()
            .min_w_0()
            .child(
                div()
                    .id(SharedString::from(format!("{id}-body")))
                    .when(collapsed, |this| {
                        this.overflow_hidden()
                            .max_h(rems(1.625 * 3.))
                            .line_height(rems(1.625))
                    })
                    .when(!collapsed, |this| this.line_height(rems(1.625)))
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .when(mono, |this| this.font_family(theme.fonts.mono.clone()))
                    .child(text.to_owned()),
            )
            .when(expandable, |this| {
                this.child(
                    Button::new(SharedString::from(format!("{id}-toggle")))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::Xs)
                        .label(if expanded {
                            SharedString::from("Show less")
                        } else {
                            SharedString::from(expand_label)
                        })
                        .mt_1()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.panels.expanded_text.contains(&toggle_id) {
                                this.panels.expanded_text.remove(&toggle_id);
                            } else {
                                this.panels.expanded_text.insert(toggle_id.clone());
                            }
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::can_expand;

    #[test]
    fn short_single_line_text_does_not_expand() {
        assert!(!can_expand("ok"));
    }

    #[test]
    fn long_text_expands() {
        assert!(can_expand(&"x".repeat(181)));
    }

    #[test]
    fn multiline_text_expands() {
        assert!(can_expand("line one\nline two"));
    }
}
