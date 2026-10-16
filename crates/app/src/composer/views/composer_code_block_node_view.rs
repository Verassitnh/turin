//! Port of upstream/apps/web/src/components/chat/ComposerCodeBlockNodeView.tsx
//!
//! Draws a composer fence in the chat view's own code block frame, so a draft
//! looks like the message it is about to become, with the language title
//! swapped for a picker that rewrites the fence's language in place.
//!
//! The chrome stops at the language. Chat's wrap and copy buttons act on text
//! the reader cannot change; here the text is the draft, and both are already
//! a selection away. The picker itself is `language_picker_view`; this frame
//! only reports where it was clicked ([`PromptEditorEvent::OpenCodeLanguagePicker`]).
//! Not ported: `MarkdownCodeBlockFrame`'s `my-[0.65rem]` margin (the editor
//! spaces its blocks) and the shiki wrapper classes (the text is the
//! highlighted flow).

use gpui_kit::{
    AnyElement, Context, CursorStyle, ElementId, FontWeight, InteractiveElement as _,
    IntoElement as _, MouseButton, MouseDownEvent, ParentElement as _, Pixels, SharedString,
    Styled as _, div, px, rems,
};

use crate::composer::views::prompt_editor::{PromptEditor, PromptEditorEvent};
use crate::theme::AppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_XS};
use crate::ui::{Icon, IconName};

/// `bg-secondary border-border/70`, dark `bg-input/32 border-transparent`.
fn frame_colors(theme: &AppTheme) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    if theme.is_dark() {
        (
            with_alpha(theme.input, 0.32).into(),
            gpui_kit::transparent_black(),
        )
    } else {
        (theme.secondary, with_alpha(theme.border, 0.7).into())
    }
}

/// The frame around one code block: the language picker trigger in the
/// header, `text` (the highlighted flow) under it. `rem` is the editor's
/// rem size, which the header padding follows.
pub fn composer_code_block_node_view(
    index: usize,
    label: impl Into<SharedString>,
    text: AnyElement,
    theme: &AppTheme,
    rem: Pixels,
    cx: &mut Context<PromptEditor>,
) -> AnyElement {
    let label = label.into();
    let (bg, border) = frame_colors(theme);
    let header_color = with_alpha(theme.foreground, 0.72);
    div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(
            div()
                .flex()
                .items_center()
                .pt(rem * 0.375)
                .pr(rem * 0.375)
                .pl(rem * 0.75)
                .child(
                    div()
                        .id(ElementId::NamedInteger(
                            "composer-code-language".into(),
                            index as u64,
                        ))
                        .flex()
                        .items_center()
                        // ComposerCodeBlockLanguagePicker: Button `ghost-muted`,
                        // size `xs` (`gap-1`, `sm:text-xs`, `font-medium`).
                        .gap(rems(0.25))
                        .h(rems(1.5))
                        .px(rems(0.5))
                        .ml(rems(-0.5))
                        .rounded(px(6.))
                        .cursor(CursorStyle::PointingHand)
                        .text_color(theme.muted_foreground)
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(TEXT_XS.size)
                        .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                cx.emit(PromptEditorEvent::OpenCodeLanguagePicker {
                                    block: index,
                                    anchor: event.position,
                                });
                            }),
                        )
                        .child(
                            Icon::new(IconName::FileText)
                                .size(rems(0.875))
                                .color(header_color.into()),
                        )
                        .child(div().max_w(rems(10.)).truncate().child(label))
                        .child(
                            // `opacity-60` on the label color.
                            Icon::new(IconName::ChevronDown)
                                .size(rems(0.875))
                                .color(with_alpha(theme.muted_foreground, 0.6).into()),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .px(rems(0.75))
                .pt(rems(0.25))
                .pb(rems(0.5))
                .child(text),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::appearance_fonts::resolve_appearance_fonts;
    use crate::theme::theme_palettes::ThemeAppearance;

    fn theme(appearance: ThemeAppearance) -> AppTheme {
        AppTheme::resolve(
            None,
            appearance,
            0,
            resolve_appearance_fonts(&Default::default(), |_| true),
        )
    }

    #[test]
    fn the_frame_is_a_tinted_fill_in_dark_and_an_outlined_one_in_light() {
        let dark = theme(ThemeAppearance::Dark);
        let (fill, outline) = frame_colors(&dark);
        assert_eq!(fill, gpui_kit::Hsla::from(with_alpha(dark.input, 0.32)));
        assert_eq!(outline, gpui_kit::transparent_black());
        let light = theme(ThemeAppearance::Light);
        let (fill, outline) = frame_colors(&light);
        assert_eq!(fill, light.secondary);
        assert_eq!(outline, gpui_kit::Hsla::from(with_alpha(light.border, 0.7)));
    }
}
