//! Settings controls: ports of upstream components/ui/{switch,select,
//! number-field}.tsx at the sizes the settings rows use.
//!
//! Everything is stateless: the owning view keeps the open menu in a
//! [`MenuState`] and the controls report intent through closures.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, CursorStyle, Div, ElementId, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div, px, rems,
};

use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_MD, RADIUS_SM, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::ui::tooltip::dropdown_glass_border;
use crate::ui::{Icon, IconName};

/// Which select / menu popup is open. One at a time, as with base-ui.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    open: Option<SharedString>,
}

impl MenuState {
    pub fn is_open(&self, id: &str) -> bool {
        self.open.as_deref() == Some(id)
    }

    pub fn toggle(&mut self, id: impl Into<SharedString>) {
        let id = id.into();
        self.open = if self.open.as_ref() == Some(&id) {
            None
        } else {
            Some(id)
        };
    }

    pub fn open(&mut self, id: impl Into<SharedString>) {
        self.open = Some(id.into());
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    pub fn any_open(&self) -> bool {
        self.open.is_some()
    }
}

/// `redactedPlaceholder` (RedactedSensitiveText.tsx): a stable scramble of
/// `value` that keeps `@`, `.`, `-` and `_`, so a hidden email keeps its shape.
/// The hash runs over UTF-16 units (`charCodeAt`); every other character, taken
/// by code point (`Array.from`), becomes one character of the alphabet.
pub fn redacted_placeholder(value: &str) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut state: u32 = 0x811c_9dc5;
    for unit in value.encode_utf16() {
        state ^= u32::from(unit);
        state = state.wrapping_mul(0x0100_0193);
    }
    let mut next_char = || {
        state = (state ^ (state >> 13)).wrapping_mul(0x85eb_ca6b);
        state = (state ^ (state >> 16)).wrapping_mul(0xc2b2_ae35);
        char::from(ALPHABET[(state as i32).unsigned_abs() as usize % ALPHABET.len()])
    };
    value
        .chars()
        .map(|c| {
            if matches!(c, '@' | '.' | '-' | '_') {
                c
            } else {
                next_char()
            }
        })
        .collect()
}

/// `RedactedSensitiveText`: `value` as its [`redacted_placeholder`] until
/// `revealed`; a click toggles. GPUI has no blur filter, so the scramble stands
/// in for upstream's `blur-xs` and the value itself never reaches the element
/// while hidden.
pub fn redacted_sensitive_text(
    id: impl Into<ElementId>,
    value: &str,
    revealed: bool,
    cx: &App,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.app_theme();
    let hover = theme.foreground;
    div()
        .id(id)
        .min_w_0()
        .rounded(RADIUS_SM)
        .font_family(theme.fonts.mono.clone())
        .text_size(TEXT_2XS.size)
        .text_color(theme.muted_foreground)
        .cursor(CursorStyle::PointingHand)
        .hover(move |this| this.text_color(hover))
        .on_mouse_down(MouseButton::Left, no_drag)
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .child(if revealed {
            value.to_owned()
        } else {
            redacted_placeholder(value)
        })
}

/// `--thumb-size` of `Switch` at its default size on `sm:` (`spacing(4)`).
const SWITCH_THUMB_SIZE: f32 = 16.;

/// Track `(width, height)`: `w-[calc(var(--thumb-size)*2-2px)]`, `h-[calc(var(--thumb-size)+2px)]`.
fn switch_track_size() -> (f32, f32) {
    (SWITCH_THUMB_SIZE * 2. - 2., SWITCH_THUMB_SIZE + 2.)
}

/// Thumb `translate-x`: `thumb-4px` when checked, centred `(thumb-4px)/2` when
/// `mixed`, otherwise 0.
fn switch_thumb_offset(checked: bool, mixed: bool) -> f32 {
    if mixed {
        (SWITCH_THUMB_SIZE - 4.) / 2.
    } else if checked {
        SWITCH_THUMB_SIZE - 4.
    } else {
        0.
    }
}

