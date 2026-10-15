//! Provider skill helpers: `formatProviderSkillDisplayName` (shared/inlineSkills.ts),
//! the skill filters of client-runtime/providerSkills.ts and the ranking of
//! providerSkillSearch.ts.

use std::sync::OnceLock;

use regex::Regex;

use crate::composer::catalog::{ProviderSkill, ProviderSlashCommand};
use crate::composer::search_ranking::{
    NormalizeSearchQueryOptions, RankedSearchResult, ScoreQueryMatchInput,
    insert_ranked_search_result, normalize_search_query, score_query_match,
};

fn title_case_words(value: &str) -> String {
    value
        .split(|c: char| c.is_whitespace() || c == ':' || c == '_' || c == '-')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `formatProviderSkillDisplayName`.
pub fn format_provider_skill_display_name(name: &str, display_name: Option<&str>) -> String {
    if let Some(display) = display_name.map(str::trim).filter(|d| !d.is_empty()) {
        return display.to_owned();
    }
    title_case_words(name)
}

pub fn skill_display_name(skill: &ProviderSkill) -> String {
    format_provider_skill_display_name(&skill.name, skill.display_name.as_deref())
}

/// `dedupeProviderSkillsByName`: the first definition of each name wins.
pub fn dedupe_provider_skills_by_name(skills: Vec<ProviderSkill>) -> Vec<ProviderSkill> {
    let mut seen = std::collections::HashSet::new();
    skills
        .into_iter()
        .filter(|skill| seen.insert(skill.name.trim().to_lowercase()))
        .collect()
}

/// `isProviderSkillUserInvocable`: a skill switched off in the provider's
/// settings will not run, and one the provider reserves for the agent
/// rejects a user invocation.
pub fn is_provider_skill_user_invocable(skill: &ProviderSkill) -> bool {
    skill.enabled && skill.user_invocable
}

/// `getProviderSkillsForSlashMenu`.
pub fn get_provider_skills_for_slash_menu(
    skills: &[ProviderSkill],
    show_skills_in_slash_menu: bool,
) -> Vec<ProviderSkill> {
    if !show_skills_in_slash_menu {
        return Vec::new();
    }
    dedupe_provider_skills_by_name(
        skills
            .iter()
            .filter(|skill| is_provider_skill_user_invocable(skill))
            .cloned()
            .collect(),
    )
}

/// `getProviderSlashCommandsForSlashMenu`.
pub fn get_provider_slash_commands_for_slash_menu(
    slash_commands: &[ProviderSlashCommand],
    visible_skills: &[ProviderSkill],
) -> Vec<ProviderSlashCommand> {
    let skill_names: std::collections::HashSet<String> = visible_skills
        .iter()
        .map(|s| s.name.trim().to_lowercase())
        .collect();
    slash_commands
        .iter()
        .filter(|command| !skill_names.contains(&command.name.trim().to_lowercase()))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSkillSourceKind {
    App,
    Repo,
    Project,
    Personal,
    System,
    Other,
}

/// `resolveProviderSkillSourceKind`.
pub fn resolve_provider_skill_source_kind(skill: &ProviderSkill) -> ProviderSkillSourceKind {
    let path = skill.path.replace('\\', "/");
    if path.contains("/.codex/plugins/") || path.contains("/.agents/plugins/") {
        return ProviderSkillSourceKind::App;
    }
    match skill
        .scope
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .as_deref()
    {
        Some("repo" | "repository") => ProviderSkillSourceKind::Repo,
        Some("project" | "workspace" | "local") => ProviderSkillSourceKind::Project,
        Some("user" | "personal") => ProviderSkillSourceKind::Personal,
        Some("system") => ProviderSkillSourceKind::System,
        _ => ProviderSkillSourceKind::Other,
    }
}

impl ProviderSkillSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Repo => "Repo",
            Self::Project => "Project",
            Self::Personal => "Personal",
            Self::System => "System",
            Self::Other => "Provider",
        }
    }
}

fn q(input: &ScoreQueryMatchInput<'_>) -> Option<i64> {
    score_query_match(input)
}

