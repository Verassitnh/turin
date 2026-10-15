//! `.settings-slider` (upstream index.css): a native range input restyled as a
//! 6px track filled with `--primary` up to a 16px thumb.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, Bounds, Context, CursorStyle, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, SharedString, Styled as _, canvas,
    div, px, relative,
};

use super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens;

/// Thumb diameter (`1rem`).
const THUMB: f32 = 16.;

pub type BoundsCell = Rc<Cell<Bounds<Pixels>>>;

/// Maps a pointer x (relative to the track) to a stepped value.
pub fn slider_value_at(x: f32, width: f32, min: f64, max: f64, step: f64) -> f64 {
    let usable = (width - THUMB).max(1.);
    let ratio = ((x - THUMB / 2.) / usable).clamp(0., 1.) as f64;
    let raw = min + ratio * (max - min);
    let snapped = (raw / step).round() * step;
    snapped.clamp(min, max)
}

impl SettingsView {
    pub(super) fn slider(
        &mut self,
        cx: &mut Context<Self>,
        id: &str,
        value: f64,
        min: f64,
        max: f64,
        step: f64,
        on_change: impl Fn(&mut Self, f64, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let key = SharedString::from(id.to_owned());
        let bounds: BoundsCell = self
            .slider_bounds
            .entry(key.clone())
            .or_insert_with(|| Rc::new(Cell::new(Bounds::default())))
            .clone();
        let ratio = (((value - min) / (max - min)) as f32).clamp(0., 1.);
        let on_change = Rc::new(on_change);

        let update_from = {
            let bounds = bounds.clone();
            let on_change = on_change.clone();
            move |this: &mut Self, x: Pixels, cx: &mut Context<Self>| {
                let bounds = bounds.get();
                let local = f32::from(x - bounds.origin.x);
                let next = slider_value_at(local, f32::from(bounds.size.width), min, max, step);
                on_change(this, next, cx);
            }
        };
        let down = {
            let key = key.clone();
            let update = update_from.clone();
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.slider_drag = Some(key.clone());
                update(this, event.position.x, cx);
                cx.stop_propagation();
            })
        };
        let moved = {
            let key = key.clone();
            cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.slider_drag.as_ref() == Some(&key) && event.dragging() {
                    update_from(this, event.position.x, cx);
                }
            })
        };
        let up = cx.listener(|this, _, _, _| {
            this.slider_drag = None;
        });

        let fill_extra = THUMB / 2. - THUMB * ratio;
        div()
            .id(SharedString::from(format!("slider-{id}")))
            .relative()
            .h_6()
            .min_w_0()
            .flex_1()
            .cursor(CursorStyle::PointingHand)
            .on_mouse_down(MouseButton::Left, down)
            .on_mouse_move(moved)
            .on_mouse_up(MouseButton::Left, up)
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            // Track.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(9.))
                    .h(px(6.))
                    .rounded_full()
                    .bg(with_alpha(theme.muted_foreground, 0.22))
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.55))
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(relative(ratio))
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .right(px(-fill_extra))
                                    .bg(theme.primary),
                            ),
                    ),
            )
            // Thumb.
            .child(
                div()
                    .absolute()
                    .top(px(4.))
                    .left(relative(ratio))
                    .ml(px(-THUMB * ratio))
                    .size(px(THUMB))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.primary)
                    .bg(theme.background)
                    .shadow(tokens::shadow_xs(0.12)),
            )
            .into_any_element()
    }
}

/// `<output>` badge next to a slider: `min-w-12 rounded-md bg-muted px-2 py-1
/// text-center font-mono text-xs font-medium tabular-nums`.
pub fn value_badge(text: String, min_width_rems: f32, cx: &gpui_kit::App) -> AnyElement {
    let theme = cx.app_theme();
    div()
        .min_w(gpui_kit::rems(min_width_rems))
        .rounded_md()
        .bg(theme.muted)
        .px_2()
        .py_1()
        .text_center()
        .font_family(theme.fonts.mono.clone())
        .text_size(tokens::TEXT_XS.size)
        .line_height(tokens::TEXT_XS.line_height)
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(theme.foreground)
        .child(text)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pointer_positions_to_stepped_values() {
        // 200px track: the usable span is 184px between thumb centres.
        assert_eq!(slider_value_at(0., 200., 50., 200., 5.), 50.);
        assert_eq!(slider_value_at(200., 200., 50., 200., 5.), 200.);
        assert_eq!(slider_value_at(100., 200., 50., 200., 5.), 125.);
        // Snaps to the step.
        assert_eq!(slider_value_at(60., 200., 0., 400., 25.) % 25., 0.);
    }
}