/// `Switch` (default size at the `sm:` breakpoint): thumb 14px, track
/// 30×18, `p-[2px]`, `bg-primary` checked / `bg-input` unchecked, thumb
/// `bg-background shadow-sm/5`. `mixed` centres the thumb on the muted track.
pub fn switch(
    id: impl Into<ElementId>,
    checked: bool,
    disabled: bool,
    cx: &App,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> gpui_kit::Stateful<Div> {
    switch_state(id, checked, false, disabled, cx, on_toggle)
}

pub fn switch_state(
    id: impl Into<ElementId>,
    checked: bool,
    mixed: bool,
    disabled: bool,
    cx: &App,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.app_theme();
    let track = if checked && !mixed {
        theme.primary
    } else {
        theme.input
    };
    let offset = switch_thumb_offset(checked, mixed);
    let (track_width, track_height) = switch_track_size();
    div()
        .id(id)
        .on_mouse_down(MouseButton::Left, no_drag)
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(px(track_width))
        .h(px(track_height))
        .p(px(2.))
        .rounded_full()
        .bg(track)
        .when(disabled, |this| {
            this.opacity(0.64).cursor(CursorStyle::OperationNotAllowed)
        })
        .when(!disabled, |this| {
            this.cursor(CursorStyle::PointingHand)
                .on_click(move |_, window, cx| on_toggle(!checked, window, cx))
        })
        .child(
            div()
                .ml(px(offset))
                .size(px(SWITCH_THUMB_SIZE - 2.))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme.background)
                .shadow(tokens::shadow_sm(0.05))
                .when(mixed, |this| this.opacity(0.7)),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectSize {
    /// `h-7 gap-1 rounded-md px-[calc(0.5rem-1px)] text-xs`
    Compact,
    #[default]
    /// `min-h-8 px-[calc(0.75rem-1px)]`, text-sm
    Default,
    /// `min-h-7 gap-1.5 px-[calc(0.625rem-1px)]`, text-sm
    Sm,
    /// `h-6 gap-1 rounded-md px-[calc(0.5rem-1px)]`, text-xs
    Xs,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SelectWidth {
    /// `w-full` (text width at least `min-w-36`).
    Full,
    /// `w-full sm:w-<rems>`
    Fixed(f32),
    /// `w-auto min-w-0`
    Auto,
}

/// `SelectTrigger`: bordered field showing the current label and a
/// `chevron-down size-3 opacity-50` (`-me-1`).
pub fn select_trigger(
    id: impl Into<ElementId>,
    label: impl IntoElement,
    size: SelectSize,
    width: SelectWidth,
    open: bool,
    disabled: bool,
    cx: &App,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.app_theme();
    let (min_h, gap, pad, radius, text) = match size {
        SelectSize::Compact => (rems(1.75), rems(0.25), px(7.), RADIUS_MD, TEXT_XS),
        SelectSize::Default => (rems(2.), rems(0.5), px(11.), RADIUS_LG, TEXT_SM),
        SelectSize::Sm => (rems(1.75), rems(0.375), px(9.), RADIUS_LG, TEXT_SM),
        SelectSize::Xs => (rems(1.5), rems(0.25), px(7.), RADIUS_MD, TEXT_XS),
    };
    let dark = theme.is_dark();
    let mut trigger = div()
        .id(id)
        .on_mouse_down(MouseButton::Left, no_drag)
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .gap(gap)
        .h(min_h)
        .pl(pad)
        // The chevron sits `-me-1` in upstream; a negative margin collapses
        // GPUI's content-sized (`w-auto`) trigger, so trim the padding instead.
        .pr(pad - px(4.))
        .rounded(radius)
        .border_1()
        .border_color(if open { theme.ring } else { theme.input })
        .bg(if dark {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .text_size(text.size)
        .line_height(text.line_height)
        .text_color(theme.foreground)
        .cursor(CursorStyle::PointingHand)
        .when(!open && !disabled, |this| {
            this.shadow(tokens::shadow_xs(0.05))
        })
        .when(disabled, |this| {
            this.opacity(0.64).cursor(CursorStyle::OperationNotAllowed)
        });
    trigger = match width {
        SelectWidth::Full => trigger.w_full().min_w(rems(9.)),
        SelectWidth::Fixed(rems_width) => trigger.w(rems(rems_width)),
        SelectWidth::Auto => trigger.flex_shrink_0(),
    };
    let label_box = match width {
        SelectWidth::Auto => div().min_w_0().truncate(),
        _ => div().min_w_0().flex_1().truncate().text_left(),
    };
    trigger.child(label_box.child(label)).child(
        div().flex().flex_shrink_0().child(
            Icon::new(IconName::ChevronDown)
                .size(rems(0.75))
                .color(with_alpha(theme.foreground, 0.5).into()),
        ),
    )
}

pub struct SelectItem {
    pub id: SharedString,
    pub label: AnyElement,
    pub selected: bool,
    pub disabled: bool,
    /// `MenuSeparator`: a rule instead of a row.
    pub separator: bool,
}

impl SelectItem {
    pub fn new(id: impl Into<SharedString>, label: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            label: label.into_any_element(),
            selected: false,
            disabled: false,
            separator: false,
        }
    }

    /// `MenuSeparator`.
    pub fn separator() -> Self {
        Self {
            id: SharedString::from("--separator"),
            label: div().into_any_element(),
            selected: false,
            disabled: true,
            separator: true,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupAlign {
    Start,
    End,
}

/// `SelectPopup` / `MenuPopup`: `dropdown-glass rounded-lg` list with `p-1`
/// rows (`min-h-7 rounded-sm px-2 py-1 text-sm`, `hover:bg-accent`; the
/// selected row `bg-foreground/[0.08]`).
pub fn popup_list(
    id: impl Into<ElementId>,
    items: Vec<SelectItem>,
    min_width: Option<Pixels>,
    cx: &App,
    on_pick: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.app_theme().clone();
    let dark = theme.is_dark();
    let id: ElementId = id.into();
    div()
        .id(id.clone())
        .occlude()
        .flex()
        .flex_col()
        .when_some(min_width, |this, width| this.min_w(width))
        .max_h(rems(18.))
        .overflow_y_scroll()
        .p_1()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(dropdown_glass_border(&theme))
        .bg(theme.popover)
        .shadow(tokens::shadow_popover(dark))
        .text_color(theme.popover_foreground)
        .children(items.into_iter().map(move |item| {
            let pick = on_pick.clone();
            let item_id = item.id.clone();
            if item.separator {
                return div()
                    .id(SharedString::from(format!("{id:?}-separator")))
                    .mx(px(-4.))
                    .my_1()
                    .h(px(1.))
                    .bg(theme.border)
                    .into_any_element();
            }
            div()
                .id(SharedString::from(format!("{id:?}-{}", item.id)))
                .flex()
                .min_h_7()
                .items_center()
                .rounded(RADIUS_SM)
                .px_2()
                .py_1()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .when(item.selected, |this| {
                    this.bg(with_alpha(theme.foreground, 0.08))
                })
                .when(item.disabled, |this| this.opacity(0.64))
                .when(!item.disabled, |this| {
                    this.cursor(CursorStyle::PointingHand)
                        .hover(|style| style.bg(theme.accent).text_color(theme.accent_foreground))
                        .on_click(move |_, window, cx| pick(item_id.clone(), window, cx))
                })
                .child(div().min_w_0().flex_1().child(item.label))
                .into_any_element()
        }))
}

/// A trigger with its popup. `popup` is rendered (deferred, above everything)
/// below the trigger while `open`, aligned to the trigger's `align` edge.
pub fn with_popup(
    trigger: impl IntoElement,
    popup: Option<AnyElement>,
    align: PopupAlign,
    trigger_height: Pixels,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .child(trigger)
        .when_some(popup, move |this, popup| {
            this.child(
                div()
                    .absolute()
                    .top(trigger_height + px(4.))
                    .map(|this| match align {
                        PopupAlign::Start => this.left_0(),
                        PopupAlign::End => this.right_0(),
                    })
                    .size_0()
                    .child(deferred(
                        anchored()
                            .anchor(match align {
                                PopupAlign::Start => Anchor::TopLeft,
                                PopupAlign::End => Anchor::TopRight,
                            })
                            .snap_to_window_with_margin(px(8.))
                            .child(popup),
                    )),
            )
        })
}

/// Convenience used by panel code: the click handler that toggles `id` in a
/// view's [`MenuState`].
pub fn toggle_menu<V: 'static>(
    cx: &mut Context<V>,
    get: impl Fn(&mut V) -> &mut MenuState + 'static,
    id: SharedString,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let entity = cx.entity().downgrade();
    let get = Rc::new(get);
    move |_, _, cx| {
        let id = id.clone();
        let get = get.clone();
        entity
            .update(cx, |this, cx| {
                get(this).toggle(id);
                cx.notify();
            })
            .ok();
    }
}

/// `h-8` etc. for popups anchored to the common trigger sizes.
pub fn trigger_height(size: SelectSize) -> Pixels {
    match size {
        SelectSize::Compact => rems(1.75).to_pixels(px(16.)),
        SelectSize::Default => rems(2.).to_pixels(px(16.)),
        SelectSize::Sm => rems(1.75).to_pixels(px(16.)),
        SelectSize::Xs => rems(1.5).to_pixels(px(16.)),
    }
}

/// Field sizes of `Input` (`sm`: `h-6.5` inner at the `sm:` breakpoint).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldSize {
    Sm,
    Default,
}

/// An `Input` frame (`rounded-lg border border-input bg-background shadow-xs/5`,
/// `dark:bg-input/32`) around a gpui-component text input.
pub fn text_field(
    state: &gpui_kit::Entity<gpui_kit::component::input::InputState>,
    width: Option<Pixels>,
    size: FieldSize,
    mono: bool,
    cx: &App,
) -> Div {
    text_field_leading(state, width, size, mono, None, cx)
}

/// Like [`text_field`], for password controls (`autoComplete="off"` upstream).
pub fn password_field(
    state: &gpui_kit::Entity<gpui_kit::component::input::InputState>,
    width: Option<Pixels>,
    size: FieldSize,
    cx: &App,
) -> Div {
    use gpui_kit::component::input::{Input, InputContentType};

    let theme = cx.app_theme();
    let height = match size {
        FieldSize::Sm => px(28.),
        FieldSize::Default => px(32.),
    };
    div()
        .flex()
        .items_center()
        .h(height)
        .when_some(width, |this, width| this.w(width))
        .when(width.is_none(), |this| this.w_full())
        .min_w_0()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.input)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .shadow(tokens::shadow_xs(0.05))
        .text_size(TEXT_SM.size)
        .px(px(9.))
        .gap(px(6.))
        .child(
            Input::new(state)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .content_type(InputContentType::Password),
        )
}

/// [`text_field`] with an `InputGroupAddon` icon before the text.
pub fn text_field_leading(
    state: &gpui_kit::Entity<gpui_kit::component::input::InputState>,
    width: Option<Pixels>,
    size: FieldSize,
    mono: bool,
    leading: Option<AnyElement>,
    cx: &App,
) -> Div {
    let theme = cx.app_theme();
    let height = match size {
        FieldSize::Sm => px(28.),
        FieldSize::Default => px(32.),
    };
    div()
        .flex()
        .items_center()
        .h(height)
        .when_some(width, |this, width| this.w(width))
        .when(width.is_none(), |this| this.w_full())
        .min_w_0()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.input)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .shadow(tokens::shadow_xs(0.05))
        .text_size(TEXT_SM.size)
        .when(mono, |this| this.font_family(theme.fonts.mono.clone()))
        .px(px(9.))
        .gap(px(6.))
        .children(leading)
        .child(
            gpui_kit::component::input::Input::new(state)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false),
        )
}

/// A disabled `Input` showing `value` (or `placeholder` when empty): `Input`'s
/// frame at `opacity-64`, with the placeholder in `muted-foreground`.
pub fn static_text_field(value: &str, placeholder: &str, width: Option<Pixels>, cx: &App) -> Div {
    let theme = cx.app_theme();
    let shown_text_color = if value.is_empty() {
        with_alpha(theme.muted_foreground, 0.72).into()
    } else {
        theme.foreground
    };
    div()
        .flex()
        .items_center()
        .h(px(32.))
        .when_some(width, |this, width| this.w(width))
        .when(width.is_none(), |this| this.w_full())
        .min_w_0()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.input)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .opacity(0.64)
        .text_size(TEXT_SM.size)
        .px(px(9.))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(shown_text_color)
                .child(if value.is_empty() {
                    placeholder.to_owned()
                } else {
                    value.to_owned()
                }),
        )
}

