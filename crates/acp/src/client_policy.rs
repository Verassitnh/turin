//! Port of upstream/packages/provider-acp/src/server/clientPolicy.ts
//!
//! Runtime-policy decisions for ACP work that T3 mediates.
//!
//! ACP agents run their own tools under their own permission model and ask
//! through `session/request_permission`, which T3 answers by policy. The one
//! client-mediated path left is Devin's `terminal/*`, which runs with the T3
//! server's own privileges; it resolves through [`acp_operation_disposition`]
//! so a client terminal can never do more than an execute permission request.

use std::fs;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use turin_contracts::provider_policy::{ProviderRequestKind, RuntimeMode};

/// Structural subset of the adapter runtime policy that decisions read.
#[derive(Debug, Clone, PartialEq)]
pub struct AcpRuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub cwd: Option<String>,
    /// `None` is upstream's `undefined`: the key is absent.
    pub approval_policy: Option<Value>,
    pub sandbox_policy: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpPermissionDisposition {
    Allow,
    Ask,
    Deny,
}

/// `ToolCallLocation`, reduced to the path the containment check reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    pub path: String,
}

/// The `toolCall` of a `RequestPermissionRequest`, reduced to the fields the policy reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionToolCall {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub locations: Option<Vec<ToolCallLocation>>,
}

/// The parts of a native operation the disposition logic reads.
struct PolicyOperation<'a> {
    kind: Option<&'a str>,
    locations: Option<&'a [ToolCallLocation]>,
}

fn unknown_record(value: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// The `type` of a sandbox policy, as the `switch` in upstream reads it.
/// `None` is `undefined`: no record, or no `type` key. A non-string `type`
/// is returned as `""`, which matches no case and so falls to `default`.
fn sandbox_type(sandbox_policy: Option<&Value>) -> Option<&str> {
    match unknown_record(sandbox_policy)?.get("type") {
        None => None,
        Some(Value::String(kind)) => Some(kind.as_str()),
        Some(_) => Some(""),
    }
}

/// Resolve a policy path against `cwd`, like upstream's `resolveAcpPermissionPath`.
fn resolve_acp_permission_path(path: &str, cwd: Option<&str>) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if Path::new(trimmed).is_absolute() {
        return Some(trimmed.to_owned());
    }
    let cwd = cwd.filter(|cwd| !cwd.trim().is_empty())?;
    let separator = if cwd.ends_with(MAIN_SEPARATOR) {
        ""
    } else {
        &MAIN_SEPARATOR.to_string()
    };
    Some(format!("{cwd}{separator}{trimmed}"))
}

