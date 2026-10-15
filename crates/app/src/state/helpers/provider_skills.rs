//! Port of upstream/packages/client-runtime/src/providerSkills.ts

use std::collections::HashSet;

use turin_contracts::js_trim;

use super::provider_types::{
    ServerProvider, ServerProviderSkill, ServerProviderSlashCommand,
    ServerProviderWorkspaceSnapshot, is_provider_workspace_snapshot_current,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSkillSourceKind {
    App,
    Repo,
    Project,
    Personal,
    System,
    Other,
}

fn normalize_path_separators(path_value: &str) -> String {
    path_value.replace('\\', "/")
}

pub fn dedupe_provider_skills_by_name(skills: &[ServerProviderSkill]) -> Vec<ServerProviderSkill> {
    let mut seen_names: HashSet<String> = HashSet::new();
    skills
        .iter()
        .filter(|skill| seen_names.insert(js_trim(&skill.name).to_lowercase()))
        .cloned()
        .collect()
}

/// Whether a composer pick can start this skill. A skill switched off in the
/// provider's settings will not run, and one the provider reserves for the
/// agent (Claude Code's `user-invocable: false`) rejects a user invocation.
/// Everything else, including skills the agent may not start on its own, is
/// fair game: the server dispatches the pick in the provider's native form.
pub fn is_provider_skill_user_invocable(skill: &ServerProviderSkill) -> bool {
    skill.enabled && skill.user_invocable != Some(false)
}

pub fn get_provider_skills_for_slash_menu(
    skills: &[ServerProviderSkill],
    show_skills_in_slash_menu: bool,
) -> Vec<ServerProviderSkill> {
    if !show_skills_in_slash_menu {
        return Vec::new();
    }
    let invocable: Vec<ServerProviderSkill> = skills
        .iter()
        .filter(|skill| is_provider_skill_user_invocable(skill))
        .cloned()
        .collect();
    dedupe_provider_skills_by_name(&invocable)
}

pub fn get_provider_slash_commands_for_slash_menu(
    slash_commands: &[ServerProviderSlashCommand],
    visible_skills: &[ServerProviderSkill],
) -> Vec<ServerProviderSlashCommand> {
    let skill_names: HashSet<String> = visible_skills
        .iter()
        .map(|skill| js_trim(&skill.name).to_lowercase())
        .collect();
    slash_commands
        .iter()
        .filter(|command| !skill_names.contains(&js_trim(&command.name).to_lowercase()))
        .cloned()
        .collect()
}

pub fn resolve_provider_skill_source_kind(skill: &ServerProviderSkill) -> ProviderSkillSourceKind {
    let normalized_path = normalize_path_separators(&skill.path);
    if normalized_path.contains("/.codex/plugins/") || normalized_path.contains("/.agents/plugins/")
    {
        return ProviderSkillSourceKind::App;
    }

    let normalized_scope = skill
        .scope
        .as_deref()
        .map(|scope| js_trim(scope).to_lowercase());
    match normalized_scope.as_deref() {
        Some("repo" | "repository") => ProviderSkillSourceKind::Repo,
        Some("project" | "workspace" | "local") => ProviderSkillSourceKind::Project,
        Some("user" | "personal") => ProviderSkillSourceKind::Personal,
        Some("system") => ProviderSkillSourceKind::System,
        _ => ProviderSkillSourceKind::Other,
    }
}

fn resolve_provider_workspace_snapshot<'a>(
    provider: &'a ServerProvider,
    cwd: Option<&str>,
) -> Option<&'a ServerProviderWorkspaceSnapshot> {
    let cwd = cwd.filter(|cwd| !cwd.is_empty())?;
    provider
        .workspace_snapshots
        .as_ref()?
        .iter()
        .find(|snapshot| snapshot.cwd == cwd)
}

pub fn has_complete_provider_workspace_snapshot(
    provider: Option<&ServerProvider>,
    cwd: Option<&str>,
) -> bool {
    provider
        .and_then(|provider| resolve_provider_workspace_snapshot(provider, cwd))
        .is_some_and(|snapshot| snapshot.slash_commands_pending != Some(true))
}

