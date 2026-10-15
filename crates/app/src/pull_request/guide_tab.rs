//! Port of the Guide tab of upstream/apps/web/src/components/pullRequest (placeholder until ported).

use gpui_kit::{Context, IntoElement, ParentElement as _, Render, Styled as _, Window, div};

use super::detail_context::DetailContext;

pub struct GuideTab {
    pub ctx: DetailContext,
}

impl GuideTab {
    pub fn new(ctx: DetailContext, _: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.store, |_, _, cx| cx.notify()).detach();
        Self { ctx }
    }
}

impl Render for GuideTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().p_4().child("Guide tab")
    }
}
