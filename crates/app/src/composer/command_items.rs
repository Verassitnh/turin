//! `ComposerCommandItem` (ComposerCommandMenu.tsx) and its search/filter
//! rules: composerSlashCommandSearch.ts.

use std::sync::OnceLock;

use regex::Regex;

use crate::composer::catalog::{ProviderSkill, ProviderSlashCommand};
use crate::composer::logic::ComposerSlashCommand;
use crate::composer::search_ranking::{
    NormalizeSearchQueryOptions, RankedSearchResult, ScoreQueryMatchInput,
    insert_ranked_search_result, normalize_search_query, score_query_match,
};
use crate::composer::skills::score_provider_skill;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestMeta {
    pub number: u64,
    pub title: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComposerCommandItem {
    Path {
        id: String,
        path: String,
        path_kind: PathKind,
        label: String,
        description: String,
    },
    SlashCommand {
        id: String,
        command: ComposerSlashCommand,
        label: String,
        description: String,
    },
    ProviderSlashCommand {
        id: String,
        provider: String,
        command: ProviderSlashCommand,
        label: String,
        description: String,
    },
    Skill {
        id: String,
        provider: String,
        skill: ProviderSkill,
        label: String,
        description: String,
    },
    PullRequest {
        id: String,
        pull_request: PullRequestMeta,
        label: String,
        description: String,
    },
    Thread {
        id: String,
        thread_id: String,
        label: String,
        description: String,
    },
}

impl ComposerCommandItem {
    pub fn id(&self) -> &str {
        match self {
            Self::Path { id, .. }
            | Self::SlashCommand { id, .. }
            | Self::ProviderSlashCommand { id, .. }
            | Self::Skill { id, .. }
            | Self::PullRequest { id, .. }
            | Self::Thread { id, .. } => id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Path { label, .. }
            | Self::SlashCommand { label, .. }
            | Self::ProviderSlashCommand { label, .. }
            | Self::Skill { label, .. }
            | Self::PullRequest { label, .. }
            | Self::Thread { label, .. } => label,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Path { description, .. }
            | Self::SlashCommand { description, .. }
            | Self::ProviderSlashCommand { description, .. }
            | Self::Skill { description, .. }
            | Self::PullRequest { description, .. }
            | Self::Thread { description, .. } => description,
        }
    }

    fn is_slash_search_item(&self) -> bool {
        matches!(
            self,
            Self::SlashCommand { .. } | Self::ProviderSlashCommand { .. } | Self::Skill { .. }
        )
    }
}

/// `composerSuggestionOptionId`: JSON-escapes lone surrogates before
/// URI-encoding, without losing identity.
pub fn composer_suggestion_option_id(list_id: &str, item_id: &str) -> String {
    let json = serde_json::to_string(item_id).unwrap_or_default();
    let encoded: String = json
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("{list_id}-{encoded}")
}

/// `slashCommandItemsForPromptPosition`: a provider expands a slash command
/// only when it opens the whole message; anywhere else it reaches the agent as
/// literal text, so it is not offered there.
pub fn slash_command_items_for_prompt_position(
    items: &[ComposerCommandItem],
    is_at_prompt_start: bool,
) -> Vec<ComposerCommandItem> {
    if is_at_prompt_start {
        return items.to_vec();
    }
    items
        .iter()
        .filter(|item| !matches!(item, ComposerCommandItem::ProviderSlashCommand { .. }))
        .cloned()
        .collect()
}

fn score_slash_command_item(item: &ComposerCommandItem, query: &str) -> Option<i64> {
    if let ComposerCommandItem::Skill { skill, .. } = item {
        if query == "skill" {
            return Some(0);
        }
        let skill_query = query.strip_prefix("skill:").unwrap_or(query);
        let skill_score = if skill_query.is_empty() {
            Some(0)
        } else {
            score_provider_skill(skill, skill_query)
        };
        if skill_score.is_some() {
            return skill_score;
        }
        return "skill".starts_with(query).then_some(i64::MAX);
    }
    let primary = match item {
        ComposerCommandItem::SlashCommand { command, .. } => match command {
            ComposerSlashCommand::Model => "model",
            ComposerSlashCommand::Plan => "plan",
            ComposerSlashCommand::Default => "default",
        }
        .to_owned(),
        ComposerCommandItem::ProviderSlashCommand { command, .. } => command.name.to_lowercase(),
        _ => return None,
    };
    let description = item.description().to_lowercase();
    let primary_score = score_query_match(&ScoreQueryMatchInput {
        value: &primary,
        query,
        exact_base: 0,
        prefix_base: Some(2),
        boundary_base: Some(4),
        includes_base: Some(6),
        fuzzy_base: Some(100),
        boundary_markers: Some(&["-", "_", "/"]),
    });
    let description_score = score_query_match(&ScoreQueryMatchInput {
        value: &description,
        query,
        exact_base: 20,
        prefix_base: Some(22),
        boundary_base: Some(24),
        includes_base: Some(26),
        fuzzy_base: None,
        boundary_markers: None,
    });
    [primary_score, description_score]
        .into_iter()
        .flatten()
        .min()
}

fn leading_slashes() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^/+").unwrap())
}