/// A complete snapshot young enough that opening a composer need not rescan.
pub fn has_current_provider_workspace_snapshot(
    provider: Option<&ServerProvider>,
    cwd: Option<&str>,
    now_ms: i64,
) -> bool {
    provider
        .and_then(|provider| resolve_provider_workspace_snapshot(provider, cwd))
        .is_some_and(|snapshot| {
            snapshot.slash_commands_pending != Some(true)
                && is_provider_workspace_snapshot_current(snapshot, now_ms)
        })
}

pub fn resolve_provider_skills_for_cwd<'a>(
    provider: &'a ServerProvider,
    cwd: Option<&str>,
) -> &'a [ServerProviderSkill] {
    resolve_provider_workspace_snapshot(provider, cwd)
        .map(|snapshot| snapshot.skills.as_slice())
        .unwrap_or(&provider.skills)
}

pub fn resolve_provider_slash_commands_for_cwd<'a>(
    provider: &'a ServerProvider,
    cwd: Option<&str>,
) -> &'a [ServerProviderSlashCommand] {
    resolve_provider_workspace_snapshot(provider, cwd)
        .map(|snapshot| snapshot.slash_commands.as_slice())
        .unwrap_or(&provider.slash_commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::helpers::provider_types::PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS;
    use crate::state::helpers::shared_support::format_provider_skill_display_name;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

    fn skill(name: &str, path: &str, enabled: bool) -> ServerProviderSkill {
        ServerProviderSkill {
            name: name.to_owned(),
            path: path.to_owned(),
            enabled,
            ..Default::default()
        }
    }

    fn command(name: &str) -> ServerProviderSlashCommand {
        ServerProviderSlashCommand {
            name: name.to_owned(),
            description: None,
        }
    }

    fn names(skills: &[ServerProviderSkill]) -> Vec<&str> {
        skills.iter().map(|skill| skill.name.as_str()).collect()
    }

    fn provider() -> ServerProvider {
        let mut provider = ServerProvider::new(
            ProviderInstanceId::new("codex").unwrap(),
            ProviderDriverKind::new("codex").unwrap(),
        );
        provider.slash_commands = vec![command("global")];
        provider.skills = vec![skill("global", "/global/SKILL.md", true)];
        provider.workspace_snapshots = Some(vec![ServerProviderWorkspaceSnapshot {
            cwd: "/workspace/project-a".to_owned(),
            checked_at: "2026-01-01T00:01:00.000Z".to_owned(),
            slash_commands: vec![command("project")],
            slash_commands_pending: None,
            skills: vec![skill("project", "/workspace/project-a/SKILL.md", true)],
        }]);
        provider
    }

    #[test]
    fn format_provider_skill_display_name_prefers_the_provider_display_name() {
        assert_eq!(
            format_provider_skill_display_name("review-follow-up", Some("Review Follow-up")),
            "Review Follow-up"
        );
    }

    #[test]
    fn format_provider_skill_display_name_falls_back_to_a_title_cased_skill_name() {
        assert_eq!(
            format_provider_skill_display_name("review-follow-up", None),
            "Review Follow Up"
        );
    }

    #[test]
    fn dedupe_keeps_the_first_resolved_skill_and_preserves_unrelated_skill_order() {
        let first_skill = skill(
            "branch-audit",
            "/Users/matt/.codex/skills/branch-audit/SKILL.md",
            true,
        );
        let other_skill = skill(
            "browser",
            "/Users/matt/.agents/skills/browser/SKILL.md",
            true,
        );
        let duplicate_skill = skill(
            "Branch-Audit",
            "/Users/matt/.agents/skills/branch-audit/SKILL.md",
            true,
        );

        assert_eq!(
            dedupe_provider_skills_by_name(&[
                first_skill.clone(),
                other_skill.clone(),
                duplicate_skill
            ]),
            vec![first_skill, other_skill]
        );
    }

    #[test]
    fn slash_menu_keeps_the_skill_alias_when_the_provider_also_exposes_it_as_a_slash_command() {
        let ask_matt = skill(
            "ask-matt",
            "/Users/matt/.agents/skills/ask-matt/SKILL.md",
            true,
        );
        assert_eq!(
            names(&get_provider_skills_for_slash_menu(&[ask_matt], true)),
            ["ask-matt"]
        );
    }

    #[test]
    fn slash_menu_shows_one_row_when_enabled_skills_share_a_name() {
        let skills = [
            skill(
                "babysit-pr",
                "/Users/matt/.codex/skills/babysit-pr/SKILL.md",
                true,
            ),
            skill(
                "browser",
                "/Users/matt/.agents/skills/browser/SKILL.md",
                true,
            ),
            skill(
                "babysit-pr",
                "/Users/matt/.agents/skills/babysit-pr/SKILL.md",
                true,
            ),
        ];
        assert_eq!(
            names(&get_provider_skills_for_slash_menu(&skills, true)),
            ["babysit-pr", "browser"]
        );
    }

    #[test]
    fn slash_menu_keeps_an_enabled_skill_when_a_disabled_duplicate_appears_first() {
        let enabled_skill = skill(
            "babysit-pr",
            "/Users/matt/.agents/skills/babysit-pr/SKILL.md",
            true,
        );
        let skills = [
            skill(
                "babysit-pr",
                "/Users/matt/.codex/skills/babysit-pr/SKILL.md",
                false,
            ),
            enabled_skill.clone(),
        ];
        assert_eq!(
            get_provider_skills_for_slash_menu(&skills, true),
            vec![enabled_skill]
        );
    }

    #[test]
    fn slash_menu_drops_a_skill_the_provider_reserves_for_the_agent() {
        let skills = [
            ServerProviderSkill {
                user_invocable: Some(false),
                ..skill(
                    "legacy-system-context",
                    "/Users/matt/.claude/skills/legacy-system-context/SKILL.md",
                    true,
                )
            },
            ServerProviderSkill {
                // Reserved for the user, not the agent: still a valid pick.
                user_invocation_only: Some(true),
                ..skill("deploy", "/Users/matt/.claude/skills/deploy/SKILL.md", true)
            },
        ];
        assert_eq!(
            names(&get_provider_skills_for_slash_menu(&skills, true)),
            ["deploy"]
        );
    }

    fn slash_menu_commands() -> (Vec<ServerProviderSlashCommand>, Vec<ServerProviderSkill>) {
        (
            vec![
                ServerProviderSlashCommand {
                    name: "ask-matt".to_owned(),
                    description: Some("Ask which skill fits your situation.".to_owned()),
                },
                ServerProviderSlashCommand {
                    name: "compact".to_owned(),
                    description: Some("Compact the conversation.".to_owned()),
                },
            ],
            vec![skill(
                "ask-matt",
                "/Users/matt/.agents/skills/ask-matt/SKILL.md",
                true,
            )],
        )
    }

    #[test]
    fn lets_the_skill_alias_win_when_a_provider_command_has_the_same_name() {
        let (commands, skills) = slash_menu_commands();
        let remaining: Vec<String> = get_provider_slash_commands_for_slash_menu(&commands, &skills)
            .into_iter()
            .map(|command| command.name)
            .collect();
        assert_eq!(remaining, ["compact"]);
    }

    #[test]
    fn keeps_the_provider_command_when_the_matching_skill_alias_is_hidden() {
        let (commands, skills) = slash_menu_commands();
        let visible_skills = get_provider_skills_for_slash_menu(&skills, false);
        let remaining: Vec<String> =
            get_provider_slash_commands_for_slash_menu(&commands, &visible_skills)
                .into_iter()
                .map(|command| command.name)
                .collect();
        assert_eq!(remaining, ["ask-matt", "compact"]);
    }

    fn source_kind(path: &str, scope: Option<&str>) -> ProviderSkillSourceKind {
        resolve_provider_skill_source_kind(&ServerProviderSkill {
            path: path.to_owned(),
            scope: scope.map(str::to_owned),
            ..Default::default()
        })
    }

    #[test]
    fn marks_plugin_backed_skills_as_app_installs() {
        assert_eq!(
            source_kind(
                "/Users/julius/.codex/plugins/cache/openai-curated/github/skills/gh-fix-ci/SKILL.md",
                Some("user")
            ),
            ProviderSkillSourceKind::App
        );
    }

    #[test]
    fn maps_standard_scopes_to_source_kinds() {
        assert_eq!(
            source_kind(
                "/workspace/.codex/skills/review-follow-up/SKILL.md",
                Some("repo")
            ),
            ProviderSkillSourceKind::Repo
        );
        assert_eq!(
            source_kind(
                "/workspace/.codex/skills/review-follow-up/SKILL.md",
                Some("project")
            ),
            ProviderSkillSourceKind::Project
        );
        assert_eq!(
            source_kind(
                "/Users/julius/.agents/skills/agent-browser/SKILL.md",
                Some("user")
            ),
            ProviderSkillSourceKind::Personal
        );
        assert_eq!(
            source_kind(
                "/usr/local/share/codex/skills/imagegen/SKILL.md",
                Some("system")
            ),
            ProviderSkillSourceKind::System
        );
    }

    #[test]
    fn keeps_unknown_and_missing_scopes_usable() {
        assert_eq!(
            source_kind("/opt/skills/team-review/SKILL.md", Some("team_shared")),
            ProviderSkillSourceKind::Other
        );
        assert_eq!(
            source_kind("/opt/skills/team-review/SKILL.md", None),
            ProviderSkillSourceKind::Other
        );
    }

    #[test]
    fn uses_the_cwd_snapshot_after_a_provider_session_has_populated_it() {
        let provider = provider();
        assert_eq!(
            resolve_provider_skills_for_cwd(&provider, Some("/workspace/project-a")),
            [skill("project", "/workspace/project-a/SKILL.md", true)]
        );
        assert_eq!(
            resolve_provider_slash_commands_for_cwd(&provider, Some("/workspace/project-a")),
            [command("project")]
        );
    }

    #[test]
    fn keeps_the_machine_snapshot_before_this_cwd_has_a_provider_snapshot() {
        let provider = provider();
        assert_eq!(
            resolve_provider_skills_for_cwd(&provider, Some("/workspace/project-b")),
            provider.skills
        );
        assert_eq!(
            resolve_provider_slash_commands_for_cwd(&provider, None),
            provider.slash_commands
        );
    }

    #[test]
    fn uses_partial_workspace_skills_and_commands_while_keeping_discovery_retryable() {
        let provider = provider();
        let mut partial = provider.clone();
        for snapshot in partial.workspace_snapshots.as_mut().unwrap() {
            snapshot.slash_commands = vec![command("compact")];
            snapshot.slash_commands_pending = Some(true);
        }
        let cwd = Some("/workspace/project-a");
        assert_eq!(
            resolve_provider_skills_for_cwd(&partial, cwd),
            provider.workspace_snapshots.as_ref().unwrap()[0].skills
        );
        assert_eq!(
            resolve_provider_slash_commands_for_cwd(&partial, cwd),
            [command("compact")]
        );
        assert!(!has_complete_provider_workspace_snapshot(
            Some(&partial),
            cwd
        ));
        assert!(has_complete_provider_workspace_snapshot(
            Some(&provider),
            cwd
        ));
        assert!(!has_complete_provider_workspace_snapshot(
            Some(&provider),
            Some("/workspace/project-b")
        ));
        assert!(!has_complete_provider_workspace_snapshot(None, cwd));
        assert!(!has_complete_provider_workspace_snapshot(
            Some(&provider),
            None
        ));
    }

    #[test]
    fn asks_for_a_rescan_once_the_workspace_snapshot_outlives_its_ttl() {
        let provider = provider();
        let scanned_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:01:00.000Z")
            .unwrap()
            .timestamp_millis();
        let cwd = Some("/workspace/project-a");
        assert!(has_current_provider_workspace_snapshot(
            Some(&provider),
            cwd,
            scanned_at
        ));
        assert!(has_current_provider_workspace_snapshot(
            Some(&provider),
            cwd,
            scanned_at + PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS - 1
        ));
        assert!(!has_current_provider_workspace_snapshot(
            Some(&provider),
            cwd,
            scanned_at + PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS
        ));
        assert!(!has_current_provider_workspace_snapshot(
            Some(&provider),
            Some("/workspace/project-b"),
            scanned_at
        ));
    }
}
