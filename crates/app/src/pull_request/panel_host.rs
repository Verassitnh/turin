//! The right-panel body the pull request page shows (upstream `RightPanelTabs` with
//! `PullRequestDetailPanel` as the active surface): the 52px tab strip with one tab per opened
//! pull request, then the active detail panel.
//!
//! Not ported: tab drag / close menus, the add-surface button and the maximize control (the
//! shell owns the panel width and layout controls).

use crate::theme::color::solid_over;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;

use super::detail_context::DetailTab;
use super::detail_panel::PullRequestDetailPanel;
use super::guide_run_coordinator::{GuideRunCoordinator, GuideRunCoordinatorEvent};
use super::guide_run_store::GuideRunStore;
use super::presentation_view::PullRequestStateGlyph;
use super::pull_request_detail_logic::{
    PullRequestDetailSnapshotRef, resolve_displayed_pull_request_detail,
};
use super::shared_summary::{newest_pull_request_summary, pull_request_detail_to_summary};
use super::toast::{ToastKind, ToastQueue, render_toasts};
use super::view_model::{PullRequestStore, pull_request_key};
use crate::components::drag_region::{DragRegion as _, no_drag};
use crate::hooks::use_open_panel_pull_request_url::pull_request_link_for_shortcut;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{RADIUS_MD, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};

struct OpenSurface {
    key: String,
    environment_id: EnvironmentId,
    reference: PullRequestRef,
    panel: Entity<PullRequestDetailPanel>,
}

pub struct PullRequestPanelHost {
    store: Entity<PullRequestStore>,
    surfaces: Vec<OpenSurface>,
    active: usize,
    /// A stack layer chosen inside a panel: opened on the next render, which has the window.
    pending_open: Option<(EnvironmentId, PullRequestRef)>,
    /// A guide a notice asked to open, on the Guide tab (`openRequestKey`).
    pending_guide: Option<(EnvironmentId, PullRequestRef)>,
    guide_runs: Entity<GuideRunCoordinator>,
    /// Said by the host itself (the copy shortcut).
    toasts: ToastQueue,
    _subscriptions: Vec<gpui_kit::Subscription>,
}

