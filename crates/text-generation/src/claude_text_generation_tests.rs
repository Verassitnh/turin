//! Port of upstream/apps/server/src/textGeneration/ClaudeTextGeneration.test.ts.
//!
//! The fake `claude` is `testing/claude_text_generation_fixture.sh`. Upstream sets its
//! expectations through `T3_FAKE_CLAUDE_*` process variables; here the test writes them
//! as shell assignments in front of the fixture, so parallel tests share no state.
//!
//! Upstream's transport tests use a synthetic model catalog (`ClaudeModelCatalog.testFixtures.ts`).
//! [`SyntheticClaudeModels`] gives the same resolution for the four synthetic models those
//! tests name, so the argv expectations hold without the real catalog port.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::settings::ClaudeSettings;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, CommitMessageGenerationInput, PrContentGenerationInput,
    ProviderTextGeneration, ThreadTitleGenerationInput,
};

use super::{
    ClaudeModelResolver, ClaudeResolvedModel, ClaudeRunner, ClaudeTextGenerationOptions,
    claude_text_generation,
};
use crate::text_generation_utils::sanitize_thread_title;

const SYNTHETIC_CLAUDE_CAPABLE_MODEL: &str = "claude-synthetic-capable";
const SYNTHETIC_CLAUDE_COLLIDING_ALIAS: &str = "synthetic-collision";
const SYNTHETIC_CLAUDE_STANDARD_MODEL: &str = "claude-synthetic-standard";
const SYNTHETIC_CLAUDE_THINKING_MODEL: &str = "claude-synthetic-thinking";

fn string_option(selection: &ModelSelection, id: &str) -> Option<String> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|option| option.id.as_str() == id)
        .and_then(|option| match &option.value {
            turin_contracts::model::ProviderOptionSelectionValue::Str(value) => {
                Some(value.as_str().to_owned())
            }
            turin_contracts::model::ProviderOptionSelectionValue::Bool(_) => None,
        })
}

fn bool_option(selection: &ModelSelection, id: &str) -> Option<bool> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|option| option.id.as_str() == id)
        .and_then(|option| match option.value {
            turin_contracts::model::ProviderOptionSelectionValue::Bool(value) => Some(value),
            turin_contracts::model::ProviderOptionSelectionValue::Str(_) => None,
        })
}

/// The four synthetic models, resolved the way the upstream catalog resolves them.
///
/// - Thinking model: a `thinking` switch and no effort, so effort is dropped.
/// - Capable model: effort and fast mode; the default expanded context window adds `[expanded]`.
/// - Standard model, and a custom alias that collides with a catalog alias: no switches, and the
///   custom alias stays opaque to the CLI.
struct SyntheticClaudeModels;

impl ClaudeModelResolver for SyntheticClaudeModels {
    fn resolve(&self, selection: &ModelSelection) -> ClaudeResolvedModel {
        let slug = selection.model.as_str();
        match slug {
            SYNTHETIC_CLAUDE_THINKING_MODEL => ClaudeResolvedModel {
                api_model_id: slug.to_owned(),
                cli_effort: None,
                ultracode: false,
                thinking: bool_option(selection, "thinking"),
                fast_mode: false,
            },
            SYNTHETIC_CLAUDE_CAPABLE_MODEL => ClaudeResolvedModel {
                api_model_id: format!("{slug}[expanded]"),
                cli_effort: string_option(selection, "effort"),
                ultracode: false,
                thinking: None,
                fast_mode: bool_option(selection, "fastMode") == Some(true),
            },
            _ => ClaudeResolvedModel {
                api_model_id: slug.to_owned(),
                cli_effort: None,
                ultracode: false,
                thinking: None,
                fast_mode: false,
            },
        }
    }
}

#[derive(Default, Clone)]
struct FakeClaudeInput {
    output: String,
    exit_code: Option<i32>,
    stderr: Option<String>,
    args_must_contain: Option<String>,
    args_must_not_contain: Option<String>,
    stdin_must_contain: Option<String>,
    config_dir_must_be: Option<String>,
    cwd_must_not_be: Option<String>,
    claude_config: Value,
}

