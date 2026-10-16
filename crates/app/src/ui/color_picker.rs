//! Port of upstream/apps/web/src/components/ui/color-picker.tsx
//!
//! The saturation plane is `h-32` (inset, `rounded-lg`) or `h-36` (edge).
//! Pointer position maps through [`saturation_value_at`]; arrow keys step
//! 0.02, or 0.1 with Shift. The hue slider is `h-6` with an `h-2.5` track.
//! Hue arrows step 1 degree, or 10 with Shift, and wrap with `(value + 360) % 360`.
//!
//! The plane takes focus on press, then tracks the pointer at window level
//! until release (`useColorDrag`). The plane has two tab stops, saturation
//! then brightness, and shows the focused axis readout. Both controls draw the
//! inset focus ring while focused.
//!
//! Not ported: the 80ms thumb transition. GPUI gradients take two stops, so
//! the hue track is six two-stop segments instead of one rainbow.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Bounds, Context, CursorStyle, DispatchPhase, ElementId, Entity, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, RenderOnce, Rgba, SharedString, Styled, Window,
    canvas, div, linear_color_stop, linear_gradient, px, relative,
};

use crate::theme::tokens::{self, RADIUS_DEFAULT, RADIUS_LG, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};

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

type Change<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;
type InteractionEnd = Rc<dyn Fn(&mut Window, &mut App)>;
type PointerUpdate = Rc<dyn Fn(Point<Pixels>, Bounds<Pixels>, &mut Window, &mut App)>;

/// `useColorDrag`: the pointer is captured while the primary button is down,
/// so moves and the release are read at window level until it ends.
struct ColorControlState {
    first: FocusHandle,
    second: FocusHandle,
    dragging: bool,
    bounds: Bounds<Pixels>,
}

impl ColorControlState {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            first: cx.focus_handle().tab_stop(true),
            second: cx.focus_handle().tab_stop(true),
            dragging: false,
            bounds: Bounds::default(),
        }
    }
}

