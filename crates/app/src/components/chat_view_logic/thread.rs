//! Port of upstream/apps/web/src/components/ChatView.logic.ts (thread start and locked provider).

use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceId, is_provider_driver_kind,
};

use crate::state::models::{EnvironmentThreadShell, ThreadRunSummary, ThreadRuntimeSummary};

/// Fields [`thread_has_started`] reads from a thread row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadStartedView<'a> {
    pub latest_run: Option<&'a ThreadRunSummary>,
    pub item_count: u64,
    pub runtime: Option<&'a ThreadRuntimeSummary>,
}

impl<'a> From<&'a EnvironmentThreadShell> for ThreadStartedView<'a> {
    fn from(shell: &'a EnvironmentThreadShell) -> Self {
        Self {
            latest_run: shell.latest_run.as_ref(),
            item_count: shell.item_count,
            runtime: shell.runtime.as_ref(),
        }
    }
}

pub fn thread_has_started(thread: Option<&ThreadStartedView<'_>>) -> bool {
    thread.is_some_and(|thread| {
        thread.latest_run.is_some() || thread.item_count > 0 || thread.runtime.is_some()
    })
}

/// Whether a thread ran at least one turn, judged from its shell alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadShellStartedView<'a> {
    pub latest_run: Option<&'a ThreadRunSummary>,
    pub latest_user_message_at: Option<&'a str>,
    pub runtime: Option<&'a ThreadRuntimeSummary>,
}

pub fn thread_shell_has_started(shell: Option<ThreadShellStartedView<'_>>) -> bool {
    shell.is_some_and(|shell| {
        shell.latest_run.is_some()
            || shell.latest_user_message_at.is_some()
            || shell.runtime.is_some()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDriverLookup<'a> {
    pub instance_id: &'a ProviderInstanceId,
    pub driver: ProviderDriverKind,
}

pub fn derive_locked_provider(input: DeriveLockedProviderInput<'_>) -> Option<ProviderDriverKind> {
    let thread = input.thread?;
    if !thread_has_started(Some(&thread)) {
        return None;
    }
    let session_provider = thread
        .runtime
        .and_then(|runtime| runtime.provider_name.as_deref());
    if let Some(session_provider) = session_provider
        && is_provider_driver_kind(session_provider)
    {
        return ProviderDriverKind::new(session_provider).ok();
    }
    let thread_provider = input
        .thread_provider
        .and_then(|instance_id| {
            input
                .providers
                .iter()
                .find(|provider| provider.instance_id == instance_id)
                .map(|provider| provider.driver.clone())
        })
        .or_else(|| {
            input
                .thread_provider
                .map(|id| id.as_str())
                .and_then(|slug| ProviderDriverKind::new(slug).ok())
        });
    let selected_provider = input
        .selected_provider
        .and_then(|instance_id| {
            input
                .providers
                .iter()
                .find(|provider| provider.instance_id == instance_id)
                .map(|provider| provider.driver.clone())
        })
        .or_else(|| {
            input
                .selected_provider
                .map(|id| id.as_str())
                .and_then(|slug| ProviderDriverKind::new(slug).ok())
        });
    thread_provider.or(selected_provider)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeriveLockedProviderInput<'a> {
    pub thread: Option<ThreadStartedView<'a>>,
    pub selected_provider: Option<&'a ProviderInstanceId>,
    pub thread_provider: Option<&'a ProviderInstanceId>,
    pub providers: &'a [ProviderDriverLookup<'a>],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::{EnvironmentThreadShell, RunStatus, fixtures};

    #[test]
    fn shell_counts_a_thread_with_a_user_message_but_no_latest_turn() {
        assert!(thread_shell_has_started(Some(ThreadShellStartedView {
            latest_run: None,
            latest_user_message_at: Some("2026-03-29T00:00:00.000Z"),
            runtime: None,
        })));
    }

    #[test]
    fn shell_counts_a_thread_with_a_live_runtime_and_nothing_else() {
        let runtime = ThreadRuntimeSummary {
            status: RunStatus::Starting,
            active_run_id: None,
            activity_started_at: None,
            provider_instance_id: "codex".to_owned(),
            provider_name: Some("codex".to_owned()),
            last_error: None,
            last_error_class: None,
            updated_at: "2026-03-29T00:00:10.000Z".to_owned(),
        };
        assert!(thread_shell_has_started(Some(ThreadShellStartedView {
            latest_run: None,
            latest_user_message_at: None,
            runtime: Some(&runtime),
        })));
    }

    #[test]
    fn shell_does_not_count_a_thread_that_never_sent_anything() {
        assert!(!thread_shell_has_started(Some(ThreadShellStartedView {
            latest_run: None,
            latest_user_message_at: None,
            runtime: None,
        })));
        assert!(!thread_shell_has_started(None));
    }

    #[test]
    fn locked_provider_leaves_a_new_draft_free_to_select_a_different_driver() {
        let codex = ProviderInstanceId::new("codex_work").unwrap();
        let claude = ProviderInstanceId::new("claude_work").unwrap();
        let providers = [
            ProviderDriverLookup {
                instance_id: &claude,
                driver: ProviderDriverKind::new("claudeAgent").unwrap(),
            },
            ProviderDriverLookup {
                instance_id: &codex,
                driver: ProviderDriverKind::new("codex").unwrap(),
            },
        ];
        let shell = EnvironmentThreadShell::new(
            fixtures::env("environment-local"),
            fixtures::thread_id("thread-1"),
            fixtures::project_id("project-1"),
            "Thread",
            "2026-03-29T00:00:00.000Z",
        );
        assert_eq!(
            derive_locked_provider(DeriveLockedProviderInput {
                thread: Some((&shell).into()),
                selected_provider: Some(&codex),
                thread_provider: Some(&claude),
                providers: &providers,
            }),
            None
        );
    }
}
