//! Port of upstream/apps/web/src/components/onboarding/FirstRunGate.tsx
//!
//! Holds back the app tree until the first-run decision is known, so a fresh
//! install never flashes the main screen before the wizard. Nothing renders
//! while pending: no shell, no dialogs.
//!
//! Decision order: a set `onboardingCompletedAt` resolves to the app as soon
//! as settings hydrate (the common case, no server round-trip). A `null` flag
//! also covers installs that predate the field, so it alone is not enough:
//! the gate waits for the shells to bootstrap and inspects the workspace. A
//! timeout shows recovery for an unreachable primary server without mounting
//! the app tree.
//!
//! Differences: upstream navigates to `/welcome` for the wizard, which
//! overlays the (empty) workspace; here the gate shows the app behind the
//! wizard dialog. The hosted-static variant is cut with the hosted app (its
//! decision function stays ported in `onboarding::first_run_logic`).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    AnyElement, AnyView, AppContext as _, Context, FontWeight, IntoElement, ParentElement as _,
    Render, Styled as _, Window, div, rems,
};

use super::backend::{FirstRunBackend, HydrationStatus, OnboardingBackend};
use super::welcome_wizard::open_welcome_wizard;
use crate::onboarding::first_run_logic::{
    FirstRunDecision, FirstRunDecisionInput, FirstRunGateEvent, FirstRunGateState,
    FirstRunWorkspaceInput, is_first_run_workspace_provenance_authoritative,
    is_fresh_first_run_workspace, resolve_first_run_decision, transition_first_run_gate_state,
};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_LG, TEXT_SM};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

const FIRST_RUN_DECISION_TIMEOUT_MS: u64 = 4_000;

pub struct FirstRunGate {
    backend: Rc<dyn FirstRunBackend>,
    wizard_backend: Rc<dyn OnboardingBackend>,
    enabled: bool,
    /// The shell the gate wraps; kept for dev hooks that target the layout directly.
    layout: gpui_kit::Entity<crate::components::app_sidebar_layout::AppSidebarLayout>,
    child: AnyView,
    state: FirstRunGateState,
    timer_started: bool,
    completion_requested: bool,
    wizard_opened: bool,
}

impl FirstRunGate {
    pub fn new(
        backend: Rc<dyn FirstRunBackend>,
        wizard_backend: Rc<dyn OnboardingBackend>,
        enabled: bool,
        layout: gpui_kit::Entity<crate::components::app_sidebar_layout::AppSidebarLayout>,
    ) -> Self {
        let evidence = backend.evidence();
        // Within a session settings stay hydrated, so remounts resolve synchronously.
        let mut decision =
            if !enabled || (evidence.hydration == HydrationStatus::Ready && evidence.completed) {
                FirstRunDecision::App
            } else {
                FirstRunDecision::Pending
            };
        let mut stalled = false;
        if std::env::var("TURIN_SHOT").is_ok() {
            match std::env::var("TURIN_FIRST_RUN").as_deref() {
                Ok("stalled") => {
                    decision = FirstRunDecision::Pending;
                    stalled = true;
                }
                Ok("fresh") => decision = FirstRunDecision::Wizard,
                _ => {}
            }
        }
        Self {
            backend,
            wizard_backend,
            enabled,
            layout: layout.clone(),
            child: layout.into(),
            state: FirstRunGateState { decision, stalled },
            timer_started: false,
            completion_requested: false,
            wizard_opened: false,
        }
    }

    pub fn layout(
        &self,
    ) -> &gpui_kit::Entity<crate::components::app_sidebar_layout::AppSidebarLayout> {
        &self.layout
    }

    fn resolve(&self) -> (crate::onboarding::first_run_logic::FirstRunResolution, bool) {
        let evidence = self.backend.evidence();
        let hydrated = evidence.hydration == HydrationStatus::Ready;
        // A workspace still counts as fresh when its only content is the server's
        // own cwd auto-bootstrap; any other project, more than one thread, or
        // state in a non-primary environment is real user state.
        let workspace_fresh = is_fresh_first_run_workspace(&FirstRunWorkspaceInput {
            primary_environment_id: evidence.primary_environment_id.clone(),
            server_cwd: evidence.server_cwd.clone(),
            bootstrap_project_id: evidence.bootstrap_project_id.clone(),
            bootstrap_thread_id: evidence.bootstrap_thread_id.clone(),
            bootstrap_project_created: evidence.bootstrap_project_created,
            bootstrap_thread_created: evidence.bootstrap_thread_created,
            projects: evidence.projects.clone(),
            threads: evidence.threads.clone(),
        });
        let resolution = resolve_first_run_decision(&FirstRunDecisionInput {
            enabled: self.enabled,
            hydrated,
            completed: evidence.completed,
            bootstrapped: evidence.bootstrapped,
            authoritative: evidence.primary_shell_live,
            workspace_authoritative: evidence.workspace_evidence_live,
            workspace_provenance_authoritative: is_first_run_workspace_provenance_authoritative(
                evidence.welcome_received,
                evidence.bootstrap_status,
            ),
            catalog_ready: evidence.catalog_ready,
            server_config_available: evidence.server_config_available,
            workspace_fresh,
            project_count: evidence.projects.len(),
            thread_count: evidence.threads.len(),
        });
        (resolution, hydrated)
    }

