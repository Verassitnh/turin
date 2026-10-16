//! Port of upstream/apps/web/src/components/ui/color-picker.tsx.
//!
//! `ColorSaturationValuePlane` and `ColorHueSlider`: pointer-dragged HSV
//! controls. Drag state lives in the view's slider fields (`slider_drag`,
//! `slider_bounds`), like the settings sliders.
//!
//! Deviations: no keyboard handling (arrow/Home/End on the plane and hue
//! slider, the `sr-only` range inputs) and no focus ring: the controls take
//! the pointer only. The CSS multi-stop hue gradient is six two-stop segments.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, Bounds, Context, CursorStyle, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, SharedString,
    Styled as _, canvas, div, linear_color_stop, linear_gradient, px, relative, rgb,
};

use super::view::SettingsView;
use crate::lib_color::HsvColor;
use crate::theme::color::with_alpha;

const PLANE_HEIGHT: f32 = 144.;
const PLANE_THUMB: f32 = 12.;
const HUE_THUMB: f32 = 16.;

/// `hsl(h 100% 50%)`.
fn hue_color(hue: f32) -> Hsla {
    gpui_kit::hsla((hue % 360.) / 360., 1., 0.5, 1.)
}

pub type BoundsCell = Rc<Cell<Bounds<Pixels>>>;

fn clamp(value: f32) -> f32 {
    value.clamp(0., 1.)
}

/// The plane position of a pointer, as saturation and value.
pub fn plane_value_at(x: f32, y: f32, width: f32, height: f32) -> Option<(f32, f32)> {
    (width > 0. && height > 0.).then(|| (clamp(x / width), 1. - clamp(y / height)))
}

/// The hue of a pointer position along the slider.
pub fn hue_at(x: f32, width: f32) -> Option<f32> {
    (width > 0.).then(|| clamp(x / width) * 360.)
}

/// The thumb's left/top offset: `calc(ratio * (100% - thumb) + thumb / 2)`.
pub fn thumb_offset(ratio: f32, extent: f32, thumb: f32) -> f32 {
    ratio * (extent - thumb) + thumb / 2.
}

impl SettingsView {
    fn color_bounds(&mut self, key: &SharedString) -> BoundsCell {
        self.slider_bounds
            .entry(key.clone())
            .or_insert_with(|| Rc::new(Cell::new(Bounds::default())))
            .clone()
    }

