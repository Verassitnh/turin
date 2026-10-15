//! Port of upstream/packages/client-runtime/src/state/subagentDisplay.ts

use std::sync::LazyLock;

use regex::Regex;

use crate::state::contracts::OrchestrationV2TurnItemStatus;
use crate::state::subagent_runtime::{RuntimeSubagentStatus, is_terminal_subagent_status};
use crate::state::support::model::{
    SelectableModel, format_model_slug_name, resolve_selectable_model,
};
use crate::state::support::path::file_basename;

fn is_active_turn_item_status(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentGroupSummary {
    pub label: String,
    pub active: bool,
    pub failed: bool,
}

/// Summarizes one adjacent group, without changing its member identities or order.
pub fn subagent_group_summary(statuses: &[OrchestrationV2TurnItemStatus]) -> SubagentGroupSummary {
    let active = statuses
        .iter()
        .any(|status| is_active_turn_item_status(*status));
    SubagentGroupSummary {
        label: format!(
            "{} {} {}",
            if active { "Kicked off" } else { "Ran" },
            statuses.len(),
            if statuses.len() == 1 {
                "subagent"
            } else {
                "subagents"
            }
        ),
        active,
        failed: statuses.contains(&OrchestrationV2TurnItemStatus::Failed),
    }
}

/// Counts a group's states in the order a reader scans them: what is still
/// running first, then outcomes, using the agents panel's words.
pub fn summarize_subagent_statuses(statuses: &[OrchestrationV2TurnItemStatus]) -> String {
    let (mut working, mut done, mut failed, mut stopped, mut idle) = (0, 0, 0, 0, 0);
    for status in statuses {
        match status {
            OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting => working += 1,
            OrchestrationV2TurnItemStatus::Completed => done += 1,
            OrchestrationV2TurnItemStatus::Failed => failed += 1,
            OrchestrationV2TurnItemStatus::Idle => idle += 1,
            _ => stopped += 1,
        }
    }
    [
        ("working", working),
        ("done", done),
        ("failed", failed),
        ("stopped", stopped),
        ("idle", idle),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(label, count)| format!("{count} {label}"))
    .collect::<Vec<_>>()
    .join(" · ")
}

/// Formats Codex task paths for display while leaving provider identity untouched.
pub fn format_subagent_display_title(title: &str) -> String {
    static PREFIX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^Subagent:\s*").expect("static regex"));
    static PATH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^/root/(?:[^/]+/)*([^/]+)/?$").expect("static regex"));
    static SEPARATORS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[_\s]+").expect("static regex"));
    static WORD_START: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|\s)\S").expect("static regex"));

    let display_title = PREFIX.replace(title, "").into_owned();
    let Some(path) = PATH.captures(&display_title) else {
        return display_title;
    };
    let replaced = SEPARATORS.replace_all(&path[1], " ");
    let name = crate::state::contracts::js_trim(&replaced);
    let titled = WORD_START
        .replace_all(name, |captures: &regex::Captures<'_>| {
            captures[0].to_uppercase()
        })
        .into_owned();
    if titled.is_empty() {
        display_title
    } else {
        titled
    }
}

/// A model catalog entry, as `ServerProvider["models"]` carries it.
#[derive(Clone, Debug, Default)]
pub struct SubagentCatalogModel {
    pub slug: String,
    pub name: String,
    pub short_name: Option<String>,
    pub aliases: Vec<String>,
    pub sub_provider: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SubagentProvider<'a> {
    pub driver: &'a str,
    pub models: &'a [SubagentCatalogModel],
}

#[derive(Clone, Debug, Default)]
pub struct SubagentParentThread<'a> {
    pub project_id: &'a str,
    pub worktree_path: Option<&'a str>,
}

#[derive(Clone, Debug, Default)]
pub struct SubagentChildThread<'a> {
    pub branch: Option<&'a str>,
    pub worktree_path: Option<&'a str>,
}

#[derive(Clone, Debug, Default)]
pub struct SubagentChildProject<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub workspace_root: &'a str,
}