/// `searchSlashCommandItems`.
pub fn search_slash_command_items(
    items: &[ComposerCommandItem],
    query: &str,
) -> Vec<ComposerCommandItem> {
    let normalized = normalize_search_query(
        query,
        Some(&NormalizeSearchQueryOptions {
            trim_leading_pattern: Some(leading_slashes()),
        }),
    );
    if normalized.is_empty() {
        return items.to_vec();
    }
    let mut ranked: Vec<RankedSearchResult<ComposerCommandItem>> = Vec::new();
    for item in items.iter().filter(|item| item.is_slash_search_item()) {
        let Some(score) = score_slash_command_item(item, &normalized) else {
            continue;
        };
        let tie_breaker = match item {
            ComposerCommandItem::SlashCommand { command, .. } => format!(
                "0\0{}",
                match command {
                    ComposerSlashCommand::Model => "model",
                    ComposerSlashCommand::Plan => "plan",
                    ComposerSlashCommand::Default => "default",
                }
            ),
            ComposerCommandItem::ProviderSlashCommand {
                command, provider, ..
            } => format!("1\0{}\0{provider}", command.name),
            ComposerCommandItem::Skill {
                skill, provider, ..
            } => format!("2\0{}\0{provider}", skill.name),
            _ => String::new(),
        };
        insert_ranked_search_result(
            &mut ranked,
            RankedSearchResult {
                item: item.clone(),
                score,
                tie_breaker,
            },
            usize::MAX,
        );
    }
    ranked.into_iter().map(|entry| entry.item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slash(
        id: &str,
        command: ComposerSlashCommand,
        label: &str,
        description: &str,
    ) -> ComposerCommandItem {
        ComposerCommandItem::SlashCommand {
            id: id.into(),
            command,
            label: label.into(),
            description: description.into(),
        }
    }

    fn provider(id: &str, name: &str, description: &str) -> ComposerCommandItem {
        ComposerCommandItem::ProviderSlashCommand {
            id: id.into(),
            provider: "claudeAgent".into(),
            command: ProviderSlashCommand {
                name: name.into(),
                ..Default::default()
            },
            label: format!("/{name}"),
            description: description.into(),
        }
    }

    fn skill_item(id: &str, skill: ProviderSkill, description: &str) -> ComposerCommandItem {
        let name = skill.name.clone();
        ComposerCommandItem::Skill {
            id: id.into(),
            provider: "claudeAgent".into(),
            skill,
            label: format!("/skill:{name}"),
            description: description.into(),
        }
    }

    fn ids(items: &[ComposerCommandItem]) -> Vec<&str> {
        items.iter().map(ComposerCommandItem::id).collect()
    }

    #[test]
    fn moves_exact_provider_command_matches_ahead_of_broader_description_matches() {
        let items = vec![
            slash(
                "slash:default",
                ComposerSlashCommand::Default,
                "/default",
                "Switch this thread back to normal build mode",
            ),
            provider(
                "provider-slash-command:claudeAgent:ui",
                "ui",
                "Explore, build, and refine UI.",
            ),
            provider(
                "provider-slash-command:claudeAgent:frontend-design",
                "frontend-design",
                "Create distinctive, production-grade frontend interfaces",
            ),
        ];
        assert_eq!(
            ids(&search_slash_command_items(&items, "ui")),
            ["provider-slash-command:claudeAgent:ui", "slash:default"]
        );
    }

    #[test]
    fn supports_fuzzy_provider_command_matches() {
        let items = vec![
            provider(
                "provider-slash-command:claudeAgent:gh-fix-ci",
                "gh-fix-ci",
                "Fix failing GitHub Actions",
            ),
            provider(
                "provider-slash-command:claudeAgent:github",
                "github",
                "General GitHub help",
            ),
        ];
        assert_eq!(
            ids(&search_slash_command_items(&items, "gfc")),
            ["provider-slash-command:claudeAgent:gh-fix-ci"]
        );
    }

    #[test]
    fn includes_skills_by_name_description_and_display_name() {
        let browser = vec![skill_item(
            "skill:claudeAgent:browser",
            ProviderSkill {
                short_description: Some("Open and control the in-app browser".into()),
                ..ProviderSkill::new("browser")
            },
            "Open and control the in-app browser",
        )];
        assert_eq!(
            ids(&search_slash_command_items(&browser, "browser")),
            ["skill:claudeAgent:browser"]
        );
        assert_eq!(
            ids(&search_slash_command_items(&browser, "control")),
            ["skill:claudeAgent:browser"]
        );
        let matt = vec![skill_item(
            "skill:claudeAgent:ask-matt",
            ProviderSkill {
                display_name: Some("Ask Matt".into()),
                short_description: Some("Find the right skill or workflow".into()),
                ..ProviderSkill::new("ask-matt")
            },
            "Find the right skill or workflow",
        )];
        assert_eq!(
            ids(&search_slash_command_items(&matt, "ask matt")),
            ["skill:claudeAgent:ask-matt"]
        );
        assert_eq!(
            ids(&search_slash_command_items(&matt, "/skill:ask-matt")),
            ["skill:claudeAgent:ask-matt"]
        );
    }

    #[test]
    fn matches_skills_by_their_rendered_prefix() {
        let items = vec![skill_item(
            "skill:claudeAgent:browser",
            ProviderSkill::new("browser"),
            "Open and control the in-app browser",
        )];
        assert_eq!(
            ids(&search_slash_command_items(&items, "/skill:brow")),
            ["skill:claudeAgent:browser"]
        );
        assert_eq!(
            ids(&search_slash_command_items(&items, "/sk")),
            ["skill:claudeAgent:browser"]
        );
        assert!(search_slash_command_items(&items, "/ill").is_empty());
    }

    #[test]
    fn keeps_skills_alongside_commands_for_an_empty_slash_query() {
        let items = vec![
            slash(
                "slash:model",
                ComposerSlashCommand::Model,
                "/model",
                "Switch model",
            ),
            skill_item(
                "skill:claudeAgent:unslop",
                ProviderSkill::new("unslop"),
                "Cut AI tells from writing",
            ),
        ];
        assert_eq!(
            ids(&search_slash_command_items(&items, "")),
            ["slash:model", "skill:claudeAgent:unslop"]
        );
    }

    #[test]
    fn hides_provider_commands_from_slash_completion_after_the_first_message_line() {
        let items = vec![
            slash(
                "slash:model",
                ComposerSlashCommand::Model,
                "/model",
                "Switch model",
            ),
            provider(
                "provider-slash-command:claudeAgent:compact",
                "compact",
                "Compact the conversation",
            ),
            skill_item(
                "skill:claudeAgent:unslop",
                ProviderSkill::new("unslop"),
                "Cut AI tells from writing",
            ),
        ];
        assert_eq!(
            ids(&slash_command_items_for_prompt_position(&items, false)),
            ["slash:model", "skill:claudeAgent:unslop"]
        );
        assert_eq!(
            ids(&slash_command_items_for_prompt_position(&items, true)),
            [
                "slash:model",
                "provider-slash-command:claudeAgent:compact",
                "skill:claudeAgent:unslop"
            ]
        );
    }

    #[test]
    fn suggestion_option_ids_encode_the_json_item_id() {
        assert_eq!(
            composer_suggestion_option_id("list", "a b"),
            "list-%22a%20b%22"
        );
    }
}
