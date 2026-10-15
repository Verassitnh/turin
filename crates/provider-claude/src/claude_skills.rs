//! Port of upstream/apps/server/src/provider/Drivers/ClaudeSkills.ts:
//! filesystem discovery of Claude Code skills for the `$` picker.
//!
//! Claude Code loads skills from `<config dir>/skills` (user scope) and
//! `<cwd>/.claude/skills` (project scope), one directory per skill with a
//! `SKILL.md` carrying YAML frontmatter. The user root wins on name collisions,
//! matching the CLI. `.agents/skills` is a Codex location: verified against the
//! CLI, a skill that lives only there is answered with `Unknown command`, so it
//! is not scanned here.
//!
//! The Agent SDK init handshake surfaces skills only as slash commands without
//! their filesystem paths, so the provider snapshot scans the same locations
//! directly. Discovery is best-effort: an unreadable root or a malformed skill
//! is skipped so a broken skill never degrades the snapshot.
use crate::claude_home::ProcessEnv;
use regex::Regex;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use turin_contracts::{TrimmedNonEmptyString, settings::ClaudeSettings};
use turin_provider_core::{
    host_process::Platform, path_expansion::expand_home_path, server_provider::ServerProviderSkill,
};
use yaml_rust2::{Yaml, YamlLoader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillScope {
    User,
    Project,
}

impl SkillScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// What a skill's frontmatter says. `Missing` means no frontmatter at all, and
/// `Malformed` means the block would not parse, so the skill is skipped.
#[derive(Debug, Clone, PartialEq)]
enum SkillFrontmatter {
    Missing,
    Malformed,
    Parsed {
        description: Option<String>,
        user_invocation_only: bool,
        user_invocable: Option<bool>,
    },
}

fn frontmatter_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)").expect("pattern"))
}

/// `[\w-]+:` keys for the colon-repair pass. ASCII only, as JS `\w` is.
fn key_line_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?m)^([A-Za-z0-9_-]+:[ \t]*)([^\r\n]*)").expect("pattern"))
}

fn comment_split_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[ \t]+#").expect("pattern"))
}

fn plain_scalar_start_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"^(?:["'\[\]{}|>&*!#%@`]|[-?:](?:[ \t]|$))"#).expect("pattern")
    })
}

