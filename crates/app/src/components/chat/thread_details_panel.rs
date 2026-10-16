//! Port of upstream/apps/web/src/components/chat/ThreadDetailsPanel.tsx (the lineage section and,
//! in the demo, the pull request rows).
//!
//! Content folds by density as upstream's does: the lineage section is `full`-only, the rest
//! stays down to `compact`, and `essential` keeps only what cannot be dropped.

use gpui_kit::{
    AnyElement, App, Entity, IntoElement, ParentElement as _, Styled as _, Window, div,
};

use super::thread_details_card_layout::ThreadDetailsCardDensity;
use super::thread_details_demo::{parse_thread_details_demo, pr_rows_demo};
use super::thread_relationships_panel::ThreadRelationshipsPanel;

pub struct ThreadDetailsPanel {
    lineage: Entity<ThreadRelationshipsPanel>,
}

impl ThreadDetailsPanel {
    pub fn from_demo(window: &mut Window, cx: &mut App) -> Option<Self> {
        let snapshot = parse_thread_details_demo()?;
        Some(Self {
            lineage: window.use_keyed_state("thread-relationships-panel", cx, |_, _| {
                ThreadRelationshipsPanel::new(snapshot)
            }),
        })
    }

    /// `children(density)`.
    pub fn render_density(
        &self,
        density: ThreadDetailsCardDensity,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .children(pr_rows_demo(window, cx))
            // `density === "full" && !props.draftId`.
            .children((density == ThreadDetailsCardDensity::Full).then(|| self.lineage.clone()))
            .into_any_element()
    }
}
