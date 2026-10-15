//! Port of upstream/apps/web/src/components/pullRequest/PullRequestDetailPanel.tsx (placeholder
//! shell until ported): the header, the tab strip and the mounted tabs.

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window,
    div,
};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;

use super::detail_context::{DetailContext, DetailNav, DetailTab};
use super::summary_tab::SummaryTab;
use super::view_model::PullRequestStore;

pub struct PullRequestDetailPanel {
    pub ctx: DetailContext,
    pub tab: DetailTab,
    summary: Entity<SummaryTab>,
}

impl PullRequestDetailPanel {
    pub fn new(
        store: Entity<PullRequestStore>,
        environment_id: EnvironmentId,
        reference: PullRequestRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Some(source) = store.read(cx).source() {
            source.open_detail(&reference, cx);
        }
        let ctx = DetailContext {
            store,
            environment_id,
            reference,
            nav: DetailNav::noop(),
        };
        let summary = cx.new(|cx| SummaryTab::new(ctx.clone(), window, cx));
        Self {
            ctx,
            tab: DetailTab::Summary,
            summary,
        }
    }

    pub fn set_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }
}

impl Render for PullRequestDetailPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().flex_1().child(self.summary.clone())
    }
}
