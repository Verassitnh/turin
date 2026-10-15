//! Port of the pure parts of upstream/apps/web/src/state/sourceControlActions.ts
//!
//! The upstream module is React hooks over atoms and the environment session. The hooks stay in
//! the UI; the pieces they decide with are pure and live here: which vcs operation an action
//! runs as, the label it shows while running, whether a scope can run it, and whether one of a
//! set of actions is in flight. The source-control write permission gate went with the auth
//! scopes (PORTING.md cut list).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceControlActionKind {
    Init,
    Pull,
    PublishRepository,
    RunStackedAction,
    PreparePullRequestThread,
}

impl SourceControlActionKind {
    /// The `VcsActionOperation` this action runs as; the manager's running state is keyed by it.
    pub fn operation(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Pull => "pull",
            Self::PublishRepository => "publish_repository",
            Self::RunStackedAction => "run_change_request",
            Self::PreparePullRequestThread => "prepare_pull_request_thread",
        }
    }

    /// The label the action tracks while it runs. `RunStackedAction`'s label is the generic one
    /// the progress presentation treats as "no phase label yet".
    pub fn label(self) -> &'static str {
        match self {
            Self::Init => "Initializing repository",
            Self::Pull => "Pulling latest changes...",
            Self::PublishRepository => "Publishing repository",
            Self::RunStackedAction => "Running source control action",
            Self::PreparePullRequestThread => "Preparing pull request thread",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceControlActionScope {
    pub environment_id: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScope {
    pub environment_id: String,
    pub cwd: String,
}

/// `resolveScope`: an action needs both an environment and a checkout to run against.
pub fn resolve_scope(scope: &SourceControlActionScope) -> Option<ResolvedScope> {
    Some(ResolvedScope {
        environment_id: scope.environment_id.clone()?,
        cwd: scope.cwd.clone()?,
    })
}

/// `useSourceControlActionRunning`: whether the tracked operation is one of `kinds`.
pub fn is_action_running(
    is_running: bool,
    operation: Option<&str>,
    kinds: &[SourceControlActionKind],
) -> bool {
    is_running
        && operation.is_some()
        && kinds.iter().any(|kind| Some(kind.operation()) == operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [SourceControlActionKind; 5] = [
        SourceControlActionKind::Init,
        SourceControlActionKind::Pull,
        SourceControlActionKind::PublishRepository,
        SourceControlActionKind::RunStackedAction,
        SourceControlActionKind::PreparePullRequestThread,
    ];

    #[test]
    fn each_action_runs_as_its_vcs_operation() {
        let operations: Vec<&str> = ALL.iter().map(|kind| kind.operation()).collect();
        assert_eq!(
            operations,
            vec![
                "init",
                "pull",
                "publish_repository",
                "run_change_request",
                "prepare_pull_request_thread",
            ]
        );
    }

    #[test]
    fn running_check_matches_only_the_tracked_operation_in_the_given_kinds() {
        let pull = [SourceControlActionKind::Pull];
        assert!(is_action_running(true, Some("pull"), &pull));
        assert!(!is_action_running(true, Some("init"), &pull));
        assert!(!is_action_running(false, Some("pull"), &pull));
        assert!(!is_action_running(true, None, &pull));
        assert!(!is_action_running(true, Some("pull"), &[]));
    }

    #[test]
    fn scope_resolves_only_with_both_an_environment_and_a_checkout() {
        let complete = SourceControlActionScope {
            environment_id: Some("secondary".to_owned()),
            cwd: Some("/repo".to_owned()),
        };
        assert_eq!(
            resolve_scope(&complete),
            Some(ResolvedScope {
                environment_id: "secondary".to_owned(),
                cwd: "/repo".to_owned(),
            })
        );
        assert_eq!(
            resolve_scope(&SourceControlActionScope {
                cwd: None,
                ..complete.clone()
            }),
            None
        );
        assert_eq!(
            resolve_scope(&SourceControlActionScope {
                environment_id: None,
                ..complete
            }),
            None
        );
    }
}
