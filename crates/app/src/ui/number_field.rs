//! Port of upstream/apps/web/src/components/ui/number-field.tsx and the step
//! math it inherits from `@base-ui/react/number-field` (`toValidatedNumber`,
//! `snapToStep`, `incrementValue`).
//!
//! Desktop heights: `sm` 26px (`h-6.5`), default 30px (`h-7.5`), `lg` 34px
//! (`h-8.5`). The group is `rounded-lg border border-input bg-background
//! shadow-xs/5`, dark `bg-input/32`. Steppers hover `bg-accent`.
//!
//! Not ported: locale numeral parsing, the scrub area, wheel scrub, and the
//! focus ring. gpui-component's `NumberInput` draws its own chrome.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, CursorStyle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use super::icon::{Icon, IconName};
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM};

const STEP_EPSILON_FACTOR: f64 = 1e-10;
const MIN_SAFE: f64 = -9_007_199_254_740_991.;
const MAX_SAFE: f64 = 9_007_199_254_740_991.;
/// `Intl.NumberFormat('en-US')` default `maximumFractionDigits`.
const DEFAULT_MAX_FRACTION_DIGITS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapMode {
    Directional,
    Nearest,
}

#[derive(Clone, Copy, Debug)]
pub struct ValidateOptions {
    pub step: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub snap_on_step: bool,
    pub small: bool,
    pub clamp: bool,
    pub maximum_fraction_digits: u32,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self {
            step: Some(1.),
            min: None,
            max: None,
            snap_on_step: false,
            small: false,
            clamp: true,
            maximum_fraction_digits: DEFAULT_MAX_FRACTION_DIGITS,
        }
    }
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

