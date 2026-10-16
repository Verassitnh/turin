//! Port of upstream/apps/web/src/components/chat/ThreadDetailsCard.tsx

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div, px,
};

use super::thread_details_card_layout::ThreadDetailsCardLayout;
use crate::theme::ActiveAppTheme;
use crate::ui::scroll_area_hidden;
use crate::ui::tooltip::{dropdown_glass_background, dropdown_glass_border};

#[derive(IntoElement)]
pub struct ThreadDetailsCard {
    pub layout: ThreadDetailsCardLayout,
    pub children: AnyElement,
}

impl RenderOnce for ThreadDetailsCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        div()
            .id("thread-details-card")
            .absolute()
            .top(px(self.layout.y as f32))
            .left(px(self.layout.x as f32))
            .w(px(self.layout.width as f32))
            .h(px(self.layout.height as f32))
            .rounded_3xl()
            .border_1()
            .border_color(dropdown_glass_border(&theme))
            .bg(dropdown_glass_background(&theme))
            .shadow(crate::theme::tokens::shadow_popover(dark))
            .overflow_hidden()
            .child(
                scroll_area_hidden("thread-details-scroll")
                    .child(div().w_full().py_2().child(self.children)),
            )
    }
}