/// A `Textarea` frame: `Input`'s border and fill around a multi-line state,
/// `min-h` of `rows` lines plus padding.
pub fn textarea_field(
    state: &gpui_kit::Entity<gpui_kit::component::input::TextareaState>,
    rows: usize,
    cx: &App,
) -> Div {
    let theme = cx.app_theme();
    div()
        .flex()
        .w_full()
        .min_h(px(rows as f32 * 20. + 14.))
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.input)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32).into()
        } else {
            theme.background
        })
        .shadow(tokens::shadow_xs(0.05))
        .text_size(TEXT_SM.size)
        .px(px(9.))
        .py(px(6.))
        .child(gpui_kit::component::input::Textarea::new(state))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn switch_matches_the_default_size_at_sm() {
        // 16px thumb-size: a 30×18 track, a 14px thumb, 12px checked travel.
        assert_eq!(switch_track_size(), (30., 18.));
        assert_eq!(SWITCH_THUMB_SIZE - 2., 14.);
        assert_eq!(switch_thumb_offset(false, false), 0.);
        assert_eq!(switch_thumb_offset(true, false), 12.);
        assert_eq!(switch_thumb_offset(true, true), 6.);
    }

    #[test]
    fn redacted_placeholder_matches_upstream() {
        // Expected values come from upstream's `redactedPlaceholder`.
        assert_eq!(
            redacted_placeholder("developer@example.com"),
            "yvs4jrutu@gyetdne.2j3"
        );
        assert_eq!(redacted_placeholder("a_b-c.d"), "s_f-e.u");
        assert_eq!(redacted_placeholder("x"), "5");
    }
}