/// `NodePath.relative(root, path)` is inside the root exactly when `path`
/// starts with `root` by whole components. Both sides are canonical here.
fn acp_path_is_within_root(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Canonicalize a path for an authorization containment check.
///
/// `realpath` cannot resolve a file that has not been created yet, so walk up
/// to the deepest existing ancestor and append the missing suffix to that
/// ancestor's canonical path. This follows symlinked directories while still
/// allowing normal writes to new files. If an existing entry cannot be
/// canonicalized (for example, a broken symlink), fail closed.
fn acp_canonical_path_for_containment(path: &str) -> Option<PathBuf> {
    // Do not lexically normalize before realpath. For a path such as
    // `workspace/link/../file`, the kernel resolves `link` before `..`; an
    // eager normalization would erase that symlink traversal and could turn
    // an outside target into an apparently in-workspace path.
    let mut candidate = PathBuf::from(path);
    let mut missing_suffix: Vec<std::ffi::OsString> = Vec::new();

    loop {
        match fs::canonicalize(&candidate) {
            Ok(canonical) => {
                let mut resolved = canonical;
                for component in &missing_suffix {
                    resolved.push(component);
                }
                return Some(resolved);
            }
            Err(cause) if cause.raw_os_error() == Some(libc::ENOENT) => {}
            Err(_) => return None,
        }

        match fs::symlink_metadata(&candidate) {
            // The entry exists but realpath could not resolve it, as with a
            // broken symlink. Treat it as untrusted rather than authorizing
            // its lexical path.
            Ok(_) => return None,
            Err(cause) if cause.raw_os_error() == Some(libc::ENOENT) => {}
            Err(_) => return None,
        }

        let parent = candidate.parent()?.to_path_buf();
        if parent == candidate {
            return None;
        }
        missing_suffix.insert(0, candidate.file_name()?.to_os_string());
        candidate = parent;
    }
}

fn acp_policy_requires_approval(runtime_policy: &AcpRuntimePolicy) -> bool {
    match &runtime_policy.approval_policy {
        None => runtime_policy.runtime_mode == RuntimeMode::ApprovalRequired,
        Some(policy) => policy.as_str() != Some("never"),
    }
}

fn is_acp_read_kind(tool_kind: &str) -> bool {
    matches!(tool_kind, "read" | "search" | "think")
}

fn is_acp_mutation_kind(tool_kind: &str) -> bool {
    matches!(tool_kind, "edit" | "delete" | "move")
}

/// Reads follow the sandbox alone and never ask. Approval policy governs writes
/// and commands, as it does for Codex and Claude, and every sandbox T3 knows
/// lets the agent read. That includes no explicit sandbox, since the strictest
/// runtime mode (approval-required) implies a read-only one. Unknown sandbox
/// types still fail closed.
fn acp_read_disposition(runtime_policy: &AcpRuntimePolicy) -> AcpPermissionDisposition {
    match sandbox_type(runtime_policy.sandbox_policy.as_ref()) {
        None | Some("readOnly" | "workspaceWrite" | "dangerFullAccess" | "externalSandbox") => {
            AcpPermissionDisposition::Allow
        }
        Some(_) => AcpPermissionDisposition::Deny,
    }
}

fn acp_workspace_write_allows_mutation(
    runtime_policy: &AcpRuntimePolicy,
    sandbox_policy: &serde_json::Map<String, Value>,
    locations: Option<&[ToolCallLocation]>,
) -> bool {
    let cwd = runtime_policy
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.trim().is_empty())
        .and_then(|cwd| {
            let process_cwd = std::env::current_dir()
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default();
            resolve_acp_permission_path(cwd, Some(&process_cwd))
        });
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(cwd) = &cwd
        && let Some(canonical_cwd) = acp_canonical_path_for_containment(cwd)
    {
        roots.push(canonical_cwd);
    }
    if let Some(Value::Array(writable_roots)) = sandbox_policy.get("writableRoots") {
        for writable_root in writable_roots {
            let Some(writable_root) = writable_root.as_str() else {
                continue;
            };
            let Some(resolved) = resolve_acp_permission_path(writable_root, cwd.as_deref()) else {
                continue;
            };
            if let Some(canonical_root) = acp_canonical_path_for_containment(&resolved) {
                roots.push(canonical_root);
            }
        }
    }
    if roots.is_empty() {
        return false;
    }

    let Some(locations) = locations.filter(|locations| !locations.is_empty()) else {
        return false;
    };
    locations.iter().all(|location| {
        let canonical_path = resolve_acp_permission_path(&location.path, cwd.as_deref())
            .and_then(|resolved| acp_canonical_path_for_containment(&resolved));
        match canonical_path {
            Some(canonical_path) => roots
                .iter()
                .any(|root| acp_path_is_within_root(&canonical_path, root)),
            None => false,
        }
    })
}

/// Disposition of one native operation under the active runtime policy. Shared
/// by provider permission requests and the client fs/terminal handlers.
fn acp_operation_disposition(
    runtime_policy: &AcpRuntimePolicy,
    operation: PolicyOperation<'_>,
) -> AcpPermissionDisposition {
    let tool_kind = operation.kind.unwrap_or("other");
    if is_acp_read_kind(tool_kind) {
        return acp_read_disposition(runtime_policy);
    }
    if acp_policy_requires_approval(runtime_policy) {
        return AcpPermissionDisposition::Ask;
    }

    match sandbox_type(runtime_policy.sandbox_policy.as_ref()) {
        Some("readOnly") => AcpPermissionDisposition::Deny,
        Some("workspaceWrite") => {
            let sandbox = unknown_record(runtime_policy.sandbox_policy.as_ref())
                .expect("workspaceWrite sandbox is a record");
            if is_acp_mutation_kind(tool_kind)
                && acp_workspace_write_allows_mutation(runtime_policy, sandbox, operation.locations)
            {
                AcpPermissionDisposition::Allow
            } else {
                AcpPermissionDisposition::Deny
            }
        }
        Some("dangerFullAccess" | "externalSandbox") => AcpPermissionDisposition::Allow,
        None => {
            if runtime_policy.runtime_mode == RuntimeMode::ApprovalRequired {
                return AcpPermissionDisposition::Deny;
            }
            // Auto-accept edits approves file changes wherever the agent makes them
            // (ACP prompts need not carry locations to confine); other actions still ask.
            if runtime_policy.runtime_mode == RuntimeMode::AutoAcceptEdits
                && runtime_policy.approval_policy.is_none()
                && !is_acp_mutation_kind(tool_kind)
            {
                return AcpPermissionDisposition::Ask;
            }
            AcpPermissionDisposition::Allow
        }
        Some(_) => AcpPermissionDisposition::Deny,
    }
}

