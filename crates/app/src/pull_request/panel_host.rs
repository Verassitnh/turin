//! The right-panel body the pull request page shows (upstream `RightPanelTabs` with
//! `PullRequestDetailPanel` as the active surface): the 52px tab strip with one tab per opened
//! pull request, then the active detail panel.
//!
//! Not ported: tab drag / close menus, the add-surface button and the maximize control (the
//! shell owns the panel width and layout controls).

use gpui_kit::{
    AppContext as _, Context, CursorStyle, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;

use super::detail_context::DetailTab;
use super::detail_panel::PullRequestDetailPanel;
use super::presentation_view::PullRequestStateGlyph;
use super::view_model::{Loadable, PullRequestStore, pull_request_key};
use crate::components::drag_region::{DragRegion as _, no_drag};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};

struct OpenSurface {
    key: String,
    reference: PullRequestRef,
    panel: Entity<PullRequestDetailPanel>,
}

pub struct PullRequestPanelHost {
    store: Entity<PullRequestStore>,
    surfaces: Vec<OpenSurface>,
    active: usize,
}

impl PullRequestPanelHost {
    pub fn new(store: Entity<PullRequestStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            surfaces: Vec::new(),
            active: 0,
        }
    }

    /// `openPullRequest`: activates the pull request's tab, adding it when it is not open yet.
    pub fn open(
        &mut self,
        environment_id: EnvironmentId,
        reference: PullRequestRef,
        tab: Option<DetailTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = pull_request_key(&reference);
        let index = match self.surfaces.iter().position(|surface| surface.key == key) {
            Some(index) => index,
            None => {
                let store = self.store.clone();
                let panel_reference = reference.clone();
                let panel = cx.new(|cx| {
                    PullRequestDetailPanel::new(store, environment_id, panel_reference, window, cx)
                });
                self.surfaces.push(OpenSurface {
                    key,
                    reference,
                    panel,
                });
                self.surfaces.len() - 1
            }
        };
        self.active = index;
        if let Some(tab) = tab {
            self.surfaces[index]
                .panel
                .update(cx, |panel, cx| panel.set_tab(tab, cx));
        }
        cx.notify();
    }

    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        self.surfaces.clear();
        self.active = 0;
        cx.notify();
    }
}

impl Render for PullRequestPanelHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let tabs: Vec<_> = self
            .surfaces
            .iter()
            .enumerate()
            .map(|(index, surface)| {
                let active = index == self.active;
                let snapshot = self.store.read(cx).detail(&surface.reference);
                let (state, is_draft) = match snapshot.map(|s| &s.detail) {
                    Some(Loadable::Ready(detail)) => (detail.state, detail.is_draft),
                    _ => (turin_contracts::pull_request::PullRequestState::Open, false),
                };
                let label: SharedString = format!("#{}", surface.reference.number.get()).into();
                let tab = div()
                    .id(SharedString::from(format!("pr-surface-{}", surface.key)))
                    .on_mouse_down(MouseButton::Left, no_drag)
                    .flex()
                    .h_6()
                    .max_w(rems(9.))
                    .flex_shrink_0()
                    .items_center()
                    .gap(rems(0.125))
                    .pl(rems(0.375))
                    .pr_2()
                    .rounded(RADIUS_MD)
                    .text_size(TEXT_XS.size)
                    .cursor(CursorStyle::PointingHand)
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .size(rems(1.))
                            .items_center()
                            .justify_center()
                            .child(PullRequestStateGlyph::new(state, is_draft).size(rems(0.75))),
                    )
                    .child(div().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = index;
                        cx.notify();
                    }));
                if active {
                    tab.bg(theme.accent).text_color(theme.foreground)
                } else {
                    tab.text_color(theme.muted_foreground).hover(|style| {
                        style
                            .bg(with_alpha(theme.accent, 0.6))
                            .text_color(theme.foreground)
                    })
                }
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(theme.background)
            .child(
                div()
                    .id("pr-panel-tab-bar")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .h(WORKSPACE_TOPBAR_HEIGHT)
                    .pl_2()
                    .pr(rems(7.))
                    .children(tabs)
                    .drag_region(window, cx),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .children(self.surfaces.get(self.active).map(|s| s.panel.clone())),
            )
    }
}
