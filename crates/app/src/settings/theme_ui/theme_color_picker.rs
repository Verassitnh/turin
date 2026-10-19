//! Port of upstream/apps/web/src/components/settings/ThemeColorPicker.tsx
//!
//! A theme colour field: the role's label (click to spotlight where it is used), a
//! swatch that opens the colour popover, and a hex input. The popover is
//! `ui::color_picker`'s saturation plane and hue slider plus HEX and RGB inputs.
//!
//! The picker stays an sRGB/hex adapter over the OKLCH palette engine. Alpha is kept
//! apart and re-attached on commit, so adjusting hue or brightness cannot change
//! transparency. [`ThemePickerModel`] holds that logic without a window; the views
//! below only draw it.

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::component::Selectable;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, AppContext as _, Context, CursorStyle, Entity, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rems,
};

use crate::settings::controls::{FieldSize, text_field};
use crate::theme::ActiveAppTheme;
use crate::theme::color::{color, with_alpha};
use crate::theme::theme_palette_library::{is_theme_color, theme_color_to_hex};
use crate::theme::tokens::{self, TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::ui::Tooltip;
use crate::ui::color::{hex_to_hsv, hsv_to_hex};
use crate::ui::color_picker::{ColorHueSlider, ColorSaturationValuePlane, HsvColor, PlaneVariant};
use crate::ui::popover::{PopoverOptions, PopoverPadding, popover};

// ── Pure rules ───────────────────────────────────────────────────────

/// `getThemeRoleLabel`. Roles are the snake_case field names of `ThemeColors`; an
/// unlisted one reads as its words with the first capitalised.
pub fn get_theme_role_label(role: &str) -> String {
    let label = match role {
        "canvas" => "Background",
        "toolbar" => "Toolbar background",
        "toolbar_foreground" => "Toolbar text",
        "toolbar_border" => "Toolbar border",
        "toolbar_control" => "Toolbar control",
        "toolbar_control_foreground" => "Toolbar control text",
        "toolbar_control_hover" => "Toolbar control hover",
        "accent" => "Accent color",
        "error_foreground" => "Error text",
        "error_surface" => "Error background",
        "warning_foreground" => "Warning text",
        "warning_surface" => "Warning background",
        "update_foreground" => "Update text",
        "update_surface" => "Update background",
        _ => {
            let spaced = role.replace('_', " ");
            let mut chars = spaced.chars();
            return match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            };
        }
    };
    label.to_owned()
}

/// `themePickerAlphaSuffix`: the two alpha digits of an 8-digit hex, empty when opaque.
pub fn theme_picker_alpha_suffix(value: &str) -> String {
    let normalized = theme_color_to_hex(value).unwrap_or_default();
    let alpha = if normalized.len() == 9 {
        &normalized[7..]
    } else {
        ""
    };
    if alpha == "ff" {
        String::new()
    } else {
        alpha.to_owned()
    }
}

/// `normalizeThemePickerColor`: the opaque `#rrggbb`, black when the value is no colour.
pub fn normalize_theme_picker_color(value: &str) -> String {
    theme_color_to_hex(value)
        .unwrap_or_else(|| "#000000".to_owned())
        .chars()
        .take(7)
        .collect()
}

/// `themeHexToRgb`.
pub fn theme_hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let normalized = normalize_theme_picker_color(hex);
    let numeric = u32::from_str_radix(&normalized[1..], 16).unwrap_or(0);
    (
        ((numeric >> 16) & 255) as u8,
        ((numeric >> 8) & 255) as u8,
        (numeric & 255) as u8,
    )
}