    /// The effects upstream runs in `useEffect`.
    fn run_effects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (resolution, hydrated) = self.resolve();
        let evidence = self.backend.evidence();
        if self.state.decision != FirstRunDecision::Wizard && hydrated {
            if resolution.persist_completion && !evidence.completed && !self.completion_requested {
                self.completion_requested = true;
                self.backend.complete_onboarding(cx).detach();
            }
            let next = transition_first_run_gate_state(
                self.state,
                FirstRunGateEvent::Evidence {
                    decision: resolution.decision,
                },
            );
            if next != self.state {
                self.state = next;
                cx.notify();
            }
        }
        // A stalled server read gets a recovery screen, but never mounts the app.
        // The timer starts after settings hydrate so slow local hydration does
        // not show a false connection failure.
        if self.enabled
            && self.state.decision == FirstRunDecision::Pending
            && hydrated
            && !self.timer_started
        {
            self.timer_started = true;
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(FIRST_RUN_DECISION_TIMEOUT_MS))
                    .await;
                this.update_in(cx, |this, _, cx| {
                    this.state =
                        transition_first_run_gate_state(this.state, FirstRunGateEvent::Timeout);
                    this.timer_started = false;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if self.state.decision == FirstRunDecision::Wizard && !self.wizard_opened {
            self.wizard_opened = true;
            let backend = self.wizard_backend.clone();
            let gate = cx.entity().downgrade();
            // Deferred, not async: the gate must finish this frame before the dialog opens.
            window.defer(cx, move |window, cx| {
                open_welcome_wizard(backend, window, cx, move |landing, window, cx| {
                    gate.update(cx, |gate, cx| {
                        gate.state = FirstRunGateState {
                            decision: FirstRunDecision::App,
                            stalled: false,
                        };
                        // `openNewThread(projectRef)`, else stay on `/`.
                        if let Some(project_id) = landing {
                            gate.layout.update(cx, |layout, cx| {
                                layout.open_new_thread_in_project(&project_id, window, cx)
                            });
                        }
                        cx.notify();
                    })
                    .ok();
                });
            });
        }
    }

    fn recovery(&self, settings: bool, retrying: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let backend = self.backend.clone();
        let gate = cx.entity().downgrade();
        div()
            .flex()
            .flex_1()
            .size_full()
            .items_center()
            .justify_center()
            .px_6()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .max_w(rems(24.))
                    .text_center()
                    .child(
                        div()
                            .text_size(TEXT_LG.size)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if settings {
                                "Could not read settings"
                            } else {
                                "Still connecting"
                            }),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(TEXT_SM.size)
                            .text_color(theme.muted_foreground)
                            .child(if settings {
                                "Your saved settings could not be loaded."
                            } else {
                                "Turin could not confirm this workspace."
                            }),
                    )
                    .child(
                        div().mt(rems(1.25)).child(
                            Button::new("first-run-recovery")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .disabled(retrying)
                                .child(Icon::new(IconName::RefreshCw).size(rems(0.875)))
                                .child(if settings { "Retry" } else { "Reload" })
                                .on_click(move |_, _, cx| {
                                    let task = if settings {
                                        backend.retry_settings(cx)
                                    } else {
                                        // A reload re-enters the connecting state.
                                        gate.update(cx, |gate, cx| {
                                            gate.state.stalled = false;
                                            gate.timer_started = false;
                                            cx.notify();
                                        })
                                        .ok();
                                        backend.reload(cx)
                                    };
                                    // The evidence changed outside the gate's entity: re-render.
                                    let gate = gate.clone();
                                    cx.spawn(async move |cx| {
                                        task.await;
                                        gate.update(cx, |_, cx| cx.notify()).ok();
                                    })
                                    .detach();
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for FirstRunGate {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.run_effects(window, cx);
        let evidence = self.backend.evidence();
        let settings_read_failed = matches!(
            evidence.hydration,
            HydrationStatus::Failed | HydrationStatus::Retrying
        );
        if settings_read_failed {
            return self.recovery(true, evidence.hydration == HydrationStatus::Retrying, cx);
        }
        match self.state.decision {
            FirstRunDecision::App | FirstRunDecision::Wizard => div()
                .size_full()
                .child(self.child.clone())
                .into_any_element(),
            FirstRunDecision::Pending if self.state.stalled => self.recovery(false, false, cx),
            FirstRunDecision::Pending => div().size_full().into_any_element(),
        }
    }
}

/// Wrap `child` in a gate entity.
pub fn first_run_gate(
    backend: Rc<dyn FirstRunBackend>,
    wizard_backend: Rc<dyn OnboardingBackend>,
    enabled: bool,
    layout: gpui_kit::Entity<crate::components::app_sidebar_layout::AppSidebarLayout>,
    cx: &mut gpui_kit::App,
) -> gpui_kit::Entity<FirstRunGate> {
    cx.new(|_| FirstRunGate::new(backend, wizard_backend, enabled, layout))
}