pub fn acp_permission_disposition(
    runtime_policy: &AcpRuntimePolicy,
    tool_call: &PermissionToolCall,
) -> AcpPermissionDisposition {
    acp_operation_disposition(
        runtime_policy,
        PolicyOperation {
            kind: tool_call.kind.as_deref(),
            locations: tool_call.locations.as_deref(),
        },
    )
}

/// Resolve explicitly tagged MCP approvals through the thread's normal policy.
///
/// `mode` is the elicitation mode (`"form"` or `"url"`), `meta` its `_meta`.
/// Returns `None` when the request is not a tagged MCP tool approval.
pub fn acp_mcp_tool_approval_elicitation_disposition(
    runtime_policy: &AcpRuntimePolicy,
    mode: &str,
    meta: Option<&Value>,
    native_request_id: Option<&str>,
) -> Option<AcpPermissionDisposition> {
    let tagged = unknown_record(meta)
        .and_then(|meta| meta.get("codex_approval_kind"))
        .and_then(Value::as_str)
        == Some("mcp_tool_call");
    let native_tagged =
        native_request_id.is_some_and(|id| id.starts_with("mcp_tool_call_approval_"));
    if mode != "form" || (!tagged && !native_tagged) {
        return None;
    }
    // This request comes from T3's authenticated, scope-checked MCP endpoint,
    // not an arbitrary provider command. Let explicit approval mode surface it
    // to the user and otherwise allow the endpoint to enforce its own policy.
    Some(if acp_policy_requires_approval(runtime_policy) {
        AcpPermissionDisposition::Ask
    } else {
        AcpPermissionDisposition::Allow
    })
}

