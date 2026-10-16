//! Port of upstream/apps/web/src/threadRoutes.ts (+ threadRoutes.test.ts).

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ThreadId};

use crate::state::shell_state::EnvironmentShellStatus;

/// `ThreadRouteTarget`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadRouteTarget {
    Server { thread_ref: ScopedThreadRef },
    Draft { draft_id: String },
}

/// Draft route state while a promotion is in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftThreadRouteState {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub promoted_to: Option<ScopedThreadRef>,
}

/// `ThreadRouteRenderState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRouteRenderState {
    Loading,
    Ready,
    Missing,
}

/// `isThreadRouteSnapshotAuthoritative`.
pub fn is_thread_route_snapshot_authoritative(status: EnvironmentShellStatus) -> bool {
    status == EnvironmentShellStatus::Live
}

/// `resolveThreadRouteRenderState`.
pub fn resolve_thread_route_render_state(
    input: ResolveThreadRouteRenderStateInput,
) -> ThreadRouteRenderState {
    if input.draft_thread_exists || (input.server_thread_exists && !input.server_thread_deleted) {
        return ThreadRouteRenderState::Ready;
    }
    if !input.bootstrap_complete {
        return ThreadRouteRenderState::Loading;
    }
    ThreadRouteRenderState::Missing
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolveThreadRouteRenderStateInput {
    pub bootstrap_complete: bool,
    pub server_thread_exists: bool,
    pub server_thread_deleted: bool,
    pub draft_thread_exists: bool,
}

/// `buildThreadRouteParams`.
pub fn build_thread_route_params(thread_ref: &ScopedThreadRef) -> ThreadRouteParams {
    ThreadRouteParams {
        environment_id: thread_ref.environment_id.clone(),
        thread_id: thread_ref.thread_id.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRouteParams {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
}

/// `buildDraftThreadRouteParams`.
pub fn build_draft_thread_route_params(draft_id: &str) -> DraftThreadRouteParams {
    DraftThreadRouteParams {
        draft_id: draft_id.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftThreadRouteParams {
    pub draft_id: String,
}

/// Route params from in-app navigation (no URL bar).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadRouteParamsPartial {
    pub environment_id: Option<String>,
    pub thread_id: Option<String>,
    pub draft_id: Option<String>,
}

/// `resolveThreadRouteRef`.
pub fn resolve_thread_route_ref(params: &ThreadRouteParamsPartial) -> Option<ScopedThreadRef> {
    let environment_id = params.environment_id.as_ref()?;
    let thread_id = params.thread_id.as_ref()?;
    Some(ScopedThreadRef {
        environment_id: EnvironmentId::new(environment_id).ok()?,
        thread_id: ThreadId::new(thread_id).ok()?,
    })
}

/// `resolveThreadRouteTarget`.
pub fn resolve_thread_route_target(params: &ThreadRouteParamsPartial) -> Option<ThreadRouteTarget> {
    if let (Some(environment_id), Some(thread_id)) = (&params.environment_id, &params.thread_id) {
        return Some(ThreadRouteTarget::Server {
            thread_ref: ScopedThreadRef {
                environment_id: EnvironmentId::new(environment_id).ok()?,
                thread_id: ThreadId::new(thread_id).ok()?,
            },
        });
    }
    let draft_id = params.draft_id.as_ref()?;
    Some(ThreadRouteTarget::Draft {
        draft_id: draft_id.clone(),
    })
}

/// `resolveActiveThreadRouteRef`.
pub fn resolve_active_thread_route_ref(
    target: Option<&ThreadRouteTarget>,
    draft_thread: Option<&DraftThreadRouteState>,
) -> Option<ScopedThreadRef> {
    match target? {
        ThreadRouteTarget::Server { thread_ref } => Some(thread_ref.clone()),
        ThreadRouteTarget::Draft { .. } => draft_thread?.promoted_to.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::{EnvironmentId, ThreadId};

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).unwrap()
    }

    fn thread(id: &str) -> ThreadId {
        ThreadId::new(id).unwrap()
    }

    fn scoped(environment_id: &str, thread_id: &str) -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: env(environment_id),
            thread_id: thread(id),
        }
    }

    #[test]
    fn snapshot_authority_matches_upstream() {
        assert!(!is_thread_route_snapshot_authoritative(
            EnvironmentShellStatus::Empty
        ));
        assert!(!is_thread_route_snapshot_authoritative(
            EnvironmentShellStatus::Cached
        ));
        assert!(!is_thread_route_snapshot_authoritative(
            EnvironmentShellStatus::Synchronizing
        ));
        assert!(is_thread_route_snapshot_authoritative(
            EnvironmentShellStatus::Live
        ));
    }

    #[test]
    fn builds_and_resolves_thread_route_params() {
        let reference = scoped("env-1", "thread-1");
        assert_eq!(
            build_thread_route_params(&reference),
            ThreadRouteParams {
                environment_id: env("env-1"),
                thread_id: thread("thread-1"),
            }
        );
        assert_eq!(
            resolve_thread_route_ref(&ThreadRouteParamsPartial {
                environment_id: Some("env-1".into()),
                thread_id: Some("thread-1".into()),
                draft_id: None,
            }),
            Some(reference.clone())
        );
        assert_eq!(
            resolve_thread_route_ref(&ThreadRouteParamsPartial {
                environment_id: Some("env-1".into()),
                thread_id: None,
                draft_id: None,
            }),
            None
        );
        assert_eq!(
            resolve_thread_route_ref(&ThreadRouteParamsPartial {
                environment_id: None,
                thread_id: Some("thread-1".into()),
                draft_id: None,
            }),
            None
        );
    }

    #[test]
    fn resolves_draft_and_server_targets() {
        assert_eq!(
            resolve_thread_route_target(&ThreadRouteParamsPartial {
                environment_id: Some("env-1".into()),
                thread_id: Some("thread-1".into()),
                draft_id: None,
            }),
            Some(ThreadRouteTarget::Server {
                thread_ref: scoped("env-1", "thread-1"),
            })
        );
        assert_eq!(
            resolve_thread_route_target(&ThreadRouteParamsPartial {
                environment_id: None,
                thread_id: None,
                draft_id: Some("draft-1".into()),
            }),
            Some(ThreadRouteTarget::Draft {
                draft_id: "draft-1".into(),
            })
        );
    }

    #[test]
    fn active_ref_during_draft_promotion() {
        let target = resolve_thread_route_target(&ThreadRouteParamsPartial {
            environment_id: None,
            thread_id: None,
            draft_id: Some("draft-1".into()),
        })
        .unwrap();
        assert_eq!(
            resolve_active_thread_route_ref(
                Some(&target),
                Some(&DraftThreadRouteState {
                    environment_id: env("env-1"),
                    thread_id: thread("draft-thread"),
                    promoted_to: Some(scoped("env-2", "server-thread")),
                }),
            ),
            Some(scoped("env-2", "server-thread"))
        );
        assert_eq!(
            resolve_active_thread_route_ref(
                Some(&target),
                Some(&DraftThreadRouteState {
                    environment_id: env("env-1"),
                    thread_id: thread("draft-thread"),
                    promoted_to: None,
                }),
            ),
            None
        );
    }

    #[test]
    fn render_state_cases() {
        let ready = ResolveThreadRouteRenderStateInput {
            bootstrap_complete: true,
            server_thread_exists: true,
            server_thread_deleted: false,
            draft_thread_exists: false,
        };
        assert_eq!(
            resolve_thread_route_render_state(ready),
            ThreadRouteRenderState::Ready
        );
        assert_eq!(
            resolve_thread_route_render_state(ResolveThreadRouteRenderStateInput {
                draft_thread_exists: true,
                ..ready
            }),
            ThreadRouteRenderState::Ready
        );
        assert_eq!(
            resolve_thread_route_render_state(ResolveThreadRouteRenderStateInput {
                bootstrap_complete: false,
                server_thread_exists: false,
                server_thread_deleted: false,
                draft_thread_exists: false,
            }),
            ThreadRouteRenderState::Loading
        );
        assert_eq!(
            resolve_thread_route_render_state(ResolveThreadRouteRenderStateInput {
                bootstrap_complete: true,
                server_thread_exists: false,
                server_thread_deleted: false,
                draft_thread_exists: false,
            }),
            ThreadRouteRenderState::Missing
        );
        assert_eq!(
            resolve_thread_route_render_state(ResolveThreadRouteRenderStateInput {
                bootstrap_complete: true,
                server_thread_exists: true,
                server_thread_deleted: true,
                draft_thread_exists: false,
            }),
            ThreadRouteRenderState::Missing
        );
        assert_eq!(
            resolve_thread_route_render_state(ResolveThreadRouteRenderStateInput {
                bootstrap_complete: is_thread_route_snapshot_authoritative(
                    EnvironmentShellStatus::Cached,
                ),
                server_thread_exists: true,
                server_thread_deleted: true,
                draft_thread_exists: false,
            }),
            ThreadRouteRenderState::Loading
        );
    }
}
