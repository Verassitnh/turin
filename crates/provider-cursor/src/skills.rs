//! Port of upstream/packages/provider-cursor/src/server/skills.ts.
//!
//! Cursor discovers Agent Skills recursively from user and project roots.
//! Scanning the same roots avoids starting an agent just to populate a menu.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::Deserialize;
use thiserror::Error;
use turin_contracts::base_schemas::TrimmedNonEmptyString as Tne;
use turin_provider_core::server_provider::ServerProviderSkill;

const MAX_SKILL_DEPTH: usize = 10;
const MAX_SKILL_BYTES: u64 = 1_000_000;
const MAX_SKILL_SCAN_ENTRIES: usize = 10_000;
const MAX_SKILL_SCAN_BYTES: u64 = 8_000_000;

static FRONTMATTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)").expect("frontmatter"));
static SKILL_MENTION: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r"(?u)(^|\s)\p{Sc}(?![0-9][0-9_]*(?:[kKmMbBtT]|[eE][0-9]+)?(?:\s|$))(?=[a-zA-Z0-9:_-]*[a-zA-Z])([a-zA-Z0-9][a-zA-Z0-9:_-]*)(?=\s|$)",
    )
    .expect("skill mention")
});

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillFrontmatter {
    description: Option<String>,
    display_name: Option<String>,
    user_invocation_only: bool,
    user_invocable: Option<bool>,
    cli_visible: bool,
}

#[derive(Debug, Default)]
struct ScanBudget {
    remaining_entries: usize,
    remaining_bytes: u64,
    exhausted: bool,
    incomplete: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("Cursor skill discovery{location} was incomplete ({reason}).")]
pub struct CursorSkillsProbeError {
    reason: &'static str,
    location: String,
}

impl CursorSkillsProbeError {
    fn new(reason: &'static str, cwd: Option<&Path>) -> Self {
        Self {
            reason,
            location: cwd
                .map(|path| format!(" for '{}'", path.display()))
                .unwrap_or_default(),
        }
    }
}

fn tne(value: &str) -> Tne {
    Tne::new(value).unwrap_or_else(|_| Tne::new("skill").expect("fallback"))
}

