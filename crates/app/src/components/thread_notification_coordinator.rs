//! Port of upstream/apps/web/src/components/ThreadNotificationCoordinator.tsx.
//!
//! [`ThreadNotificationScanner`] is `EnvironmentNotifications`' effect: it
//! compares each scan of the shell's threads with the last one and says which
//! threads just finished or just need the user. The layout turns those notices
//! into the sound, the in-app toast or the desktop notification. The
//! coordinator itself owns the dock badge, which counts the desktop
//! notifications waiting to be seen (`pending`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use turin_contracts::ThreadId;
use turin_contracts::settings::{ClientSettings, NotificationMode};

use crate::components::sidebar::sidebar_logic::{
    SidebarThreadStatus, resolve_sidebar_thread_status,
};
use crate::state::models::{EnvironmentThreadShell, RunStatus, ThreadRelationship};
use crate::timestamp_format::parse_timestamp_ms;

use crate::shell::thread_notifications::{has_desktop_notifications, set_notification_badge};

/// Owns the dock badge: one count per desktop notification not yet seen.
pub struct ThreadNotificationCoordinator {
    pending: HashSet<String>,
    scanner: ThreadNotificationScanner,
}

impl ThreadNotificationCoordinator {
    pub fn new() -> Self {
        Self {
            pending: HashSet::new(),
            scanner: ThreadNotificationScanner::default(),
        }
    }

    pub fn sync_mode(&mut self, settings: &ClientSettings) {
        if !has_desktop_notifications(settings.notification_mode) {
            self.clear();
        }
    }

    /// `onNotification`: a desktop notification for `tag` was posted (a later
    /// one with the same tag replaces it) and the badge counts it.
    pub fn note_desktop_notification(&mut self, tag: &str) {
        self.pending.insert(tag.to_owned());
        set_notification_badge(self.pending.len() as u32);
    }

    /// The window gained focus, or the mode no longer shows notifications.
    pub fn clear(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.pending.clear();
        set_notification_badge(0);
    }

    /// Compares the shell's threads with the previous scan.
    pub fn scan(&mut self, threads: Option<&[Arc<EnvironmentThreadShell>]>) -> Vec<ThreadNotice> {
        self.scanner.scan(threads)
    }

    /// Forgets the previous scan: the next one only learns the threads, so
    /// turning notifications on does not announce what already happened.
    pub fn reset_scan(&mut self) {
        self.scanner = ThreadNotificationScanner::default();
    }
}

impl Default for ThreadNotificationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a thread is being announced: it finished, or it needs the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Completion,
    Input,
}

/// What a notice looks like as a toast (`type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeSeverity {
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadNotice {
    pub thread_id: ThreadId,
    pub kind: NoticeKind,
    pub severity: NoticeSeverity,
    /// "Thread completed", "Approval needed", ...
    pub title: &'static str,
    /// The thread's title.
    pub body: String,
}

#[derive(Clone)]
struct ScannedThread {
    raw: Arc<EnvironmentThreadShell>,
    attention: Option<String>,
    completion: Option<i64>,
}

/// `previous` of `EnvironmentNotifications`.
#[derive(Default)]
pub struct ThreadNotificationScanner {
    previous: HashMap<ThreadId, ScannedThread>,
}

