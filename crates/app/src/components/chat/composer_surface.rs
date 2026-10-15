//! Port of upstream/apps/web/src/components/chat/ComposerSurface.tsx plus the
//! static frame of ChatComposer.tsx (body, placeholder, footer controls and
//! the primary send action from ComposerPrimaryActions.tsx).
//!
//! Placeholder only: the prompt editor, model picker and actions are inert
//! until the composer task. GPUI has no backdrop blur, so the glass layer
//! is the translucent fill alone.

use gpui_kit::{
    App, CursorStyle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, StatefulInteractiveElement as _, Styled as _, Window, div, px, rems, svg,
};

use crate::assets::{CHEVRON_DOWN, SEND_ARROW};
use crate::theme::ActiveAppTheme;
use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, LEADING_RELAXED, RADIUS_3XL, RADIUS_MD, TEXT_SM, tw};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// Composer placeholder copy (ChatComposer.tsx, default state).
pub const COMPOSER_PLACEHOLDER: &str =
    "Ask anything, @tag files/folders, $use skills, or / for commands";

/// `ComposerControl size=sm`: `h-7 gap-1.5 px-2.5 font-medium
/// text-secondary-label text-sm`, hover `bg-accent`, icon `size-4` muted.
fn composer_control(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.app_theme();
    div()
        .id(id)
        .flex()
        .h_7()
        .flex_shrink_0()
        .items_center()
        .gap(rems(0.375))
        .px(rems(0.625))
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(with_alpha(tw::BLACK, 0.))
        .text_size(TEXT_SM.size)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.secondary_label)
        .cursor(CursorStyle::PointingHand)
        .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
        .child(Icon::new(icon).color(theme.muted_foreground))
        .child(label)
        .child(
            // ComposerControlChevron size=sm: `size-3.5 text-icon-muted`,
            // pulled in by the control's `[&_svg]:-mx-0.5`. Stroke 2.25
            // (`brand/chevron-down.svg`), not the stock lucide stroke 2.
            div().mx(px(-2.)).child(
                Icon::path(CHEVRON_DOWN)
                    .size(rems(0.875))
                    .color(theme.icon_muted),
            ),
        )
}

#[derive(IntoElement)]
pub struct ComposerSurface;

impl RenderOnce for ComposerSurface {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let dark = theme.is_dark();
        // Light: `--card` at the 80% glass opacity; dark: `--surface-raised`.
        // Flattened over the canvas: GPUI paints box shadows under the
        // element (CSS clips them to outside the border box), so a
        // translucent fill would show its own drop shadow through it.
        let glass = if dark {
            theme.surface_raised
        } else {
            theme.card
        };
        let surface = color_mix(MixSpace::Srgb, glass, 0.8, theme.background);
        let outline = if dark {
            with_alpha(tw::WHITE, 0.05)
        } else {
            with_alpha(tw::BLACK, 0.08)
        };
        let prompt_size = theme.fonts.size_prompt;

        div()
            .relative()
            .w_full()
            .rounded(RADIUS_3XL)
            .bg(surface)
            .border_1()
            .border_color(outline)
            .shadow(if dark {
                Vec::new()
            } else {
                tokens::shadow_composer(false)
            })
            .child(
                // Body: `px-4 pt-4 pb-2`; editor `min-h-19.5 leading-relaxed`.
                div().px_4().pt_4().pb_2().child(
                    div()
                        .min_h(rems(4.875))
                        .font_family(theme.fonts.composer.clone())
                        .text_size(px(prompt_size))
                        .line_height(px(prompt_size * LEADING_RELAXED))
                        .text_color(with_alpha(theme.placeholder, 0.75))
                        .child(COMPOSER_PLACEHOLDER),
                ),
            )
            .child(
                // Footer: `px-4 pb-4`, controls left, actions right.
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_4()
                    .pb_4()
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_center()
                            .gap_1()
                            .ml(rems(-0.625))
                            .child(composer_control(
                                "composer-model-picker",
                                IconName::Bot,
                                "Model",
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("composer-attach")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::IconSm)
                                    .icon(IconName::Paperclip)
                                    .tooltip(Tooltip::new("Attach files")),
                            )
                            .child(
                                // Send: 32px round `bg-message-action`, disabled while empty.
                                div()
                                    .id("composer-send")
                                    .flex()
                                    .size_8()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(theme.message_action)
                                    // Empty draft → disabled: `disabled:opacity-64
                                    // disabled:shadow-none`. The enabled
                                    // message-action/24 shadow lands with the composer.
                                    .opacity(0.64)
                                    .child(
                                        svg()
                                            .path(SEND_ARROW)
                                            .size(px(14.))
                                            .text_color(theme.message_action_foreground),
                                    )
                                    .tooltip(|window, cx| {
                                        Tooltip::new("Submit message").build(window, cx)
                                    }),
                            ),
                    ),
            )
    }
}