/// Disposition of a client-mediated `terminal/create` (Devin's client terminals).
pub fn acp_client_execute_disposition(
    runtime_policy: &AcpRuntimePolicy,
) -> AcpPermissionDisposition {
    acp_operation_disposition(
        runtime_policy,
        PolicyOperation {
            kind: Some("execute"),
            locations: None,
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpApprovalGrantInput<'a> {
    pub kind: ProviderRequestKind,
    pub scope: ApprovalScope,
    pub turn_key: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalScope {
    Session,
    Turn,
}

/// Command approvals recorded when the user accepts a `session/request_permission`,
/// so an "ask" disposition at the client terminal boundary honors a command the
/// user already approved. ACP does not link an approved tool call to the
/// terminals the agent then creates, so an approved command authorizes client
/// terminals for the approving turn, or for the session on accept-for-session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AcpClientPolicyGrants {
    session_execute: bool,
    turn_execute: Option<String>,
}

impl AcpClientPolicyGrants {
    pub fn record_approval(&mut self, input: &AcpApprovalGrantInput<'_>) {
        if input.kind != ProviderRequestKind::Command {
            return;
        }
        match input.scope {
            ApprovalScope::Session => self.session_execute = true,
            ApprovalScope::Turn => self.turn_execute = Some(input.turn_key.to_owned()),
        }
    }

    pub fn allows_execute(&self, turn_key: Option<&str>) -> bool {
        self.session_execute
            || turn_key.is_some_and(|turn_key| self.turn_execute.as_deref() == Some(turn_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn current_dir_join(name: &str) -> String {
        std::env::current_dir()
            .expect("current dir")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn permission_request<S: AsRef<str>>(
        kind: &str,
        locations: Option<&[S]>,
    ) -> PermissionToolCall {
        PermissionToolCall {
            kind: Some(kind.to_owned()),
            locations: locations.map(|paths| {
                paths
                    .iter()
                    .map(|path| ToolCallLocation {
                        path: path.as_ref().to_owned(),
                    })
                    .collect()
            }),
        }
    }

    fn disposition(policy: &AcpRuntimePolicy, kind: &str) -> AcpPermissionDisposition {
        acp_permission_disposition(policy, &permission_request::<&str>(kind, None))
    }

    fn policy(runtime_mode: RuntimeMode, cwd: &str) -> AcpRuntimePolicy {
        AcpRuntimePolicy {
            runtime_mode,
            cwd: Some(cwd.to_owned()),
            approval_policy: None,
            sandbox_policy: None,
        }
    }

    fn workspace_write(writable_roots: Value) -> Value {
        json!({
            "type": "workspaceWrite",
            "writableRoots": writable_roots,
            "networkAccess": false,
        })
    }

    use AcpPermissionDisposition::{Allow, Ask, Deny};

    #[test]
    fn auto_allows_mutations_only_when_every_location_is_in_cwd_or_an_additional_writable_root() {
        let cwd = current_dir_join("acp-permission-workspace");
        let writable_root = current_dir_join("acp-additional-writable-root");
        let policy = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            cwd: Some(cwd.clone()),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([writable_root.clone()]))),
        };

        let edit = permission_request("edit", Some(&["src/index.ts"]));
        assert_eq!(acp_permission_disposition(&policy, &edit), Allow);
        let delete =
            permission_request("delete", Some(&[&format!("{writable_root}/generated.ts")]));
        assert_eq!(acp_permission_disposition(&policy, &delete), Allow);
        let move_request = permission_request(
            "move",
            Some(&[&format!("{cwd}/from.ts"), &format!("{writable_root}/to.ts")]),
        );
        assert_eq!(acp_permission_disposition(&policy, &move_request), Allow);
    }

    #[test]
    fn denies_missing_or_out_of_root_mutation_locations() {
        let cwd = current_dir_join("acp-permission-workspace");
        let writable_root = current_dir_join("acp-additional-writable-root");
        let outside = current_dir_join("outside-acp-permission-workspace/file.ts");
        let policy = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            cwd: Some(cwd.clone()),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([writable_root]))),
        };

        assert_eq!(
            acp_permission_disposition(&policy, &permission_request::<&str>("edit", None)),
            Deny
        );
        assert_eq!(
            acp_permission_disposition(
                &policy,
                &permission_request("delete", Some(&["../escape.ts"]))
            ),
            Deny
        );
        let move_request =
            permission_request("move", Some(&[&format!("{cwd}/inside.ts"), &outside]));
        assert_eq!(acp_permission_disposition(&policy, &move_request), Deny);
    }

    #[test]
    fn keeps_non_mutating_workspace_permissions_and_denials_unchanged() {
        let cwd = current_dir_join("acp-permission-workspace");
        let policy = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            cwd: Some(cwd),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([]))),
        };

        assert_eq!(disposition(&policy, "read"), Allow);
        assert_eq!(disposition(&policy, "execute"), Deny);
    }

    #[test]
    fn auto_accept_edits_approves_file_changes_without_locations_and_asks_for_the_rest() {
        // ACP permission requests need not carry locations.
        let cwd = current_dir_join("acp-permission-workspace");
        let auto_accept_edits = policy(RuntimeMode::AutoAcceptEdits, &cwd);
        for kind in ["edit", "delete", "move"] {
            assert_eq!(disposition(&auto_accept_edits, kind), Allow, "{kind}");
        }
        for kind in ["execute", "fetch", "other"] {
            assert_eq!(disposition(&auto_accept_edits, kind), Ask, "{kind}");
        }
        assert_eq!(disposition(&auto_accept_edits, "read"), Allow);
        let on_request = AcpRuntimePolicy {
            approval_policy: Some(json!("on-request")),
            ..auto_accept_edits
        };
        assert_eq!(disposition(&on_request, "edit"), Ask);
    }

    #[test]
    fn auto_allows_read_kind_permission_requests_under_on_request_approval() {
        let cwd = current_dir_join("acp-permission-workspace");
        let workspace = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            cwd: Some(cwd.clone()),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([current_dir_join(
                "acp-additional-writable-root"
            )]))),
        };
        let policies = [
            AcpRuntimePolicy {
                approval_policy: Some(json!("on-request")),
                ..workspace.clone()
            },
            AcpRuntimePolicy {
                approval_policy: Some(json!("on-request")),
                sandbox_policy: Some(json!({ "type": "readOnly" })),
                ..workspace
            },
            policy(RuntimeMode::ApprovalRequired, &cwd),
        ];
        for runtime_policy in &policies {
            for kind in ["read", "search", "think"] {
                assert_eq!(disposition(runtime_policy, kind), Allow, "{kind}");
            }
            for kind in ["edit", "delete", "move", "execute", "fetch", "other"] {
                assert_eq!(disposition(runtime_policy, kind), Ask, "{kind}");
            }
        }
    }

    #[test]
    fn denies_mutations_through_workspace_symlinks_that_escape_the_writable_roots() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("existing.ts");
        fs::write(&outside_file, "outside").expect("outside file");
        std::os::unix::fs::symlink(&outside_file, workspace.path().join("linked-file.ts"))
            .expect("file symlink");
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked-directory"))
            .expect("directory symlink");

        let real_policy = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([]))),
        };

        assert_eq!(
            acp_permission_disposition(
                &real_policy,
                &permission_request("edit", Some(&["linked-file.ts"]))
            ),
            Deny
        );
        // A missing leaf below an escaping directory symlink must not be auto-approved.
        assert_eq!(
            acp_permission_disposition(
                &real_policy,
                &permission_request("edit", Some(&["linked-directory/new-file.ts"]))
            ),
            Deny
        );
        // Physical symlink traversal must be resolved before parent segments.
        assert_eq!(
            acp_permission_disposition(
                &real_policy,
                &permission_request("edit", Some(&["linked-directory/../escaped-file.ts"]))
            ),
            Deny
        );
    }

    #[test]
    fn allows_existing_and_new_files_beneath_canonical_writable_roots() {
        let workspace = tempfile::tempdir().expect("workspace");
        let link_parent = tempfile::tempdir().expect("link parent");
        let workspace_link = link_parent.path().join("workspace-link");
        fs::create_dir_all(workspace.path().join("src")).expect("src");
        fs::write(workspace.path().join("src").join("existing.ts"), "existing").expect("file");
        std::os::unix::fs::symlink(workspace.path(), &workspace_link).expect("workspace link");

        let real_policy = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            cwd: Some(workspace_link.to_string_lossy().into_owned()),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([]))),
        };

        assert_eq!(
            acp_permission_disposition(
                &real_policy,
                &permission_request("edit", Some(&["src/existing.ts"]))
            ),
            Allow
        );
        // Non-existent descendants of a real in-root ancestor remain writable.
        assert_eq!(
            acp_permission_disposition(
                &real_policy,
                &permission_request("edit", Some(&["src/generated/new-file.ts"]))
            ),
            Allow
        );
    }

    #[test]
    fn applies_runtime_policy_only_to_explicitly_tagged_mcp_approval_elicitations() {
        let cwd = std::env::current_dir()
            .expect("current dir")
            .to_string_lossy()
            .into_owned();
        let full_access = policy(RuntimeMode::FullAccess, &cwd);
        let approval_required = policy(RuntimeMode::ApprovalRequired, &cwd);
        let tagged_meta = json!({ "codex_approval_kind": "mcp_tool_call" });

        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &full_access,
                "form",
                Some(&tagged_meta),
                None
            ),
            Some(Allow)
        );
        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &approval_required,
                "form",
                Some(&tagged_meta),
                None
            ),
            Some(Ask)
        );
        let on_request = AcpRuntimePolicy {
            approval_policy: Some(json!("on-request")),
            ..full_access.clone()
        };
        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &on_request,
                "form",
                Some(&tagged_meta),
                None
            ),
            Some(Ask)
        );
        let never_workspace = AcpRuntimePolicy {
            runtime_mode: RuntimeMode::AutoAcceptEdits,
            cwd: Some(cwd.clone()),
            approval_policy: Some(json!("never")),
            sandbox_policy: Some(workspace_write(json!([]))),
        };
        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &never_workspace,
                "form",
                Some(&tagged_meta),
                None
            ),
            Some(Allow)
        );
        // An untagged form is recognized by its native request id prefix.
        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &full_access,
                "form",
                None,
                Some("mcp_tool_call_approval_exec-123")
            ),
            Some(Allow)
        );
        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &full_access,
                "form",
                Some(&json!({ "codex_approval_kind": "ordinary_form" })),
                None
            ),
            None
        );
        // URL elicitations (authentication) are never MCP tool approvals.
        assert_eq!(
            acp_mcp_tool_approval_elicitation_disposition(
                &full_access,
                "url",
                Some(&tagged_meta),
                None
            ),
            None
        );
    }

    #[test]
    fn client_terminal_asks_in_approval_required_and_auto_accept_edits() {
        let cwd = current_dir_join("acp-client-policy-workspace");
        assert_eq!(
            acp_client_execute_disposition(&policy(RuntimeMode::ApprovalRequired, &cwd)),
            Ask
        );
        assert_eq!(
            acp_client_execute_disposition(&policy(RuntimeMode::AutoAcceptEdits, &cwd)),
            Ask
        );
        assert_eq!(
            acp_client_execute_disposition(&policy(RuntimeMode::Auto, &cwd)),
            Allow
        );
        assert_eq!(
            acp_client_execute_disposition(&policy(RuntimeMode::FullAccess, &cwd)),
            Allow
        );
    }

    #[test]
    fn client_terminal_denies_under_explicit_read_only_and_workspace_write_sandboxes() {
        let cwd = current_dir_join("acp-client-policy-workspace");
        for sandbox_policy in [json!({ "type": "readOnly" }), workspace_write(json!([]))] {
            let runtime_policy = AcpRuntimePolicy {
                runtime_mode: RuntimeMode::FullAccess,
                cwd: Some(cwd.clone()),
                approval_policy: Some(json!("never")),
                sandbox_policy: Some(sandbox_policy),
            };
            assert_eq!(acp_client_execute_disposition(&runtime_policy), Deny);
        }
    }

    #[test]
    fn grants_terminals_only_from_commands_never_from_an_approved_read_or_edit() {
        let mut grants = AcpClientPolicyGrants::default();
        grants.record_approval(&AcpApprovalGrantInput {
            kind: ProviderRequestKind::FileRead,
            scope: ApprovalScope::Turn,
            turn_key: "turn-1",
        });
        grants.record_approval(&AcpApprovalGrantInput {
            kind: ProviderRequestKind::FileChange,
            scope: ApprovalScope::Session,
            turn_key: "turn-1",
        });
        assert!(!grants.allows_execute(Some("turn-1")));
        grants.record_approval(&AcpApprovalGrantInput {
            kind: ProviderRequestKind::Command,
            scope: ApprovalScope::Turn,
            turn_key: "turn-1",
        });
        assert!(grants.allows_execute(Some("turn-1")));
        assert!(!grants.allows_execute(Some("turn-2")));
        assert!(!grants.allows_execute(None));
    }

    #[test]
    fn keeps_accept_for_session_command_grants_across_turns() {
        let mut grants = AcpClientPolicyGrants::default();
        grants.record_approval(&AcpApprovalGrantInput {
            kind: ProviderRequestKind::Command,
            scope: ApprovalScope::Session,
            turn_key: "turn-1",
        });
        assert!(grants.allows_execute(Some("turn-9")));
        assert!(grants.allows_execute(None));
    }

    #[test]
    fn drops_a_turns_command_grant_when_a_later_turn_approves_one() {
        let mut grants = AcpClientPolicyGrants::default();
        grants.record_approval(&AcpApprovalGrantInput {
            kind: ProviderRequestKind::Command,
            scope: ApprovalScope::Turn,
            turn_key: "turn-1",
        });
        grants.record_approval(&AcpApprovalGrantInput {
            kind: ProviderRequestKind::Command,
            scope: ApprovalScope::Turn,
            turn_key: "turn-2",
        });
        assert!(!grants.allows_execute(Some("turn-1")));
        assert!(grants.allows_execute(Some("turn-2")));
    }
}
