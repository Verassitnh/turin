//! Port of upstream/apps/server/src/provider/Drivers/ClaudeSkills.test.ts.
//! Each upstream `it.effect` case is a Rust test with the same name, in order.
use super::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;
use turin_contracts::settings::ClaudeSettings;

fn settings(home_path: &str) -> ClaudeSettings {
    serde_json::from_value(json!({ "homePath": home_path })).expect("settings decode")
}

fn env(pairs: &[(&str, &str)]) -> ProcessEnv {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn write_skill(skills_dir: &Path, directory_name: &str, contents: &str) {
    let skill_dir = skills_dir.join(directory_name);
    fs::create_dir_all(&skill_dir).expect("skill dir");
    fs::write(skill_dir.join("SKILL.md"), contents).expect("SKILL.md");
}

fn lines(parts: &[&str]) -> String {
    parts.join("\n")
}

fn names(skills: &[ServerProviderSkill]) -> Vec<&str> {
    skills.iter().map(|skill| skill.name.as_str()).collect()
}

fn name_enabled(skills: &[ServerProviderSkill]) -> Vec<(&str, bool)> {
    skills
        .iter()
        .map(|skill| (skill.name.as_str(), skill.enabled))
        .collect()
}

fn discover(config_dir: &Path, cwd: Option<&Path>) -> Vec<ServerProviderSkill> {
    discover_claude_skills_for(
        &settings(config_dir.to_str().expect("utf8")),
        cwd,
        &ProcessEnv::new(),
        "/home/test",
        Platform::Linux,
    )
}

fn tmp() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn discovers_user_and_project_skills_with_frontmatter_metadata() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let workspace = temp.path().join("workspace");
    write_skill(
        &config_dir.join("skills"),
        "codex-review",
        &lines(&[
            "---",
            "name: codex-review",
            "description: Ask Codex for a review.",
            "---",
            "",
            "# Body",
        ]),
    );
    write_skill(
        &workspace.join(".claude").join("skills"),
        "deploy",
        &lines(&[
            "---",
            "name: deploy",
            "description: Deploy the app.",
            "---",
            "",
            "# Deploy",
        ]),
    );

    let skills = discover(&config_dir, Some(&workspace));

    assert_eq!(
        skills,
        vec![
            ServerProviderSkill {
                name: req("codex-review"),
                description: opt("Ask Codex for a review."),
                path: req(&config_dir
                    .join("skills")
                    .join("codex-review")
                    .join("SKILL.md")
                    .to_string_lossy()),
                scope: opt("user"),
                enabled: true,
                display_name: None,
                short_description: None,
                user_invocation_only: None,
                user_invocable: None,
            },
            ServerProviderSkill {
                name: req("deploy"),
                description: opt("Deploy the app."),
                path: req(&workspace
                    .join(".claude")
                    .join("skills")
                    .join("deploy")
                    .join("SKILL.md")
                    .to_string_lossy()),
                scope: opt("project"),
                enabled: true,
                display_name: None,
                short_description: None,
                user_invocation_only: None,
                user_invocable: None,
            },
        ]
    );
}

#[test]
fn ignores_agents_skills_which_claude_code_does_not_load() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let workspace = temp.path().join("workspace");
    // Verified against the CLI: `/review` here is answered with `Unknown command`.
    write_skill(
        &workspace.join(".agents").join("skills"),
        "review",
        &lines(&[
            "---",
            "name: review",
            "description: Review the changes.",
            "---",
        ]),
    );

    assert!(discover(&config_dir, Some(&workspace)).is_empty());
}

#[test]
fn prefers_user_skills_on_name_collisions_even_with_a_stray_agents_copy() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let workspace = temp.path().join("workspace");
    write_skill(
        &config_dir.join("skills"),
        "deploy",
        &lines(&["---", "name: deploy", "description: User deploy.", "---"]),
    );
    write_skill(
        &workspace.join(".agents").join("skills"),
        "deploy",
        &lines(&["---", "name: deploy", "description: Agents deploy.", "---"]),
    );
    write_skill(
        &workspace.join(".claude").join("skills"),
        "deploy",
        &lines(&["---", "name: deploy", "description: Claude deploy.", "---"]),
    );

    let skills = discover(&config_dir, Some(&workspace));

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name.as_str(), "deploy");
    assert_eq!(skills[0].scope.as_ref().map(|s| s.as_str()), Some("user"));
    assert_eq!(
        skills[0].description.as_ref().map(|s| s.as_str()),
        Some("User deploy.")
    );
    assert_eq!(
        skills[0].path.as_str(),
        config_dir
            .join("skills")
            .join("deploy")
            .join("SKILL.md")
            .to_string_lossy()
    );
}

