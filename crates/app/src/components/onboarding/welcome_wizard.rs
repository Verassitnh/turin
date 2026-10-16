//! Port of upstream/apps/web/src/components/onboarding/WelcomeWizard.tsx
//!
//! First-run welcome wizard, opened over the workspace on a fresh install.
//! Every step is skippable; the whole wizard is re-runnable by clearing the
//! completed flag.
//!
//! Kept steps: **Agents** (provider readiness) and **Projects** (import).
//! Differences from upstream, all logged in PORTING.md:
//!
//! - The "Connect your computers" step is cut with T3 Connect, pairing and
//!   multi-computer setup; Turin has one local environment.
//! - The install/sign-in terminal drawer is cut with the terminal feature; the
//!   card shows the command to run in the user's own terminal, with a copy
//!   button.
//! - Codex shows the plain agent card; the managed ChatGPT account flow
//!   (`CodexSetupSection`) belongs to Settings.
//! - The Projects step also offers to import a T3 Code database
//!   (`turin_persistence::T3Import`), driven through `OnboardingBackend`.
//! - Identity reads "Turin" instead of the T3 wordmark and "Code".

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, CursorStyle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};
use turin_contracts::agent_sessions::{
    AgentSessionProjectCandidate, AgentSessionScanResult, AgentSessionSource,
};
use turin_contracts::{EnvironmentId, ProjectId};
use turin_persistence::{ImportReport, T3ImportError};

use super::backend::{CreateProjectInput, OnboardingBackend, T3DatabaseCandidate};
use crate::components::provider_instance_icon::provider_instance_icon;
use crate::onboarding::project_import_logic::{
    CandidateGit, OnboardingCandidate, group_onboarding_projects, onboarding_project_key,
    partition_onboarding_projects, resolve_onboarding_landing_project,
    resolve_onboarding_project_id, split_onboarding_project_path,
};
use crate::onboarding::provider_readiness_logic::{
    OnboardingInstallDriver, OnboardingProvider, OnboardingProviderState,
    get_onboarding_provider_state, resolve_onboarding_provider_install_command,
    resolve_onboarding_provider_login_command, select_onboarding_providers_by_driver,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_2XL, RADIUS_LG, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::timestamp_format::{format_relative_time, now_ms};
use crate::ui::checkbox::{CheckState, Checkbox};
use crate::ui::dialog::{DialogOptions, dialog_border, open_dialog};
use crate::ui::spinner::{Spinner, SpinnerSize, SpinnerTone};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

const ONBOARDING_STAGES: [&str; 2] = ["Agents", "Projects"];
const SCAN_LIMIT_MESSAGE: &str =
    "Scan limit reached. Some projects or conversations may be missing.";
/// The two agents the wizard offers.
const PRIMARY_AGENT_DRIVERS: [&str; 2] = ["codex", "claudeAgent"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Agents,
    Import,
}

#[derive(Clone, Debug)]
enum T3State {
    Idle,
    Importing,
    Done(String),
    Failed(String),
}

#[derive(Clone)]
struct Candidate {
    key: String,
    inner: AgentSessionProjectCandidate,
}

impl OnboardingCandidate for Candidate {
    fn path(&self) -> &str {
        self.inner.path()
    }
    fn title(&self) -> &str {
        self.inner.title()
    }
    fn git(&self) -> CandidateGit<'_> {
        self.inner.git()
    }
    fn thread_count(&self) -> u64 {
        self.inner.thread_count()
    }
    fn last_active_at(&self) -> Option<&str> {
        self.inner.last_active_at()
    }
    fn project_id(&self) -> Option<&ProjectId> {
        self.inner.project_id()
    }
}

struct ScanState {
    pending: bool,
    data: Option<AgentSessionScanResult>,
    error: Option<String>,
}

pub struct WelcomeWizard {
    backend: Rc<dyn OnboardingBackend>,
    on_done: Rc<dyn Fn(&mut App)>,
    step: Step,
    is_importing: bool,
    finishing: bool,
    /// Provider command shown in place of upstream's setup terminal.
    command_panel: Option<(String, String)>,
    scan: ScanState,
    scan_started: bool,
    selected_keys: Option<HashSet<String>>,
    collapsed_groups: HashSet<String>,
    other_open: bool,
    imported_projects: HashMap<String, ProjectId>,
    projects_with_imported_history: HashMap<String, ProjectId>,
    project_attempts: HashMap<String, ProjectId>,
    last_import_selection: Vec<String>,
    import_warning: String,
    imported_thread_count: u64,
    import_error: String,
    generation: u64,
    t3: T3State,
}