/// Zero-size canvas that records the control's bounds and, while dragging,
/// listens for the pointer outside of it.
fn drag_tracker(
    state: &Entity<ColorControlState>,
    update: PointerUpdate,
    on_end: Option<InteractionEnd>,
) -> impl IntoElement {
    let measure = state.clone();
    let state = state.clone();
    canvas(
        move |bounds, _, cx| {
            measure.update(cx, |state, _| state.bounds = bounds);
        },
        move |bounds, _, window, cx| {
            if !state.read(cx).dragging {
                return;
            }
            let on_move_state = state.clone();
            let on_move = update.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || event.pressed_button != Some(MouseButton::Left)
                    || !on_move_state.read(cx).dragging
                {
                    return;
                }
                on_move(event.position, bounds, window, cx);
            });
            let on_up_state = state.clone();
            let on_end = on_end.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                    return;
                }
                let was_dragging = on_up_state.update(cx, |state, cx| {
                    let was = state.dragging;
                    state.dragging = false;
                    cx.notify();
                    was
                });
                if was_dragging && let Some(on_end) = &on_end {
                    on_end(window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// `focus-visible:ring-2 ring-inset ring-ring ring-offset-2 ring-offset-popover`:
/// a 2px popover band, then the 2px ring inside it.
fn inset_focus_ring(theme: &AppTheme, rounded: bool) -> impl IntoElement {
    let band = div()
        .absolute()
        .inset_0()
        .border_2()
        .border_color(theme.popover);
    let ring = div()
        .absolute()
        .top(px(2.))
        .left(px(2.))
        .right(px(2.))
        .bottom(px(2.))
        .border_2()
        .border_color(theme.ring);
    if rounded {
        band.rounded_full().child(ring.rounded_full())
    } else {
        band.child(ring)
    }
}

#[derive(IntoElement)]
pub struct ColorSaturationValuePlane {
    id: ElementId,
    label: SharedString,
    value: HsvColor,
    variant: PlaneVariant,
    on_change: Option<Change<HsvColor>>,
    on_interaction_end: Option<InteractionEnd>,
}

impl ColorSaturationValuePlane {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, value: HsvColor) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            variant: PlaneVariant::Inset,
            on_change: None,
            on_interaction_end: None,
        }
    }

    pub fn variant(mut self, variant: PlaneVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn on_change(
        mut self,
        handler: impl Fn(HsvColor, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    /// Flush pending consumer updates when pointer interaction finishes.
    pub fn on_interaction_end(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_interaction_end = Some(Rc::new(handler));
        self
    }
}

fn color_key(key: &str) -> ColorKey {
    match key {
        "right" => ColorKey::ArrowRight,
        "up" => ColorKey::ArrowUp,
        "left" => ColorKey::ArrowLeft,
        "down" => ColorKey::ArrowDown,
        "home" => ColorKey::Home,
        "end" => ColorKey::End,
        _ => ColorKey::Other,
    }
}

impl RenderOnce for ColorSaturationValuePlane {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| ColorControlState::new(cx));
        let (saturation_focus, brightness_focus) = {
            let state = state.read(cx);
            (state.first.clone(), state.second.clone())
        };
        let saturation_focused = saturation_focus.is_focused(window);
        let brightness_focused = brightness_focus.is_focused(window);
        let focused = saturation_focused || brightness_focused;
        let height = if self.variant == PlaneVariant::Edge {
            px(144.)
        } else {
            px(128.)
        };
        let value = self.value;
        let s = value.s as f32;
        let v = value.v as f32;

        let on_change = self.on_change.clone();
        let pointer = {
            let on_change = on_change.clone();
            Rc::new(
                move |position: Point<Pixels>,
                      bounds: Bounds<Pixels>,
                      window: &mut Window,
                      cx: &mut App| {
                    let Some(on_change) = &on_change else { return };
                    let local = position - bounds.origin;
                    if let Some((s, v)) = saturation_value_at(
                        f64::from(local.x),
                        f64::from(local.y),
                        f64::from(bounds.size.width),
                        f64::from(bounds.size.height),
                    ) {
                        on_change(HsvColor { s, v, ..value }, window, cx);
                    }
                },
            )
        };
        let down_state = state.clone();
        let down_pointer = pointer.clone();
        let down_focus = saturation_focus.clone();
        let key_change = on_change.clone();
        let key_saturation = saturation_focus.clone();
        let readout = if brightness_focused {
            Some(("Brightness", value.v))
        } else if saturation_focused {
            Some(("Saturation", value.s))
        } else {
            None
        };
        let mut root = div()
            .id(self.id.clone())
            .relative()
            .overflow_hidden()
            .h(height)
            .cursor(CursorStyle::Crosshair)
            .when(self.variant == PlaneVariant::Inset, |this| {
                this.rounded(RADIUS_LG)
            })
            .bg(hue_color(value.h))
            .track_focus(&saturation_focus)
            .on_mouse_down(
                MouseButton::Left,
                move |event: &MouseDownEvent, window, cx| {
                    if down_state.read(cx).dragging {
                        return;
                    }
                    window.focus(&down_focus, cx);
                    let bounds = down_state.update(cx, |state, cx| {
                        state.dragging = true;
                        cx.notify();
                        state.bounds
                    });
                    down_pointer(event.position, bounds, window, cx);
                },
            )
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let Some(on_change) = &key_change else { return };
                let key = color_key(&event.keystroke.key);
                let shift = event.keystroke.modifiers.shift;
                let on_saturation = key_saturation.is_focused(window);
                let next = if on_saturation {
                    apply_plane_key(value.s, key, shift).map(|s| HsvColor { s, ..value })
                } else {
                    apply_plane_key(value.v, key, shift).map(|v| HsvColor { v, ..value })
                };
                if let Some(next) = next {
                    cx.stop_propagation();
                    on_change(next, window, cx);
                }
            })
            .child(drag_tracker(&state, pointer, self.on_interaction_end))
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
                    .id("brightness")
                    .absolute()
                    .size_0()
                    .track_focus(&brightness_focus),
            )
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
            );
        // Hidden until a range input has keyboard focus (`peer-focus-visible:visible`).
        if let Some((axis_label, amount)) = readout {
            root = root.child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .rounded(RADIUS_DEFAULT)
                    .bg(theme.popover)
                    .px(px(6.))
                    .py(px(2.))
                    .text_size(TEXT_XS.size)
                    .text_color(theme.popover_foreground)
                    .child(SharedString::from(format!(
                        "{axis_label} {}%",
                        (amount * 100.).round() as i32
                    ))),
            );
        }
        if focused {
            root = root.child(inset_focus_ring(&theme, false));
        }
        let _ = &self.label;
        root
    }
}