impl ThreadNotificationScanner {
    /// One run of the effect. `None` is a shell that is not live yet.
    pub fn scan(&mut self, threads: Option<&[Arc<EnvironmentThreadShell>]>) -> Vec<ThreadNotice> {
        let Some(threads) = threads else {
            self.previous.clear();
            return Vec::new();
        };
        let mut next = HashMap::with_capacity(threads.len());
        let mut notices = Vec::new();
        for raw in threads {
            // Subagents notify through the thread above them.
            if raw.lineage.relationship_to_parent == Some(ThreadRelationship::Subagent) {
                continue;
            }
            let prior = self.previous.get(&raw.id);
            // The same object cannot produce a new notification.
            if let Some(prior) = prior
                && Arc::ptr_eq(&prior.raw, raw)
            {
                next.insert(raw.id.clone(), prior.clone());
                continue;
            }
            let mut status = resolve_sidebar_thread_status(raw);
            let latest_status = raw.latest_run.as_ref().map(|run| run.status);
            if status == SidebarThreadStatus::Ready && latest_status == Some(RunStatus::Failed) {
                status = SidebarThreadStatus::Failed;
            }
            let attention = matches!(
                status,
                SidebarThreadStatus::Input
                    | SidebarThreadStatus::Approval
                    | SidebarThreadStatus::Failed
                    | SidebarThreadStatus::Limited
            )
            .then(|| {
                let run_id = raw.latest_run.as_ref().map(|run| run.run_id.as_str());
                format!("{}:{status:?}", run_id.unwrap_or_default())
            });
            let completed_at = raw
                .latest_run
                .as_ref()
                .and_then(|run| run.completed_at.as_deref())
                .and_then(parse_timestamp_ms);
            // Commands left running (a dev server) read as ready; subagents
            // and monitors wait.
            let completion = match completed_at {
                Some(at)
                    if status == SidebarThreadStatus::Ready
                        && latest_status == Some(RunStatus::Completed) =>
                {
                    Some(at)
                }
                _ => prior.and_then(|prior| prior.completion),
            };
            next.insert(
                raw.id.clone(),
                ScannedThread {
                    raw: raw.clone(),
                    attention: attention.clone(),
                    completion,
                },
            );
            let Some(prior) = prior else { continue };
            if raw.archived_at.is_some() {
                continue;
            }
            let kind = if attention.is_some() && attention != prior.attention {
                NoticeKind::Input
            } else if completion.is_some_and(|at| prior.completion.is_none_or(|before| at > before))
            {
                NoticeKind::Completion
            } else {
                continue;
            };
            let (title, severity) = match kind {
                NoticeKind::Completion => ("Thread completed", NoticeSeverity::Success),
                NoticeKind::Input => match status {
                    SidebarThreadStatus::Approval => ("Approval needed", NoticeSeverity::Warning),
                    SidebarThreadStatus::Limited => {
                        ("Usage limit reached", NoticeSeverity::Warning)
                    }
                    SidebarThreadStatus::Failed => ("Thread failed", NoticeSeverity::Error),
                    _ => ("Input needed", NoticeSeverity::Warning),
                },
            };
            notices.push(ThreadNotice {
                thread_id: raw.id.clone(),
                kind,
                severity,
                title,
                body: raw.title.clone(),
            });
        }
        self.previous = next;
        notices
    }
}

