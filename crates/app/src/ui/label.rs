//! Port of upstream/apps/web/src/components/ui/label.tsx
//!
//! Desktop (`sm:`): `inline-flex items-center gap-2 text-sm/4 font-medium
//! text-foreground`. `text-sm/4` is 14px type on a 16px line.

use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_SM;

#[derive(IntoElement)]
pub struct Label {
    text: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: Some(text.into()),
            children: Vec::new(),
        }
    }
}

impl ParentElement for Label {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Label {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(TEXT_SM.size)
            .line_height(rems(1.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(cx.app_theme().foreground)
            .children(self.text)
            .children(self.children)
    }
}