/// Writes the expectations as shell assignments, then the upstream fixture logic.
fn fake_claude_script(input: &FakeClaudeInput) -> String {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let optional = |value: &Option<String>| {
        value
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "''".to_owned())
    };
    let header = [
        "#!/bin/sh".to_owned(),
        format!("T3_FAKE_CLAUDE_OUTPUT={}", quote(&input.output)),
        format!("T3_FAKE_CLAUDE_EXIT_CODE={}", input.exit_code.unwrap_or(0)),
        format!("T3_FAKE_CLAUDE_STDERR={}", optional(&input.stderr)),
        format!(
            "T3_FAKE_CLAUDE_ARGS_MUST_CONTAIN={}",
            optional(&input.args_must_contain)
        ),
        format!(
            "T3_FAKE_CLAUDE_ARGS_MUST_NOT_CONTAIN={}",
            optional(&input.args_must_not_contain)
        ),
        format!(
            "T3_FAKE_CLAUDE_STDIN_MUST_CONTAIN={}",
            optional(&input.stdin_must_contain)
        ),
        format!(
            "T3_FAKE_CLAUDE_CONFIG_DIR_MUST_BE={}",
            optional(&input.config_dir_must_be)
        ),
        format!(
            "T3_FAKE_CLAUDE_CWD_MUST_NOT_BE={}",
            optional(&input.cwd_must_not_be)
        ),
    ]
    .join("\n");
    format!(
        "{header}\n{}",
        include_str!("../testing/claude_text_generation_fixture.sh")
    )
}

fn write_fake_claude(dir: &Path, input: &FakeClaudeInput) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("bin").join("claude");
    std::fs::create_dir_all(path.parent().expect("bin dir")).expect("bin dir");
    std::fs::write(&path, fake_claude_script(input)).expect("fake claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// Upstream `withFakeClaudeEnv`: a `claude` binary that checks its own argv, then the test body.
async fn with_fake_claude<F, Fut>(input: FakeClaudeInput, check: F)
where
    F: FnOnce(Arc<dyn ProviderTextGeneration>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let dir = tempfile::tempdir().expect("temp dir");
    let binary = write_fake_claude(dir.path(), &input);
    let mut settings_json = input.claude_config.clone();
    if !settings_json.is_object() {
        settings_json = json!({});
    }
    settings_json["binaryPath"] = json!(binary.to_string_lossy());
    let settings: ClaudeSettings = serde_json::from_value(settings_json).expect("claude settings");
    let text_generation: Arc<dyn ProviderTextGeneration> =
        Arc::new(claude_text_generation(ClaudeTextGenerationOptions {
            settings,
            environment: None,
            models: Arc::new(SyntheticClaudeModels),
            has_managed_mcp_config: Arc::new(|| false),
            home_dir: dir.path().to_string_lossy().into_owned(),
        }));
    check(text_generation).await;
}

fn cwd() -> String {
    std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .into_owned()
}

fn selection(instance: &str, model: &str, options: Value) -> ModelSelection {
    let mut value = json!({ "instanceId": instance, "model": model });
    if !options.is_null() {
        value["options"] = options;
    }
    serde_json::from_value(value).expect("model selection")
}

fn standard_selection() -> ModelSelection {
    selection("claudeAgent", SYNTHETIC_CLAUDE_STANDARD_MODEL, Value::Null)
}

fn title_input(message: &str, previous_title: Option<&str>) -> ThreadTitleGenerationInput {
    ThreadTitleGenerationInput {
        linked_context: None,
        cwd: cwd(),
        message: message.to_owned(),
        previous_title: previous_title.map(str::to_owned),
        attachments: Vec::new(),
        model_selection: standard_selection(),
    }
}

fn structured(value: Value) -> String {
    json!({ "structured_output": value }).to_string()
}

#[tokio::test]
async fn forwards_claude_thinking_settings_without_passing_unsupported_effort() {
    let input = FakeClaudeInput {
        output: structured(json!({ "subject": "Add important change", "body": "" })),
        args_must_contain: Some(
            "--settings {\"disableAllHooks\":true,\"alwaysThinkingEnabled\":false}".to_owned(),
        ),
        args_must_not_contain: Some("--effort".to_owned()),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_commit_message(CommitMessageGenerationInput {
                cwd: cwd(),
                branch: Some("feature/claude-effect".to_owned()),
                staged_summary: "M README.md".to_owned(),
                staged_patch: "diff --git a/README.md b/README.md".to_owned(),
                include_branch: false,
                model_selection: selection(
                    "claudeAgent",
                    SYNTHETIC_CLAUDE_THINKING_MODEL,
                    json!([
                        { "id": "thinking", "value": false },
                        { "id": "effort", "value": "high" },
                    ]),
                ),
            })
            .await
            .expect("commit");
        assert_eq!(generated.subject, "Add important change");
    })
    .await;
}