#[derive(IntoElement)]
pub struct ColorHueSlider {
    id: ElementId,
    label: SharedString,
    value: f64,
    on_change: Option<Change<f64>>,
    on_interaction_end: Option<InteractionEnd>,
}

impl ColorHueSlider {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            on_change: None,
            on_interaction_end: None,
        }
    }

    pub fn on_change(mut self, handler: impl Fn(f64, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    pub fn on_interaction_end(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_interaction_end = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ColorHueSlider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| ColorControlState::new(cx));
        let focus = state.read(cx).first.clone();
        let focused = focus.is_focused(window);
        let value = self.value;
        let amount = (value / 360.) as f32;
        let stops = [0., 60., 120., 180., 240., 300., 360.];
        let mut track = div().relative().flex().flex_1().h(px(10.));
        let last = stops.len() - 2;
        for (index, pair) in stops.windows(2).enumerate() {
            // GPUI does not clip children to rounded corners, so the two
            // end segments carry the pill's radius themselves.
            track = track.child(
                div()
                    .flex_1()
                    .h_full()
                    .when(index == 0, |this| this.rounded_l_full())
                    .when(index == last, |this| this.rounded_r_full())
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(hue_color(pair[0]), 0.),
                        linear_color_stop(hue_color(pair[1]), 1.),
                    )),
            );
        }
        // `shadow-[inset_0_0_0_1px_rgb(0_0_0_/_12%)]`
        track = track.child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .border_1()
                .border_color(solid(0, 0, 0, 0.12)),
        );
        let on_change = self.on_change.clone();
        let pointer = {
            let on_change = on_change.clone();
            Rc::new(
                move |position: Point<Pixels>,
                      bounds: Bounds<Pixels>,
                      window: &mut Window,
                      cx: &mut App| {
                    let Some(on_change) = &on_change else { return };
                    let local = position - bounds.origin;
                    if let Some(hue) = hue_at(f64::from(local.x), f64::from(bounds.size.width)) {
                        on_change(hue, window, cx);
                    }
                },
            )
        };
        let down_state = state.clone();
        let down_pointer = pointer.clone();
        let down_focus = focus.clone();
        let mut root = div()
            .id(self.id.clone())
            .relative()
            .flex()
            .h(px(24.))
            .items_center()
            .rounded_full()
            .cursor(CursorStyle::PointingHand)
            .track_focus(&focus)
            .on_mouse_down(
                MouseButton::Left,
                move |event: &MouseDownEvent, window, cx| {
                    if down_state.read(cx).dragging {
                        return;
                    }
                    window.focus(&down_focus, cx);
                    let bounds = down_state.update(cx, |state, cx| {
                        state.dragging = true;
                        cx.notify();
                        state.bounds
                    });
                    down_pointer(event.position, bounds, window, cx);
                },
            )
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let Some(on_change) = &on_change else { return };
                let key = color_key(&event.keystroke.key);
                if let Some(next) = apply_hue_key(value, key, event.keystroke.modifiers.shift) {
                    cx.stop_propagation();
                    on_change(next, window, cx);
                }
            })
            .child(drag_tracker(&state, pointer, self.on_interaction_end))
            .child(track)
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
                    .shadow(tokens::shadow_sm(0.4))
                    .bg(hue_color(value)),
            );
        if focused {
            root = root.child(inset_focus_ring(&theme, true));
        }
        let _ = &self.label;
        root
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

    #[test]
    fn gpui_key_names_map_to_color_keys() {
        assert_eq!(color_key("right"), ColorKey::ArrowRight);
        assert_eq!(color_key("up"), ColorKey::ArrowUp);
        assert_eq!(color_key("left"), ColorKey::ArrowLeft);
        assert_eq!(color_key("down"), ColorKey::ArrowDown);
        assert_eq!(color_key("home"), ColorKey::Home);
        assert_eq!(color_key("end"), ColorKey::End);
        assert_eq!(color_key("tab"), ColorKey::Other);
    }
}