/// `themeRgbToHex`: `rgb(r, g, b)`, `r, g, b` or `r g b` with integer channels 0-255.
pub fn theme_rgb_to_hex(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let without_prefix = match trimmed.get(..4) {
        Some(prefix) if prefix.eq_ignore_ascii_case("rgb(") => trimmed[4..].trim_start(),
        _ => trimmed,
    };
    let normalized = without_prefix
        .strip_suffix(')')
        .map_or(without_prefix, str::trim_end);
    let channels: Vec<&str> = normalized
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|channel| !channel.is_empty())
        .collect();
    if channels.len() != 3 {
        return None;
    }
    let mut hex = String::from("#");
    for channel in channels {
        // JS `Number(channel)` then `Number.isInteger`: plain unsigned integers only here;
        // a signed, fractional or hex spelling is rejected as out of range or non-integer.
        let digits_only = !channel.is_empty() && channel.chars().all(|ch| ch.is_ascii_digit());
        if !digits_only {
            return None;
        }
        let number: u32 = channel.parse().ok()?;
        if number > 255 {
            return None;
        }
        hex.push_str(&format!("{number:02x}"));
    }
    Some(hex)
}

/// `themeRgbValue`: the `r, g, b` text of a hex.
pub fn theme_rgb_value(hex: &str) -> String {
    let (red, green, blue) = theme_hex_to_rgb(hex);
    format!("{red}, {green}, {blue}")
}

fn is_hex_six(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn to_picker_hsv(hsv: crate::ui::color::HsvColor) -> HsvColor {
    HsvColor {
        h: hsv.h,
        s: hsv.s,
        v: hsv.v,
    }
}

fn hex_of(hsv: HsvColor) -> String {
    hsv_to_hex(hsv.h, hsv.s, hsv.v)
}

/// The state of `ThemeColorPickerPanel`: the HSV the controls show, the text drafts, and
/// the commit batched to the next frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemePickerModel {
    pub hsv: HsvColor,
    pub hex_draft: String,
    pub rgb_draft: String,
    /// A text field is focused, so an incoming value is not written over what is typed.
    pub editing_text: bool,
    pub alpha_suffix: String,
    /// The colour waiting for the next frame (`pendingCommitRef`).
    pub pending_commit: Option<String>,
}

impl ThemePickerModel {
    pub fn new(value: &str) -> Self {
        let normalized = normalize_theme_picker_color(value);
        Self {
            hsv: to_picker_hsv(hex_to_hsv(&normalized)),
            hex_draft: normalized.clone(),
            rgb_draft: theme_rgb_value(&normalized),
            editing_text: false,
            alpha_suffix: theme_picker_alpha_suffix(value),
            pending_commit: None,
        }
    }

    /// The colour the controls currently show.
    pub fn current_color(&self) -> String {
        hex_of(self.hsv)
    }

    /// The value changed from outside. While a text field is focused the incoming value may
    /// be the guided editor's readability-adjusted echo of what is being typed, so the draft
    /// is left alone. The hue and saturation are kept when the value is only our own change
    /// echoed back: hex to HSV is lossy for greys, white and black.
    pub fn sync_value(&mut self, value: &str) {
        let normalized = normalize_theme_picker_color(value);
        self.alpha_suffix = theme_picker_alpha_suffix(value);
        if !self.editing_text {
            self.hex_draft = normalized.clone();
            self.rgb_draft = theme_rgb_value(&normalized);
        }
        if self.current_color() != normalized {
            self.hsv = to_picker_hsv(hex_to_hsv(&normalized));
        }
    }

    /// `commitHsv`: the controls move at once; the parent hears about it on the next frame.
    pub fn commit_hsv(&mut self, next: HsvColor) {
        self.hsv = next;
        let next_color = hex_of(next);
        self.rgb_draft = theme_rgb_value(&next_color);
        self.hex_draft = next_color.clone();
        self.pending_commit = Some(format!("{next_color}{}", self.alpha_suffix));
    }

    /// `flushPendingCommit`: the colour to report now, if one is waiting.
    pub fn take_pending(&mut self) -> Option<String> {
        self.pending_commit.take()
    }

    /// `handleHexChange`: the colour to report at once when the text is a full `#rrggbb`.
    pub fn hex_change(&mut self, text: &str) -> Option<String> {
        self.hex_draft = text.to_owned();
        if !is_hex_six(text) {
            return None;
        }
        self.hsv = to_picker_hsv(hex_to_hsv(text));
        self.rgb_draft = theme_rgb_value(text);
        Some(text.to_lowercase())
    }