#[test]
fn prefers_user_skills_over_project_skills_on_name_collisions() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let workspace = temp.path().join("workspace");
    write_skill(
        &config_dir.join("skills"),
        "deploy",
        &lines(&["---", "name: deploy", "description: User deploy.", "---"]),
    );
    write_skill(
        &workspace.join(".claude").join("skills"),
        "deploy",
        &lines(&["---", "name: deploy", "description: Project deploy.", "---"]),
    );

    let skills = discover(&config_dir, Some(&workspace));

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].scope.as_ref().map(|s| s.as_str()), Some("user"));
    assert_eq!(
        skills[0].description.as_ref().map(|s| s.as_str()),
        Some("User deploy.")
    );
}

#[test]
fn recovers_colon_bearing_descriptions_with_invocation_metadata_intact() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let cases = [
        (
            "Browser automation + AI test authoring via kane-cli: run browser objectives, ...",
            "",
        ),
        (
            "Read C:\\skills\\guide#tag: continue with \"quoted\".",
            " # trailing: comment",
        ),
    ];
    for (description, comment) in cases {
        write_skill(
            &config_dir.join("skills"),
            "kane-cli",
            &lines(&[
                "---",
                "name: frontmatter-alias",
                &format!("description: {description}{comment}"),
                "allowed-tools: [Read, Write]",
                "disable-model-invocation: yes",
                "user-invocable: no",
                "---",
            ]),
        );

        let skills = discover(&config_dir, None);

        assert_eq!(
            skills,
            vec![ServerProviderSkill {
                name: req("kane-cli"),
                description: opt(description),
                path: req(&config_dir
                    .join("skills")
                    .join("kane-cli")
                    .join("SKILL.md")
                    .to_string_lossy()),
                scope: opt("user"),
                enabled: true,
                display_name: None,
                short_description: None,
                user_invocation_only: Some(true),
                user_invocable: Some(false),
            }]
        );
    }
}

#[test]
fn falls_back_to_the_directory_name_and_skips_malformed_frontmatter() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let skills_dir = config_dir.join("skills");
    write_skill(&skills_dir, "no-frontmatter", "# Just a heading\n");
    for (directory_name, field) in [
        ("broken-yaml", "name: [unclosed"),
        ("broken-tools", "allowed-tools: [Read, Write"),
        ("broken-quoted", "name: \"unclosed: text"),
    ] {
        write_skill(
            &skills_dir,
            directory_name,
            &lines(&["---", "description: Run: browser objectives.", field, "---"]),
        );
    }
    // A stray file (not a directory with SKILL.md) must be skipped.
    fs::write(skills_dir.join("README.md"), "not a skill").expect("stray file");

    let skills = discover(&config_dir, None);

    assert_eq!(names(&skills), vec!["no-frontmatter"]);
    assert_eq!(skills[0].description, None);
}

#[test]
fn honors_claude_config_dir_from_the_environment_when_home_path_is_unset() {
    let temp = tmp();
    let environment_config_dir = temp.path().join("env-config");
    write_skill(
        &environment_config_dir.join("skills"),
        "env-skill",
        &lines(&[
            "---",
            "name: env-skill",
            "description: From env config dir.",
            "---",
        ]),
    );

    let skills = discover_claude_skills_for(
        &settings(""),
        None,
        &env(&[(
            "CLAUDE_CONFIG_DIR",
            environment_config_dir.to_str().expect("utf8"),
        )]),
        "/home/test",
        Platform::Linux,
    );
    assert_eq!(names(&skills), vec!["env-skill"]);

    // An explicit homePath wins over the environment variable, as makeClaudeEnvironment does.
    let explicit_home = temp.path().join("explicit-home");
    write_skill(
        &explicit_home.join("skills"),
        "explicit-skill",
        &lines(&["---", "name: explicit-skill", "---"]),
    );
    let explicit = discover_claude_skills_for(
        &settings(explicit_home.to_str().expect("utf8")),
        None,
        &env(&[(
            "CLAUDE_CONFIG_DIR",
            environment_config_dir.to_str().expect("utf8"),
        )]),
        "/home/test",
        Platform::Linux,
    );
    assert_eq!(names(&explicit), vec!["explicit-skill"]);
}

