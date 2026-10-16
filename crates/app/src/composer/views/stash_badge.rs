//! Port of upstream/apps/web/src/components/chat/ComposerStashBadge.tsx
//!
//! The bookmark tab that shows the stash count beside the composer's other
//! attachments and opens the stash menu. On save the count takes the primary
//! tone for a moment and ticks in (opacity and a 2px rise over 180ms, ease
//! out); `pulse_key` changes per stash so the transition replays.

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, App, CursorStyle, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::components::chat::composer_pending_panels::{BannerVariant, banner_surface};
use crate::composer::views::composer_banner::{WindowCallback, banner_row, count_text, small_icon};
use crate::theme::ActiveAppTheme;
use crate::ui::IconName;
use crate::ui::animated_size::css_ease_out;

/// `aria-label` of the tab.
pub fn stash_badge_label(count: usize) -> String {
    format!("Stashed prompts: {count}. Open stash.")
}

#[derive(IntoElement)]
pub struct ComposerStashBadge {
    pub count: usize,
    pub menu_open: bool,
    pub pulse_key: usize,
    pub pulsing: bool,
    pub on_toggle_menu: WindowCallback,
}

impl RenderOnce for ComposerStashBadge {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.count == 0 {
            return div().into_any_element();
        }
        let theme = cx.app_theme().clone();
        let highlighted = self.menu_open || self.pulsing;
        let count_color = if self.pulsing {
            theme.primary
        } else {
            theme.muted_foreground
        };
        let count = {
            let element = count_text(self.count.to_string(), count_color);
            if self.pulsing {
                div()
                    .child(element)
                    .with_animation(
                        ("stash-pulse", self.pulse_key),
                        Animation::new(Duration::from_millis(180)).with_easing(css_ease_out),
                        |this, delta| this.opacity(delta).relative().top(px(2. * (1. - delta))),
                    )
                    .into_any_element()
            } else {
                element
            }
        };
        let foreground = theme.foreground;
        let on_toggle = self.on_toggle_menu;
        let row = banner_row(
            Some(small_icon(IconName::Bookmark, &theme)),
            div().child("Stash").into_any_element(),
            vec![count],
        );
        let tab = div()
            .id("composer-stash-badge")
            .relative()
            .rounded(px(6.))
            .text_color(if highlighted {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .when(self.menu_open, |this| this.occlude())
            .when(!self.menu_open, |this| {
                this.cursor(CursorStyle::PointingHand)
                    .hover(move |style| style.text_color(foreground))
                    .on_click(move |_, window, cx| on_toggle(window, cx))
            })
            .child(row)
            .into_any_element();
        div()
            .flex()
            .justify_end()
            .child(banner_surface(
                BannerVariant::Default,
                false,
                tab,
                window,
                &theme,
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::stash_badge_label;

    #[test]
    fn names_the_count_for_assistive_tech() {
        assert_eq!(stash_badge_label(2), "Stashed prompts: 2. Open stash.");
    }
}