/// `parseFrontmatterBoolean`: YAML 1.1 boolean spellings, as Claude Code reads them.
fn parse_frontmatter_boolean(value: &Yaml) -> Option<bool> {
    match value {
        Yaml::Boolean(flag) => Some(*flag),
        Yaml::Integer(1) => Some(true),
        Yaml::Integer(0) => Some(false),
        Yaml::Real(text) => match text.parse::<f64>().ok()? {
            number if number == 1.0 => Some(true),
            number if number == 0.0 => Some(false),
            _ => None,
        },
        Yaml::String(text) => match text.trim().to_lowercase().as_str() {
            "true" | "yes" | "on" | "y" => Some(true),
            "false" | "no" | "off" | "n" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `parseSkillFrontmatter`. A plain scalar with `: ` inside (a common hand-written
/// description) fails strict YAML, so those lines are repaired by quoting them,
/// leaving comments and the rest of the document alone.
fn parse_skill_frontmatter(contents: &str) -> SkillFrontmatter {
    let Some(captures) = frontmatter_pattern().captures(contents) else {
        return SkillFrontmatter::Missing;
    };
    let frontmatter = captures.get(1).map_or("", |m| m.as_str());
    let document = match YamlLoader::load_from_str(frontmatter) {
        Ok(documents) => documents,
        Err(_) => {
            let repaired = repair_plain_scalars(frontmatter);
            match YamlLoader::load_from_str(&repaired) {
                Ok(documents) => documents,
                Err(_) => return SkillFrontmatter::Malformed,
            }
        }
    };
    let Some(Yaml::Hash(record)) = document.into_iter().next() else {
        return SkillFrontmatter::Malformed;
    };
    let field = |key: &str| record.get(&Yaml::String(key.to_owned()));

    let description = match field("description") {
        Some(Yaml::String(text)) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    };
    let user_invocation_only =
        field("disable-model-invocation").and_then(parse_frontmatter_boolean) == Some(true);
    SkillFrontmatter::Parsed {
        description,
        user_invocation_only,
        user_invocable: field("user-invocable").and_then(parse_frontmatter_boolean),
    }
}

/// The colon-repair pass: quotes a `key: value` scalar that contains `: `,
/// unless the value already starts a YAML structure or is commented out.
fn repair_plain_scalars(frontmatter: &str) -> String {
    key_line_pattern()
        .replace_all(frontmatter, |captures: &regex::Captures<'_>| {
            let whole = captures.get(0).map_or("", |m| m.as_str());
            let prefix = captures.get(1).map_or("", |m| m.as_str());
            let value = captures.get(2).map_or("", |m| m.as_str());
            let scalar = comment_split_pattern().split(value).next().unwrap_or("");
            let has_colon_space = scalar.contains(": ") || scalar.contains(":\t");
            if !has_colon_space || plain_scalar_start_pattern().is_match(scalar) {
                return whole.to_owned();
            }
            let quoted = serde_json::to_string(scalar).unwrap_or_default();
            format!("{prefix}{quoted}{}", &value[scalar.len()..])
        })
        .into_owned()
}

/// Where the managed policy file sits for a platform. Absent on almost every
/// machine, which is why a missing file is the normal case.
fn claude_managed_settings_path(platform: Platform, environment: &ProcessEnv) -> Option<String> {
    match platform {
        Platform::Darwin => {
            Some("/Library/Application Support/ClaudeCode/managed-settings.json".to_owned())
        }
        Platform::Win32 => {
            let program_data = environment.get("PROGRAMDATA")?.trim();
            (!program_data.is_empty()).then(|| {
                join_path(
                    platform,
                    program_data,
                    &["ClaudeCode", "managed-settings.json"],
                )
            })
        }
        _ => Some("/etc/claude-code/managed-settings.json".to_owned()),
    }
}

/// Joins with the platform's separator, so the path lists read the same on every
/// host. The discovery itself uses the real filesystem.
fn join_path(platform: Platform, base: &str, segments: &[&str]) -> String {
    let separator = if platform == Platform::Win32 {
        '\\'
    } else {
        '/'
    };
    let mut joined = base.to_owned();
    for segment in segments {
        if !joined.ends_with(separator) {
            joined.push(separator);
        }
        joined.push_str(segment);
    }
    joined
}

/// `skillOverrideSettingsPaths`: the settings files Claude Code merges for
/// `skillOverrides`, in increasing precedence. User, project, project-local, the
/// repository root's local file when the workspace sits in a nested repository,
/// then the administrator's managed policy, which wins outright.
pub fn skill_override_settings_paths(
    config_dir: &str,
    cwd: Option<&str>,
    repository_root: Option<&str>,
    platform: Platform,
    environment: &ProcessEnv,
) -> Vec<String> {
    let mut paths = vec![join_path(platform, config_dir, &["settings.json"])];
    if let Some(cwd) = cwd {
        paths.push(join_path(platform, cwd, &[".claude", "settings.json"]));
        paths.push(join_path(
            platform,
            cwd,
            &[".claude", "settings.local.json"],
        ));
    }
    if let Some(root) = repository_root.filter(|root| Some(*root) != cwd) {
        paths.push(join_path(
            platform,
            root,
            &[".claude", "settings.local.json"],
        ));
    }
    if let Some(managed) = claude_managed_settings_path(platform, environment) {
        paths.push(managed);
    }
    paths
}

/// Nearest ancestor of `cwd` (inclusive) holding a `.git` entry, the boundary
/// Claude Code walks up to for project settings. `None` outside a repository.
fn find_repository_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = std::path::absolute(cwd).ok()?;
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

/// Settings `skillOverrides` values. The CLI validates the whole map: one
/// unknown value or a boolean drops every override in that file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SkillOverride {
    enabled: bool,
    user_invocation_only: bool,
}

fn parse_skill_override(value: &str) -> Option<SkillOverride> {
    Some(match value {
        "off" => SkillOverride {
            enabled: false,
            user_invocation_only: false,
        },
        "user-invocable-only" => SkillOverride {
            enabled: true,
            user_invocation_only: true,
        },
        "on" | "name-only" => SkillOverride {
            enabled: true,
            user_invocation_only: false,
        },
        _ => return None,
    })
}

/// `skillOverrides` from one file, or `None` when the file is unreadable, not
/// JSON, or has any invalid entry (the whole file is then ignored).
fn read_skill_override_file(path: &str) -> Option<HashMap<String, SkillOverride>> {
    let contents = fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&lenient_json_to_strict(&contents)).ok()?;
    let overrides = parsed.as_object()?.get("skillOverrides")?;
    let entries = overrides.as_object()?;
    entries
        .iter()
        .map(|(name, value)| Some((name.clone(), parse_skill_override(value.as_str()?)?)))
        .collect()
}

/// `parseLenientJsonGetter`: strips `//` and `/* */` comments and trailing
/// commas before `}` or `]`. String literals are left intact, so a `//` or `,]`
/// inside a value survives.
pub fn lenient_json_to_strict(input: &str) -> String {
    static LINE: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static TRAILING: OnceLock<Regex> = OnceLock::new();
    let line = LINE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|//[^\n]*"#).expect("pattern"));
    let block =
        BLOCK.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|/\*[\s\S]*?\*/"#).expect("pattern"));
    let trailing =
        TRAILING.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|,(\s*[}\]])"#).expect("pattern"));

    let strip = |pattern: &Regex, text: &str, keep: &dyn Fn(&regex::Captures<'_>) -> String| {
        pattern
            .replace_all(text, |captures: &regex::Captures<'_>| {
                if captures.get(1).is_some() {
                    captures
                        .get(0)
                        .map_or(String::new(), |m| m.as_str().to_owned())
                } else {
                    keep(captures)
                }
            })
            .into_owned()
    };
    let without_lines = strip(line, input, &|_| String::new());
    let without_blocks = strip(block, &without_lines, &|_| String::new());
    strip(trailing, &without_blocks, &|captures| {
        captures
            .get(2)
            .map_or(String::new(), |m| m.as_str().to_owned())
    })
}