/// `scoreProviderSkill`.
pub fn score_provider_skill(skill: &ProviderSkill, query: &str) -> Option<i64> {
    let name = skill.name.to_lowercase();
    let label = skill_display_name(skill).to_lowercase();
    let short = skill
        .short_description
        .as_deref()
        .unwrap_or("")
        .to_lowercase();
    let description = skill.description.as_deref().unwrap_or("").to_lowercase();
    let scope = skill.scope.as_deref().unwrap_or("").to_lowercase();
    let scores = [
        q(&ScoreQueryMatchInput {
            value: &name,
            query,
            exact_base: 0,
            prefix_base: Some(2),
            boundary_base: Some(4),
            includes_base: Some(6),
            fuzzy_base: Some(100),
            boundary_markers: Some(&["-", "_", "/"]),
        }),
        q(&ScoreQueryMatchInput {
            value: &label,
            query,
            exact_base: 1,
            prefix_base: Some(3),
            boundary_base: Some(5),
            includes_base: Some(7),
            fuzzy_base: Some(110),
            boundary_markers: None,
        }),
        q(&ScoreQueryMatchInput {
            value: &short,
            query,
            exact_base: 20,
            prefix_base: Some(22),
            boundary_base: Some(24),
            includes_base: Some(26),
            fuzzy_base: None,
            boundary_markers: None,
        }),
        q(&ScoreQueryMatchInput {
            value: &description,
            query,
            exact_base: 30,
            prefix_base: Some(32),
            boundary_base: Some(34),
            includes_base: Some(36),
            fuzzy_base: None,
            boundary_markers: None,
        }),
        q(&ScoreQueryMatchInput {
            value: &scope,
            query,
            exact_base: 40,
            prefix_base: Some(42),
            boundary_base: None,
            includes_base: Some(44),
            fuzzy_base: None,
            boundary_markers: None,
        }),
    ];
    scores.into_iter().flatten().min()
}

fn currency_prefix_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\p{Sc}+").unwrap())
}