/// Opens the wizard as a modal that cannot be dismissed (`disablePointerDismissal`).
pub fn open_welcome_wizard(
    backend: Rc<dyn OnboardingBackend>,
    window: &mut Window,
    cx: &mut App,
    on_done: impl Fn(&mut App) + 'static,
) {
    let wizard = cx.new(|cx| WelcomeWizard::new(backend, Rc::new(on_done), window, cx));
    open_dialog(
        window,
        cx,
        DialogOptions {
            max_width: rems(48.),
            dismissable: false,
            show_close_button: false,
        },
        wizard,
    );
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl WelcomeWizard {
    pub fn new(
        backend: Rc<dyn OnboardingBackend>,
        on_done: Rc<dyn Fn(&mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut wizard = Self {
            backend,
            on_done,
            step: Step::Agents,
            is_importing: false,
            finishing: false,
            command_panel: None,
            scan: ScanState {
                pending: false,
                data: None,
                error: None,
            },
            scan_started: false,
            selected_keys: None,
            collapsed_groups: HashSet::new(),
            other_open: false,
            imported_projects: HashMap::new(),
            projects_with_imported_history: HashMap::new(),
            project_attempts: HashMap::new(),
            last_import_selection: Vec::new(),
            import_warning: String::new(),
            imported_thread_count: 0,
            import_error: String::new(),
            generation: 0,
            t3: T3State::Idle,
        };
        // Re-probe on entry so freshly installed CLIs show up without a manual
        // refresh; harmless when nothing changed (single-flighted).
        wizard.refresh_providers(window, cx);
        match std::env::var("TURIN_WIZARD_STEP").as_deref() {
            Ok("import") => wizard.go_to_import(window, cx),
            Ok("command") => {
                wizard.command_panel =
                    Some(("claudeAgent".to_owned(), "claude auth login".to_owned()));
            }
            _ => {}
        }
        wizard
    }

    fn refresh_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.backend.refresh_providers(cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await;
            this.update_in(cx, |_, _, cx| cx.notify()).ok();
        })
        .detach();
    }

    fn go_to_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Import;
        if !self.scan_started {
            self.scan_started = true;
            self.scan.pending = true;
            let task = self.backend.scan_projects(cx);
            cx.spawn_in(window, async move |this, cx| {
                let result = task.await;
                this.update_in(cx, |this, _, cx| {
                    this.scan.pending = false;
                    match result {
                        Ok(data) => this.scan.data = Some(data),
                        Err(error) => this.scan.error = Some(error),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn rescan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scan_started = false;
        self.scan.error = None;
        self.go_to_import(window, cx);
    }

    /// `finish`: persist completion, then hand back to the app with a toast.
    fn finish(
        &mut self,
        landing: Option<ProjectId>,
        warning: String,
        imported_threads: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.finishing {
            return;
        }
        self.finishing = true;
        let task = self.backend.complete_onboarding(cx);
        let on_done = self.on_done.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.finishing = false;
                match result {
                    Ok(()) => {
                        window.close_dialog(cx);
                        on_done(cx);
                        let _ = landing;
                        if !warning.is_empty() {
                            window.push_notification(
                                Notification::warning(warning)
                                    .title("Some history was not imported")
                                    .autohide(false),
                                cx,
                            );
                        } else if imported_threads > 0 {
                            window.push_notification(
                                Notification::success(String::new()).title(format!(
                                    "Imported {}",
                                    plural(imported_threads, "thread", "threads")
                                )),
                                cx,
                            );
                        }
                    }
                    Err(()) => window.push_notification(
                        Notification::error("Your settings could not be saved. Try again.")
                            .title("Could not finish setup"),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn candidates(&self) -> Vec<Candidate> {
        let environment = self.backend.environment_id();
        self.scan
            .data
            .iter()
            .flat_map(|data| data.candidates.iter())
            .map(|candidate| Candidate {
                key: onboarding_project_key(&environment, candidate.path.as_str()),
                inner: candidate.clone(),
            })
            .collect()
    }

    fn selected_key_set(&self, recent: &[Candidate]) -> HashSet<String> {
        self.selected_keys.clone().unwrap_or_else(|| {
            recent
                .iter()
                .map(|candidate| candidate.key.clone())
                .collect()
        })
    }

    fn finish_after_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let landing = resolve_onboarding_landing_project(
            &self.last_import_selection,
            &self.projects_with_imported_history,
            &self.imported_projects,
        );
        let warning = self.import_warning.clone();
        let count = self.imported_thread_count;
        self.finish(landing, warning, count, window, cx);
    }

    /// `runImport`: create each selected project (once), then import its history.
    fn run_import(
        &mut self,
        selection: Vec<Candidate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_importing {
            return;
        }
        if selection.is_empty() {
            self.finish(None, String::new(), 0, window, cx);
            return;
        }
        self.is_importing = true;
        self.import_warning.clear();
        self.imported_thread_count = 0;
        self.last_import_selection = selection.iter().map(|c| c.key.clone()).collect();
        self.import_error.clear();
        self.generation += 1;
        let generation = self.generation;
        cx.notify();
        let backend = self.backend.clone();
        let environment: EnvironmentId = backend.environment_id();
        cx.spawn_in(window, async move |this, cx| {
            // Interrupted imports are neither failures nor successes, but they
            // still did not land, so they must not read as "imported everything".
            // Retries skip paths that already landed this session.
            let mut imported_projects_count = this
                .read_with(cx, |this, _| {
                    selection
                        .iter()
                        .filter(|candidate| this.imported_projects.contains_key(&candidate.key))
                        .count()
                })
                .unwrap_or(0);
            let mut imported_thread_count = 0u64;
            let mut skipped_thread_count = 0u64;
            for candidate in &selection {
                let stale = this
                    .read_with(cx, |this, _| this.generation != generation)
                    .unwrap_or(true);
                if stale {
                    return;
                }
                let already = this
                    .read_with(cx, |this, _| {
                        this.imported_projects.contains_key(&candidate.key)
                    })
                    .unwrap_or(false);
                if already {
                    continue;
                }
                let known = backend.known_projects();
                let mut project_id = resolve_onboarding_project_id(&known, &environment, candidate);
                if project_id.is_none() {
                    let attempt = this
                        .update(cx, |this, _| {
                            this.project_attempts
                                .entry(candidate.key.clone())
                                .or_insert_with(ProjectId::random)
                                .clone()
                        })
                        .ok();
                    let Some(attempt) = attempt else { return };
                    project_id = Some(attempt.clone());
                    let task = cx
                        .update(|_, app| {
                            backend.create_project(
                                CreateProjectInput {
                                    project_id: attempt,
                                    title: candidate.title().to_owned(),
                                    workspace_root: candidate.path().to_owned(),
                                },
                                app,
                            )
                        })
                        .ok();
                    let Some(task) = task else { return };
                    let created = task.await;
                    let stale = this
                        .read_with(cx, |this, _| this.generation != generation)
                        .unwrap_or(true);
                    if stale {
                        return;
                    }
                    if created.is_err() {
                        this.update(cx, |this, _| {
                            this.project_attempts.remove(&candidate.key);
                        })
                        .ok();
                        continue;
                    }
                }
                let Some(project_id) = project_id else {
                    continue;
                };
                let task = cx
                    .update(|_, app| {
                        backend.import_agent_sessions(
                            project_id.clone(),
                            candidate.path().to_owned(),
                            app,
                        )
                    })
                    .ok();
                let Some(task) = task else { return };
                let result = task.await;
                let stale = this
                    .read_with(cx, |this, _| this.generation != generation)
                    .unwrap_or(true);
                if stale {
                    return;
                }
                match result {
                    Ok(counts) => {
                        imported_thread_count += counts.imported;
                        skipped_thread_count += counts.skipped;
                        let key = candidate.key.clone();
                        let id = project_id.clone();
                        this.update(cx, |this, _| {
                            if counts.imported > 0 {
                                this.projects_with_imported_history
                                    .insert(key.clone(), id.clone());
                            }
                            if counts.skipped == 0 {
                                this.imported_projects.insert(key, id);
                            }
                        })
                        .ok();
                        if counts.skipped == 0 {
                            imported_projects_count += 1;
                        }
                    }
                    Err(_) => {
                        this.update(cx, |this, _| {
                            this.project_attempts.remove(&candidate.key);
                        })
                        .ok();
                    }
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.is_importing = false;
                this.imported_thread_count = imported_thread_count;
                if imported_projects_count < selection.len() {
                    this.import_warning = if imported_thread_count > 0 && skipped_thread_count > 0 {
                        format!(
                            "Imported {}. {} could not be imported.",
                            plural(imported_thread_count, "thread", "threads"),
                            plural(skipped_thread_count, "thread", "threads")
                        )
                    } else if skipped_thread_count > 0 {
                        format!(
                            "{} not be imported.",
                            plural(skipped_thread_count, "thread could", "threads could")
                        )
                    } else if imported_thread_count > 0 {
                        format!(
                            "Imported {}. Some thread history could not be imported.",
                            plural(imported_thread_count, "thread", "threads")
                        )
                    } else {
                        "Could not import thread history.".to_owned()
                    };
                }
                this.finish_after_import(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn import_t3(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.t3, T3State::Importing) {
            return;
        }
        self.t3 = T3State::Importing;
        cx.notify();
        let task = self.backend.import_t3_database(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<ImportReport, T3ImportError> = task.await;
            this.update_in(cx, |this, _, cx| {
                this.t3 = match result {
                    Ok(report) => T3State::Done(format!(
                        "Imported {} and {}.",
                        plural(report.active_projects.max(0) as u64, "project", "projects"),
                        plural(report.active_threads.max(0) as u64, "thread", "threads"),
                    )),
                    Err(error) => T3State::Failed(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

// ── Pieces ──────────────────────────────────────────────────────────────────

fn step_shell(title: &str, description: Option<&str>, theme: &AppTheme) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(rems(1.5))
                .line_height(rems(2.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(title.to_owned()),
        )
        .children(description.map(|description| {
            div()
                .mt(rems(0.625))
                .text_size(TEXT_SM.size)
                .line_height(rems(1.4))
                .text_color(theme.muted_foreground)
                .child(description.to_owned())
        }))
}

/// `WizardSteps`.
fn wizard_steps(
    current: usize,
    is_disabled: impl Fn(usize) -> bool,
    on_change: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    theme: &AppTheme,
) -> AnyElement {
    let dark = theme.is_dark();
    let ring = |alpha: f32| -> Hsla {
        if dark {
            with_alpha(tokens::tw::WHITE, alpha).into()
        } else {
            with_alpha(tokens::tw::BLACK, alpha).into()
        }
    };
    div()
        .flex()
        .gap_1()
        .rounded(tokens::RADIUS_XL)
        .bg(if dark {
            with_alpha(tokens::tw::WHITE, 0.04).into()
        } else {
            theme.background
        })
        .border_1()
        .border_color(ring(0.05))
        .p_1()
        .children(ONBOARDING_STAGES.iter().enumerate().map(|(index, label)| {
            let disabled = is_disabled(index);
            let is_current = index == current;
            let on_change = on_change.clone();
            let circle = div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(rems(1.25))
                .rounded_full()
                .border_1()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .map_circle(index, current, theme, dark);
            div()
                .id(SharedString::from(format!("wizard-step:{index}")))
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .rounded(RADIUS_LG)
                .px(rems(0.625))
                .py_2()
                .when(!disabled, |this| {
                    let hover = theme.card;
                    this.cursor(CursorStyle::PointingHand)
                        .hover(move |style| style.bg(hover))
                })
                .when(is_current, |this| {
                    this.bg(theme.card)
                        .border_1()
                        .border_color(ring(0.05))
                        .when(!dark, |this| this.shadow(tokens::shadow_xs(0.05)))
                })
                .child(circle.child(if index < current {
                    Icon::new(IconName::Check).size(rems(1.)).into_any_element()
                } else {
                    (index + 1).to_string().into_any_element()
                }))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(TEXT_SM.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if is_current {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(*label),
                )
                .when(!disabled, |this| {
                    this.on_click(move |_, window, cx| on_change(index, window, cx))
                })
        }))
        .into_any_element()
}

trait CircleStyle {
    fn map_circle(self, index: usize, current: usize, theme: &AppTheme, dark: bool) -> Self;
}

impl CircleStyle for gpui_kit::Div {
    fn map_circle(self, index: usize, current: usize, theme: &AppTheme, dark: bool) -> Self {
        if index < current {
            self.bg(theme.primary)
                .border_color(theme.primary)
                .text_color(theme.primary_foreground)
        } else if index == current {
            self.bg(with_alpha(theme.primary, 0.1))
                .border_color(with_alpha(theme.primary, 0.3))
                .text_color(theme.primary)
        } else {
            self.bg(theme.card)
                .border_color(if dark {
                    with_alpha(tokens::tw::WHITE, 0.1)
                } else {
                    with_alpha(tokens::tw::BLACK, 0.1)
                })
                .text_color(theme.muted_foreground)
        }
    }
}

impl WelcomeWizard {
    fn agent_card(
        &mut self,
        driver: &'static str,
        provider: Option<&OnboardingProvider>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let fallback = if driver == "claudeAgent" {
            "Claude Code"
        } else {
            turin_contracts::model::provider_display_name(driver).unwrap_or(driver)
        };
        let display_name = provider
            .and_then(|provider| provider.display_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback.to_owned());
        let state = get_onboarding_provider_state(provider);
        let summary = provider.map(|p| p.summary.clone()).unwrap_or_default();
        let install = match driver {
            "claudeAgent" => OnboardingInstallDriver::ClaudeAgent,
            _ => OnboardingInstallDriver::Codex,
        };
        let command = match provider {
            Some(provider) if provider.installed => resolve_onboarding_provider_login_command(
                driver,
                self.backend.instance_config(&provider.instance_id).as_ref(),
                self.backend.platform(),
            ),
            _ => resolve_onboarding_provider_install_command(install, self.backend.platform())
                .to_owned(),
        };
        let terminal_open = self
            .command_panel
            .as_ref()
            .is_some_and(|(d, _)| d == driver);
        let detail = if state == OnboardingProviderState::Ready {
            "Ready to code.".to_owned()
        } else {
            format!(
                "{}{}",
                summary.headline,
                match (&summary.detail, state) {
                    (Some(detail), s) if s != OnboardingProviderState::Ready =>
                        format!(" · {detail}"),
                    _ => String::new(),
                }
            )
        };
        let action: AnyElement = match state {
            OnboardingProviderState::Ready => div()
                .flex()
                .items_center()
                .gap(rems(0.375))
                .text_size(TEXT_XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.success_foreground)
                .child(
                    Icon::new(IconName::Check)
                        .size(rems(0.875))
                        .color(theme.success_foreground),
                )
                .child("Ready")
                .into_any_element(),
            OnboardingProviderState::Checking => div()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child("Checking...")
                .into_any_element(),
            OnboardingProviderState::Disabled => div()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child("Disabled")
                .into_any_element(),
            OnboardingProviderState::Attention => div()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(summary.headline.clone())
                .into_any_element(),
            OnboardingProviderState::Install | OnboardingProviderState::SignIn => {
                let label = if state == OnboardingProviderState::SignIn {
                    "Sign in"
                } else {
                    "Install"
                };
                Button::new(SharedString::from(format!("agent-action:{driver}")))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Xs)
                    .disabled(terminal_open)
                    .child(Icon::new(IconName::Terminal).size(rems(0.875)))
                    .child(label)
                    .on_click({
                        let entity = entity.clone();
                        let command = command.clone();
                        move |_, _, cx| {
                            let command = command.clone();
                            entity.update(cx, |this, cx| {
                                this.command_panel = Some((driver.to_owned(), command));
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element()
            }
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .px_4()
            .py_4()
            .child(provider_instance_icon(
                driver,
                &display_name,
                rems(1.25),
                None,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(display_name),
                    )
                    .child(
                        div()
                            .mt(rems(0.125))
                            .text_size(TEXT_XS.size)
                            .line_height(rems(1.2))
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    ),
            )
            .child(div().flex_shrink_0().child(action))
            .into_any_element()
    }

    /// Replaces upstream's inline setup terminal: the command to run in the
    /// user's own terminal, with a copy button.
    fn command_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, command) = self.command_panel.clone()?;
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let copy_entity = entity.clone();
        let backend = self.backend.clone();
        let command_to_copy = command.clone();
        Some(
            div()
                .mt_4()
                .overflow_hidden()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(with_alpha(theme.border, 0.7))
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(with_alpha(theme.border, 0.6))
                        .bg(with_alpha(theme.background, 0.6))
                        .px_3()
                        .py(rems(0.375))
                        .child(
                            div()
                                .text_size(TEXT_2XS.size)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child("Run this in your terminal, then come back."),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Button::new("agent-command-copy")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::Xs)
                                        .icon(IconName::Copy)
                                        .label("Copy")
                                        .on_click(move |_, _, cx| {
                                            backend.copy_to_clipboard(&command_to_copy, cx);
                                            copy_entity.update(cx, |_, cx| cx.notify());
                                        }),
                                )
                                .child(
                                    Button::new("agent-command-close")
                                        .variant(ButtonVariant::GhostMuted)
                                        .size(ButtonSize::Xs)
                                        .label("Close")
                                        .on_click(move |_, window, cx| {
                                            entity.update(cx, |this, cx| {
                                                this.command_panel = None;
                                                this.refresh_providers(window, cx);
                                                cx.notify();
                                            });
                                        }),
                                ),
                        ),
                )
                .child(
                    div()
                        .px_3()
                        .py_3()
                        .font_family(theme.fonts.mono.clone())
                        .text_size(TEXT_XS.size)
                        .child(command),
                )
                .into_any_element(),
        )
    }

    fn agents_step(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let providers = self.backend.providers();
        let by_driver = select_onboarding_providers_by_driver(providers.as_deref());
        let cards: Vec<AnyElement> = PRIMARY_AGENT_DRIVERS
            .iter()
            .map(|driver| {
                let provider = by_driver.get(driver).copied();
                self.agent_card(driver, provider, cx)
            })
            .collect();
        let entity = cx.entity();
        let codex_ready = providers.as_ref().is_some_and(|providers| {
            providers.iter().any(|provider| {
                provider.driver == "codex"
                    && get_onboarding_provider_state(Some(provider))
                        == OnboardingProviderState::Ready
            })
        });
        let command_panel = self.command_panel(cx);
        step_shell(
            "Connect your agents",
            Some("Choose an agent to start coding. You can add more later."),
            &theme,
        )
        .child(
            div()
                .id("wizard-agents-scroll")
                .mt(rems(1.25))
                .max_h(rems(32.))
                .overflow_y_scroll()
                .child(
                    div().pr_3().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(1.25))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .mb_2()
                                            .text_size(TEXT_SM.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(self.backend.machine_label()),
                                    )
                                    .child(div().flex().flex_col().gap(rems(0.375)).children(cards))
                                    .when(codex_ready, |this| {
                                        let entity = entity.clone();
                                        this.child(
                                            div().mt_3().flex().child(
                                                Button::new("wizard-add-chatgpt")
                                                    .variant(ButtonVariant::GhostMuted)
                                                    .size(ButtonSize::Xs)
                                                    .label("Connect another ChatGPT account")
                                                    .on_click(move |_, window, cx| {
                                                        entity.update(cx, |_, _| {});
                                                        window.push_notification(
                                                            Notification::info(
                                                                "Add more ChatGPT accounts from Settings → Providers.",
                                                            ),
                                                            cx,
                                                        );
                                                    }),
                                            ),
                                        )
                                    })
                                    .children(command_panel),
                            ),
                    ),
                ),
        )
        .child(
            div().mt_6().flex().justify_end().child(
                Button::new("wizard-agents-continue")
                    .child("Continue")
                    .child(Icon::new(IconName::ArrowRight).size(rems(0.875)))
                    .on_click({
                        let entity = cx.entity();
                        move |_, window, cx| entity.update(cx, |this, cx| this.go_to_import(window, cx))
                    }),
            ),
        )
        .into_any_element()
    }
}

impl WelcomeWizard {
    fn source_icons(sources: &[AgentSessionSource], cx: &App) -> (AnyElement, AnyElement) {
        let slot = |present: bool, driver: &str, name: &str| -> AnyElement {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(rems(1.))
                .when(present, |this| {
                    this.child(provider_instance_icon(driver, name, rems(0.75), None, cx))
                })
                .into_any_element()
        };
        (
            slot(
                sources.contains(&AgentSessionSource::ClaudeAgent),
                "claudeAgent",
                "Claude Code",
            ),
            slot(
                sources.contains(&AgentSessionSource::Codex),
                "codex",
                "Codex",
            ),
        )
    }

    /// Trailing columns shared by every import row: source icons, thread count,
    /// last activity. Each column has a fixed width and each icon its own slot
    /// so nothing shifts between rows that differ in sources or digit count.
    fn row_meta(
        sources: Option<&[AgentSessionSource]>,
        thread_count: u64,
        last_active_at: Option<&str>,
        theme: &AppTheme,
        cx: &App,
    ) -> AnyElement {
        let age = last_active_at
            .and_then(|at| format_relative_time(at, now_ms()))
            // "just now" does not fit the fixed column, so collapse it.
            .map(|(value, suffix)| {
                if suffix.is_none() {
                    "now".to_owned()
                } else {
                    value
                }
            })
            .unwrap_or_default();
        let (claude, codex) = Self::source_icons(sources.unwrap_or(&[]), cx);
        div()
            .ml_auto()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_x_1()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(claude)
            .child(codex)
            .child(
                div()
                    .w(rems(2.5))
                    .text_right()
                    .child(thread_count.to_string()),
            )
            .child(
                div()
                    .w(rems(2.25))
                    .text_right()
                    .whitespace_nowrap()
                    .child(age),
            )
            .into_any_element()
    }

    fn path_text(path: &str, small_muted: bool, theme: &AppTheme) -> AnyElement {
        let split = split_onboarding_project_path(path);
        div()
            .flex()
            .min_w_0()
            .items_baseline()
            .font_family(theme.fonts.mono.clone())
            .text_size(if small_muted {
                TEXT_2XS.size
            } else {
                TEXT_XS.size
            })
            .when(small_muted, |this| this.text_color(theme.muted_foreground))
            .when(!split.parent.is_empty(), |this| {
                this.child(div().min_w_0().truncate().child(split.parent.clone()))
            })
            .child(
                div()
                    .flex_shrink_0()
                    .max_w_full()
                    .truncate()
                    .child(split.name),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn candidate_row(
        &self,
        candidate: &Candidate,
        label: Option<&str>,
        nested: bool,
        checked: bool,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let key = candidate.key.clone();
        let disabled = self.is_importing;
        let toggle = {
            let entity = entity.clone();
            let key = key.clone();
            move |next: bool, _: &mut Window, cx: &mut App| {
                let key = key.clone();
                entity.update(cx, |this, cx| this.set_keys(&[key], next, cx));
            }
        };
        let hover = with_alpha(theme.muted, 0.4);
        div()
            .id(SharedString::from(format!("import-row:{key}")))
            .flex()
            .items_center()
            .gap(rems(0.625))
            .rounded(px(6.))
            .px_2()
            .py(rems(0.375))
            .when(nested, |this| this.pl_8())
            .when(!disabled, |this| {
                let toggle = toggle.clone();
                this.cursor(CursorStyle::PointingHand)
                    .hover(move |style| style.bg(hover))
                    .on_click(move |_, window, cx| toggle(!checked, window, cx))
            })
            .child(
                Checkbox::new(SharedString::from(format!("import-check:{key}")))
                    .checked(checked)
                    .disabled(disabled)
                    .on_change(toggle),
            )
            .child(
                div()
                    .id(SharedString::from(format!("import-path:{key}")))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .gap_2()
                    .overflow_hidden()
                    .children(label.map(|label| {
                        div()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child(label.to_owned())
                    }))
                    .child(Self::path_text(candidate.path(), label.is_some(), theme))
                    .tooltip({
                        let path = candidate.path().to_owned();
                        move |window, cx| {
                            Tooltip::new(path.clone())
                                .variant(crate::ui::TooltipVariant::Code)
                                .build(window, cx)
                        }
                    }),
            )
            .child(Self::row_meta(
                if nested {
                    None
                } else {
                    Some(&candidate.inner.sources)
                },
                candidate.thread_count(),
                candidate.last_active_at(),
                theme,
                cx,
            ))
            .into_any_element()
    }

    fn set_keys(&mut self, keys: &[String], checked: bool, cx: &mut Context<Self>) {
        let recent: Vec<Candidate> = {
            let candidates = self.candidates();
            partition_onboarding_projects(&candidates, now_ms()).recent
        };
        let mut next = self.selected_key_set(&recent);
        for key in keys {
            if checked {
                next.insert(key.clone());
            } else {
                next.remove(key);
            }
        }
        self.selected_keys = Some(next);
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn group_header(
        &self,
        id: &str,
        label: &str,
        open: bool,
        state: CheckState,
        keys: Vec<String>,
        trailing: AnyElement,
        on_toggle: impl Fn(&mut WelcomeWizard, &mut Context<WelcomeWizard>) + 'static,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let toggle_entity = entity.clone();
        let hover = with_alpha(theme.muted, 0.4);
        let disabled = self.is_importing;
        let check_entity = entity.clone();
        div()
            .flex()
            .items_center()
            .gap(rems(0.625))
            .rounded(px(6.))
            .px_2()
            .py(rems(0.375))
            .hover(move |style| style.bg(hover))
            .child(
                Checkbox::new(SharedString::from(format!("import-group-check:{id}")))
                    .state(state)
                    .disabled(disabled)
                    .on_change(move |next, _, cx| {
                        let keys = keys.clone();
                        check_entity.update(cx, |this, cx| this.set_keys(&keys, next, cx));
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("import-group:{id}")))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .cursor(CursorStyle::PointingHand)
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(rems(0.875))
                        .color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .when(id != "other", |this| this.font_weight(FontWeight::MEDIUM))
                            .when(id == "other", |this| {
                                this.text_color(theme.muted_foreground)
                            })
                            .child(label.to_owned()),
                    )
                    .child(trailing)
                    .on_click(move |_, _, cx| {
                        toggle_entity.update(cx, |this, cx| {
                            on_toggle(this, cx);
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// Repositories first, newest activity on top. Clones of one repository
    /// share a group with a tri-state checkbox. Folders that are not git
    /// repositories sit collapsed at the bottom so they stay reachable without
    /// adding noise.
    fn candidate_list(
        &self,
        candidates: &[Candidate],
        selected: &HashSet<String>,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let grouped = group_onboarding_projects(candidates);
        let tri = |keys: &[String]| -> CheckState {
            let count = keys.iter().filter(|key| selected.contains(*key)).count();
            if count == keys.len() {
                CheckState::Checked
            } else if count > 0 {
                CheckState::Indeterminate
            } else {
                CheckState::Unchecked
            }
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for group in &grouped.repositories {
            let keys: Vec<String> = group.candidates.iter().map(|c| c.key.clone()).collect();
            if let [only] = group.candidates.as_slice() {
                rows.push(self.candidate_row(
                    only,
                    Some(&group.label),
                    false,
                    selected.contains(&only.key),
                    theme,
                    cx,
                ));
                continue;
            }
            let open = !self.collapsed_groups.contains(&group.key);
            let sources: Vec<AgentSessionSource> = {
                let mut all: Vec<AgentSessionSource> = Vec::new();
                for candidate in &group.candidates {
                    for source in &candidate.inner.sources {
                        if !all.contains(source) {
                            all.push(*source);
                        }
                    }
                }
                all
            };
            let group_key = group.key.clone();
            rows.push(self.group_header(
                &group.key,
                &group.label,
                open,
                tri(&keys),
                keys.clone(),
                Self::row_meta(
                    Some(&sources),
                    group.thread_count,
                    group.last_active_at.as_deref(),
                    theme,
                    cx,
                ),
                move |this, _| {
                    if !this.collapsed_groups.remove(&group_key) {
                        this.collapsed_groups.insert(group_key.clone());
                    }
                },
                theme,
                cx,
            ));
            if open {
                for candidate in &group.candidates {
                    rows.push(self.candidate_row(
                        candidate,
                        None,
                        true,
                        selected.contains(&candidate.key),
                        theme,
                        cx,
                    ));
                }
            }
        }
        if !grouped.other.is_empty() {
            let keys: Vec<String> = grouped.other.iter().map(|c| c.key.clone()).collect();
            let count = grouped.other.len() as u64;
            rows.push(
                self.group_header(
                    "other",
                    "Other folders",
                    self.other_open,
                    tri(&keys),
                    keys,
                    div()
                        .ml_auto()
                        .flex_shrink_0()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(plural(count, "folder", "folders"))
                        .into_any_element(),
                    |this, _| this.other_open = !this.other_open,
                    theme,
                    cx,
                ),
            );
            if self.other_open {
                for candidate in &grouped.other {
                    rows.push(self.candidate_row(
                        candidate,
                        None,
                        true,
                        selected.contains(&candidate.key),
                        theme,
                        cx,
                    ));
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(rows)
            .into_any_element()
    }

    /// Turin's own import: adopt a T3 Code database.
    fn t3_card(&self, candidate: &T3DatabaseCandidate, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let path = candidate.path.display().to_string();
        let (status, status_color): (Option<String>, Hsla) = match &self.t3 {
            T3State::Done(text) => (Some(text.clone()), theme.success_foreground),
            T3State::Failed(text) => (Some(text.clone()), theme.destructive),
            _ => (None, theme.muted_foreground),
        };
        let importing = matches!(self.t3, T3State::Importing);
        let done = matches!(self.t3, T3State::Done(_));
        div()
            .mt_5()
            .flex()
            .items_center()
            .gap_3()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .px_4()
            .py_4()
            .child(Icon::new(IconName::Database).size(rems(1.25)).color(theme.muted_foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Import from T3 Code"),
                    )
                    .child(
                        div()
                            .mt(rems(0.125))
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child("Bring over your projects and thread history. Your T3 Code data is copied, never changed."),
                    )
                    .child(
                        div()
                            .mt(rems(0.25))
                            .min_w_0()
                            .truncate()
                            .font_family(theme.fonts.mono.clone())
                            .text_size(TEXT_2XS.size)
                            .text_color(theme.muted_foreground)
                            .child(path),
                    )
                    .children(status.map(|text| {
                        div()
                            .mt(rems(0.375))
                            .text_size(TEXT_XS.size)
                            .text_color(status_color)
                            .child(text)
                    })),
            )
            .child(div().flex_shrink_0().child(if done {
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.success_foreground)
                    .child(Icon::new(IconName::Check).size(rems(0.875)).color(theme.success_foreground))
                    .child("Imported")
                    .into_any_element()
            } else {
                Button::new("wizard-t3-import")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .disabled(importing)
                    .child(if importing {
                        Spinner::new("wizard-t3-spinner").size(SpinnerSize::Sm).into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(if importing { "Importing…" } else if matches!(self.t3, T3State::Failed(_)) { "Retry" } else { "Import" })
                    .on_click(move |_, window, cx| entity.update(cx, |this, cx| this.import_t3(window, cx)))
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn import_step(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let skip = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.finish(None, String::new(), 0, window, cx)
                })
            }
        };
        if self.scan.pending && self.scan.data.is_none() {
            return div()
                .flex()
                .flex_col()
                .min_h(rems(10.))
                .child(
                    div()
                        .text_size(rems(1.5))
                        .line_height(rems(2.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Your projects"),
                )
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .py_6()
                        .child(
                            Spinner::new("wizard-scan-spinner")
                                .size(SpinnerSize::Lg)
                                .tone(SpinnerTone::Muted),
                        )
                        .child(
                            div()
                                .text_size(TEXT_SM.size)
                                .text_color(theme.muted_foreground)
                                .child("Looking for projects from Claude Code and Codex…"),
                        ),
                )
                .child(
                    div().flex().justify_end().child(
                        Button::new("wizard-skip-scan")
                            .variant(ButtonVariant::GhostMuted)
                            .label("Do not import projects")
                            .on_click(skip),
                    ),
                )
                .into_any_element();
        }

        let all = self.candidates();
        let recent = partition_onboarding_projects(&all, now_ms()).recent;
        let selected_keys = self.selected_key_set(&recent);
        let selected: Vec<Candidate> = all
            .iter()
            .filter(|candidate| selected_keys.contains(&candidate.key))
            .cloned()
            .collect();
        let importing = self.is_importing;
        let truncated = self
            .scan
            .data
            .as_ref()
            .is_some_and(|data| data.truncated == Some(true));
        let all_keys: Vec<String> = all.iter().map(|c| c.key.clone()).collect();
        let select_all = {
            let entity = entity.clone();
            let keys = all_keys.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                let keys = keys.clone();
                entity.update(cx, |this, cx| {
                    this.selected_keys = Some(keys.into_iter().collect());
                    cx.notify();
                })
            }
        };
        let select_none = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.selected_keys = Some(HashSet::new());
                    cx.notify();
                })
            }
        };

        let body: AnyElement = if let Some(error) = &self.scan.error {
            let retry = entity.clone();
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child(format!("Could not check projects. {error}"))
                .child(
                    Button::new("wizard-rescan")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .label("Retry")
                        .on_click(move |_, window, cx| {
                            retry.update(cx, |this, cx| this.rescan(window, cx))
                        }),
                )
                .into_any_element()
        } else if all.is_empty() {
            div()
                .py_2()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child("No existing Claude Code or Codex projects found.")
                .into_any_element()
        } else {
            self.candidate_list(&all, &selected_keys, &theme, cx)
        };

        let t3 = self
            .backend
            .t3_database()
            .map(|candidate| self.t3_card(&candidate, cx));
        let import_label = if importing {
            "Importing…".to_owned()
        } else {
            format!(
                "Import {}",
                plural(selected.len() as u64, "project", "projects")
            )
        };
        let run = {
            let entity = entity.clone();
            let selection = selected.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                let selection = selection.clone();
                entity.update(cx, |this, cx| this.run_import(selection, window, cx))
            }
        };
        step_shell(
            "Import your projects",
            Some("Import projects and conversations from Claude Code and Codex on this computer."),
            &theme,
        )
        .children(t3)
        .when(!all.is_empty(), |this| {
            this.child(
                div()
                    .mt(rems(1.25))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(format!("{} of {} selected", selected.len(), all.len()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("wizard-select-all")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Xs)
                                    .label("Select all")
                                    .disabled(importing || selected.len() == all.len())
                                    .on_click(select_all),
                            )
                            .child(
                                Button::new("wizard-select-none")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Xs)
                                    .label("Select none")
                                    .disabled(importing || selected.is_empty())
                                    .on_click(select_none),
                            ),
                    ),
            )
        })
        .child(
            div()
                .id("wizard-import-scroll")
                .mt_2()
                .max_h(rems(20.))
                .overflow_y_scroll()
                .child(
                    div()
                        .pr_3()
                        .flex()
                        .flex_col()
                        .gap(rems(1.25))
                        .child(body)
                        .when(truncated, |this| {
                            this.child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(SCAN_LIMIT_MESSAGE),
                            )
                        }),
                ),
        )
        .when(!self.import_error.is_empty(), |this| {
            this.child(
                div()
                    .mt_3()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.destructive)
                    .child(self.import_error.clone()),
            )
        })
        .child(
            div()
                .mt_6()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_3()
                .child(
                    Button::new("wizard-skip-import")
                        .variant(ButtonVariant::GhostMuted)
                        .label("Do not import projects")
                        .disabled(importing)
                        .on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                entity.update(cx, |this, cx| this.finish_after_import(window, cx))
                            }
                        }),
                )
                .child(
                    Button::new("wizard-import")
                        .label(import_label)
                        .disabled(importing || selected.is_empty())
                        .on_click(run),
                ),
        )
        .into_any_element()
    }
}

impl Render for WelcomeWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let current = if self.step == Step::Agents { 0 } else { 1 };
        let importing = self.is_importing;
        let steps = wizard_steps(
            current,
            move |index| importing || index >= current,
            move |index, window, cx| {
                if index > current {
                    return;
                }
                entity.update(cx, |this, cx| {
                    this.step = if index == 0 {
                        Step::Agents
                    } else {
                        Step::Import
                    };
                    let _ = window;
                    cx.notify();
                });
            },
            &theme,
        );
        let dark = theme.is_dark();
        let body = match self.step {
            Step::Agents => self.agents_step(cx),
            Step::Import => self.import_step(cx),
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .min_h_0()
            .max_h(window.viewport_size().height * 0.8)
            .overflow_hidden()
            .rounded(RADIUS_2XL)
            .border_color(dialog_border(&theme))
            .child(
                // `WizardHeader`: `DialogHeader` (`p-6 pb-3`) with the identity and the steps.
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap_2()
                    .p_6()
                    .pb_3()
                    .child(
                        div()
                            .text_size(rems(1.5))
                            .line_height(rems(2.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Turin"),
                    )
                    .child(steps),
            )
            .child(
                // `WizardPanel`: `bg-background/80 px-6 py-5 ring-1 ring-black/5 dark:bg-white/2`.
                div()
                    .id("wizard-panel")
                    .flex_auto()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(if dark {
                        with_alpha(tokens::tw::WHITE, 0.05)
                    } else {
                        with_alpha(tokens::tw::BLACK, 0.05)
                    })
                    .bg(if dark {
                        with_alpha(tokens::tw::WHITE, 0.02)
                    } else {
                        with_alpha(theme.background, 0.8)
                    })
                    .px_6()
                    .py(rems(1.25))
                    .child(body),
            )
    }
}