#[tokio::test]
async fn keeps_a_configured_custom_alias_opaque_to_the_claude_cli() {
    let input = FakeClaudeInput {
        output: structured(json!({ "title": "Keep custom model", "body": "" })),
        args_must_contain: Some(format!(
            "--model {SYNTHETIC_CLAUDE_COLLIDING_ALIAS} --settings"
        )),
        claude_config: json!({
            "customModels": [SYNTHETIC_CLAUDE_COLLIDING_ALIAS],
        }),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_pr_content(PrContentGenerationInput {
                cwd: cwd(),
                base_branch: "main".to_owned(),
                head_branch: "feature/custom-model".to_owned(),
                commit_summary: "Keep custom model".to_owned(),
                diff_summary: "1 file changed".to_owned(),
                diff_patch: "diff --git a/README.md b/README.md".to_owned(),
                change_request_template: None,
                model_selection: selection(
                    "claudeAgent",
                    SYNTHETIC_CLAUDE_COLLIDING_ALIAS,
                    json!([
                        { "id": "effort", "value": "max" },
                        { "id": "fastMode", "value": true },
                        { "id": "contextWindow", "value": "expanded" },
                    ]),
                ),
            })
            .await
            .expect("pr content");
        // The PR result's title is the structured output's title; upstream checks the same.
        assert_eq!(generated.title, "Keep custom model");
    })
    .await;
}

#[tokio::test]
async fn keeps_canonical_built_in_capabilities_when_a_custom_model_collides_with_its_alias() {
    let input = FakeClaudeInput {
        output: structured(json!({ "title": "Improve orchestration flow", "body": "Body" })),
        args_must_contain: Some(format!(
            "--model {SYNTHETIC_CLAUDE_CAPABLE_MODEL}[expanded] --effort max --settings {{\"disableAllHooks\":true,\"fastMode\":true}}"
        )),
        claude_config: json!({
            "customModels": [SYNTHETIC_CLAUDE_COLLIDING_ALIAS],
        }),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_pr_content(PrContentGenerationInput {
                cwd: cwd(),
                base_branch: "main".to_owned(),
                head_branch: "feature/claude-effect".to_owned(),
                commit_summary: "Improve orchestration".to_owned(),
                diff_summary: "1 file changed".to_owned(),
                diff_patch: "diff --git a/README.md b/README.md".to_owned(),
                change_request_template: None,
                model_selection: selection(
                    "claudeAgent",
                    SYNTHETIC_CLAUDE_CAPABLE_MODEL,
                    json!([
                        { "id": "effort", "value": "max" },
                        { "id": "fastMode", "value": true },
                    ]),
                ),
            })
            .await
            .expect("pr content");
        assert_eq!(generated.title, "Improve orchestration flow");
    })
    .await;
}