#[test]
fn resolves_a_relative_claude_config_dir_against_the_workspace_cwd() {
    let temp = tmp();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    // The spawned CLI resolves a relative CLAUDE_CONFIG_DIR against its own cwd (the workspace).
    write_skill(
        &workspace.join("relative-config").join("skills"),
        "relative-skill",
        &lines(&["---", "name: relative-skill", "---"]),
    );

    let skills = discover_claude_skills_for(
        &settings(""),
        Some(&workspace),
        &env(&[("CLAUDE_CONFIG_DIR", "relative-config")]),
        "/home/test",
        Platform::Linux,
    );

    assert_eq!(names(&skills), vec!["relative-skill"]);
    assert_eq!(skills[0].scope.as_ref().map(|s| s.as_str()), Some("user"));
}

#[test]
fn marks_skills_that_only_the_user_can_invoke() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let workspace = temp.path().join("workspace");
    write_skill(
        &workspace.join(".claude").join("skills"),
        "re-release-version",
        &lines(&[
            "---",
            "name: re-release-version",
            "description: Move the current tag forward.",
            "disable-model-invocation: true",
            "---",
            "",
            "# Body",
        ]),
    );
    write_skill(
        &workspace.join(".claude").join("skills"),
        "release-version",
        &lines(&[
            "---",
            "name: release-version",
            "description: Cut a release.",
            "---",
            "",
            "# Body",
        ]),
    );

    let skills = discover(&config_dir, Some(&workspace));

    let find = |name: &str| {
        skills
            .iter()
            .find(|skill| skill.name.as_str() == name)
            .expect("skill present")
    };
    assert_eq!(find("re-release-version").user_invocation_only, Some(true));
    assert_eq!(find("release-version").user_invocation_only, None);
}

#[test]
fn disables_skills_switched_off_by_skill_overrides() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let workspace = temp.path().join("workspace");
    for name in ["kept", "off-by-user", "off-by-project"] {
        write_skill(
            &config_dir.join("skills"),
            name,
            &lines(&["---", &format!("name: {name}"), "---", "", "# Body"]),
        );
    }
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("settings.json"),
        r#"{ "skillOverrides": { "off-by-user": "off", "kept": "on" } }"#,
    )
    .expect("user settings");
    fs::create_dir_all(workspace.join(".claude")).expect("project dir");
    fs::write(
        workspace.join(".claude").join("settings.json"),
        r#"{ "skillOverrides": { "off-by-project": "off" } }"#,
    )
    .expect("project settings");

    let skills = discover(&config_dir, Some(&workspace));

    assert_eq!(
        name_enabled(&skills),
        vec![
            ("kept", true),
            ("off-by-project", false),
            ("off-by-user", false)
        ]
    );
}

#[test]
fn ignores_unreadable_settings_when_resolving_skill_overrides() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    write_skill(
        &config_dir.join("skills"),
        "kept",
        &lines(&["---", "name: kept", "---", "", "# Body"]),
    );
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(config_dir.join("settings.json"), "{ not json").expect("settings");

    let skills = discover(&config_dir, None);

    assert_eq!(name_enabled(&skills), vec![("kept", true)]);
}

#[test]
fn treats_a_user_invocable_only_override_like_disable_model_invocation() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    write_skill(
        &config_dir.join("skills"),
        "ask-matt",
        &lines(&["---", "name: ask-matt", "---", "", "# Body"]),
    );
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("settings.json"),
        r#"{ "skillOverrides": { "ask-matt": "user-invocable-only" } }"#,
    )
    .expect("settings");

    let skills = discover(&config_dir, None);

    assert_eq!(name_enabled(&skills), vec![("ask-matt", true)]);
    assert_eq!(skills[0].user_invocation_only, Some(true));
}