fn parse_frontmatter_boolean(value: Option<&serde_yaml::Value>) -> Option<bool> {
    let value = value?;
    match value {
        serde_yaml::Value::Bool(value) => Some(*value),
        serde_yaml::Value::Number(number) => number.as_i64().and_then(|number| match number {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }),
        serde_yaml::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" => Some(true),
            "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Deserialize)]
struct RawFrontmatter {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: Option<serde_yaml::Value>,
    #[serde(default, rename = "user-invocable")]
    user_invocable: Option<serde_yaml::Value>,
    #[serde(default)]
    metadata: Option<RawMetadata>,
}

#[derive(Deserialize)]
struct RawMetadata {
    #[serde(default)]
    surfaces: Option<serde_yaml::Value>,
}

fn parse_skill_frontmatter(contents: &str) -> Option<SkillFrontmatter> {
    let captures = FRONTMATTER.captures(contents)?;
    let body = captures.get(1)?.as_str();
    let record: RawFrontmatter = serde_yaml::from_str(body).ok()?;
    let surfaces: Vec<String> = match record
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.surfaces.as_ref())
    {
        Some(serde_yaml::Value::Sequence(values)) => values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
        Some(serde_yaml::Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    Some(SkillFrontmatter {
        cli_visible: surfaces.is_empty()
            || surfaces
                .iter()
                .any(|surface| surface.trim().eq_ignore_ascii_case("cli")),
        description: record
            .description
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty()),
        display_name: record
            .name
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty()),
        user_invocation_only: parse_frontmatter_boolean(record.disable_model_invocation.as_ref())
            .is_some_and(|value| value),
        user_invocable: parse_frontmatter_boolean(record.user_invocable.as_ref()),
    })
}

fn path_within(root: &Path, child: &Path) -> bool {
    child == root
        || child
            .strip_prefix(root)
            .ok()
            .is_some_and(|suffix| !suffix.as_os_str().is_empty())
}

fn user_home(environment: &HashMap<String, String>) -> Option<PathBuf> {
    environment
        .get("HOME")
        .or_else(|| environment.get("USERPROFILE"))
        .map(|home| home.trim())
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn skill_roots(base: &Path) -> [PathBuf; 4] {
    [
        base.join(".cursor/skills"),
        base.join(".agents/skills"),
        base.join(".codex/skills"),
        base.join(".claude/skills"),
    ]
}

async fn discover_skills_in_root(
    directory: &Path,
    scope: &str,
    budget: &mut ScanBudget,
) -> Vec<ServerProviderSkill> {
    let mut skills = Vec::new();
    if budget.exhausted {
        return skills;
    }
    let root_directory = match tokio::fs::canonicalize(directory).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return skills,
        Err(_) => {
            budget.incomplete = true;
            return skills;
        }
    };
    let mut visited = HashSet::new();
    visit_skill_directory(
        &root_directory,
        directory,
        scope,
        0,
        budget,
        &mut visited,
        &mut skills,
    )
    .await;
    skills
}

async fn visit_skill_directory(
    root_directory: &Path,
    directory: &Path,
    scope: &str,
    depth: usize,
    budget: &mut ScanBudget,
    visited: &mut HashSet<PathBuf>,
    skills: &mut Vec<ServerProviderSkill>,
) {
    if budget.exhausted {
        return;
    }
    let resolved = match tokio::fs::canonicalize(directory).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            budget.incomplete = true;
            return;
        }
    };
    if !visited.insert(resolved.clone()) {
        return;
    }
    let inside_root = path_within(root_directory, &resolved);

    let skill_path = directory.join("SKILL.md");
    if let Ok(meta) = tokio::fs::metadata(&skill_path).await
        && meta.is_file()
    {
        let mut frontmatter = SkillFrontmatter {
            cli_visible: true,
            description: None,
            display_name: None,
            user_invocation_only: false,
            user_invocable: None,
        };
        if meta.len() <= MAX_SKILL_BYTES
            && meta.len() <= budget.remaining_bytes
            && let Ok(contents) = tokio::fs::read_to_string(&skill_path).await
        {
            budget.remaining_bytes = budget.remaining_bytes.saturating_sub(meta.len());
            if FRONTMATTER.is_match(&contents) {
                if let Some(parsed) = parse_skill_frontmatter(&contents) {
                    frontmatter = parsed;
                } else {
                    frontmatter.cli_visible = false;
                }
            }
        }
        if let Some(name) = directory
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            && frontmatter.cli_visible
        {
            skills.push(ServerProviderSkill {
                name: tne(name),
                description: frontmatter.description.as_deref().map(tne),
                path: tne(skill_path.to_string_lossy().as_ref()),
                scope: Some(tne(scope)),
                enabled: true,
                display_name: frontmatter
                    .display_name
                    .filter(|display| display != name)
                    .as_deref()
                    .map(tne),
                short_description: None,
                user_invocation_only: frontmatter.user_invocation_only.then_some(true),
                user_invocable: frontmatter.user_invocable,
            });
        }
    }

    if !inside_root {
        return;
    }
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            budget.incomplete = true;
            return;
        }
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        if budget.remaining_entries == 0 {
            budget.exhausted = true;
            return;
        }
        budget.remaining_entries -= 1;
        let child = directory.join(name);
        let Ok(meta) = tokio::fs::metadata(&child).await else {
            budget.incomplete = true;
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        if depth + 1 > MAX_SKILL_DEPTH {
            continue;
        }
        Box::pin(visit_skill_directory(
            root_directory,
            &child,
            scope,
            depth + 1,
            budget,
            visited,
            skills,
        ))
        .await;
    }
}