/// `searchProviderSkills`.
pub fn search_provider_skills(
    skills: &[ProviderSkill],
    query: &str,
    limit: usize,
) -> Vec<ProviderSkill> {
    let enabled = dedupe_provider_skills_by_name(
        skills
            .iter()
            .filter(|s| is_provider_skill_user_invocable(s))
            .cloned()
            .collect(),
    );
    let normalized = normalize_search_query(
        query,
        Some(&NormalizeSearchQueryOptions {
            trim_leading_pattern: Some(currency_prefix_pattern()),
        }),
    );
    if normalized.is_empty() {
        return enabled;
    }
    let mut ranked: Vec<RankedSearchResult<ProviderSkill>> = Vec::new();
    for skill in enabled {
        let Some(score) = score_provider_skill(&skill, &normalized) else {
            continue;
        };
        let tie_breaker = format!(
            "{}\0{}",
            skill_display_name(&skill).to_lowercase(),
            skill.name
        );
        insert_ranked_search_result(
            &mut ranked,
            RankedSearchResult {
                item: skill,
                score,
                tie_breaker,
            },
            limit,
        );
    }
    ranked.into_iter().map(|entry| entry.item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> ProviderSkill {
        ProviderSkill::new(name)
    }

    fn named(skills: &[ProviderSkill]) -> Vec<&str> {
        skills.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn searches_skills_with_any_currency_prefix() {
        for prefix in ["$", "€", "£", "¥", "₹", "₩", "₿", "\u{11FDD}"] {
            let found =
                search_provider_skills(&[skill("review")], &format!("{prefix}review"), usize::MAX);
            assert_eq!(named(&found), ["review"], "{prefix}");
        }
    }

    #[test]
    fn moves_exact_ui_matches_ahead_of_broader_ones() {
        let skills = vec![
            ProviderSkill {
                display_name: Some("Agent Browser".into()),
                short_description: Some("Browser automation CLI for AI agents".into()),
                ..skill("agent-browser")
            },
            ProviderSkill {
                display_name: Some("Building Native Ui".into()),
                short_description: Some(
                    "Complete guide for building beautiful apps with Expo Router".into(),
                ),
                ..skill("building-native-ui")
            },
            ProviderSkill {
                display_name: Some("Ui".into()),
                short_description: Some("Explore, build, and refine UI.".into()),
                ..skill("ui")
            },
        ];
        assert_eq!(
            named(&search_provider_skills(&skills, "ui", usize::MAX)),
            ["ui", "building-native-ui"]
        );
    }

    #[test]
    fn uses_fuzzy_ranking_for_abbreviated_queries() {
        let skills = vec![skill("gh-fix-ci"), skill("github"), skill("agent-browser")];
        assert_eq!(
            named(&search_provider_skills(&skills, "gfc", usize::MAX)),
            ["gh-fix-ci"]
        );
    }

    #[test]
    fn keeps_user_only_skills_and_omits_agent_only_ones() {
        let skills = vec![
            ProviderSkill {
                user_invocation_only: true,
                ..skill("re-release-version")
            },
            ProviderSkill {
                user_invocable: false,
                ..skill("release-context")
            },
            skill("release-version"),
        ];
        assert_eq!(
            named(&search_provider_skills(&skills, "release", usize::MAX)),
            ["release-version", "re-release-version"]
        );
    }

    #[test]
    fn omits_disabled_skills_and_returns_every_enabled_skill_for_an_empty_query() {
        let skills = vec![
            ProviderSkill {
                enabled: false,
                ..skill("ui")
            },
            skill("frontend-design"),
        ];
        assert!(search_provider_skills(&skills, "ui", usize::MAX).is_empty());
        let skills = vec![
            skill("unslop"),
            skill("browser"),
            ProviderSkill {
                enabled: false,
                ..skill("disabled")
            },
        ];
        assert_eq!(
            named(&search_provider_skills(&skills, "", usize::MAX)),
            ["unslop", "browser"]
        );
    }

    #[test]
    fn returns_the_first_enabled_definition_for_each_skill_name() {
        let skills = vec![
            ProviderSkill {
                path: "/Users/matt/.codex/skills/branch-audit/SKILL.md".into(),
                ..skill("branch-audit")
            },
            skill("browser"),
            ProviderSkill {
                path: "/Users/matt/.agents/skills/branch-audit/SKILL.md".into(),
                ..skill("branch-audit")
            },
        ];
        let paths: Vec<_> = search_provider_skills(&skills, "", usize::MAX)
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(
            paths,
            [
                "/Users/matt/.codex/skills/branch-audit/SKILL.md",
                "/skills/browser/SKILL.md"
            ]
        );
    }

    #[test]
    fn formats_display_names_and_source_kinds() {
        assert_eq!(
            format_provider_skill_display_name("review-follow-up", None),
            "Review Follow Up"
        );
        assert_eq!(
            format_provider_skill_display_name("plugin:skill_name", None),
            "Plugin Skill Name"
        );
        assert_eq!(
            format_provider_skill_display_name("x", Some("  Custom ")),
            "Custom"
        );
        let source = |path: &str, scope: Option<&str>| {
            resolve_provider_skill_source_kind(&ProviderSkill {
                path: path.into(),
                scope: scope.map(str::to_owned),
                ..skill("x")
            })
        };
        assert_eq!(
            source("/home/me/.codex/plugins/a/SKILL.md", None),
            ProviderSkillSourceKind::App
        );
        assert_eq!(source("/a", Some("Repo")), ProviderSkillSourceKind::Repo);
        assert_eq!(
            source("/a", Some("workspace")),
            ProviderSkillSourceKind::Project
        );
        assert_eq!(
            source("/a", Some("user")),
            ProviderSkillSourceKind::Personal
        );
        assert_eq!(
            source("/a", Some("system")),
            ProviderSkillSourceKind::System
        );
        assert_eq!(source("/a", None), ProviderSkillSourceKind::Other);
    }

    #[test]
    fn slash_menu_hides_commands_shadowed_by_visible_skills() {
        let skills = vec![skill("review")];
        let commands = vec![
            ProviderSlashCommand {
                name: "Review".into(),
                ..Default::default()
            },
            ProviderSlashCommand {
                name: "compact".into(),
                ..Default::default()
            },
        ];
        let visible = get_provider_skills_for_slash_menu(&skills, true);
        let remaining = get_provider_slash_commands_for_slash_menu(&commands, &visible);
        assert_eq!(
            remaining
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["compact"]
        );
        assert!(get_provider_skills_for_slash_menu(&skills, false).is_empty());
    }
}