#[derive(Clone, Debug, Default)]
pub struct ResolveSubagentMetadataInput<'a> {
    pub model: Option<&'a str>,
    pub provider: Option<SubagentProvider<'a>>,
    pub parent_thread: Option<SubagentParentThread<'a>>,
    pub child_thread: Option<SubagentChildThread<'a>>,
    pub parent_project_workspace_root: Option<&'a str>,
    pub child_project: Option<SubagentChildProject<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentWorkspaceEntry {
    pub label: &'static str,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentMetadata {
    pub model_label: String,
    pub workspace: Vec<SubagentWorkspaceEntry>,
}

/// Match desktop's model resolution and show only changes from the parent's workspace.
pub fn resolve_subagent_metadata(input: &ResolveSubagentMetadataInput<'_>) -> SubagentMetadata {
    let model = input.model.map(crate::state::contracts::js_trim);
    let slug: Option<String> = match &input.provider {
        Some(provider) => {
            let options: Vec<SelectableModel> = provider
                .models
                .iter()
                .map(|model| SelectableModel {
                    slug: model.slug.clone(),
                    name: model.name.clone(),
                    aliases: model.aliases.clone(),
                })
                .collect();
            resolve_selectable_model(provider.driver, model, &options).map(str::to_owned)
        }
        None => model.map(str::to_owned),
    };
    let catalog_model = input.provider.as_ref().and_then(|provider| {
        provider
            .models
            .iter()
            .find(|candidate| Some(candidate.slug.as_str()) == slug.as_deref())
    });
    let reported_label = match catalog_model {
        Some(catalog_model) => catalog_model
            .short_name
            .as_deref()
            .filter(|short| !short.is_empty())
            .unwrap_or(&catalog_model.name)
            .to_owned(),
        None => match model.filter(|model| !model.is_empty()) {
            Some(model) => format_model_slug_name(model),
            None => "Not reported".to_owned(),
        },
    };
    let qualifier = catalog_model
        .and_then(|model| model.sub_provider.as_deref())
        .map(crate::state::contracts::js_trim)
        .filter(|qualifier| !qualifier.is_empty());
    let model_label = match qualifier {
        Some(qualifier) => {
            let pattern = format!(r"(?iu)^{}(?:\s*[.:/-]\s*|\s+)", regex::escape(qualifier));
            let stripped = Regex::new(&pattern)
                .map(|regex| regex.replace(&reported_label, "").into_owned())
                .unwrap_or_else(|_| reported_label.clone());
            let stripped = crate::state::contracts::js_trim(&stripped).to_owned();
            if stripped.is_empty() {
                reported_label
            } else {
                stripped
            }
        }
        None => reported_label,
    };
    let parent_workspace = input
        .parent_thread
        .as_ref()
        .and_then(|thread| thread.worktree_path)
        .or(input.parent_project_workspace_root);
    let child_workspace = input
        .child_thread
        .as_ref()
        .and_then(|thread| thread.worktree_path)
        .or(input
            .child_project
            .as_ref()
            .map(|project| project.workspace_root));
    let mut workspace = Vec::new();
    if let (Some(parent), Some(child)) = (&input.parent_thread, &input.child_project)
        && child.id != parent.project_id
    {
        workspace.push(SubagentWorkspaceEntry {
            label: "Project",
            value: child.title.to_owned(),
        });
    }
    if let (Some(parent_workspace), Some(child_workspace)) = (parent_workspace, child_workspace)
        && parent_workspace != child_workspace
    {
        let child_thread = input.child_thread.as_ref();
        let branch = child_thread
            .and_then(|thread| thread.branch)
            .filter(|b| !b.is_empty());
        let worktree = child_thread
            .and_then(|thread| thread.worktree_path)
            .filter(|p| !p.is_empty());
        workspace.push(SubagentWorkspaceEntry {
            label: if branch.is_some() {
                "Branch"
            } else if worktree.is_some() {
                "Worktree"
            } else {
                "Workspace"
            },
            value: branch
                .map(str::to_owned)
                .unwrap_or_else(|| file_basename(child_workspace).to_owned()),
        });
    }
    SubagentMetadata {
        model_label,
        workspace,
    }
}

