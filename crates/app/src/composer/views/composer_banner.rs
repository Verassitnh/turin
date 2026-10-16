//! Port of the row parts of upstream/apps/web/src/components/chat/ComposerBanner.tsx:
//! `Row`, `Icon`, `Content`, `Actions`, `Count` and `ToggleIcon`.
//!
//! `Root`/`Attachment` (the glass surface that tucks under the composer) is
//! [`banner_surface`](crate::components::chat::composer_pending_panels::banner_surface).
//! The row is a flex strip instead of upstream's three-column grid; the icon
//! column is `--composer-banner-icon-column` (`6`, 1.5rem, at `sm:`).

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    px, rems,
};

use crate::theme::AppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

use gpui_kit::prelude::FluentBuilder as _;

/// A click handler that needs the window, as the banner and chip parts take.
pub type WindowCallback = Rc<dyn Fn(&mut Window, &mut App)>;

/// `ComposerBanner.Row` with its `Icon`, `Content` and `Actions` slots.
pub fn banner_row(
    icon: Option<AnyElement>,
    content: AnyElement,
    actions: Vec<AnyElement>,
) -> gpui_kit::Div {
    div()
        .flex()
        .w_full()
        .min_w_0()
        .min_h(rems(1.5))
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .flex()
                .flex_none()
                .w(rems(1.5))
                .items_center()
                .justify_center()
                .children(icon),
        )
        .child(
            div()
                .flex()
                .min_w_0()
                .flex_1()
                .items_center()
                .gap_1()
                .child(content),
        )
        .when(!actions.is_empty(), |this| {
            this.child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .children(actions),
            )
        })
}

/// `ComposerBanner.Icon`: a `size-3` glyph in `text-muted-foreground`.
pub fn small_icon(name: IconName, theme: &AppTheme) -> AnyElement {
    Icon::new(name)
        .size(rems(0.75))
        .color(theme.muted_foreground)
        .into_any_element()
}

/// `ComposerBanner.Count`: `min-w-(--icon-column) justify-center font-medium
/// tabular-nums text-muted-foreground`; `color` replaces the muted tone.
pub fn count_text(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    div()
        .flex()
        .min_w(rems(1.5))
        .flex_none()
        .justify_center()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_size(TEXT_XS.size)
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// The common count: a number in the muted tone.
pub fn count_label(count: usize, theme: &AppTheme) -> AnyElement {
    count_text(count.to_string(), theme.muted_foreground)
}

/// `ComposerBanner.ToggleIcon`: decorative `icon-xs` ghost button holding a
/// chevron (`rotate-180` while collapsed). The row is the control.
pub fn toggle_icon(expanded: bool) -> AnyElement {
    Button::new("composer-banner-toggle")
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::IconXs)
        .icon(if expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronUp
        })
        .icon_size(rems(0.875))
        .into_any_element()
}