    /// `handleRgbChange`: RGB cannot express alpha, so a commit keeps the incoming suffix
    /// just like the plane and hue controls do.
    pub fn rgb_change(&mut self, text: &str) -> Option<String> {
        self.rgb_draft = text.to_owned();
        let next_color = theme_rgb_to_hex(text)?;
        self.hsv = to_picker_hsv(hex_to_hsv(&next_color));
        self.hex_draft = next_color.clone();
        Some(format!("{next_color}{}", self.alpha_suffix))
    }

    /// A text field lost focus: both drafts return to the current colour.
    pub fn end_text_edit(&mut self) {
        self.editing_text = false;
        let current = self.current_color();
        self.rgb_draft = theme_rgb_value(&current);
        self.hex_draft = current;
    }
}

// ── Views ────────────────────────────────────────────────────────────

type ValueChange = Rc<dyn Fn(&str, &mut App)>;

/// `ThemeColorPickerPanel`: the popover's content.
pub struct ThemeColorPickerPanel {
    label: SharedString,
    model: ThemePickerModel,
    on_change: ValueChange,
    hex_input: Entity<InputState>,
    rgb_input: Entity<InputState>,
    frame_scheduled: bool,
    _subscriptions: Vec<Subscription>,
}

impl ThemeColorPickerPanel {
    pub fn new(
        label: impl Into<SharedString>,
        value: &str,
        on_change: ValueChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = ThemePickerModel::new(value);
        let hex_input =
            cx.new(|cx| InputState::new(window, cx).default_value(model.hex_draft.clone()));
        let rgb_input =
            cx.new(|cx| InputState::new(window, cx).default_value(model.rgb_draft.clone()));
        let subscriptions = vec![
            cx.subscribe_in(
                &hex_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.model.editing_text = true;
                        let text = input.read(cx).value().to_string();
                        if let Some(color) = this.model.hex_change(&text) {
                            (this.on_change)(&color, cx);
                        }
                        this.sync_inputs(window, cx);
                    }
                    InputEvent::Focus => this.model.editing_text = true,
                    InputEvent::Blur => {
                        this.model.end_text_edit();
                        this.sync_inputs(window, cx);
                    }
                    InputEvent::PressEnter { .. } => {}
                },
            ),
            cx.subscribe_in(
                &rgb_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.model.editing_text = true;
                        let text = input.read(cx).value().to_string();
                        if let Some(color) = this.model.rgb_change(&text) {
                            (this.on_change)(&color, cx);
                        }
                        this.sync_inputs(window, cx);
                    }
                    InputEvent::Focus => this.model.editing_text = true,
                    InputEvent::Blur => {
                        this.model.end_text_edit();
                        this.sync_inputs(window, cx);
                    }
                    InputEvent::PressEnter { .. } => {}
                },
            ),
        ];
        Self {
            label: label.into(),
            model,
            on_change,
            hex_input,
            rgb_input,
            frame_scheduled: false,
            _subscriptions: subscriptions,
        }
    }

    /// The field's value changed (`useEffect([normalizedValue])`).
    pub fn set_value(&mut self, label: &str, value: &str) {
        self.label = label.to_owned().into();
        self.model.sync_value(value);
    }

    /// Writes the drafts into the inputs, except the one being typed in.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hex_focused = self.hex_input.read(cx).focus_handle(cx).is_focused(window);
        let rgb_focused = self.rgb_input.read(cx).focus_handle(cx).is_focused(window);
        if !hex_focused && self.hex_input.read(cx).value().as_ref() != self.model.hex_draft {
            let draft = self.model.hex_draft.clone();
            self.hex_input
                .update(cx, |input, cx| input.set_value(draft, window, cx));
        }
        if !rgb_focused && self.rgb_input.read(cx).value().as_ref() != self.model.rgb_draft {
            let draft = self.model.rgb_draft.clone();
            self.rgb_input
                .update(cx, |input, cx| input.set_value(draft, window, cx));
        }
    }

    /// `flushPendingCommit`.
    fn flush(&mut self, cx: &mut Context<Self>) {
        if let Some(color) = self.model.take_pending() {
            (self.on_change)(&color, cx);
        }
    }

    /// `scheduleCommit`: one report per frame however fast the pointer moves.
    fn commit_hsv(&mut self, next: HsvColor, window: &mut Window, cx: &mut Context<Self>) {
        self.model.commit_hsv(next);
        self.sync_inputs(window, cx);
        if !self.frame_scheduled {
            self.frame_scheduled = true;
            let panel = cx.entity();
            window.on_next_frame(move |_, cx| {
                panel.update(cx, |this, cx| {
                    this.frame_scheduled = false;
                    this.flush(cx);
                });
            });
        }
        cx.notify();
    }
}