#[test]
fn drops_every_override_in_a_file_when_one_value_is_invalid_as_claude_code_does() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    for name in ["unknown-mode", "boolean-false", "sibling-off"] {
        write_skill(
            &config_dir.join("skills"),
            name,
            &lines(&["---", &format!("name: {name}"), "---", "", "# Body"]),
        );
    }
    // Verified against the CLI: an unknown string or a boolean in the map makes
    // the valid "off" sibling ignored too, so every skill runs.
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("settings.json"),
        r#"{ "skillOverrides": { "unknown-mode": "some-future-mode", "boolean-false": false, "sibling-off": "off" } }"#,
    )
    .expect("settings");

    let skills = discover(&config_dir, None);

    assert_eq!(
        name_enabled(&skills),
        vec![
            ("boolean-false", true),
            ("sibling-off", true),
            ("unknown-mode", true)
        ]
    );
}

#[test]
fn reads_repository_root_settings_from_a_nested_workspace() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let repo = temp.path().join("repo");
    let workspace = repo.join("packages").join("app");
    for name in ["root-off", "root-off-cwd-on", "cwd-off-root-on"] {
        write_skill(
            &config_dir.join("skills"),
            name,
            &lines(&["---", &format!("name: {name}"), "---", "", "# Body"]),
        );
    }
    fs::create_dir_all(repo.join(".git")).expect("git dir");
    fs::create_dir_all(repo.join(".claude")).expect("root claude dir");
    fs::create_dir_all(workspace.join(".claude")).expect("workspace claude dir");
    // The CLI ignores the root's plain settings.json from a nested cwd.
    fs::write(
        repo.join(".claude").join("settings.json"),
        r#"{ "skillOverrides": { "cwd-off-root-on": "off" } }"#,
    )
    .expect("root settings");
    // The root local file outranks the workspace local file, as in the CLI.
    fs::write(
        repo.join(".claude").join("settings.local.json"),
        r#"{ "skillOverrides": { "root-off": "off", "root-off-cwd-on": "off", "cwd-off-root-on": "on" } }"#,
    )
    .expect("root local");
    fs::write(
        workspace.join(".claude").join("settings.local.json"),
        r#"{ "skillOverrides": { "root-off-cwd-on": "on", "cwd-off-root-on": "off" } }"#,
    )
    .expect("workspace local");

    let skills = discover(&config_dir, Some(&workspace));

    assert_eq!(
        name_enabled(&skills),
        vec![
            ("cwd-off-root-on", true),
            ("root-off", false),
            ("root-off-cwd-on", false)
        ]
    );
}

#[test]
fn ignores_ancestor_settings_outside_a_repository() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let parent = temp.path().join("not-a-repo");
    let workspace = parent.join("workspace");
    write_skill(
        &config_dir.join("skills"),
        "kept",
        &lines(&["---", "name: kept", "---", "", "# Body"]),
    );
    fs::create_dir_all(parent.join(".claude")).expect("parent claude dir");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(
        parent.join(".claude").join("settings.local.json"),
        r#"{ "skillOverrides": { "kept": "off" } }"#,
    )
    .expect("ancestor settings");

    let skills = discover(&config_dir, Some(&workspace));

    assert_eq!(name_enabled(&skills), vec![("kept", true)]);
}

#[test]
fn lets_the_administrators_managed_policy_outrank_every_other_settings_file() {
    let expected_managed = [
        (
            Platform::Darwin,
            "/Library/Application Support/ClaudeCode/managed-settings.json",
        ),
        (Platform::Linux, "/etc/claude-code/managed-settings.json"),
    ];
    for (platform, expected) in expected_managed {
        let paths = skill_override_settings_paths(
            "/home/.claude",
            Some("/workspace"),
            None,
            platform,
            &ProcessEnv::new(),
        );
        assert_eq!(
            paths,
            vec![
                "/home/.claude/settings.json",
                "/workspace/.claude/settings.json",
                "/workspace/.claude/settings.local.json",
                expected,
            ]
        );
    }

    let win32_env = env(&[("PROGRAMDATA", "C:\\ProgramData")]);
    let win32 = skill_override_settings_paths(
        "C:\\Users\\me\\.claude",
        None,
        None,
        Platform::Win32,
        &win32_env,
    );
    assert_eq!(
        win32.last().map(String::as_str),
        Some("C:\\ProgramData\\ClaudeCode\\managed-settings.json")
    );
    let win32_without_program_data = skill_override_settings_paths(
        "C:\\Users\\me\\.claude",
        None,
        None,
        Platform::Win32,
        &ProcessEnv::new(),
    );
    assert_eq!(
        win32_without_program_data,
        vec!["C:\\Users\\me\\.claude\\settings.json"]
    );

    // Only the repository root's local file joins in, after the workspace's own local file so it wins.
    assert_eq!(
        skill_override_settings_paths(
            "/home/.claude",
            Some("/repo/packages/app"),
            Some("/repo"),
            Platform::Linux,
            &ProcessEnv::new(),
        ),
        vec![
            "/home/.claude/settings.json",
            "/repo/packages/app/.claude/settings.json",
            "/repo/packages/app/.claude/settings.local.json",
            "/repo/.claude/settings.local.json",
            "/etc/claude-code/managed-settings.json",
        ]
    );
    // A workspace that is the root itself is not read twice.
    assert_eq!(
        skill_override_settings_paths(
            "/home/.claude",
            Some("/repo"),
            Some("/repo"),
            Platform::Linux,
            &ProcessEnv::new()
        ),
        vec![
            "/home/.claude/settings.json",
            "/repo/.claude/settings.json",
            "/repo/.claude/settings.local.json",
            "/etc/claude-code/managed-settings.json",
        ]
    );
}

