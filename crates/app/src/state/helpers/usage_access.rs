//! Port of upstream/packages/client-runtime/src/state/usageAccess.ts
//!
//! Auth is cut from Turin (single process, no sessions), so the session grant shape and the
//! scope check it needs are local copies of `packages/contracts/src/auth.ts`
//! (`SessionGrantInput`, `sessionGrantsScope`, the legacy-parent table). `resolve_usage_access`
//! keeps upstream's decision table for callers that still model a remote environment.

/// `AuthDiagnosticsReadScope`.
pub const AUTH_DIAGNOSTICS_READ_SCOPE: &str = "diagnostics:read";
/// `AuthOrchestrationReadScope`.
pub const AUTH_ORCHESTRATION_READ_SCOPE: &str = "orchestration:read";
const AUTH_ORCHESTRATION_OPERATE_SCOPE: &str = "orchestration:operate";
const AUTH_TERMINAL_OPERATE_SCOPE: &str = "terminal:operate";

/// `EnvironmentConnectionPhase` (connection/presentation.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentConnectionPhase {
    Available,
    Offline,
    Connecting,
    Reconnecting,
    Connected,
    Error,
    Unsupported,
}

/// `SessionGrantInput`. `scopes` and `permissions` are `None` where upstream leaves them
/// `undefined`; an explicitly empty `permissions` grants nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionGrantInput {
    pub authenticated: bool,
    pub scopes: Option<Vec<String>>,
    pub permissions: Option<Vec<String>>,
    /// `auth.serverUpdateScope`.
    pub server_update_scope: Option<String>,
}

/// `legacyParents`: only clients talking to an old server use these parent checks.
fn legacy_parent(scope: &str) -> Option<&'static str> {
    match scope {
        "filesystem:read" | "diagnostics:read" => Some(AUTH_ORCHESTRATION_READ_SCOPE),
        "settings:write"
        | "providers:manage"
        | "environment:maintain"
        | "preview:operate"
        | "source-control:write"
        | "filesystem:write" => Some(AUTH_ORCHESTRATION_OPERATE_SCOPE),
        "terminal:read" => Some(AUTH_TERMINAL_OPERATE_SCOPE),
        _ => None,
    }
}

/// `sessionGrantsScope`.
pub fn session_grants_scope(session: &SessionGrantInput, scope: &str) -> bool {
    if !session.authenticated {
        return false;
    }
    if let Some(permissions) = &session.permissions {
        return permissions.iter().any(|entry| entry == scope);
    }
    let has = |wanted: &str| {
        session
            .scopes
            .as_ref()
            .is_some_and(|scopes| scopes.iter().any(|entry| entry == wanted))
    };
    if has(scope) {
        return true;
    }
    // Also recognize servers from the first granular-scope release.
    if session.server_update_scope.is_some() {
        return false;
    }
    legacy_parent(scope).is_some_and(has)
}

/// `resolveUsageAccess` input.
#[derive(Debug, Clone)]
pub struct ResolveUsageAccessInput<'a> {
    pub connection_phase: EnvironmentConnectionPhase,
    pub session: Option<&'a SessionGrantInput>,
    pub has_session_error: bool,
}

/// `resolveUsageAccess` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAccess {
    pub can_read_diagnostics: bool,
    pub is_pending: bool,
    pub error: Option<String>,
}