    /// `ColorSaturationValuePlane variant="edge"` (`h-36 rounded-none`).
    pub(super) fn color_saturation_value_plane(
        &mut self,
        cx: &mut Context<Self>,
        id: &str,
        value: HsvColor,
        on_change: impl Fn(&mut Self, HsvColor, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let key = SharedString::from(format!("plane-{id}"));
        let bounds = self.color_bounds(&key);
        let on_change = Rc::new(on_change);
        let update = {
            let bounds = bounds.clone();
            move |this: &mut Self, position: gpui_kit::Point<Pixels>, cx: &mut Context<Self>| {
                let bounds = bounds.get();
                if let Some((s, v)) = plane_value_at(
                    f32::from(position.x - bounds.origin.x),
                    f32::from(position.y - bounds.origin.y),
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                ) {
                    on_change(this, HsvColor { s, v, ..value }, cx);
                }
            }
        };
        let down = {
            let key = key.clone();
            let update = update.clone();
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.slider_drag = Some(key.clone());
                update(this, event.position, cx);
                cx.stop_propagation();
            })
        };
        let moved = {
            let key = key.clone();
            cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.slider_drag.as_ref() == Some(&key) && event.dragging() {
                    update(this, event.position, cx);
                }
            })
        };
        let up = cx.listener(|this, _, _, _| this.slider_drag = None);
        let width = f32::from(bounds.get().size.width).max(1.);
        div()
            .id(SharedString::from(key.to_string()))
            .relative()
            .w_full()
            .h(px(PLANE_HEIGHT))
            .overflow_hidden()
            .cursor(CursorStyle::Crosshair)
            .bg(hue_color(value.h))
            .on_mouse_down(MouseButton::Left, down)
            .on_mouse_move(moved)
            .on_mouse_up(MouseButton::Left, up)
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            // `linear-gradient(to right, #fff, transparent)`.
            .child(div().absolute().size_full().bg(linear_gradient(
                90.,
                linear_color_stop(rgb(0xffffff), 0.),
                linear_color_stop(with_alpha(rgb(0xffffff), 0.), 1.),
            )))
            // `linear-gradient(to top, #000, transparent)`.
            .child(div().absolute().size_full().bg(linear_gradient(
                0.,
                linear_color_stop(rgb(0x000000), 0.),
                linear_color_stop(with_alpha(rgb(0x000000), 0.), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .left(px(
                        thumb_offset(value.s, width, PLANE_THUMB) - PLANE_THUMB / 2.
                    ))
                    .top(px(
                        thumb_offset(1. - value.v, PLANE_HEIGHT, PLANE_THUMB) - PLANE_THUMB / 2.
                    ))
                    .size(px(PLANE_THUMB))
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(0xffffff))
                    .shadow(vec![gpui_kit::BoxShadow {
                        color: with_alpha(rgb(0x000000), 0.4).into(),
                        offset: gpui_kit::point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.),
                        inset: false,
                    }]),
            )
            .into_any_element()
    }

    /// `ColorHueSlider`: `h-6` hit area, a `h-2.5` rainbow track and a `size-4`
    /// thumb in the current hue.
    pub(super) fn color_hue_slider(
        &mut self,
        cx: &mut Context<Self>,
        id: &str,
        hue: f32,
        on_change: impl Fn(&mut Self, f32, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let key = SharedString::from(format!("hue-{id}"));
        let bounds = self.color_bounds(&key);
        let on_change = Rc::new(on_change);
        let update = {
            let bounds = bounds.clone();
            move |this: &mut Self, x: Pixels, cx: &mut Context<Self>| {
                let bounds = bounds.get();
                if let Some(next) =
                    hue_at(f32::from(x - bounds.origin.x), f32::from(bounds.size.width))
                {
                    on_change(this, next, cx);
                }
            }
        };
        let down = {
            let key = key.clone();
            let update = update.clone();
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
                    update(this, event.position.x, cx);
                }
            })
        };
        let up = cx.listener(|this, _, _, _| this.slider_drag = None);
        let width = f32::from(bounds.get().size.width).max(1.);
        let stops = [
            0xff0000u32,
            0xffff00,
            0x00ff00,
            0x00ffff,
            0x0000ff,
            0xff00ff,
            0xff0000,
        ];
        let track = div()
            .flex()
            .w_full()
            .h(px(10.))
            .rounded_full()
            .overflow_hidden()
            .children(stops.windows(2).map(|pair| {
                div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(rgb(pair[0]), 0.),
                    linear_color_stop(rgb(pair[1]), 1.),
                ))
            }));
        div()
            .id(SharedString::from(key.to_string()))
            .relative()
            .flex()
            .h_6()
            .items_center()
            .rounded_full()
            .cursor(CursorStyle::PointingHand)
            .on_mouse_down(MouseButton::Left, down)
            .on_mouse_move(moved)
            .on_mouse_up(MouseButton::Left, up)
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(track)
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .mt(px(-HUE_THUMB / 2.))
                    .left(px(
                        thumb_offset(hue / 360., width, HUE_THUMB) - HUE_THUMB / 2.
                    ))
                    .size(px(HUE_THUMB))
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(0xffffff))
                    .bg(hue_color(hue))
                    .shadow(vec![gpui_kit::BoxShadow {
                        color: with_alpha(rgb(0x000000), 0.4).into(),
                        offset: gpui_kit::point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.),
                        inset: false,
                    }]),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pointer_positions_on_the_plane_to_saturation_and_value() {
        assert_eq!(plane_value_at(0., 0., 200., 100.), Some((0., 1.)));
        assert_eq!(plane_value_at(200., 100., 200., 100.), Some((1., 0.)));
        assert_eq!(plane_value_at(100., 50., 200., 100.), Some((0.5, 0.5)));
        // A pointer outside the plane clamps to its edge.
        assert_eq!(plane_value_at(-40., 500., 200., 100.), Some((0., 0.)));
        assert_eq!(plane_value_at(10., 10., 0., 100.), None);
    }

    #[test]
    fn maps_pointer_positions_on_the_slider_to_a_hue() {
        assert_eq!(hue_at(0., 200.), Some(0.));
        assert_eq!(hue_at(100., 200.), Some(180.));
        assert_eq!(hue_at(999., 200.), Some(360.));
        assert_eq!(hue_at(5., 0.), None);
    }

    #[test]
    fn keeps_the_thumb_inside_the_control_at_its_extremes() {
        assert_eq!(thumb_offset(0., 200., 16.), 8.);
        assert_eq!(thumb_offset(1., 200., 16.), 192.);
        assert_eq!(thumb_offset(0.5, 200., 12.), 100.);
    }
}