#[test]
fn records_a_skill_claude_code_keeps_out_of_its_own_slash_commands() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    write_skill(
        &config_dir.join("skills"),
        "agent-only",
        &lines(&[
            "---",
            "name: agent-only",
            "user-invocable: false",
            "---",
            "",
            "# Body",
        ]),
    );

    let skills = discover(&config_dir, None);

    assert_eq!(
        skills
            .iter()
            .map(|s| (s.name.as_str(), s.user_invocable))
            .collect::<Vec<_>>(),
        vec![("agent-only", Some(false))]
    );
}

#[test]
fn identifies_a_skill_by_its_directory_as_claude_code_does() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    write_skill(
        &config_dir.join("skills"),
        "probe-alias",
        &lines(&["---", "name: probe-alias-frontmatter", "---", "", "# Body"]),
    );
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("settings.json"),
        r#"{ "skillOverrides": { "probe-alias-frontmatter": "off" } }"#,
    )
    .expect("settings");

    // The frontmatter name is not the command, so an override naming it is not one Claude Code would apply.
    let skills = discover(&config_dir, None);

    assert_eq!(name_enabled(&skills), vec![("probe-alias", true)]);
}

#[test]
fn switches_a_skill_off_by_its_directory_name() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    write_skill(
        &config_dir.join("skills"),
        "probe-alias",
        &lines(&["---", "name: probe-alias-frontmatter", "---", "", "# Body"]),
    );
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("settings.json"),
        r#"{ "skillOverrides": { "probe-alias": "off" } }"#,
    )
    .expect("settings");

    let skills = discover(&config_dir, None);

    assert_eq!(name_enabled(&skills), vec![("probe-alias", false)]);
}

#[test]
fn accepts_the_yaml_1_1_boolean_spellings_claude_code_allows() {
    let temp = tmp();
    let config_dir = temp.path().join("claude-home");
    let skills_dir = config_dir.join("skills");
    write_skill(
        &skills_dir,
        "user-only-yes",
        &lines(&["---", "disable-model-invocation: yes", "---", "", "# Body"]),
    );
    write_skill(
        &skills_dir,
        "agent-only-no",
        &lines(&["---", "user-invocable: no", "---", "", "# Body"]),
    );
    write_skill(
        &skills_dir,
        "plain-off",
        &lines(&["---", "disable-model-invocation: off", "---", "", "# Body"]),
    );

    let skills = discover(&config_dir, None);

    let flags: Vec<(&str, bool, bool)> = skills
        .iter()
        .map(|skill| {
            (
                skill.name.as_str(),
                skill.user_invocation_only == Some(true),
                skill.user_invocable == Some(false),
            )
        })
        .collect();
    assert_eq!(
        flags,
        vec![
            ("agent-only-no", false, true),
            ("plain-off", false, false),
            ("user-only-yes", true, false)
        ]
    );
}

#[test]
fn returns_an_empty_list_when_no_skill_roots_exist() {
    let temp = tmp();
    let skills = discover(
        &temp.path().join("missing-home"),
        Some(&temp.path().join("missing-workspace")),
    );
    assert!(skills.is_empty());
}

fn req(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("non-empty literal")
}

fn opt(value: &str) -> Option<TrimmedNonEmptyString> {
    Some(req(value))
}