pub fn resolve_usage_access(input: &ResolveUsageAccessInput<'_>) -> UsageAccess {
    if input.has_session_error {
        return UsageAccess {
            can_read_diagnostics: false,
            is_pending: false,
            error: Some(
                "Could not check this connection's access to diagnostics and usage.".to_owned(),
            ),
        };
    }
    let Some(session) = input.session else {
        // An unprepared offline connection cannot finish its session check. Keep
        // it out of pending totals until the connection starts another attempt.
        let is_pending = matches!(
            input.connection_phase,
            EnvironmentConnectionPhase::Connected
                | EnvironmentConnectionPhase::Connecting
                | EnvironmentConnectionPhase::Reconnecting
        );
        return UsageAccess {
            can_read_diagnostics: false,
            is_pending,
            error: (!is_pending).then(|| "This environment is not connected.".to_owned()),
        };
    };
    let can_read_diagnostics = session_grants_scope(session, AUTH_DIAGNOSTICS_READ_SCOPE);
    UsageAccess {
        can_read_diagnostics,
        is_pending: false,
        error: (!can_read_diagnostics)
            .then(|| "This connection does not have access to diagnostics and usage.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EnvironmentConnectionPhase::*;

    fn resolve(
        connection_phase: EnvironmentConnectionPhase,
        session: Option<&SessionGrantInput>,
        has_session_error: bool,
    ) -> UsageAccess {
        resolve_usage_access(&ResolveUsageAccessInput {
            connection_phase,
            session,
            has_session_error,
        })
    }

    #[test]
    fn reports_connections_without_a_session_instead_of_keeping_totals_pending() {
        for phase in [Available, Offline, Error] {
            assert_eq!(
                resolve(phase, None, false),
                UsageAccess {
                    can_read_diagnostics: false,
                    is_pending: false,
                    error: Some("This environment is not connected.".into()),
                },
                "{phase:?}"
            );
        }
    }

    #[test]
    fn waits_for_a_session_check_while_connecting() {
        for phase in [Connecting, Reconnecting, Connected] {
            assert_eq!(
                resolve(phase, None, false),
                UsageAccess {
                    can_read_diagnostics: false,
                    is_pending: true,
                    error: None,
                },
                "{phase:?}"
            );
        }
    }

    #[test]
    fn preserves_a_known_grant_so_disconnected_usage_can_retain_its_cached_summary() {
        let session = SessionGrantInput {
            authenticated: true,
            scopes: Some(vec![AUTH_DIAGNOSTICS_READ_SCOPE.into()]),
            ..Default::default()
        };
        assert_eq!(
            resolve(Offline, Some(&session), false),
            UsageAccess {
                can_read_diagnostics: true,
                is_pending: false,
                error: None,
            }
        );
    }

    #[test]
    fn distinguishes_a_failed_check_from_a_resolved_grant_without_diagnostics_access() {
        let session = SessionGrantInput {
            authenticated: true,
            scopes: Some(vec![AUTH_ORCHESTRATION_READ_SCOPE.into()]),
            server_update_scope: Some("environment:maintain".into()),
            ..Default::default()
        };
        let denied = resolve(Connected, Some(&session), false);
        let failed = resolve(Connected, None, true);
        assert_eq!(
            denied,
            UsageAccess {
                can_read_diagnostics: false,
                is_pending: false,
                error: Some(
                    "This connection does not have access to diagnostics and usage.".into()
                ),
            }
        );
        assert_eq!(
            failed,
            UsageAccess {
                can_read_diagnostics: false,
                is_pending: false,
                error: Some(
                    "Could not check this connection's access to diagnostics and usage.".into()
                ),
            }
        );
    }

    #[test]
    fn legacy_parent_scope_grants_diagnostics_only_from_pre_granular_servers() {
        let legacy = SessionGrantInput {
            authenticated: true,
            scopes: Some(vec![AUTH_ORCHESTRATION_READ_SCOPE.into()]),
            ..Default::default()
        };
        assert!(session_grants_scope(&legacy, AUTH_DIAGNOSTICS_READ_SCOPE));
        let empty_permissions = SessionGrantInput {
            permissions: Some(vec![]),
            ..legacy.clone()
        };
        assert!(!session_grants_scope(
            &empty_permissions,
            AUTH_DIAGNOSTICS_READ_SCOPE
        ));
        let unauthenticated = SessionGrantInput {
            authenticated: false,
            ..legacy
        };
        assert!(!session_grants_scope(
            &unauthenticated,
            AUTH_ORCHESTRATION_READ_SCOPE
        ));
    }
}
