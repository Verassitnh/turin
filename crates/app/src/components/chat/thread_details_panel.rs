//! Port of upstream/apps/web/src/components/chat/ThreadDetailsPanel.tsx (lineage section).

use gpui_kit::{
    App, Entity, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
};

use super::thread_details_demo::{parse_thread_details_demo, pr_rows_demo};
use super::thread_relationships_panel::ThreadRelationshipsPanel;

#[derive(IntoElement)]
pub struct ThreadDetailsPanel {
    lineage: Entity<ThreadRelationshipsPanel>,
    pr_rows: Option<gpui_kit::AnyElement>,
}

impl ThreadDetailsPanel {
    pub fn from_demo(window: &mut Window, cx: &mut App) -> Option<Self> {
        let snapshot = parse_thread_details_demo()?;
        Some(Self {
            pr_rows: pr_rows_demo(window, cx),
            lineage: window.use_keyed_state("thread-relationships-panel", cx, |_, _| {
                ThreadRelationshipsPanel::new(snapshot)
            }),
        })
    }
}

impl RenderOnce for ThreadDetailsPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .children(self.pr_rows)
            .child(self.lineage.clone())
            .into_any_element()
    }
}