#[tokio::test]
async fn generates_thread_titles_outside_the_project_with_tools_skills_and_hooks_disabled() {
    let input = FakeClaudeInput {
        output: structured(json!({
            "title": "  \"Reconnect failures after restart because the session state does not recover\"  ",
        })),
        cwd_must_not_be: Some(cwd()),
        stdin_must_contain: Some("/call-script".to_owned()),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_thread_title(title_input("/call-script", None))
            .await
            .expect("title");
        assert_eq!(
            generated.title,
            sanitize_thread_title(
                "\"Reconnect failures after restart because the session state does not recover\""
            )
        );
    })
    .await;
}

#[tokio::test]
async fn generates_branch_names_from_skill_prompts_without_executable_capabilities() {
    let input = FakeClaudeInput {
        output: structured(json!({ "branch": "call-script" })),
        stdin_must_contain: Some("/call-script".to_owned()),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_branch_name(BranchNameGenerationInput {
                naming: None,
                cwd: cwd(),
                message: "/call-script".to_owned(),
                attachments: Vec::new(),
                model_selection: standard_selection(),
            })
            .await
            .expect("branch");
        assert_eq!(generated.branch, "call-script");
    })
    .await;
}

#[tokio::test]
async fn runs_claude_text_generation_with_the_configured_claude_config_dir() {
    let claude_config_dir = std::env::current_dir()
        .expect("cwd")
        .join(".claude-work-test")
        .to_string_lossy()
        .into_owned();
    let input = FakeClaudeInput {
        output: structured(json!({ "title": "Use Claude home" })),
        config_dir_must_be: Some(claude_config_dir.clone()),
        claude_config: json!({ "homePath": claude_config_dir }),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_thread_title(title_input("thread title", None))
            .await
            .expect("title");
        assert_eq!(generated.title, sanitize_thread_title("Use Claude home"));
    })
    .await;
}

#[tokio::test]
async fn unwraps_a_json_title_in_normal_and_verbose_claude_output() {
    for verbose in [false, true] {
        let result = json!({
            "type": "result",
            "structured_output": { "title": "{\"title\": \"Refresh ev-stg APP ASG instances\"}" },
        });
        let output = if verbose {
            json!([result]).to_string()
        } else {
            result.to_string()
        };
        let input = FakeClaudeInput {
            output,
            ..Default::default()
        };
        with_fake_claude(input, |text_generation| async move {
            let generated = text_generation
                .generate_thread_title(title_input("Refresh ev-stg APP ASG instances", None))
                .await
                .expect("title");
            assert_eq!(generated.title, "Refresh ev-stg APP ASG instances");
        })
        .await;
    }
}

#[tokio::test]
async fn reads_the_result_from_verbose_claude_output_when_generating_or_regenerating_a_title() {
    for previous_title in [None, Some("Old thread title")] {
        let output = json!([
            { "type": "system", "subtype": "init" },
            { "type": "assistant", "message": { "content": [] } },
            { "type": "user", "message": { "content": [] } },
            { "type": "rate_limit_event" },
            {
                "type": "result",
                "subtype": "success",
                "result": "{\"title\":\"Refresh ev-stg APP ASG Instances\"}",
                "structured_output": { "title": "Refresh ev-stg APP ASG Instances" },
            },
        ])
        .to_string();
        let input = FakeClaudeInput {
            output,
            ..Default::default()
        };
        with_fake_claude(input, |text_generation| async move {
            let generated = text_generation
                .generate_thread_title(title_input(
                    "Refresh ev-stg APP ASG instances",
                    previous_title,
                ))
                .await
                .expect("title");
            assert_eq!(generated.title, "Refresh ev-stg APP ASG Instances");
        })
        .await;
    }
}