impl PullRequestPanelHost {
    pub fn new(store: Entity<PullRequestStore>, cx: &mut Context<Self>) -> Self {
        let guide_runs = cx.new(|cx| GuideRunCoordinator::new(store.clone(), cx));
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&guide_runs, |_, _, cx| cx.notify()),
            cx.subscribe(&guide_runs, |this, _, event, cx| {
                let GuideRunCoordinatorEvent::OpenGuide {
                    environment_id,
                    reference,
                } = event;
                this.pending_guide = Some((environment_id.clone(), reference.clone()));
                cx.notify();
            }),
        ];
        Self {
            store,
            surfaces: Vec::new(),
            active: 0,
            pending_open: None,
            pending_guide: None,
            guide_runs,
            toasts: ToastQueue::default(),
            _subscriptions: subscriptions,
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
                let panel_environment = environment_id.clone();
                let panel = cx.new(|cx| {
                    PullRequestDetailPanel::new(
                        store,
                        panel_environment,
                        panel_reference,
                        window,
                        cx,
                    )
                });
                // A stack layer chosen in the panel opens as another tab of this host.
                let host = cx.entity().downgrade();
                let select_environment = environment_id.clone();
                panel.update(cx, |panel, cx| {
                    panel.set_select_pull_request(
                        std::rc::Rc::new(move |reference, cx| {
                            let reference = reference.clone();
                            let environment_id = select_environment.clone();
                            host.update(cx, |host, cx| {
                                host.pending_open = Some((environment_id, reference));
                                cx.notify();
                            })
                            .ok();
                        }),
                        cx,
                    )
                });
                self.surfaces.push(OpenSurface {
                    key,
                    environment_id,
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

    /// The open panel's pull request link, in `useOpenPanelPullRequestUrl`'s three states.
    fn open_panel_url(&self, cx: &App) -> Option<Option<String>> {
        let surface = self.surfaces.get(self.active)?;
        let live = self
            .store
            .read(cx)
            .detail(&surface.reference)
            .and_then(|snapshot| snapshot.detail.ready())
            .map(super::detail_panel::core_detail);
        let snapshot_ref = PullRequestDetailSnapshotRef {
            host: surface.reference.host.as_ref().map(|host| host.as_str()),
            project_id: surface.reference.project_id.as_str(),
            repository: surface.reference.repository.as_str(),
            number: surface.reference.number.get(),
        };
        Some(
            resolve_displayed_pull_request_detail(live.as_ref(), None, snapshot_ref)
                .map(|detail| detail.url.as_str().to_owned()),
        )
    }

    /// `copyPullRequestFromShortcut`: the pull requests route copies the open pull request's link.
    fn copy_pull_request_link(&mut self, cx: &mut Context<Self>) {
        let open = self.open_panel_url(cx);
        let Some(url) = pull_request_link_for_shortcut(open.as_ref()).map(str::to_owned) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
        self.toasts.push(
            cx,
            |host| &mut host.toasts,
            ToastKind::Success,
            "PR link copied",
            Some(url),
        );
    }

    /// The shortcut is window-level while the host is on screen: this page has no chat view to
    /// hold it.
    fn shortcut_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let host = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let host = host.clone();
                window.on_key_event(move |event: &KeyDownEvent, phase, _, cx| {
                    let modifiers = event.keystroke.modifiers;
                    if phase != DispatchPhase::Capture
                        || event.is_held
                        || !(modifiers.platform && modifiers.shift && !modifiers.alt)
                        || event.keystroke.key != "c"
                    {
                        return;
                    }
                    host.update(cx, |host, cx| {
                        if matches!(host.open_panel_url(cx), Some(Some(_))) {
                            cx.stop_propagation();
                            host.copy_pull_request_link(cx);
                        }
                    })
                    .ok();
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element()
    }

    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        self.surfaces.clear();
        self.active = 0;
        cx.notify();
    }
}

impl Render for PullRequestPanelHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((environment_id, reference)) = self.pending_open.take() {
            self.open(environment_id, reference, None, window, cx);
        }
        if let Some((environment_id, reference)) = self.pending_guide.take() {
            self.open(
                environment_id,
                reference,
                Some(DetailTab::Guide),
                window,
                cx,
            );
            // The request is answered once the panel is on the Guide tab.
            GuideRunStore::global(cx).update(cx, |runs, cx| runs.request_open(None, cx));
        }
        let theme = cx.app_theme().clone();
        let tabs: Vec<_> = self
            .surfaces
            .iter()
            .enumerate()
            .map(|(index, surface)| {
                let active = index == self.active;
                // The compact tab shows lifecycle and draft state only, from the newest of the
                // detail read and what the rest of the app saw (`RightPanelTabs`).
                let store = self.store.read(cx);
                let detail = store
                    .detail(&surface.reference)
                    .and_then(|snapshot| snapshot.detail.ready())
                    .map(|view| {
                        pull_request_detail_to_summary(&super::detail_panel::core_detail(view))
                    });
                let shared =
                    store.shared_summary(&surface.environment_id, &surface.reference, None);
                let status = newest_pull_request_summary(detail.as_ref(), shared.as_ref());
                let (state, is_draft) = match &status {
                    Some(summary) => (summary.state, summary.is_draft.unwrap_or(false)),
                    None => (turin_contracts::pull_request::PullRequestState::Open, false),
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
                            .bg(solid_over(theme.accent, 0.6, theme.background))
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
            .child(self.shortcut_listener(cx))
            .children(render_toasts(self.guide_runs.read(cx).toasts(), cx))
            .children(render_toasts(&self.toasts, cx))
    }
}

/// The thread view's right panel with the "Pull requests" surface active: the strip with its one
/// tab, then the linked pull requests of the thread (upstream `RightPanelTabs` with
/// `ThreadPullRequestsPanel`).
pub struct ThreadPanelHost {
    panel: Entity<super::thread_panel::ThreadPullRequestsPanel>,
}

impl ThreadPanelHost {
    pub fn new(
        store: Entity<PullRequestStore>,
        thread_id: turin_contracts::ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let panel = cx.new(|cx| {
            super::thread_panel::ThreadPullRequestsPanel::new(store, thread_id, window, cx)
        });
        Self { panel }
    }
}

impl ThreadPanelHost {
    pub fn panel(&self) -> &Entity<super::thread_panel::ThreadPullRequestsPanel> {
        &self.panel
    }
}

impl Render for ThreadPanelHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let tab = div()
            .id("pr-thread-surface")
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
            .bg(theme.accent)
            .text_color(theme.foreground)
            .text_size(TEXT_XS.size)
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .size(rems(1.))
                    .items_center()
                    .justify_center()
                    .child(crate::ui::Icon::new(crate::ui::IconName::Link2).size(rems(0.75))),
            )
            .child(div().min_w_0().truncate().child("Pull requests"));
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(theme.background)
            .child(
                div()
                    .id("pr-thread-tab-bar")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .h(WORKSPACE_TOPBAR_HEIGHT)
                    .pl_2()
                    .pr(rems(7.))
                    .child(tab)
                    .drag_region(window, cx),
            )
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
    }
}