async fn inspect_cursor_skills(
    cwd: Option<&Path>,
    environment: &HashMap<String, String>,
) -> (Vec<ServerProviderSkill>, Option<&'static str>) {
    let Some(user_home) = user_home(environment) else {
        return (Vec::new(), None);
    };
    let mut roots: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Some(cwd) = cwd {
        for directory in skill_roots(cwd) {
            roots.push((directory, "project"));
        }
    }
    for directory in skill_roots(&user_home) {
        roots.push((directory, "user"));
    }
    let mut skills_by_name = HashMap::new();
    let mut budget = ScanBudget {
        remaining_entries: MAX_SKILL_SCAN_ENTRIES,
        remaining_bytes: MAX_SKILL_SCAN_BYTES,
        ..ScanBudget::default()
    };
    for (directory, scope) in roots {
        if budget.exhausted {
            break;
        }
        for skill in discover_skills_in_root(&directory, scope, &mut budget).await {
            skills_by_name
                .entry(skill.name.as_str().to_owned())
                .or_insert(skill);
        }
    }
    let mut skills: Vec<_> = skills_by_name.into_values().collect();
    skills.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    let failure = if budget.exhausted {
        Some("scan-budget-exhausted")
    } else if budget.incomplete {
        Some("filesystem-error")
    } else {
        None
    };
    (skills, failure)
}

pub async fn discover_cursor_skills(
    cwd: Option<&Path>,
    environment: &HashMap<String, String>,
) -> Vec<ServerProviderSkill> {
    inspect_cursor_skills(cwd, environment).await.0
}

pub async fn probe_cursor_skills(
    cwd: Option<&Path>,
    environment: &HashMap<String, String>,
) -> Result<Vec<ServerProviderSkill>, CursorSkillsProbeError> {
    let (skills, failure) = inspect_cursor_skills(cwd, environment).await;
    if let Some(reason) = failure {
        Err(CursorSkillsProbeError::new(reason, cwd))
    } else {
        Ok(skills)
    }
}

pub fn has_cursor_skill_mention(prompt: &str) -> bool {
    SKILL_MENTION.is_match(prompt).unwrap_or(false)
}