pub fn round_to_fraction_digits(value: f64, maximum_fraction_digits: u32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let digits = maximum_fraction_digits.min(20);
    let factor = 10f64.powi(digits as i32);
    let scaled = value * factor;
    let rounded = if scaled >= 0. {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    rounded / factor
}

pub fn remove_floating_point_errors(value: f64, maximum_fraction_digits: u32) -> f64 {
    round_to_fraction_digits(value, maximum_fraction_digits)
}

pub fn snap_to_step(clamped_value: f64, base: f64, step: f64, mode: SnapMode) -> f64 {
    if step == 0. {
        return clamped_value;
    }
    let step_size = step.abs();
    let direction = step.signum();
    let tolerance = step_size * STEP_EPSILON_FACTOR * direction;
    let divisor = if mode == SnapMode::Nearest {
        step
    } else {
        step_size
    };
    let raw_steps = (clamped_value - base + tolerance) / divisor;
    let snapped_steps = match mode {
        SnapMode::Nearest => raw_steps.round(),
        SnapMode::Directional if direction > 0. => raw_steps.floor(),
        SnapMode::Directional => raw_steps.ceil(),
    };
    let step_for_result = if mode == SnapMode::Nearest {
        step
    } else {
        step_size
    };
    base + snapped_steps * step_for_result
}

pub fn to_validated_number(value: Option<f64>, options: &ValidateOptions) -> Option<f64> {
    let value = value?;
    let min_with_default = options.min.unwrap_or(MIN_SAFE);
    let max_with_default = options.max.unwrap_or(MAX_SAFE);
    let clamped = if options.clamp {
        clamp(value, min_with_default, max_with_default)
    } else {
        value
    };
    let Some(step) = options.step.filter(|_| options.snap_on_step) else {
        return Some(remove_floating_point_errors(
            clamped,
            options.maximum_fraction_digits,
        ));
    };
    if step == 0. {
        return Some(remove_floating_point_errors(
            clamped,
            options.maximum_fraction_digits,
        ));
    }
    let min_with_zero = options.min.unwrap_or(0.);
    let base = if !options.small && min_with_default != MIN_SAFE {
        min_with_default
    } else {
        min_with_zero
    };
    let mode = if options.small {
        SnapMode::Nearest
    } else {
        SnapMode::Directional
    };
    Some(remove_floating_point_errors(
        snap_to_step(clamped, base, step, mode),
        options.maximum_fraction_digits,
    ))
}

/// `incrementValue`: a null current value starts at `max(0, min ?? 0)`.
pub fn increment_number(
    current: Option<f64>,
    amount: f64,
    direction: f64,
    options: &ValidateOptions,
) -> Option<f64> {
    let next = match current {
        Some(value) => value + amount * direction,
        None => options.min.unwrap_or(0.).max(0.),
    };
    to_validated_number(Some(next), options)
}

pub fn format_number(value: f64) -> String {
    let rounded = round_to_fraction_digits(value, DEFAULT_MAX_FRACTION_DIGITS);
    let text = format!("{rounded:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" {
        "0".into()
    } else {
        trimmed.to_owned()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NumberFieldSize {
    Sm,
    #[default]
    Default,
    Lg,
}

fn field_height(size: NumberFieldSize) -> f32 {
    match size {
        NumberFieldSize::Sm => 1.625,
        NumberFieldSize::Default => 1.875,
        NumberFieldSize::Lg => 2.125,
    }
}

type ChangeHandler = Rc<dyn Fn(Option<f64>, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct NumberField {
    id: SharedString,
    value: Option<f64>,
    options: ValidateOptions,
    size: NumberFieldSize,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl NumberField {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            value: None,
            options: ValidateOptions::default(),
            size: NumberFieldSize::Default,
            disabled: false,
            on_change: None,
        }
    }

    pub fn value(mut self, value: Option<f64>) -> Self {
        self.value = value;
        self
    }

    pub fn options(mut self, options: ValidateOptions) -> Self {
        self.options = options;
        self
    }

    pub fn size(mut self, size: NumberFieldSize) -> Self {
        self.size = size;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(
        mut self,
        handler: impl Fn(Option<f64>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NumberField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let pad = if self.size == NumberFieldSize::Sm {
            px(9.)
        } else {
            px(11.)
        };
        let step = self.options.step.unwrap_or(1.);
        let options = self.options;
        let value = self.value;
        let disabled = self.disabled;
        let label: SharedString = value.map(format_number).unwrap_or_default().into();
        let decrement_id = SharedString::from(format!("{}-decrement", self.id));
        let increment_id = SharedString::from(format!("{}-increment", self.id));
        let step_button =
            |id: SharedString, icon: IconName, direction: f64, on_change: Option<ChangeHandler>| {
                div()
                    .id(id)
                    .on_mouse_down(MouseButton::Left, no_drag)
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .px(pad)
                    .h_full()
                    .when(!disabled, |this| {
                        this.cursor(CursorStyle::PointingHand)
                            .hover(|style| style.bg(theme.accent))
                            .when_some(on_change, |this, on_change| {
                                this.on_click(move |_: &ClickEvent, window, cx| {
                                    let next = increment_number(value, step, direction, &options);
                                    on_change(next, window, cx);
                                })
                            })
                    })
                    .child(Icon::new(icon).size(rems(1.)))
            };
        div()
            .id(self.id)
            .flex()
            .w_full()
            .items_center()
            .justify_between()
            .h(rems(field_height(self.size)))
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.input)
            .bg(if theme.is_dark() {
                with_alpha(theme.input, 0.32).into()
            } else {
                theme.background
            })
            .text_color(theme.foreground)
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .shadow(tokens::shadow_xs(0.05))
            .when(disabled, |this| this.opacity(0.64))
            .child(step_button(
                decrement_id,
                IconName::Minus,
                -1.,
                self.on_change.clone(),
            ))
            .child(div().flex_1().text_center().child(label))
            .child(step_button(
                increment_id,
                IconName::Plus,
                1.,
                self.on_change,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_down_on_a_positive_step() {
        assert_eq!(snap_to_step(1.2, 0., 1., SnapMode::Directional), 1.);
        assert_eq!(snap_to_step(2., 0., 1., SnapMode::Directional), 2.);
    }

    #[test]
    fn clamps_then_rounds_floating_point_noise() {
        let options = ValidateOptions {
            min: Some(0.),
            max: Some(5.),
            ..ValidateOptions::default()
        };
        assert_eq!(to_validated_number(Some(9.), &options), Some(5.));
        assert_eq!(to_validated_number(Some(0.1 + 0.2), &options), Some(0.3));
    }

    #[test]
    fn increment_from_empty_starts_at_zero_or_min() {
        let options = ValidateOptions::default();
        assert_eq!(increment_number(None, 1., 1., &options), Some(0.));
        assert_eq!(increment_number(Some(1.), 1., 1., &options), Some(2.));
        assert_eq!(increment_number(Some(1.), 1., -1., &options), Some(0.));
        let raised = ValidateOptions {
            min: Some(4.),
            ..ValidateOptions::default()
        };
        assert_eq!(increment_number(None, 1., 1., &raised), Some(4.));
    }

    #[test]
    fn formats_without_trailing_zeros() {
        assert_eq!(format_number(2.), "2");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(1.2346), "1.235");
    }
}