impl Render for ThemeColorPickerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let current = self.model.current_color();
        let swatch = color(&current);
        let panel = cx.entity();
        let for_plane = panel.clone();
        let for_hue = panel.clone();
        let for_end_plane = panel.clone();
        let for_end_hue = panel.clone();
        let label = self.label.clone();
        let caption = |text: &'static str| {
            div()
                .px(px(4.))
                .text_size(TEXT_3XS.size)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        div()
            .w(rems(18.))
            .bg(theme.popover)
            .child(
                // `flex items-center justify-between border-b border-border/70 px-4 py-3`
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(with_alpha(theme.border, 0.7))
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(label.clone()),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_2XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child("Choose a color"),
                            ),
                    )
                    .child(
                        div()
                            .size(rems(1.75))
                            .flex_shrink_0()
                            .rounded_full()
                            .shadow(tokens::shadow_sm(0.1))
                            .bg(swatch),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px_3()
                    .pb_3()
                    .pt_3()
                    .child(
                        ColorSaturationValuePlane::new(
                            SharedString::from(format!("theme-plane-{label}")),
                            label.clone(),
                            self.model.hsv,
                        )
                        .variant(PlaneVariant::Inset)
                        .on_change(move |next, window, cx| {
                            for_plane.update(cx, |this, cx| this.commit_hsv(next, window, cx));
                        })
                        .on_interaction_end(move |_, cx| {
                            for_end_plane.update(cx, |this, cx| this.flush(cx));
                        }),
                    )
                    .child(
                        ColorHueSlider::new(
                            SharedString::from(format!("theme-hue-{label}")),
                            format!("{label} hue"),
                            self.model.hsv.h,
                        )
                        .on_change({
                            let hsv = self.model.hsv;
                            move |h, window, cx| {
                                for_hue.update(cx, |this, cx| {
                                    this.commit_hsv(HsvColor { h, ..hsv }, window, cx)
                                });
                            }
                        })
                        .on_interaction_end(move |_, cx| {
                            for_end_hue.update(cx, |this, cx| this.flush(cx));
                        }),
                    )
                    .child(
                        // `grid grid-cols-[1fr_1.2fr] gap-2`
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .flex_1()
                                    .min_w_0()
                                    .child(caption("HEX"))
                                    .child(text_field(
                                        &self.hex_input,
                                        None,
                                        FieldSize::Sm,
                                        true,
                                        cx,
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .flex_grow(1.2)
                                    .min_w_0()
                                    .child(caption("RGB"))
                                    .child(text_field(
                                        &self.rgb_input,
                                        None,
                                        FieldSize::Sm,
                                        true,
                                        cx,
                                    )),
                            ),
                    ),
            )
    }
}

/// `ThemeColorField`: one row of the editor. The label toggles the usage spotlight, the
/// swatch opens the picker, the input takes any colour the palette engine reads.
pub struct ThemeColorField {
    role: &'static str,
    label: SharedString,
    value: String,
    selected: bool,
    input: Entity<InputState>,
    picker: Entity<ThemeColorPickerPanel>,
    on_change: Rc<dyn Fn(&str, &str, &mut App)>,
    on_select: Option<Rc<dyn Fn(&str, &mut App)>>,
    on_toggle_selected: Option<Rc<dyn Fn(&str, &mut App)>>,
    _subscriptions: Vec<Subscription>,
}