/// `readSkillOverrides`: later files win, in the precedence order of
/// [`skill_override_settings_paths`].
fn read_skill_overrides(
    config_dir: &str,
    cwd: Option<&Path>,
    platform: Platform,
    environment: &ProcessEnv,
) -> HashMap<String, SkillOverride> {
    let cwd_text = cwd.map(|path| path.to_string_lossy().into_owned());
    let repository_root = cwd
        .and_then(find_repository_root)
        .map(|root| root.to_string_lossy().into_owned());
    let mut overrides = HashMap::new();
    for path in skill_override_settings_paths(
        config_dir,
        cwd_text.as_deref(),
        repository_root.as_deref(),
        platform,
        environment,
    ) {
        if let Some(file_overrides) = read_skill_override_file(&path) {
            overrides.extend(file_overrides);
        }
    }
    overrides
}

/// `resolveClaudeConfigDirPath`: the instance's `homePath`, then `CLAUDE_CONFIG_DIR`
/// (relative values resolve against the workspace cwd, as the spawned CLI sees
/// them), then `~/.claude`.
fn resolve_config_dir(
    home_path: &str,
    cwd: Option<&Path>,
    environment: &ProcessEnv,
    home: &str,
) -> String {
    let home_path = home_path.trim();
    if !home_path.is_empty() {
        return absolute_text(&expand_home_path(home_path, home));
    }
    let inherited = environment
        .get("CLAUDE_CONFIG_DIR")
        .map(|value| value.trim())
        .unwrap_or("");
    if !inherited.is_empty() {
        let base = match cwd {
            Some(cwd) => cwd.join(inherited),
            None => PathBuf::from(inherited),
        };
        return absolute_text(&base.to_string_lossy());
    }
    Path::new(home)
        .join(".claude")
        .to_string_lossy()
        .into_owned()
}

