//! Port of upstream/apps/web/src/components/chat/ThreadDetailsSection.tsx

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, px, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_2XS;

#[derive(IntoElement)]
pub struct ThreadDetailsSection {
    pub heading_id: SharedString,
    pub title: SharedString,
    pub separated: bool,
    pub show_heading: bool,
    pub actions: Option<AnyElement>,
    pub children: Vec<AnyElement>,
}

impl ThreadDetailsSection {
    pub fn new(title: impl Into<SharedString>) -> Self {
        let title = title.into();
        Self {
            heading_id: title.clone(),
            title,
            separated: true,
            show_heading: true,
            actions: None,
            children: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn separated(mut self, separated: bool) -> Self {
        self.separated = separated;
        self
    }

    #[allow(dead_code)]
    pub fn show_heading(mut self, show: bool) -> Self {
        self.show_heading = show;
        self
    }

    #[allow(dead_code)]
    pub fn actions(mut self, actions: impl IntoElement) -> Self {
        self.actions = Some(actions.into_any_element());
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }
}

impl RenderOnce for ThreadDetailsSection {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        div()
            .px_2()
            .pt_2()
            .pb(px(10.))
            .when(self.separated, |this| {
                this.border_t_1()
                    .border_color(with_alpha(theme.border, 0.65))
            })
            .child(div().when(self.show_heading, |this| {
                this.flex()
                    .min_h(rems(2.))
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px(px(6.))
                    .mb_1()
                    .child(
                        div()
                            .id(self.heading_id)
                            .min_w_0()
                            .truncate()
                            .text_size(TEXT_2XS.size)
                            .line_height(TEXT_2XS.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child(self.title),
                    )
                    .when(self.actions.is_some(), |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap_1()
                                .children(self.actions),
                        )
                    })
            }))
            .children(self.children)
    }
}
