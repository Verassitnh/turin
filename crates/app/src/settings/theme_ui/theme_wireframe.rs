//! Port of upstream/apps/web/src/components/settings/ThemeWireframe.tsx
//!
//! A simple miniature of the app: sidebar, a short conversation, the
//! composer, and the orchestrator panel floating over the interface as an
//! island with horizontal agent rows. Geometry is percentage based; the
//! caller sizes the frame.

use gpui_kit::{
    AnyElement, Div, Hsla, IntoElement, ParentElement as _, Styled as _, div, px, relative,
};

use super::theme_preview_circles::ThemeCardPreviewColors;
use crate::theme::AppTheme;
use crate::theme::color::{color, with_alpha};

/// Which half of the frame a pane is clipped to (`clip: "left" | "right"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireframeClip {
    Left,
    Right,
}

pub struct WireframePane {
    pub colors: ThemeCardPreviewColors,
    pub clip: Option<WireframeClip>,
}

/// `rgb(127 127 127 / 0.25)`.
fn line_color() -> Hsla {
    with_alpha(gpui_kit::rgba(0x7f7f7f00), 0.25).into()
}

/// `ThemeWireframePane`.
fn wireframe_pane(colors: &ThemeCardPreviewColors) -> Div {
    let line = line_color();
    let c = |value: &gpui_kit::SharedString| color(value.as_ref());
    let region = |left: f32, top: f32, w: f32, h: f32, bg: Hsla| {
        div()
            .absolute()
            .left(relative(left))
            .top(relative(top))
            .w(relative(w))
            .h(relative(h))
            .bg(bg)
    };
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(c(&colors.canvas))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(0.22))
                .bg(c(&colors.sidebar))
                .border_r_1()
                .border_color(line),
        )
        // Sidebar: search, then thread rows.
        .child(
            region(0.03, 0.08, 0.16, 0.08, c(&colors.surface))
                .rounded_md()
                .border_1()
                .border_color(line),
        )
        .child(region(0.03, 0.22, 0.16, 0.07, c(&colors.accent_surface)).rounded_md())
        .child(
            region(0.03, 0.32, 0.16, 0.07, c(&colors.message_surface))
                .rounded_md()
                .opacity(0.7),
        )
        .child(
            region(0.03, 0.42, 0.16, 0.07, c(&colors.message_surface))
                .rounded_md()
                .opacity(0.5),
        )
        // Conversation.
        .child(region(0.48, 0.11, 0.24, 0.09, c(&colors.message_surface)).rounded_lg())
        .child(region(0.27, 0.28, 0.34, 0.05, line).rounded_sm())
        .child(region(0.27, 0.38, 0.26, 0.05, line).rounded_sm())
        // Composer.
        .child(
            region(0.26, 0.77, 0.68, 0.15, c(&colors.surface))
                .rounded_md()
                .border_1()
                .border_color(line)
                .flex()
                .items_center()
                .justify_between()
                .px_1()
                .child(
                    div()
                        .h(relative(0.26))
                        .w(relative(0.34))
                        .rounded_full()
                        .bg(line)
                        .opacity(0.7),
                )
                .child(
                    div()
                        .h(relative(0.58))
                        .w(px(10.))
                        .rounded_full()
                        .bg(c(&colors.message_action)),
                ),
        )
        // Orchestrator island floating over the composer.
        .child({
            let island = region(0.75, 0.08, 0.2, 0.46, c(&colors.surface))
                .rounded_lg()
                .border_1()
                .border_color(line);
            (0..3).fold(island, |island, row| {
                island.child(
                    div()
                        .absolute()
                        .left(relative(0.11))
                        .right(relative(0.11))
                        .top(relative(0.10 + row as f32 * 0.30))
                        .h(relative(0.20))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size(px(5.))
                                .rounded_full()
                                .opacity(0.55)
                                .bg(match row {
                                    0 => color("#34d399"),
                                    1 => c(&colors.message_action),
                                    _ => color("#fbbf24"),
                                }),
                        )
                        .child(
                            div()
                                .h(relative(0.3))
                                .w(relative(0.52))
                                .rounded_sm()
                                .bg(line),
                        ),
                )
            })
        })
}

/// `ThemeWireframe`: `relative block w-full overflow-hidden rounded-lg border
/// border-border/60`; `height` is the caller's sizing class (`h-[8.75rem]`).
pub fn theme_wireframe(
    panes: Vec<WireframePane>,
    height: gpui_kit::Rems,
    theme: &AppTheme,
) -> AnyElement {
    let frame = div()
        .relative()
        .w_full()
        .h(height)
        .overflow_hidden()
        .rounded_lg()
        .border_1()
        .border_color(with_alpha(theme.border, 0.6));
    panes
        .into_iter()
        .fold(frame, |frame, pane| match pane.clip {
            None => frame.child(wireframe_pane(&pane.colors)),
            // `clip: "left"` keeps the left half (`calc(50% - 1px)`).
            Some(clip) => {
                let left = clip == WireframeClip::Left;
                let half = |this: Div| {
                    if left { this.left_0() } else { this.right_0() }
                };
                frame.child(
                    half(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(relative(0.5))
                            .overflow_hidden(),
                    )
                    .child(
                        half(div().absolute().top_0().bottom_0().w(relative(2.0)))
                            .child(wireframe_pane(&pane.colors)),
                    ),
                )
            }
        })
        .into_any_element()
}