fn absolute_text(value: &str) -> String {
    std::path::absolute(value)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| value.to_owned())
}

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

/// `discoverClaudeSkills`: user skills and workspace skills, with the user root
/// winning on a name collision. Sorted by name.
pub fn discover_claude_skills(
    settings: &ClaudeSettings,
    cwd: Option<&Path>,
    environment: &ProcessEnv,
    home: &str,
) -> Vec<ServerProviderSkill> {
    discover_claude_skills_for(settings, cwd, environment, home, Platform::current())
}

fn discover_claude_skills_for(
    settings: &ClaudeSettings,
    cwd: Option<&Path>,
    environment: &ProcessEnv,
    home: &str,
    platform: Platform,
) -> Vec<ServerProviderSkill> {
    let config_dir = resolve_config_dir(settings.home_path.as_str(), cwd, environment, home);
    let overrides = read_skill_overrides(&config_dir, cwd, platform, environment);

    let mut roots: Vec<(PathBuf, SkillScope)> =
        vec![(Path::new(&config_dir).join("skills"), SkillScope::User)];
    if let Some(cwd) = cwd {
        roots.push((cwd.join(".claude").join("skills"), SkillScope::Project));
    }

    let mut by_name: HashMap<String, ServerProviderSkill> = HashMap::new();
    for (root, scope) in roots {
        let Ok(read_dir) = fs::read_dir(&root) else {
            continue;
        };
        let mut entries: Vec<String> = read_dir
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        entries.sort();
        for entry in entries {
            let skill_path = root.join(&entry).join("SKILL.md");
            let Ok(contents) = fs::read_to_string(&skill_path) else {
                continue;
            };
            let frontmatter = parse_skill_frontmatter(&contents);
            // Malformed frontmatter means Claude Code will not load the skill either.
            if frontmatter == SkillFrontmatter::Malformed {
                continue;
            }
            // Claude Code identifies a skill by its directory, not the frontmatter `name`.
            let name = entry.trim();
            if name.is_empty() || by_name.contains_key(name) {
                // First root wins: a later root never displaces a higher-precedence skill.
                continue;
            }
            let override_ = overrides.get(name).copied();
            let (description, parsed_only, invocable) = match &frontmatter {
                SkillFrontmatter::Parsed {
                    description,
                    user_invocation_only,
                    user_invocable,
                } => (description.clone(), *user_invocation_only, *user_invocable),
                _ => (None, false, None),
            };
            let user_invocation_only =
                parsed_only || override_.is_some_and(|o| o.user_invocation_only);
            let (Some(name_tne), Some(path_tne)) = (tne(name), tne(&skill_path.to_string_lossy()))
            else {
                continue;
            };
            by_name.insert(
                name.to_owned(),
                ServerProviderSkill {
                    name: name_tne,
                    description: description.as_deref().and_then(tne),
                    path: path_tne,
                    scope: tne(scope.as_str()),
                    enabled: override_.map_or(true, |o| o.enabled),
                    display_name: None,
                    short_description: None,
                    user_invocation_only: user_invocation_only.then_some(true),
                    user_invocable: (invocable == Some(false)).then_some(false),
                },
            );
        }
    }

    let mut skills: Vec<ServerProviderSkill> = by_name.into_values().collect();
    // The CLI's list is locale-sorted; case-insensitive order approximates it.
    skills.sort_by(|left, right| {
        left.name
            .as_str()
            .to_lowercase()
            .cmp(&right.name.as_str().to_lowercase())
            .then_with(|| left.name.as_str().cmp(right.name.as_str()))
    });
    skills
}

/// `enabled`, invocable-by-user skills: what the composer may offer under `/`.
pub fn user_invocable_skill_names(
    settings: &ClaudeSettings,
    cwd: Option<&Path>,
    environment: &ProcessEnv,
    home: &str,
) -> HashSet<String> {
    discover_claude_skills(settings, cwd, environment, home)
        .into_iter()
        .filter(|skill| skill.enabled && skill.user_invocable != Some(false))
        .map(|skill| skill.name.as_str().to_owned())
        .collect()
}

#[cfg(test)]
mod tests;