/// Live work leads with progress; settled work leads with its result.
pub fn subagent_detail_preview(
    status: RuntimeSubagentStatus,
    result: Option<&str>,
    progress: Option<&str>,
) -> Option<String> {
    static WHITESPACE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+").expect("static regex"));
    let result = result
        .map(crate::state::contracts::js_trim)
        .filter(|v| !v.is_empty());
    let progress = progress
        .map(crate::state::contracts::js_trim)
        .filter(|v| !v.is_empty());
    let detail = if is_terminal_subagent_status(status) {
        result.or(progress)
    } else {
        progress.or(result)
    }
    .unwrap_or("");
    let compact = WHITESPACE.replace_all(detail, " ").into_owned();
    let units: Vec<u16> = compact.encode_utf16().collect();
    if units.len() > 280 {
        let head = String::from_utf16_lossy(&units[..280]);
        return Some(format!("{}…", head.trim_end()));
    }
    (!compact.is_empty()).then_some(compact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use OrchestrationV2TurnItemStatus as Status;

    #[test]
    fn a_mixed_group_stays_live_while_a_member_is_unfinished() {
        for status in [Status::Pending, Status::Running, Status::Waiting] {
            assert_eq!(
                subagent_group_summary(&[Status::Completed, status]),
                SubagentGroupSummary {
                    label: "Kicked off 2 subagents".into(),
                    active: true,
                    failed: false,
                }
            );
        }
    }

    #[test]
    fn settles_the_label_without_disguising_a_failed_member_as_success() {
        assert_eq!(
            subagent_group_summary(&[
                Status::Completed,
                Status::Failed,
                Status::Cancelled,
                Status::Interrupted
            ]),
            SubagentGroupSummary {
                label: "Ran 4 subagents".into(),
                active: false,
                failed: true,
            }
        );
    }

    #[test]
    fn uses_a_singular_label_for_one_idle_member() {
        assert_eq!(
            subagent_group_summary(&[Status::Idle]),
            SubagentGroupSummary {
                label: "Ran 1 subagent".into(),
                active: false,
                failed: false,
            }
        );
    }

    #[test]
    fn summarizes_statuses_in_scan_order() {
        assert_eq!(
            summarize_subagent_statuses(&[
                Status::Completed,
                Status::Running,
                Status::Cancelled,
                Status::Failed,
                Status::Completed
            ]),
            "1 working · 2 done · 1 failed · 1 stopped"
        );
    }

    #[test]
    fn formats_codex_task_paths() {
        assert_eq!(
            format_subagent_display_title("Subagent: /root/review_agent"),
            "Review Agent"
        );
        assert_eq!(
            format_subagent_display_title("/root/a/b/code_search/"),
            "Code Search"
        );
        assert_eq!(
            format_subagent_display_title("subagent:  Plain title"),
            "Plain title"
        );
        assert_eq!(
            format_subagent_display_title("/elsewhere/x"),
            "/elsewhere/x"
        );
    }

    fn haiku_provider() -> Vec<SubagentCatalogModel> {
        vec![SubagentCatalogModel {
            slug: "claude-haiku-4-5".into(),
            name: "Claude Haiku 4.5".into(),
            short_name: Some("Haiku 4.5".into()),
            aliases: vec!["claude-haiku-4-5-20251001".into()],
            sub_provider: None,
        }]
    }

    #[test]
    fn resolves_provider_aliases_to_catalog_names_including_custom_models() {
        let models = haiku_provider();
        let label =
            |input: ResolveSubagentMetadataInput<'_>| resolve_subagent_metadata(&input).model_label;
        assert_eq!(
            label(ResolveSubagentMetadataInput {
                model: Some("claude-haiku-4-5-20251001"),
                provider: Some(SubagentProvider {
                    driver: "claudeAgent",
                    models: &models
                }),
                ..Default::default()
            }),
            "Haiku 4.5"
        );
        let custom = vec![SubagentCatalogModel {
            slug: "my-model".into(),
            name: "Cloud+ / My custom model".into(),
            short_name: None,
            aliases: vec![],
            sub_provider: Some("Cloud+".into()),
        }];
        assert_eq!(
            label(ResolveSubagentMetadataInput {
                model: Some("my-model"),
                provider: Some(SubagentProvider {
                    driver: "acpRegistry",
                    models: &custom
                }),
                ..Default::default()
            }),
            "My custom model"
        );
    }

    #[test]
    fn keeps_unknown_model_identities_and_does_not_invent_an_unreported_model() {
        let label = |model: Option<&str>, provider: Option<SubagentProvider<'_>>| {
            resolve_subagent_metadata(&ResolveSubagentMetadataInput {
                model,
                provider,
                ..Default::default()
            })
            .model_label
        };
        assert_eq!(label(Some(" custom/model "), None), "custom/model");
        assert_eq!(
            label(
                None,
                Some(SubagentProvider {
                    driver: "codex",
                    models: &[]
                })
            ),
            "Not reported"
        );
        assert_eq!(label(Some(" "), None), "Not reported");
    }

    fn parent() -> SubagentParentThread<'static> {
        SubagentParentThread {
            project_id: "parent",
            worktree_path: None,
        }
    }

    #[test]
    fn shows_another_project_and_its_branch_when_the_child_has_a_different_workspace() {
        let metadata = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
            parent_thread: Some(parent()),
            parent_project_workspace_root: Some("/repo"),
            child_thread: Some(SubagentChildThread {
                branch: Some("fix/agents"),
                worktree_path: Some("/worktrees/agents"),
            }),
            child_project: Some(SubagentChildProject {
                id: "child",
                title: "Other project",
                workspace_root: "/other",
            }),
            ..Default::default()
        });
        assert_eq!(
            metadata.workspace,
            vec![
                SubagentWorkspaceEntry {
                    label: "Project",
                    value: "Other project".into()
                },
                SubagentWorkspaceEntry {
                    label: "Branch",
                    value: "fix/agents".into()
                },
            ]
        );
    }

    #[test]
    fn labels_a_detached_worktree_or_another_project_workspace_without_a_branch() {
        let worktree = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
            parent_thread: Some(parent()),
            parent_project_workspace_root: Some("/repo"),
            child_thread: Some(SubagentChildThread {
                branch: None,
                worktree_path: Some("/worktrees/agents"),
            }),
            ..Default::default()
        });
        assert_eq!(
            worktree.workspace,
            vec![SubagentWorkspaceEntry {
                label: "Worktree",
                value: "agents".into()
            }]
        );
        let workspace = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
            parent_thread: Some(parent()),
            parent_project_workspace_root: Some("/repo"),
            child_project: Some(SubagentChildProject {
                id: "parent",
                title: "Same project",
                workspace_root: "/other",
            }),
            ..Default::default()
        });
        assert_eq!(
            workspace.workspace,
            vec![SubagentWorkspaceEntry {
                label: "Workspace",
                value: "other".into()
            }]
        );
    }

    #[test]
    fn hides_redundant_workspace_metadata_and_tolerates_unavailable_child_shells() {
        let redundant = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
            parent_thread: Some(parent()),
            parent_project_workspace_root: Some("/repo"),
            child_thread: Some(SubagentChildThread {
                branch: Some("main"),
                worktree_path: Some("/repo"),
            }),
            child_project: Some(SubagentChildProject {
                id: "parent",
                title: "Same project",
                workspace_root: "/repo",
            }),
            ..Default::default()
        });
        assert_eq!(redundant.workspace, Vec::new());
        let missing = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
            parent_thread: Some(parent()),
            parent_project_workspace_root: Some("/repo"),
            ..Default::default()
        });
        assert_eq!(missing.workspace, Vec::new());
    }

    #[test]
    fn detail_preview_prefers_progress_for_live_work_and_results_for_settled_work() {
        let (progress, result) = (Some("Reading files"), Some("Found two\n  problems"));
        assert_eq!(
            subagent_detail_preview(RuntimeSubagentStatus::Running, result, progress).as_deref(),
            Some("Reading files")
        );
        assert_eq!(
            subagent_detail_preview(RuntimeSubagentStatus::Completed, result, progress).as_deref(),
            Some("Found two problems")
        );
        assert_eq!(
            subagent_detail_preview(
                RuntimeSubagentStatus::Failed,
                Some(" "),
                Some("Last progress")
            )
            .as_deref(),
            Some("Last progress")
        );
        assert_eq!(
            subagent_detail_preview(RuntimeSubagentStatus::Pending, None, None),
            None
        );
    }
}
