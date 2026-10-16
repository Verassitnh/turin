//! In-memory [`SidebarStore`] content and [`SidebarBackend`] until the
//! client view-model reducers (T14) and the orchestrator land. Not an
//! upstream module.
//!
//! The fixture covers every row state upstream renders (working, waiting,
//! approval, input, failed, usage-limited, unread, woke, pinned, snoozed,
//! settled, drafts, PR links, worktrees), timed relative to launch so the
//! relative labels read naturally. The backend applies the same lifecycle
//! rules the server deciders do, synchronously.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, AppContext as _, Entity, Task, WeakEntity};
use turin_contracts::{EnvironmentId, ProjectId, RunId, ThreadId};

use super::runtime::thread_sort::pin_order_key_between;
use super::sidebar_logic::CommandOutcome;
use super::view_model::{
    ProviderDriverKind, ProviderEntry, Shelf, SidebarBackend, SidebarDraft, SidebarEnvironment,
    SidebarSettings, SidebarSnapshot, SidebarStore, ThreadCapabilities, ThreadRef,
    ThreadSearchMatch,
};
use crate::components::thread_search_match::SearchMatchSource;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProviderGoal, OrchestrationV2ProviderGoalStatus,
};

use crate::state::models::{
    BackgroundTaskKind, DiffStat, EnvironmentProject, EnvironmentThreadShell, InteractionMode,
    ModelSelection, PendingBackgroundTask, ProjectIcon, ProjectIconColor, ProviderFailureClass,
    PullRequestState, RunStatus, SettledOverride, ThreadPullRequestLink, ThreadRunSummary,
    ThreadRuntimeSummary, TitleRegeneration,
};
use crate::state::project_grouping::ProjectGroupingSettings;
use crate::timestamp_format::{now_ms, to_iso_string};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

fn env_id() -> EnvironmentId {
    EnvironmentId::new("local").expect("non-blank")
}

fn project(id: &str, title: &str, root: &str, icon: Option<ProjectIcon>) -> EnvironmentProject {
    EnvironmentProject {
        environment_id: env_id(),
        id: ProjectId::new(id).expect("non-blank"),
        title: title.into(),
        workspace_root: root.into(),
        favicon_path: None,
        project_icon: icon,
        created_at: Some(to_iso_string(now_ms() - 30 * DAY)),
        updated_at: Some(to_iso_string(now_ms() - 30 * DAY)),
        // The fixture has no scratch root and no contract row behind it.
        is_scratch: false,
        source: None,
    }
}

struct Fixture {
    now: i64,
}

impl Fixture {
    fn at(&self, ago: i64) -> String {
        to_iso_string(self.now - ago)
    }

    fn thread(
        &self,
        id: &str,
        project: &str,
        title: &str,
        created_ago: i64,
    ) -> EnvironmentThreadShell {
        let mut thread = EnvironmentThreadShell::new(
            env_id(),
            ThreadId::new(id).expect("non-blank"),
            ProjectId::new(project).expect("non-blank"),
            title,
            self.at(created_ago),
        );
        thread.model_selection = ModelSelection {
            instance_id: "codex".into(),
            model: "gpt-5.4".into(),
        };
        thread.latest_user_message_at = Some(self.at(created_ago));
        thread.last_visited_at = Some(Some(self.at(0)));
        thread
    }

    fn run(
        &self,
        id: &str,
        status: RunStatus,
        requested_ago: i64,
        completed_ago: Option<i64>,
    ) -> ThreadRunSummary {
        ThreadRunSummary {
            run_id: RunId::new(id).expect("non-blank"),
            status,
            requested_at: Some(self.at(requested_ago)),
            started_at: Some(self.at(requested_ago)),
            completed_at: completed_ago.map(|ago| self.at(ago)),
        }
    }

    fn runtime(
        &self,
        status: RunStatus,
        instance: &str,
        run: Option<&str>,
        started_ago: i64,
    ) -> ThreadRuntimeSummary {
        ThreadRuntimeSummary {
            status,
            active_run_id: run.map(|run| RunId::new(run).expect("non-blank")),
            activity_started_at: Some(Some(self.at(started_ago))),
            provider_instance_id: instance.into(),
            provider_name: None,
            last_error: None,
            last_error_class: None,
            updated_at: self.at(started_ago),
        }
    }
}

