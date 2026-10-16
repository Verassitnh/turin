//! Port of upstream/apps/web/src/components/ui/alert.tsx (the `default` and
//! `info` variants the settings pages use).
//!
//! `relative rounded-xl border px-3.5 py-3 text-sm text-card-foreground`; the
//! `default` variant is `bg-transparent dark:bg-input/32` with a muted icon,
//! `info` is `border-info/32 bg-info/4` with an `info` icon. The icon sits in a
//! `size-4` box, the content takes the remaining width and the optional action
//! is vertically centred.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, IntoElement as _, ParentElement as _, Styled as _, div, px, rems};

use crate::theme::AppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_XL, TEXT_SM};
use crate::ui::{Icon, IconName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertVariant {
    Default,
    Info,
}

/// `<Alert variant><Icon /><AlertDescription>…</AlertDescription><AlertAction>…</AlertAction></Alert>`.
pub fn alert(
    theme: &AppTheme,
    variant: AlertVariant,
    icon: Option<IconName>,
    content: Vec<AnyElement>,
    action: Option<AnyElement>,
) -> AnyElement {
    let dark = theme.is_dark();
    let (border, background, icon_color) = match variant {
        AlertVariant::Default => (
            theme.border,
            if dark {
                with_alpha(theme.input, 0.32).into()
            } else {
                gpui_kit::transparent_black()
            },
            theme.muted_foreground,
        ),
        AlertVariant::Info => (
            with_alpha(theme.info, 0.32).into(),
            with_alpha(theme.info, 0.04).into(),
            theme.info,
        ),
    };
    div()
        .relative()
        .rounded(RADIUS_XL)
        .border_1()
        .border_color(border)
        .bg(background)
        .px(rems(0.875))
        .py_3()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(theme.card_foreground)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(icon.map(|icon| {
                    div()
                        .flex()
                        .size_4()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size(rems(1.)).color(icon_color))
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .gap(px(2.))
                        .text_color(theme.muted_foreground)
                        .children(content),
                )
                .when_some(action, |this, action| {
                    this.child(div().flex().flex_shrink_0().items_center().child(action))
                }),
        )
        .into_any_element()
}
