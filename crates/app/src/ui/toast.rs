//! Port of the toast card in upstream/apps/web/src/components/ui/toast.tsx.
//!
//! `dropdown-glass`, `rounded-lg`, `shadow-xl shadow-black/25`, a corner
//! dismiss orb, and the body from [`super::toast_logic::derive_toast_body`].
//! Collapsed cards use the peek translate (as `top`) and scale (as width).
//! Swipe, enter, and exit transforms are not ported: GPUI has no element
//! transforms. Copy-to-clipboard and thread routing stay with the caller.

use std::rc::Rc;

use gpui_kit::{
    App, ClickEvent, CursorStyle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use super::icon::{Icon, IconName};
use super::toast_logic::{
    ToastBodyDescriptor, ToastBodyInput, ToastType, derive_toast_body, toast_collapsed_scale,
    toast_collapsed_translate_y, toast_expanded_translate_y,
};
use super::tooltip::{dropdown_glass_background, dropdown_glass_border};
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM};

fn toast_icon(type_: ToastType) -> IconName {
    match type_ {
        ToastType::Error => IconName::CircleAlert,
        ToastType::Info => IconName::Info,
        ToastType::Loading => IconName::LoaderCircle,
        ToastType::Success => IconName::CircleCheck,
        ToastType::Warning => IconName::TriangleAlert,
    }
}

fn icon_color(type_: ToastType, cx: &App) -> gpui_kit::Hsla {
    let theme = cx.app_theme();
    match type_ {
        ToastType::Error => theme.destructive,
        ToastType::Info => theme.info,
        ToastType::Success => theme.success,
        ToastType::Warning => theme.warning,
        ToastType::Loading => theme.foreground,
    }
}

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ToastCard {
    id: SharedString,
    title: SharedString,
    description: Option<SharedString>,
    body: ToastBodyDescriptor,
    type_: Option<ToastType>,
    on_dismiss: Option<Dismiss>,
}

impl ToastCard {
    pub fn new(
        id: impl Into<SharedString>,
        title: impl Into<SharedString>,
        input: ToastBodyInput<'_>,
    ) -> Self {
        let type_ = input.type_;
        Self {
            id: id.into(),
            title: title.into(),
            description: input.description.map(SharedString::from),
            body: derive_toast_body(input),
            type_,
            on_dismiss: None,
        }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ToastCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let end_pad = if self.body.inline_end_pad_is_pr6 {
            rems(1.5)
        } else {
            rems(2.5)
        };
        let stacked = self.body.stacked_action_layout;
        let mut content = div()
            .flex()
            .min_h_0()
            .pl(rems(0.875))
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height);
        content = if stacked {
            content.flex_col().gap_2().py(rems(0.625)).pr(rems(0.875))
        } else {
            content
                .items_center()
                .justify_between()
                .gap(rems(0.375))
                .py_3()
                .pr(end_pad)
        };
        let description = self.description.clone();
        let clamped =
            super::toast_logic::error_description_is_clamped(self.type_, description.as_deref());
        content = content.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .min_w_0()
                        .children(self.type_.map(|type_| {
                            let mut icon = Icon::new(toast_icon(type_))
                                .size(rems(1.))
                                .color(icon_color(type_, cx));
                            if type_ == ToastType::Loading {
                                icon = icon.color(with_alpha(theme.foreground, 0.8).into());
                            }
                            icon
                        }))
                        .child(
                            div()
                                .min_w_0()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(self.title),
                        ),
                )
                .children(description.map(|text| {
                    let mut line = div()
                        .min_w_0()
                        .text_color(theme.muted_foreground)
                        .child(text);
                    if clamped {
                        line = line.line_clamp(4);
                    }
                    line
                })),
        );
        let mut card = div()
            .id(self.id.clone())
            .relative()
            .w_full()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(dropdown_glass_border(theme))
            .bg(dropdown_glass_background(theme))
            .text_color(theme.popover_foreground)
            .shadow(tokens::shadow_xl(0.25))
            .child(content);
        if let Some(on_dismiss) = self.on_dismiss {
            card = card.child(
                div()
                    .absolute()
                    .top(rems(-0.375))
                    .right(rems(-0.375))
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-dismiss", self.id)))
                            .on_mouse_down(MouseButton::Left, no_drag)
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(rems(1.5))
                            .rounded_full()
                            .border_1()
                            .border_color(with_alpha(theme.border, 0.6))
                            .bg(with_alpha(theme.popover, 0.92))
                            .text_color(theme.muted_foreground)
                            .shadow(tokens::shadow_sm(0.1))
                            .cursor(CursorStyle::PointingHand)
                            .hover(|style| style.bg(theme.popover).text_color(theme.foreground))
                            .on_click(move |_: &ClickEvent, window, cx| on_dismiss(window, cx))
                            .child(Icon::new(IconName::X).size(rems(0.75))),
                    ),
            );
        }
        card
    }
}

/// Places cards with the collapsed or expanded stack offsets.
pub fn toast_stack_offset(
    index: usize,
    frontmost_height: f64,
    offset_y: f64,
    expanded: bool,
    top: bool,
) -> f64 {
    if expanded {
        toast_expanded_translate_y(offset_y, index, top)
    } else {
        toast_collapsed_translate_y(index, frontmost_height, top)
    }
}

pub fn toast_stack_width_scale(index: usize, expanded: bool) -> f64 {
    if expanded {
        1.
    } else {
        toast_collapsed_scale(index)
    }
}