/// Whether notifications of any kind are on (`mode === "off" &&
/// !inAppNotificationsEnabled` renders nothing).
pub fn notifications_enabled(mode: NotificationMode, in_app_enabled: bool) -> bool {
    mode != NotificationMode::Off || in_app_enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::ThreadRunSummary;
    use crate::state::models::fixtures::{run_id, runtime, shell};

    fn with_run(
        mut thread: EnvironmentThreadShell,
        status: RunStatus,
        completed_at: Option<&str>,
    ) -> EnvironmentThreadShell {
        thread.latest_run = Some(ThreadRunSummary {
            run_id: run_id("run-1"),
            status,
            requested_at: None,
            started_at: None,
            completed_at: completed_at.map(str::to_owned),
        });
        thread.runtime = Some(runtime(status));
        thread
    }

    fn scan(
        scanner: &mut ThreadNotificationScanner,
        threads: Vec<EnvironmentThreadShell>,
    ) -> Vec<ThreadNotice> {
        let threads: Vec<_> = threads.into_iter().map(Arc::new).collect();
        scanner.scan(Some(&threads))
    }

    #[test]
    fn the_first_scan_only_learns_the_threads() {
        let mut scanner = ThreadNotificationScanner::default();
        let done = with_run(
            shell("a"),
            RunStatus::Completed,
            Some("2026-03-09T10:05:00.000Z"),
        );
        assert!(scan(&mut scanner, vec![done]).is_empty());
    }

    #[test]
    fn a_finished_run_announces_completion_once() {
        let mut scanner = ThreadNotificationScanner::default();
        let running = with_run(shell("a"), RunStatus::Running, None);
        assert!(scan(&mut scanner, vec![running]).is_empty());
        let done = with_run(
            shell("a"),
            RunStatus::Completed,
            Some("2026-03-09T10:05:00.000Z"),
        );
        let notices = scan(&mut scanner, vec![done.clone()]);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].kind, NoticeKind::Completion);
        assert_eq!(notices[0].title, "Thread completed");
        assert_eq!(notices[0].severity, NoticeSeverity::Success);
        assert_eq!(notices[0].body, "a");
        // The same completion again (a different object, same run) is quiet.
        assert!(scan(&mut scanner, vec![done]).is_empty());
        // A later run that completes announces again.
        let mut next = with_run(
            shell("a"),
            RunStatus::Completed,
            Some("2026-03-09T11:00:00.000Z"),
        );
        next.latest_run.as_mut().expect("run").run_id = run_id("run-2");
        assert_eq!(scan(&mut scanner, vec![next]).len(), 1);
    }

    #[test]
    fn a_pending_approval_or_question_asks_for_the_user() {
        let mut scanner = ThreadNotificationScanner::default();
        assert!(
            scan(
                &mut scanner,
                vec![with_run(shell("a"), RunStatus::Running, None)]
            )
            .is_empty()
        );
        let mut approval = with_run(shell("a"), RunStatus::Running, None);
        approval.has_pending_approvals = true;
        let notices = scan(&mut scanner, vec![approval.clone()]);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, NoticeKind::Input);
        assert_eq!(notices[0].title, "Approval needed");
        // Still waiting on the same run: not announced again.
        assert!(scan(&mut scanner, vec![approval]).is_empty());
        let mut question = with_run(shell("a"), RunStatus::Running, None);
        question.has_pending_user_input = true;
        let notices = scan(&mut scanner, vec![question]);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Input needed");
    }

    #[test]
    fn a_failed_run_is_reported_as_failed() {
        let mut scanner = ThreadNotificationScanner::default();
        assert!(
            scan(
                &mut scanner,
                vec![with_run(shell("a"), RunStatus::Running, None)]
            )
            .is_empty()
        );
        let failed = with_run(
            shell("a"),
            RunStatus::Failed,
            Some("2026-03-09T10:05:00.000Z"),
        );
        let notices = scan(&mut scanner, vec![failed]);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Thread failed");
        assert_eq!(notices[0].severity, NoticeSeverity::Error);
    }

    #[test]
    fn subagents_and_archived_threads_stay_quiet() {
        let mut scanner = ThreadNotificationScanner::default();
        let mut sub = with_run(shell("sub"), RunStatus::Running, None);
        sub.lineage.relationship_to_parent = Some(ThreadRelationship::Subagent);
        let archived = with_run(shell("old"), RunStatus::Running, None);
        assert!(scan(&mut scanner, vec![sub.clone(), archived.clone()]).is_empty());
        sub.has_pending_approvals = true;
        let mut archived = archived;
        archived.has_pending_approvals = true;
        archived.archived_at = Some("2026-03-09T10:06:00.000Z".into());
        assert!(scan(&mut scanner, vec![sub, archived]).is_empty());
    }

    #[test]
    fn a_shell_that_is_not_live_forgets_what_it_knew() {
        let mut scanner = ThreadNotificationScanner::default();
        assert!(
            scan(
                &mut scanner,
                vec![with_run(shell("a"), RunStatus::Running, None)]
            )
            .is_empty()
        );
        assert!(scanner.scan(None).is_empty());
        let done = with_run(
            shell("a"),
            RunStatus::Completed,
            Some("2026-03-09T10:05:00.000Z"),
        );
        assert!(scan(&mut scanner, vec![done]).is_empty());
    }

    #[test]
    fn notifications_are_off_only_when_every_channel_is() {
        assert!(!notifications_enabled(NotificationMode::Off, false));
        assert!(notifications_enabled(NotificationMode::Off, true));
        assert!(notifications_enabled(NotificationMode::Sound, false));
    }
}
