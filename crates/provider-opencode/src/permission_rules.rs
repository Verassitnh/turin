//! Session permission rules from `server/v2/adapter.ts` `sessionRules`.
//!
//! Order is mode rules, then session grants, then the plan edit-deny, then
//! agent and plan path allows. The last matching rule wins. Mode rules and
//! the plan deny use `resource: "*"`. MCP allow/deny rules are omitted; the
//! orchestrator adds those.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionPermissionRule {
    pub action: String,
    pub resource: String,
    pub effect: String,
}

pub fn rule(action: impl Into<String>, effect: impl Into<String>) -> SessionPermissionRule {
    SessionPermissionRule {
        action: action.into(),
        resource: "*".into(),
        effect: effect.into(),
    }
}

/// `auto` asks for shell, edit, and external_directory. Edit is allow only
/// under `auto-accept-edits`. `full-access` is a single `*` allow.
pub fn session_permission_rules(
    runtime_mode: &str,
    interaction_mode: &str,
    grants: &[SessionPermissionRule],
    paths: &[SessionPermissionRule],
) -> Vec<SessionPermissionRule> {
    let mut rules = Vec::new();
    if runtime_mode == "full-access" {
        rules.push(rule("*", "allow"));
    } else {
        rules.push(rule("shell", "ask"));
        let edit = if runtime_mode == "auto-accept-edits" {
            "allow"
        } else {
            "ask"
        };
        rules.push(rule("edit", edit));
        rules.push(rule("external_directory", "ask"));
    }
    rules.extend(grants.iter().cloned());
    if interaction_mode == "plan" {
        rules.push(rule("edit", "deny"));
    }
    rules.extend(agent_path_allows(paths));
    rules
}

/// Allows an agent keeps for its own directories. A `*` resource would undo
/// the mode rules that come before it.
pub fn agent_path_allows(rules: &[SessionPermissionRule]) -> Vec<SessionPermissionRule> {
    rules
        .iter()
        .filter(|entry| {
            entry.effect == "allow"
                && entry.resource != "*"
                && (entry.action == "edit" || entry.action == "external_directory")
        })
        .cloned()
        .collect()
}

/// Last matching rule. `*` matches any action or resource.
pub fn last_effect<'a>(
    rules: &'a [SessionPermissionRule],
    action: &str,
    resource: &str,
) -> Option<&'a str> {
    rules
        .iter()
        .rev()
        .find(|entry| {
            (entry.action == "*" || entry.action == action)
                && (entry.resource == "*" || entry.resource == resource)
        })
        .map(|entry| entry.effect.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_asks_for_shell_edit_and_external_directory() {
        let rules = session_permission_rules("auto", "default", &[], &[]);
        assert!(rules.iter().all(|entry| entry.resource == "*"));
        assert_eq!(last_effect(&rules, "shell", "*"), Some("ask"));
        assert_eq!(last_effect(&rules, "edit", "*"), Some("ask"));
        assert_eq!(last_effect(&rules, "external_directory", "*"), Some("ask"));
    }

    #[test]
    fn auto_accept_edits_allows_edit_only() {
        let rules = session_permission_rules("auto-accept-edits", "default", &[], &[]);
        assert_eq!(last_effect(&rules, "edit", "*"), Some("allow"));
        assert_eq!(last_effect(&rules, "shell", "*"), Some("ask"));
        assert_eq!(last_effect(&rules, "external_directory", "*"), Some("ask"));
    }

    #[test]
    fn full_access_is_a_single_allow() {
        let rules = session_permission_rules("full-access", "default", &[], &[]);
        assert_eq!(rules, vec![rule("*", "allow")]);
    }

    #[test]
    fn plan_edit_deny_overrides_a_session_grant() {
        let rules = session_permission_rules("auto", "plan", &[rule("edit", "allow")], &[]);
        assert_eq!(
            rules
                .iter()
                .map(|entry| {
                    (
                        entry.action.as_str(),
                        entry.effect.as_str(),
                        entry.resource.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("shell", "ask", "*"),
                ("edit", "ask", "*"),
                ("external_directory", "ask", "*"),
                ("edit", "allow", "*"),
                ("edit", "deny", "*"),
            ]
        );
        assert_eq!(last_effect(&rules, "edit", "*"), Some("deny"));
        assert_eq!(last_effect(&rules, "shell", "*"), Some("ask"));
    }

    #[test]
    fn a_plan_path_allow_wins_for_that_resource() {
        let paths = [SessionPermissionRule {
            action: "edit".into(),
            resource: "/plan.md".into(),
            effect: "allow".into(),
        }];
        let rules = session_permission_rules("auto", "plan", &[rule("edit", "allow")], &paths);
        assert_eq!(last_effect(&rules, "edit", "*"), Some("deny"));
        assert_eq!(last_effect(&rules, "edit", "/plan.md"), Some("allow"));
        assert!(rules.iter().all(|entry| entry.action != "t3-code-*"));
    }

    #[test]
    fn path_filter_keeps_specific_edit_allows() {
        let rules = [
            rule("edit", "allow"),
            SessionPermissionRule {
                action: "shell".into(),
                resource: "/bin".into(),
                effect: "allow".into(),
            },
            SessionPermissionRule {
                action: "edit".into(),
                resource: "/plan.md".into(),
                effect: "deny".into(),
            },
            SessionPermissionRule {
                action: "external_directory".into(),
                resource: "/tmp".into(),
                effect: "allow".into(),
            },
        ];
        let kept = agent_path_allows(&rules);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].action, "external_directory");
        assert_eq!(kept[0].resource, "/tmp");
    }
}