pub fn rewrite_cursor_skill_mentions(prompt: &str, skill_names: &HashSet<String>) -> String {
    let mut out = String::with_capacity(prompt.len());
    let mut last = 0usize;
    for cap in SKILL_MENTION.captures_iter(prompt).flatten() {
        let Some(matched) = cap.get(0) else {
            continue;
        };
        out.push_str(&prompt[last..matched.start()]);
        let prefix = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        if skill_names.contains(name) {
            out.push_str(&format!("{prefix}/{name}"));
        } else {
            out.push_str(matched.as_str());
        }
        last = matched.end();
    }
    out.push_str(&prompt[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn env(home: &Path) -> HashMap<String, String> {
        HashMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())])
    }

    async fn write_skill(root: &Path, name: &str, contents: &str) {
        let directory = root.join(name);
        tokio::fs::create_dir_all(&directory).await.unwrap();
        tokio::fs::write(directory.join("SKILL.md"), contents)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn discovers_recursive_project_skills_with_project_precedence() {
        let user_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        write_skill(
            &user_home.path().join(".cursor/skills"),
            "review",
            "---\ndescription: user review\n---\n",
        )
        .await;
        write_skill(
            &workspace.path().join(".agents/skills/nested"),
            "review",
            "---\nname: Review changes\ndescription: project review\n---\n",
        )
        .await;
        write_skill(
            &workspace.path().join(".cursor/skills"),
            "internal",
            "---\nuser-invocable: false\n---\n",
        )
        .await;
        write_skill(
            &workspace.path().join(".cursor/skills"),
            "oversized",
            &"x".repeat(1_000_001),
        )
        .await;
        tokio::fs::create_dir_all(user_home.path().join(".codex"))
            .await
            .unwrap();
        tokio::fs::write(user_home.path().join(".codex/skills"), "not a directory")
            .await
            .unwrap();

        let skills = discover_cursor_skills(Some(workspace.path()), &env(user_home.path())).await;
        assert_eq!(skills.len(), 3);
        assert!(skills.iter().any(|skill| skill.name.as_str() == "internal"));
        assert!(
            skills
                .iter()
                .any(|skill| skill.name.as_str() == "oversized")
        );
        let review = skills
            .iter()
            .find(|skill| skill.name.as_str() == "review")
            .unwrap();
        assert_eq!(
            review.display_name.as_ref().map(Tne::as_str),
            Some("Review changes")
        );
        assert!(
            probe_cursor_skills(Some(workspace.path()), &env(user_home.path()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn skips_a_skill_tree_deeper_than_the_limit_instead_of_failing_discovery() {
        let user_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        write_skill(
            &workspace.path().join(".agents/skills"),
            "review",
            "---\n---\n",
        )
        .await;
        let at_limit = user_home
            .path()
            .join(".claude/skills/deep/2/3/4/5/6/7/8/9/deep-skill");
        tokio::fs::create_dir_all(&at_limit).await.unwrap();
        tokio::fs::write(at_limit.join("SKILL.md"), "---\n---\n")
            .await
            .unwrap();
        write_skill(&at_limit, "below", "---\n---\n").await;
        write_skill(
            &user_home.path().join(".claude/skills"),
            "later",
            "---\n---\n",
        )
        .await;
        let skills = discover_cursor_skills(Some(workspace.path()), &env(user_home.path())).await;
        assert_eq!(skills.len(), 3);
        assert!(
            probe_cursor_skills(Some(workspace.path()), &env(user_home.path()))
                .await
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn treats_a_symlinked_skill_outside_the_root_as_a_package_boundary() {
        let user_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let library = tempfile::tempdir().unwrap();
        write_skill(
            library.path(),
            "shared-review",
            "---\ndescription: shared\n---\n",
        )
        .await;
        write_skill(
            &library.path().join("shared-review"),
            "hidden",
            "---\n---\n",
        )
        .await;
        let root = workspace.path().join(".cursor/skills");
        tokio::fs::create_dir_all(&root).await.unwrap();
        if std::os::unix::fs::symlink(library.path().join("shared-review"), root.join("review"))
            .is_err()
        {
            return;
        }
        let skills = discover_cursor_skills(Some(workspace.path()), &env(user_home.path())).await;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name.as_str(), "review");
        assert_eq!(
            skills[0].description.as_ref().map(Tne::as_str),
            Some("shared")
        );
        assert!(
            skills[0]
                .path
                .as_str()
                .ends_with(".cursor/skills/review/SKILL.md")
        );
        assert_eq!(skills[0].scope.as_ref().map(Tne::as_str), Some("project"));
        assert!(skills[0].enabled);
        assert!(skills.iter().all(|skill| skill.name.as_str() != "hidden"));
        assert!(
            probe_cursor_skills(Some(workspace.path()), &env(user_home.path()))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn still_fails_discovery_when_a_root_holds_more_entries_than_the_scan_allows() {
        let user_home = tempfile::tempdir().unwrap();
        let root = user_home.path().join(".cursor/skills");
        tokio::fs::create_dir_all(&root).await.unwrap();
        for index in 0..10_001 {
            tokio::fs::write(root.join(format!("entry-{index}")), "")
                .await
                .unwrap();
        }
        let error = probe_cursor_skills(None, &env(user_home.path()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("scan-budget-exhausted"));
    }

    #[test]
    fn rewrites_only_discovered_skill_mentions_into_cursor_slash_invocations() {
        assert!(has_cursor_skill_mention("use $Review_Pr:V2 here"));
        assert!(has_cursor_skill_mention("please $review this"));
        assert_eq!(
            rewrite_cursor_skill_mentions(
                "use $review, keep $HOME and 5$review",
                &HashSet::from(["review".to_owned()])
            ),
            "use $review, keep $HOME and 5$review"
        );
        assert_eq!(
            rewrite_cursor_skill_mentions(
                "please $review this",
                &HashSet::from(["review".to_owned()])
            ),
            "please /review this"
        );
    }

    #[test]
    fn detects_and_invokes_digit_leading_cursor_skills_without_rewriting_money() {
        let names = HashSet::from([
            "2spec".to_owned(),
            "20k".to_owned(),
            "100M".to_owned(),
            "1e6".to_owned(),
        ]);
        assert!(has_cursor_skill_mention("use $2spec here"));
        assert_eq!(
            rewrite_cursor_skill_mentions("use $2spec here", &names),
            "use /2spec here"
        );
        assert_eq!(
            rewrite_cursor_skill_mentions("use $2spec here", &HashSet::new()),
            "use $2spec here"
        );
        for text in [
            "pay $20 tomorrow",
            "budget $20k here",
            "cost $100M total",
            "limit $1e6 here",
        ] {
            assert!(!has_cursor_skill_mention(text));
            assert_eq!(rewrite_cursor_skill_mentions(text, &names), text);
        }
    }
}