fn pr(number: u64, title: &str, state: PullRequestState) -> ThreadPullRequestLink {
    ThreadPullRequestLink {
        repository: "pingdotgg/turin".into(),
        number,
        url: format!("https://github.com/pingdotgg/turin/pull/{number}"),
        title: Some(title.into()),
        state: Some(state),
        is_draft: false,
        dismissed: false,
    }
}

/// The fixture snapshot.
pub fn mock_snapshot() -> SidebarSnapshot {
    let f = Fixture { now: now_ms() };
    let projects = vec![
        // Dev hook for screenshots: a workspace that really holds an icon file.
        project(
            "turin",
            "turin",
            &std::env::var("TURIN_MOCK_FAVICON_ROOT")
                .unwrap_or_else(|_| "/Users/dev/code/turin".to_owned()),
            None,
        ),
        project("t3code", "t3code", "/Users/dev/code/t3code", None),
        project(
            "website",
            "website",
            "/Users/dev/code/website",
            Some(ProjectIcon::Monogram {
                text: "W".into(),
                color: ProjectIconColor::Violet,
            }),
        ),
    ];
    let mut threads = Vec::new();

    // Pinned: a long-running goal and a keyless legacy pin.
    let mut t = f.thread(
        "pin-roadmap",
        "turin",
        "Track the Turin port roadmap",
        6 * DAY,
    );
    t.pinned_at = Some(f.at(5 * DAY));
    t.pin_order_key = Some("n".into());
    t.branch = Some("main".into());
    t.latest_run = Some(f.run("run-roadmap", RunStatus::Completed, 2 * DAY, Some(2 * DAY)));
    threads.push(t);

    let mut t = f.thread(
        "pin-release",
        "t3code",
        "Release checklist for 0.0.28",
        9 * DAY,
    );
    t.pinned_at = Some(f.at(8 * DAY));
    t.latest_run = Some(f.run("run-release", RunStatus::Completed, 3 * DAY, Some(3 * DAY)));
    threads.push(t);

    // Active inbox.
    let mut t = f.thread(
        "sidebar",
        "turin",
        "Port the sidebar: projects, threads, snooze and drag",
        3 * HOUR,
    );
    t.branch = Some("task/T15-sidebar".into());
    t.worktree_path = Some("/Users/dev/code/turin-wt/T15-sidebar".into());
    t.latest_run = Some(f.run("run-sidebar", RunStatus::Running, 4 * MINUTE, None));
    t.runtime = Some(f.runtime(
        RunStatus::Running,
        "claude",
        Some("run-sidebar"),
        4 * MINUTE,
    ));
    t.model_selection = ModelSelection {
        instance_id: "claude".into(),
        model: "claude-opus-5-5".into(),
    };
    t.provider_instance_history = vec!["codex".into(), "claude".into()];
    t.latest_run_diff = Some(DiffStat {
        insertions: 1240,
        deletions: 18,
    });
    threads.push(t);

    let mut t = f.thread(
        "approval",
        "t3code",
        "Migrate settings to the new schema",
        50 * MINUTE,
    );
    t.branch = Some("settings-schema".into());
    t.has_pending_approvals = true;
    t.latest_run = Some(f.run("run-approval", RunStatus::Running, 6 * MINUTE, None));
    t.runtime = Some(f.runtime(
        RunStatus::Running,
        "codex",
        Some("run-approval"),
        6 * MINUTE,
    ));
    t.pull_requests = vec![pr(2412, "Migrate settings schema", PullRequestState::Open)];
    threads.push(t);

    let mut t = f.thread("input", "website", "Draft the launch blog post", 2 * HOUR);
    t.has_pending_user_input = true;
    t.latest_run = Some(f.run("run-input", RunStatus::Waiting, 20 * MINUTE, None));
    threads.push(t);

    let mut t = f.thread("done", "turin", "Port orchestration contracts", 5 * HOUR);
    t.branch = Some("task/T01-contracts".into());
    t.latest_run = Some(f.run(
        "run-done",
        RunStatus::Completed,
        40 * MINUTE,
        Some(12 * MINUTE),
    ));
    t.last_visited_at = Some(Some(f.at(30 * MINUTE)));
    t.pull_requests = vec![pr(
        311,
        "Port orchestration contracts",
        PullRequestState::Merged,
    )];
    t.latest_run_diff = Some(DiffStat {
        insertions: 3812,
        deletions: 4,
    });
    threads.push(t);

    let mut t = f.thread(
        "waiting",
        "t3code",
        "Watch CI on the composer refactor",
        7 * HOUR,
    );
    t.branch = Some("composer-refactor".into());
    t.latest_run = Some(f.run("run-waiting", RunStatus::Completed, 2 * HOUR, Some(HOUR)));
    t.runtime = Some(f.runtime(RunStatus::Idle, "codex", None, HOUR));
    t.pending_background_tasks = vec![PendingBackgroundTask {
        task_id: "watch-ci".into(),
        description: Some("Watching pull request #2398".into()),
        kind: BackgroundTaskKind::Monitor,
    }];
    threads.push(t);

    let mut t = f.thread(
        "failed",
        "turin",
        "Wire the Codex app-server client",
        9 * HOUR,
    );
    t.branch = Some("task/T09a-codex".into());
    t.latest_run = Some(f.run("run-failed", RunStatus::Failed, 3 * HOUR, Some(3 * HOUR)));
    let mut rt = f.runtime(RunStatus::Failed, "codex", None, 3 * HOUR);
    rt.last_error = Some("Provider exited with status 1".into());
    t.runtime = Some(rt);
    threads.push(t);

    let mut t = f.thread("limited", "website", "Generate social cards", 11 * HOUR);
    t.latest_run = Some(f.run("run-limited", RunStatus::Failed, 4 * HOUR, Some(4 * HOUR)));
    let mut rt = f.runtime(RunStatus::Failed, "claude", None, 4 * HOUR);
    rt.last_error = Some("Usage limit reached".into());
    rt.last_error_class = Some(ProviderFailureClass::UsageLimit);
    t.runtime = Some(rt);
    t.model_selection.instance_id = "claude".into();
    threads.push(t);

    let mut t = f.thread("woke", "t3code", "Follow up on the flaky e2e test", 2 * DAY);
    t.snoozed_at = Some(f.at(DAY));
    t.snoozed_until = Some(f.at(2 * HOUR));
    t.last_visited_at = Some(Some(f.at(DAY)));
    t.latest_run = Some(f.run(
        "run-woke",
        RunStatus::Completed,
        DAY + HOUR,
        Some(DAY + HOUR),
    ));
    threads.push(t);

    let mut t = f.thread("goal", "turin", "Keep the gate green", 13 * HOUR);
    t.goal = Some(OrchestrationV2ProviderGoal {
        objective: turin_contracts::TrimmedNonEmptyString::new("Keep the gate green")
            .expect("non-blank"),
        status: OrchestrationV2ProviderGoalStatus::Active,
        tokens_used: None,
        token_budget: None,
        time_used_seconds: None,
        checks: None,
        last_check: None,
    });
    t.latest_run = Some(f.run("run-goal", RunStatus::Running, 50 * MINUTE, None));
    t.runtime = Some(f.runtime(RunStatus::Running, "cursor", Some("run-goal"), 50 * MINUTE));
    t.model_selection.instance_id = "cursor".into();
    threads.push(t);

    let mut t = f.thread("plain", "website", "Tidy up the footer links", 15 * HOUR);
    t.latest_run = Some(f.run(
        "run-plain",
        RunStatus::Completed,
        14 * HOUR,
        Some(14 * HOUR),
    ));
    t.interaction_mode = InteractionMode::Default;
    t.title_regeneration = None;
    threads.push(t);

    // Snoozed.
    let mut t = f.thread(
        "snoozed-1",
        "turin",
        "Revisit the hooks config format",
        3 * DAY,
    );
    t.snoozed_at = Some(f.at(HOUR));
    t.snoozed_until = Some(f.at(-3 * HOUR));
    t.latest_run = Some(f.run("run-s1", RunStatus::Completed, 2 * DAY, Some(2 * DAY)));
    threads.push(t);
    let mut t = f.thread(
        "snoozed-2",
        "t3code",
        "Profile the timeline virtualizer",
        4 * DAY,
    );
    t.snoozed_at = Some(f.at(HOUR));
    t.snoozed_until = Some(f.at(-2 * DAY));
    t.latest_run = Some(f.run("run-s2", RunStatus::Completed, 3 * DAY, Some(3 * DAY)));
    threads.push(t);

    // Settled history.
    for (index, (id, project, title)) in [
        ("settled-1", "turin", "Scaffold the workspace"),
        ("settled-2", "t3code", "Investigate sidebar drag jank"),
        ("settled-3", "website", "Fix the dark-mode hero image"),
        ("settled-4", "turin", "Port the persistence layer"),
        ("settled-5", "t3code", "Bump Effect to 4.0"),
        ("settled-6", "turin", "Design system and app shell"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut t = f.thread(id, project, title, (index as i64 + 2) * DAY);
        t.settled_override = Some(SettledOverride::Settled);
        t.settled_at = Some(f.at((index as i64 + 1) * DAY - 3 * HOUR));
        t.latest_run = Some(f.run(
            &format!("run-{id}"),
            RunStatus::Completed,
            (index as i64 + 2) * DAY,
            Some((index as i64 + 1) * DAY),
        ));
        threads.push(t);
    }

    // A hidden subagent (lineage) never shows in the sidebar.
    let mut t = f.thread("subagent", "turin", "Subagent: read upstream", 3 * HOUR);
    t.lineage.relationship_to_parent = Some(crate::state::models::ThreadRelationship::Subagent);
    threads.push(t);

    let mut snapshot = SidebarSnapshot {
        project_groups: Vec::new(),
        projects,
        threads,
        environments: vec![SidebarEnvironment {
            id: env_id(),
            label: "This Mac".into(),
            capabilities: ThreadCapabilities::ALL,
        }],
        primary_environment_id: Some(env_id()),
        provider_entries: vec![
            provider(
                "codex",
                "Codex",
                ProviderDriverKind::Codex,
                &[("gpt-5.4", "GPT-5.4")],
            ),
            provider(
                "claude",
                "Claude",
                ProviderDriverKind::Claude,
                &[("claude-opus-5-5", "Opus 5.5")],
            ),
            provider(
                "cursor",
                "Cursor",
                ProviderDriverKind::Cursor,
                &[("gpt-5.4", "GPT-5.4")],
            ),
            provider("opencode", "OpenCode", ProviderDriverKind::OpenCode, &[]),
        ],
        drafts: vec![SidebarDraft {
            draft_id: "draft-1".into(),
            environment_id: env_id(),
            project_id: ProjectId::new("turin").expect("non-blank"),
            prompt_preview: "Add a hooks.toml example for worktree setup".into(),
            attachment_count: 0,
            created_at: f.at(25 * MINUTE),
            branch: Some("main".into()),
            worktree_path: None,
        }],
        settings: SidebarSettings::default(),
        local_last_visited_at: HashMap::new(),
        project_scope_key: None,
        settled_shelf_expanded: true,
        snoozed_shelf_expanded: false,
        working_shelf_expanded: false,
    };
    snapshot.refresh_project_groups(&[], &ProjectGroupingSettings::default());
    snapshot
}

fn provider(
    id: &str,
    name: &str,
    kind: ProviderDriverKind,
    models: &[(&str, &str)],
) -> ProviderEntry {
    ProviderEntry {
        instance_id: id.into(),
        display_name: name.into(),
        driver_kind: kind,
        models: models
            .iter()
            .map(|(slug, label)| (slug.to_string(), label.to_string()))
            .collect(),
    }
}

/// Messages the mock search scans (`useThreadSearch` server hits).
fn mock_messages() -> Vec<(&'static str, SearchMatchSource, &'static str)> {
    vec![
        (
            "sidebar",
            SearchMatchSource::User,
            "Port the sidebar drag preview so pinned rows open label space",
        ),
        (
            "sidebar",
            SearchMatchSource::Assistant,
            "The motion engine now rides entering rows on the shelf displacement",
        ),
        (
            "approval",
            SearchMatchSource::Assistant,
            "I need approval to run the settings migration against the dev database",
        ),
        (
            "done",
            SearchMatchSource::Assistant,
            "All orchestration contracts round-trip through serde with upstream fixtures",
        ),
        (
            "settled-2",
            SearchMatchSource::User,
            "Why does the drag preview jank when a shelf collapses?",
        ),
        (
            "woke",
            SearchMatchSource::User,
            "Rerun the e2e suite after the snapshot fix lands",
        ),
    ]
}

/// The in-memory backend: applies each command to the store right away.
pub struct MockSidebarBackend {
    store: WeakEntity<SidebarStore>,
}

impl MockSidebarBackend {
    pub fn new(store: &Entity<SidebarStore>) -> Rc<Self> {
        Rc::new(Self {
            store: store.downgrade(),
        })
    }

    fn with_snapshot(&self, cx: &mut App, f: impl FnOnce(&mut SidebarSnapshot)) {
        self.store
            .update(cx, |store, cx| store.update_snapshot(cx, f))
            .ok();
    }

    fn thread_command(
        &self,
        thread: &ThreadRef,
        cx: &mut App,
        f: impl FnOnce(&mut EnvironmentThreadShell, &str),
    ) -> Task<CommandOutcome> {
        let now = to_iso_string(now_ms());
        let mut found = false;
        self.with_snapshot(cx, |snapshot| {
            if let Some(shell) = snapshot.threads.iter_mut().find(|shell| {
                shell.environment_id == thread.environment_id && shell.id == thread.thread_id
            }) {
                found = true;
                f(shell, &now);
            }
        });
        Task::ready(if found {
            CommandOutcome::Success
        } else {
            CommandOutcome::Failure("Thread not found".into())
        })
    }
}

impl SidebarBackend for MockSidebarBackend {
    fn settle(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        // Settling clears the pin ("done" beats "keep on top") and any snooze.
        self.thread_command(&thread, cx, |shell, now| {
            shell.settled_override = Some(SettledOverride::Settled);
            shell.settled_at = Some(now.to_string());
            shell.unsettled_at = None;
            shell.pinned_at = None;
            shell.pin_order_key = None;
            shell.active_order_key = None;
            shell.snoozed_at = None;
            shell.snoozed_until = None;
        })
    }

    fn unsettle(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, now| {
            shell.settled_override = Some(SettledOverride::Active);
            shell.settled_at = None;
            shell.unsettled_at = Some(now.to_string());
        })
    }

    fn snooze(
        &self,
        thread: ThreadRef,
        snoozed_until: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, now| {
            shell.snoozed_at = Some(now.to_string());
            shell.snoozed_until = Some(snoozed_until);
        })
    }

    fn unsnooze(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, _| {
            shell.snoozed_at = None;
            shell.snoozed_until = None;
        })
    }

    fn pin(
        &self,
        thread: ThreadRef,
        order_key: Option<String>,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        // Fresh pins take a key before the smallest key across all pins.
        let head = self.store.upgrade().and_then(|store| {
            store
                .read(cx)
                .snapshot()
                .threads
                .iter()
                .filter(|shell| shell.pinned_at.is_some())
                .filter_map(|shell| shell.pin_order_key.clone())
                .min()
        });
        let key = order_key.or_else(|| pin_order_key_between(None, head.as_deref()));
        self.thread_command(&thread, cx, |shell, now| {
            shell.pinned_at = Some(now.to_string());
            shell.pin_order_key = key;
            shell.snoozed_at = None;
            shell.snoozed_until = None;
            if shell.settled_override == Some(SettledOverride::Settled) {
                shell.settled_override = Some(SettledOverride::Active);
                shell.settled_at = None;
                shell.unsettled_at = Some(now.to_string());
            }
        })
    }

    fn unpin(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, _| {
            shell.pinned_at = None;
            shell.pin_order_key = None;
        })
    }

    fn reorder_pinned(
        &self,
        thread: ThreadRef,
        order_key: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, _| {
            shell.pin_order_key = Some(order_key)
        })
    }

    fn reorder_active(
        &self,
        thread: ThreadRef,
        order_key: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, _| {
            shell.active_order_key = Some(order_key)
        })
    }

    fn set_auto_settle(
        &self,
        thread: ThreadRef,
        enabled: bool,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, now| {
            shell.auto_settle_disabled_at = (!enabled).then(|| now.to_string());
        })
    }

    fn rename(&self, thread: ThreadRef, title: String, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, _| shell.title = title)
    }

    fn regenerate_title(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        let store = self.store.clone();
        let key = thread.clone();
        let task = self.thread_command(&thread, cx, |shell, now| {
            shell.title_regeneration = Some(TitleRegeneration {
                request_id: "mock".into(),
                started_at: now.to_string(),
            });
        });
        // The mock "server" finishes the regeneration after a beat.
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1200))
                .await;
            store
                .update(cx, |store, cx| {
                    store.update_snapshot(cx, |snapshot| {
                        if let Some(shell) = snapshot
                            .threads
                            .iter_mut()
                            .find(|shell| shell.id == key.thread_id)
                        {
                            shell.title_regeneration = None;
                        }
                    })
                })
                .ok();
        })
        .detach();
        task
    }

    fn mark_unread(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        // Rewind the visited watermark to before the latest completion.
        self.thread_command(&thread, cx, |shell, _| {
            shell.last_visited_at = Some(Some(to_iso_string(0)));
        })
    }

    fn mark_visited(
        &self,
        thread: ThreadRef,
        visited_at: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, _| {
            shell.last_visited_at = Some(Some(visited_at));
        })
    }

    fn archive(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, cx, |shell, now| {
            shell.archived_at = Some(now.to_string())
        })
    }

    fn delete(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        let mut found = false;
        self.with_snapshot(cx, |snapshot| {
            let before = snapshot.threads.len();
            snapshot.threads.retain(|shell| {
                !(shell.environment_id == thread.environment_id && shell.id == thread.thread_id)
            });
            found = snapshot.threads.len() != before;
        });
        Task::ready(if found {
            CommandOutcome::Success
        } else {
            CommandOutcome::Failure("Thread not found".into())
        })
    }

    fn discard_draft(&self, draft_id: String, cx: &mut App) {
        self.with_snapshot(cx, |snapshot| {
            snapshot.drafts.retain(|draft| draft.draft_id != draft_id)
        });
    }

    fn set_project_scope(&self, project_key: Option<String>, cx: &mut App) {
        self.with_snapshot(cx, |snapshot| snapshot.project_scope_key = project_key);
    }

    fn set_shelf_expanded(&self, shelf: Shelf, expanded: bool, cx: &mut App) {
        self.with_snapshot(cx, |snapshot| match shelf {
            Shelf::Working => snapshot.working_shelf_expanded = expanded,
            Shelf::Snoozed => snapshot.snoozed_shelf_expanded = expanded,
            Shelf::Settled => snapshot.settled_shelf_expanded = expanded,
        });
    }

    fn search_messages(&self, query: String, _: &mut App) -> Task<Vec<ThreadSearchMatch>> {
        let needle = query.trim().to_ascii_lowercase();
        if needle.chars().count() < 2 {
            return Task::ready(Vec::new());
        }
        let matches = mock_messages()
            .into_iter()
            .filter(|(_, _, text)| text.to_ascii_lowercase().contains(&needle))
            .map(|(thread, source, text)| ThreadSearchMatch {
                environment_id: env_id(),
                thread_id: ThreadId::new(thread).expect("non-blank"),
                source,
                snippet: text.to_string(),
            })
            .collect();
        Task::ready(matches)
    }
}

/// Builds the mock store and backend.
pub fn mock_sidebar(cx: &mut App) -> (Entity<SidebarStore>, Rc<MockSidebarBackend>) {
    let store = cx.new(|_| SidebarStore::new(mock_snapshot()));
    let backend = MockSidebarBackend::new(&store);
    (store, backend)
}
