//! Port of upstream/apps/web/src/components/ui/color-picker.tsx
//!
//! The saturation plane is `h-32` (inset, `rounded-lg`) or `h-36` (edge).
//! Pointer position maps through [`saturation_value_at`]; arrow keys step
//! 0.02, or 0.1 with Shift. The hue slider is `h-6` with an `h-2.5` track.
//! Hue arrows step 1 degree, or 10 with Shift, and wrap with `(value + 360) % 360`.
//!
//! Not ported: the 80ms thumb transition. GPUI gradients take two stops, so
//! the hue track is six two-stop segments instead of one rainbow.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, IntoElement, ParentElement, RenderOnce, Rgba, SharedString, Styled, Window, div,
    linear_color_stop, linear_gradient, px, relative,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_XS};

fn solid(red: u8, green: u8, blue: u8, alpha: f32) -> Rgba {
    Rgba {
        r: red as f32 / 255.,
        g: green as f32 / 255.,
        b: blue as f32 / 255.,
        a: alpha,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HsvColor {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneVariant {
    Inset,
    Edge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorKey {
    ArrowRight,
    ArrowUp,
    ArrowLeft,
    ArrowDown,
    Home,
    End,
    Other,
}

pub fn clamp01(value: f64) -> f64 {
    value.clamp(0., 1.)
}

/// Pointer position inside the saturation plane. `None` when a side is 0.
pub fn saturation_value_at(x: f64, y: f64, width: f64, height: f64) -> Option<(f64, f64)> {
    if width == 0. || height == 0. {
        return None;
    }
    Some((clamp01(x / width), 1. - clamp01(y / height)))
}

fn saturation_step(shift: bool) -> f64 {
    if shift { 0.1 } else { 0.02 }
}

/// Arrow / Home / End adjustment for one axis. `None` leaves the value alone.
pub fn apply_plane_key(value: f64, key: ColorKey, shift: bool) -> Option<f64> {
    let next = match key {
        ColorKey::ArrowRight | ColorKey::ArrowUp => clamp01(value + saturation_step(shift)),
        ColorKey::ArrowLeft | ColorKey::ArrowDown => clamp01(value - saturation_step(shift)),
        ColorKey::Home => 0.,
        ColorKey::End => 1.,
        ColorKey::Other => return None,
    };
    Some(next)
}

/// Hue from a horizontal pointer. `None` when the track has no width.
pub fn hue_at(x: f64, width: f64) -> Option<f64> {
    if width == 0. {
        return None;
    }
    Some(clamp01(x / width) * 360.)
}

/// Hue arrows wrap. Other keys do not change the value.
pub fn apply_hue_key(value: f64, key: ColorKey, shift: bool) -> Option<f64> {
    let direction = match key {
        ColorKey::ArrowRight | ColorKey::ArrowUp => 1.,
        ColorKey::ArrowLeft | ColorKey::ArrowDown => -1.,
        _ => return None,
    };
    let step = if shift { 10. } else { 1. };
    Some((value + direction * step + 360.) % 360.)
}

/// Thumb center inset so a `size-3` (12px) knob stays inside the plane.
pub fn plane_thumb_origin(amount: f64, span: f64) -> f64 {
    amount * (span - 12.) + 6.
}

/// Thumb center inset for the `size-4` (16px) hue knob.
pub fn hue_thumb_origin(hue: f64, span: f64) -> f64 {
    (hue / 360.) * (span - 16.) + 8.
}

fn hue_color(hue: f64) -> Rgba {
    let (r, g, b) = hue_rgb(hue);
    solid(r, g, b, 1.)
}

fn hue_rgb(hue: f64) -> (u8, u8, u8) {
    let hue = ((hue % 360.) + 360.) % 360.;
    let section = hue / 60.;
    let rising = ((section.fract()) * 255.).round() as u8;
    let falling = 255 - rising;
    match section as u8 {
        0 => (255, rising, 0),
        1 => (falling, 255, 0),
        2 => (0, 255, rising),
        3 => (0, falling, 255),
        4 => (rising, 0, 255),
        _ => (255, 0, falling),
    }
}

#[derive(IntoElement)]
pub struct ColorSaturationValuePlane {
    label: SharedString,
    value: HsvColor,
    variant: PlaneVariant,
}

impl ColorSaturationValuePlane {
    pub fn new(label: impl Into<SharedString>, value: HsvColor) -> Self {
        Self {
            label: label.into(),
            value,
            variant: PlaneVariant::Inset,
        }
    }

    pub fn variant(mut self, variant: PlaneVariant) -> Self {
        self.variant = variant;
        self
    }
}

impl RenderOnce for ColorSaturationValuePlane {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let height = if self.variant == PlaneVariant::Edge {
            px(144.)
        } else {
            px(128.)
        };
        let s = self.value.s as f32;
        let v = self.value.v as f32;
        div()
            .relative()
            .overflow_hidden()
            .h(height)
            .when(self.variant == PlaneVariant::Inset, |this| {
                this.rounded(RADIUS_LG)
            })
            .bg(hue_color(self.value.h))
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(solid(255, 255, 255, 1.), 0.),
                linear_color_stop(solid(255, 255, 255, 0.), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(solid(0, 0, 0, 0.), 0.),
                linear_color_stop(solid(0, 0, 0, 1.), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .left(relative(s))
                    .top(relative(1. - v))
                    .ml(px(6. - 12. * s))
                    .mt(px(6. - 12. * (1. - v)))
                    .size(px(12.))
                    .rounded_full()
                    .border_2()
                    .border_color(solid(255, 255, 255, 1.))
                    .shadow(tokens::shadow_sm(0.4)),
            )
            .child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .rounded(RADIUS_LG)
                    .bg(theme.popover)
                    .px(px(6.))
                    .py(px(2.))
                    .text_size(TEXT_XS.size)
                    .text_color(theme.popover_foreground)
                    .child(SharedString::from(format!(
                        "{} {}%",
                        self.label,
                        (self.value.s * 100.).round() as i32
                    ))),
            )
    }
}

#[derive(IntoElement)]
pub struct ColorHueSlider {
    value: f64,
}

impl ColorHueSlider {
    pub fn new(value: f64) -> Self {
        Self { value }
    }
}

impl RenderOnce for ColorHueSlider {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let amount = (self.value / 360.) as f32;
        let stops = [0., 60., 120., 180., 240., 300., 360.];
        let mut track = div()
            .flex()
            .flex_1()
            .h(px(10.))
            .rounded_full()
            .overflow_hidden();
        for pair in stops.windows(2) {
            track = track.child(div().flex_1().h_full().bg(linear_gradient(
                90.,
                linear_color_stop(hue_color(pair[0]), 0.),
                linear_color_stop(hue_color(pair[1]), 1.),
            )));
        }
        div()
            .relative()
            .flex()
            .h(px(24.))
            .items_center()
            .child(track.border_1().border_color(solid(0, 0, 0, 0.12)))
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .left(relative(amount))
                    .ml(px(8. - 16. * amount))
                    .mt(px(-8.))
                    .size(px(16.))
                    .rounded_full()
                    .border_2()
                    .border_color(solid(255, 255, 255, 1.))
                    .bg(hue_color(self.value)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_maps_the_plane_and_rejects_an_empty_box() {
        assert_eq!(saturation_value_at(0., 0., 100., 50.), Some((0., 1.)));
        assert_eq!(saturation_value_at(100., 50., 100., 50.), Some((1., 0.)));
        assert_eq!(saturation_value_at(25., 10., 100., 40.), Some((0.25, 0.75)));
        assert_eq!(saturation_value_at(-10., 80., 100., 40.), Some((0., 0.)));
        assert_eq!(saturation_value_at(1., 1., 0., 10.), None);
    }

    #[test]
    fn plane_keys_step_and_jump_to_the_ends() {
        assert_eq!(
            apply_plane_key(0.5, ColorKey::ArrowRight, false),
            Some(0.52)
        );
        assert_eq!(apply_plane_key(0.5, ColorKey::ArrowUp, true), Some(0.6));
        let stepped = apply_plane_key(0.05, ColorKey::ArrowLeft, false).unwrap();
        assert!(
            (stepped - 0.03).abs() < 1e-12,
            "left step from 0.05 landed on {stepped}"
        );
        assert_eq!(apply_plane_key(0.01, ColorKey::ArrowDown, false), Some(0.));
        assert_eq!(apply_plane_key(0.4, ColorKey::Home, false), Some(0.));
        assert_eq!(apply_plane_key(0.4, ColorKey::End, true), Some(1.));
        assert_eq!(apply_plane_key(0.4, ColorKey::Other, false), None);
    }

    #[test]
    fn hue_wraps_and_ignores_home() {
        assert_eq!(hue_at(0., 200.), Some(0.));
        assert_eq!(hue_at(100., 200.), Some(180.));
        assert_eq!(hue_at(1., 0.), None);
        assert_eq!(apply_hue_key(10., ColorKey::ArrowRight, false), Some(11.));
        assert_eq!(apply_hue_key(10., ColorKey::ArrowUp, true), Some(20.));
        assert_eq!(apply_hue_key(5., ColorKey::ArrowLeft, true), Some(355.));
        assert_eq!(apply_hue_key(359., ColorKey::ArrowRight, false), Some(0.));
        assert_eq!(apply_hue_key(10., ColorKey::Home, false), None);
    }

    #[test]
    fn thumbs_stay_inside_the_track() {
        assert_eq!(plane_thumb_origin(0., 100.), 6.);
        assert_eq!(plane_thumb_origin(1., 100.), 94.);
        assert_eq!(hue_thumb_origin(0., 200.), 8.);
        assert_eq!(hue_thumb_origin(360., 200.), 192.);
    }
}