impl ThemeColorField {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        role: &'static str,
        label: Option<&str>,
        value: &str,
        on_change: Rc<dyn Fn(&str, &str, &mut App)>,
        on_select: Option<Rc<dyn Fn(&str, &mut App)>>,
        on_toggle_selected: Option<Rc<dyn Fn(&str, &mut App)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let label: SharedString = label
            .map_or_else(|| get_theme_role_label(role), str::to_owned)
            .into();
        let editor_value = editor_value(value);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(editor_value));
        let picker_change = {
            let on_change = on_change.clone();
            Rc::new(move |color: &str, cx: &mut App| on_change(role, color, cx)) as ValueChange
        };
        let swatch_value = swatch_value(value);
        let label_for_picker = label.clone();
        let picker = cx.new(|cx| {
            ThemeColorPickerPanel::new(label_for_picker, &swatch_value, picker_change, window, cx)
        });
        let subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    // An echo of our own `set_value` is not an edit.
                    if text != this.value {
                        (this.on_change)(this.role, &text, cx);
                    }
                }
                InputEvent::Focus => {
                    if let Some(select) = &this.on_select {
                        select(this.role, cx);
                    }
                }
                InputEvent::Blur | InputEvent::PressEnter { .. } => {}
            },
        )];
        Self {
            role,
            label,
            value: value.to_owned(),
            selected: false,
            input,
            picker,
            on_change,
            on_select,
            on_toggle_selected,
            _subscriptions: subscriptions,
        }
    }

    /// New props from the editor (the draft changed).
    pub fn update_props(
        &mut self,
        value: &str,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = selected;
        if self.value != value {
            self.value = value.to_owned();
            let label = self.label.to_string();
            let swatch = swatch_value(value);
            self.picker
                .update(cx, |picker, _| picker.set_value(&label, &swatch));
            let text = editor_value(value);
            let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
            if !focused && self.input.read(cx).value().as_ref() != text {
                self.input
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
    }
}

/// `value.trim().toLowerCase().startsWith("oklch(") ? themeColorToHex(value) ?? value : value`.
fn editor_value(value: &str) -> String {
    if value.trim().to_lowercase().starts_with("oklch(") {
        theme_color_to_hex(value).unwrap_or_else(|| value.to_owned())
    } else {
        value.to_owned()
    }
}

/// The swatch shows black for a value that is no colour.
fn swatch_value(value: &str) -> String {
    if is_theme_color(value) {
        value.to_owned()
    } else {
        "#000000".to_owned()
    }
}

impl Render for ThemeColorField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let is_color = is_theme_color(&self.value);
        let swatch = color(&theme_color_to_hex(&swatch_value(&self.value)).unwrap_or_default());
        let role = self.role;
        let label = self.label.clone();
        let selected = self.selected;
        let toggle = self.on_toggle_selected.clone();
        let select_on_press = self.on_select.clone();
        let usage_tooltip = format!(
            "{} where {} is used",
            if selected { "Hide" } else { "Show" },
            label
        );
        let picker = self.picker.clone();
        let swatch_button = SwatchTrigger {
            id: SharedString::from(format!("theme-swatch-{role}")),
            color: swatch,
            border: with_alpha(theme.foreground, 0.3).into(),
            tooltip: format!("Choose {label} color").into(),
            on_press: select_on_press,
            role,
            pressed: false,
        };
        let swatch_popover = popover(
            SharedString::from(format!("theme-color-popover-{role}")),
            PopoverOptions {
                padding: PopoverPadding::None,
                ..PopoverOptions::default()
            },
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
        .trigger(swatch_button)
        .content(move |_, _, _| picker.clone());
        // `flex min-h-11 min-w-0 items-center gap-2 rounded-lg px-2 py-1.5`; selected:
        // `bg-accent/60 inset-ring inset-ring-ring`.
        div()
            .flex()
            .items_center()
            .gap_2()
            .min_h(rems(2.75))
            .min_w_0()
            .px_2()
            .py(px(6.))
            .rounded(tokens::RADIUS_LG)
            .when(selected, |this| {
                this.bg(solid_over(theme.accent, 0.6, theme.background))
                    .border_1()
                    .border_color(theme.ring)
            })
            .child(
                div()
                    .id(SharedString::from(format!("theme-role-{role}")))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .cursor(CursorStyle::PointingHand)
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.text_color(theme.foreground))
                    .tooltip(move |window, cx| {
                        Tooltip::new(usage_tooltip.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| {
                        if let Some(toggle) = &toggle {
                            toggle(role, cx);
                        }
                    })
                    .child(div().min_w_0().flex_1().truncate().child(label)),
            )
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .child(swatch_popover)
                    .child(
                        div()
                            .w(rems(7.))
                            .flex_shrink_0()
                            .when(!is_color, |this| {
                                this.rounded(tokens::RADIUS_LG)
                                    .border_1()
                                    .border_color(theme.destructive)
                            })
                            .child(text_field(&self.input, None, FieldSize::Sm, true, cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::ui::color_picker::{hue_at, saturation_value_at};

    #[test]
    fn labels_roles_with_the_listed_names_or_their_words() {
        assert_eq!(get_theme_role_label("canvas"), "Background");
        assert_eq!(get_theme_role_label("accent"), "Accent color");
        assert_eq!(
            get_theme_role_label("toolbar_control_hover"),
            "Toolbar control hover"
        );
        assert_eq!(get_theme_role_label("error_surface"), "Error background");
        assert_eq!(get_theme_role_label("surface_raised"), "Surface raised");
        assert_eq!(get_theme_role_label("text"), "Text");
        assert_eq!(get_theme_role_label(""), "");
    }

    #[test]
    fn keeps_alpha_apart_from_the_opaque_hex() {
        assert_eq!(theme_picker_alpha_suffix("#ff000080"), "80");
        assert_eq!(theme_picker_alpha_suffix("#ff0000"), "");
        assert_eq!(theme_picker_alpha_suffix("#ff0000ff"), "");
        assert_eq!(theme_picker_alpha_suffix("not a colour"), "");
        assert_eq!(normalize_theme_picker_color("#ff000080"), "#ff0000");
        assert_eq!(normalize_theme_picker_color("nonsense"), "#000000");
    }

    #[test]
    fn converts_between_hex_and_rgb_text() {
        assert_eq!(theme_hex_to_rgb("#336699"), (51, 102, 153));
        assert_eq!(theme_rgb_value("#336699"), "51, 102, 153");
        for text in [
            "51, 102, 153",
            "rgb(51,102,153)",
            "RGB( 51 102 153 )",
            "51 102 153",
        ] {
            assert_eq!(theme_rgb_to_hex(text).as_deref(), Some("#336699"), "{text}");
        }
        for text in [
            "256, 0, 0",
            "-1, 0, 0",
            "1.5, 0, 0",
            "1, 2",
            "1, 2, 3, 4",
            "a, b, c",
            "",
        ] {
            assert_eq!(theme_rgb_to_hex(text), None, "{text}");
        }
    }

    fn hsv(h: f64, s: f64, v: f64) -> HsvColor {
        HsvColor { h, s, v }
    }

    #[test]
    fn independent_saturation_and_brightness_changes_batch_into_one_commit_with_alpha() {
        let mut model = ThemePickerModel::new("#ff000080");
        // Saturation to 50%, then brightness to 50%: only the last colour is reported.
        model.commit_hsv(hsv(0., 0.5, 1.));
        model.commit_hsv(hsv(0., 0.5, 0.5));
        assert_eq!(model.take_pending().as_deref(), Some("#80404080"));
        assert_eq!(model.take_pending(), None);
        // A keyboard step starts from the last values.
        model.commit_hsv(hsv(0., 0.5, 0.4));
        assert_eq!(model.take_pending().as_deref(), Some("#66333380"));
    }

    #[test]
    fn a_hue_drag_reports_the_final_colour_with_alpha() {
        let mut model = ThemePickerModel::new("#ff000080");
        let drag = |model: &mut ThemePickerModel, x: f64| {
            let hue = hue_at(x, 100.).expect("track has width");
            model.commit_hsv(hsv(hue, model.hsv.s, model.hsv.v));
        };
        drag(&mut model, 25.);
        drag(&mut model, 50.);
        assert_eq!(model.take_pending().as_deref(), Some("#00ffff80"));
    }

    #[test]
    fn out_of_bounds_plane_drags_clamp_to_the_corners() {
        let mut model = ThemePickerModel::new("#ff000080");
        let (s, v) = saturation_value_at(-50., -50., 100., 100.).expect("plane has size");
        model.commit_hsv(hsv(model.hsv.h, s, v));
        assert_eq!(model.take_pending().as_deref(), Some("#ffffff80"));
        let (s, v) = saturation_value_at(150., 150., 100., 100.).expect("plane has size");
        model.commit_hsv(hsv(model.hsv.h, s, v));
        assert_eq!(model.take_pending().as_deref(), Some("#00000080"));
    }

    #[test]
    fn keeps_the_selected_hue_when_a_grey_is_echoed_back() {
        let mut model = ThemePickerModel::new("#ff0000");
        let hue = crate::ui::color_picker::apply_hue_key(
            model.hsv.h,
            crate::ui::color_picker::ColorKey::ArrowRight,
            true,
        )
        .expect("arrow right steps the hue");
        model.commit_hsv(hsv(hue, model.hsv.s, model.hsv.v));
        let (s, v) = saturation_value_at(0., 0., 100., 100.).expect("plane has size");
        model.commit_hsv(hsv(model.hsv.h, s, v));
        assert_eq!(model.take_pending().as_deref(), Some("#ffffff"));

        model.sync_value("#ffffff");

        assert_eq!(model.hsv.h, 10.);
    }

    #[test]
    fn a_value_from_outside_replaces_the_controls() {
        let mut model = ThemePickerModel::new("#ff0000");
        model.sync_value("#00ff00");
        assert_eq!(model.current_color(), "#00ff00");
        assert_eq!(model.hex_draft, "#00ff00");
        assert_eq!(model.rgb_draft, "0, 255, 0");
    }

    #[test]
    fn an_incoming_value_does_not_overwrite_text_being_typed() {
        let mut model = ThemePickerModel::new("#ff0000");
        model.editing_text = true;
        assert_eq!(model.hex_change("#12"), None);

        // The guided editor echoes back a readability-adjusted colour mid-keystroke.
        model.sync_value("#123456");

        assert_eq!(model.hex_draft, "#12");
        // The swatch still tracks through the HSV.
        assert_eq!(model.current_color(), "#123456");

        model.end_text_edit();
        assert_eq!(model.hex_draft, "#123456");
        assert_eq!(model.rgb_draft, "18, 52, 86");
    }

    #[test]
    fn a_full_hex_reports_lowercase_immediately_without_alpha() {
        let mut model = ThemePickerModel::new("#ff000080");
        assert_eq!(model.hex_change("#ABCDEF").as_deref(), Some("#abcdef"));
        assert_eq!(model.rgb_draft, "171, 205, 239");
        assert_eq!(model.hex_change("#abcde"), None);
        assert_eq!(model.hex_change("abcdef"), None);
    }

    #[test]
    fn an_rgb_edit_keeps_the_alpha_suffix() {
        let mut model = ThemePickerModel::new("#ff000080");
        assert_eq!(
            model.rgb_change("51, 102, 153").as_deref(),
            Some("#33669980")
        );
        assert_eq!(model.hex_draft, "#336699");
        assert_eq!(model.rgb_change("51, 102"), None);
        assert_eq!(model.rgb_draft, "51, 102");
    }
}

/// The swatch button that opens the picker: `relative flex size-6 shrink-0 cursor-pointer
/// items-center justify-center rounded-full border border-foreground/30`, the colour
/// filling it with a soft shadow. Pressing it selects the role (`onInteract`).
#[derive(IntoElement)]
struct SwatchTrigger {
    id: SharedString,
    color: Hsla,
    border: Hsla,
    tooltip: SharedString,
    on_press: Option<Rc<dyn Fn(&str, &mut App)>>,
    role: &'static str,
    pressed: bool,
}

impl Selectable for SwatchTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.pressed = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.pressed
    }
}

impl RenderOnce for SwatchTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (role, on_press, tooltip) = (self.role, self.on_press, self.tooltip);
        div()
            .id(self.id)
            .relative()
            .flex()
            .size(rems(1.5))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(self.border)
            .cursor(CursorStyle::PointingHand)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
                if let Some(select) = &on_press {
                    select(role, cx);
                }
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded_full()
                    .shadow(tokens::shadow_sm(0.1))
                    .bg(self.color),
            )
    }
}