#[tokio::test]
async fn rejects_verbose_claude_output_with_each_malformed_shape() {
    let cases = [
        ("empty message array", json!([])),
        (
            "missing result",
            json!([{ "type": "assistant", "structured_output": { "title": "Not a result" } }]),
        ),
        (
            "invalid title",
            json!([{ "type": "result", "structured_output": { "title": 42 } }]),
        ),
        (
            "final result without structured output",
            json!([
                { "type": "result", "structured_output": { "title": "Earlier result" } },
                { "type": "result", "subtype": "error_max_structured_output_retries" },
            ]),
        ),
    ];
    for (label, output) in cases {
        let input = FakeClaudeInput {
            output: output.to_string(),
            ..Default::default()
        };
        with_fake_claude(input, |text_generation| async move {
            let error = text_generation
                .generate_thread_title(title_input("Name this thread", None))
                .await
                .expect_err(label);
            assert!(matches!(
                error,
                TextGenerationError::TextGenerationError { .. }
            ));
        })
        .await;
    }
}

#[tokio::test]
async fn reports_the_claude_cli_error_over_stderr_when_the_cli_fails() {
    let api_error = "API Error: 400 Model blocked by guardrail: 4 endpoints excluded";
    let wrapper_notice = "Using the OpenRouter credential from the global credential file.";
    let cases = [
        (
            "the stdout error over a stderr notice",
            json!({ "is_error": true, "api_error_status": 400, "result": api_error }).to_string(),
            api_error.to_owned(),
        ),
        (
            "the verbose stdout error over a stderr notice",
            json!([
                { "type": "system", "subtype": "init" },
                { "type": "result", "is_error": true, "result": api_error },
                { "type": "result", "is_error": true, "result": " " },
            ])
            .to_string(),
            api_error.to_owned(),
        ),
        (
            "stderr when stdout has no error",
            "not json".to_owned(),
            wrapper_notice.to_owned(),
        ),
    ];
    for (label, output, expected) in cases {
        let input = FakeClaudeInput {
            output,
            exit_code: Some(1),
            stderr: Some(wrapper_notice.to_owned()),
            ..Default::default()
        };
        with_fake_claude(input, |text_generation| async move {
            let error = text_generation
                .generate_thread_title(title_input("Name this thread", None))
                .await
                .expect_err(label);
            let TextGenerationError::TextGenerationError { detail, .. } = error;
            assert_eq!(
                detail,
                format!("Claude CLI command failed: {expected}"),
                "{label}"
            );
        })
        .await;
    }
}

#[tokio::test]
async fn falls_back_when_claude_thread_title_normalization_becomes_whitespace_only() {
    let input = FakeClaudeInput {
        output: structured(json!({ "title": "  \"\"\"   \"\"\"  " })),
        ..Default::default()
    };
    with_fake_claude(input, |text_generation| async move {
        let generated = text_generation
            .generate_thread_title(title_input("Name this thread.", None))
            .await
            .expect("title");
        assert_eq!(generated.title, "New thread");
    })
    .await;
}

#[test]
fn claude_config_dir_resolves_a_relative_home_path_against_the_server_cwd() {
    let runner = ClaudeRunner {
        options: ClaudeTextGenerationOptions {
            settings: serde_json::from_value(json!({ "homePath": "claude-work/../claude-home" }))
                .expect("claude settings"),
            environment: Some(BTreeMap::new()),
            models: Arc::new(SyntheticClaudeModels),
            has_managed_mcp_config: Arc::new(|| false),
            home_dir: "/home/someone".to_owned(),
        },
    };
    let expected = std::env::current_dir()
        .expect("cwd")
        .join("claude-home")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        runner.environment().get("CLAUDE_CONFIG_DIR"),
        Some(&expected)
    );
}

#[test]
fn claude_config_dir_expands_home_then_keeps_an_absolute_path() {
    let runner = ClaudeRunner {
        options: ClaudeTextGenerationOptions {
            settings: serde_json::from_value(json!({ "homePath": "~/.claude-work" }))
                .expect("claude settings"),
            environment: Some(BTreeMap::new()),
            models: Arc::new(SyntheticClaudeModels),
            has_managed_mcp_config: Arc::new(|| false),
            home_dir: "/home/someone".to_owned(),
        },
    };
    assert_eq!(
        runner.environment().get("CLAUDE_CONFIG_DIR"),
        Some(&"/home/someone/.claude-work".to_owned())
    );
}
