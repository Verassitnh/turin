//! Port of upstream/apps/server/src/textGeneration/CodexTextGeneration.test.ts.
//!
//! The fake `codex` is a POSIX shell script that walks argv the way the Node stub
//! upstream does: `--image`, `--config key=value`, and `--output-last-message <path>`
//! are consumed, the prompt arrives on stdin, and each failed check exits with its own
//! code so a failing test names the assertion that tripped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::settings::BranchNamingMode;
use turin_contracts::settings::BranchNamingOptions;
use turin_contracts::settings::CodexSettings;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, CommitMessageGenerationInput, PrContentGenerationInput,
    ProviderTextGeneration, ThreadTitleGenerationInput,
};

use super::{
    CodexEffectiveRuntime, CodexManagedRuntimeSource, CodexModel, CodexModelSource,
    CodexTextGenerationOptions, codex_text_generation,
};

const DEFAULT_TEST_MODEL: &str = "gpt-5.4-mini";

fn selection(model: &str, options: Value) -> ModelSelection {
    let mut value = json!({ "instanceId": "codex", "model": model });
    if !options.is_null() {
        value["options"] = options;
    }
    serde_json::from_value(value).expect("model selection")
}

fn default_selection() -> ModelSelection {
    selection(DEFAULT_TEST_MODEL, Value::Null)
}

fn image_attachment(id: &str, name: &str) -> ChatAttachment {
    serde_json::from_value(json!({
        "type": "image",
        "id": id,
        "name": name,
        "mimeType": "image/png",
        "sizeBytes": 5,
    }))
    .expect("image attachment")
}

/// What `codex mcp list --json` answers: a server list, or an exit code.
#[derive(Clone)]
enum McpAnswer {
    Servers(Vec<(&'static str, bool)>),
    ExitCode(i32),
}

#[derive(Clone)]
struct FakeCodexInput {
    output: String,
    exit_code: i32,
    stderr: Option<String>,
    require_image: bool,
    require_service_tier: Option<String>,
    require_reasoning_effort: Option<String>,
    forbid_reasoning_effort: bool,
    require_arg: Option<String>,
    forbid_arg: Option<String>,
    stdin_must_contain: Option<String>,
    stdin_must_not_contain: Option<String>,
    mcp: McpAnswer,
    launch_args: Option<String>,
    environment: Option<BTreeMap<String, String>>,
    models: Vec<&'static str>,
    managed_runtime: bool,
}

impl FakeCodexInput {
    fn new(output: Value) -> Self {
        Self {
            output: output.to_string(),
            exit_code: 0,
            stderr: None,
            require_image: false,
            require_service_tier: None,
            require_reasoning_effort: None,
            forbid_reasoning_effort: false,
            require_arg: None,
            forbid_arg: None,
            stdin_must_contain: None,
            stdin_must_not_contain: None,
            mcp: McpAnswer::Servers(Vec::new()),
            launch_args: None,
            environment: None,
            models: Vec::new(),
            managed_runtime: false,
        }
    }
}

/// `'…'` with embedded single quotes escaped, for splicing a value into the shell script.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fake_codex_script(input: &FakeCodexInput) -> String {
    let mcp_json = match &input.mcp {
        McpAnswer::Servers(servers) => serde_json::to_string(
            &servers
                .iter()
                .map(|(name, enabled)| json!({ "name": name, "enabled": enabled }))
                .collect::<Vec<_>>(),
        )
        .expect("mcp json"),
        McpAnswer::ExitCode(_) => "[]".to_owned(),
    };
    let mcp_exit = match input.mcp {
        McpAnswer::ExitCode(code) => code.to_string(),
        McpAnswer::Servers(_) => String::new(),
    };
    let optional = |value: &Option<String>| {
        value
            .as_deref()
            .map(shell_quote)
            .unwrap_or_else(|| "''".to_owned())
    };
    [
        "#!/bin/sh".to_owned(),
        format!("REQUIRE_IMAGE={}", u8::from(input.require_image)),
        format!("REQUIRE_SERVICE_TIER={}", optional(&input.require_service_tier)),
        format!("REQUIRE_REASONING_EFFORT={}", optional(&input.require_reasoning_effort)),
        format!("FORBID_REASONING_EFFORT={}", u8::from(input.forbid_reasoning_effort)),
        format!("REQUIRE_ARG={}", optional(&input.require_arg)),
        format!("FORBID_ARG={}", optional(&input.forbid_arg)),
        format!("STDIN_MUST_CONTAIN={}", optional(&input.stdin_must_contain)),
        format!("STDIN_MUST_NOT_CONTAIN={}", optional(&input.stdin_must_not_contain)),
        format!("STDERR_TEXT={}", optional(&input.stderr)),
        format!("OUTPUT_TEXT={}", shell_quote(&input.output)),
        format!("EXIT_CODE={}", input.exit_code),
        format!("MCP_JSON={}", shell_quote(&mcp_json)),
        format!("MCP_EXIT={}", shell_quote(&mcp_exit)),
        r#"
originalArgs=" $* "
isMcp=0
isList=0
for arg in "$@"; do
  if [ "$arg" = "mcp" ]; then isMcp=1; fi
  if [ "$arg" = "list" ]; then isList=1; fi
done
if [ "$isMcp" = 1 ] && [ "$isList" = 1 ]; then
  if [ -n "$MCP_EXIT" ]; then exit "$MCP_EXIT"; fi
  printf '%s' "$MCP_JSON"
  exit 0
fi

seenImage=0
seenServiceTier=""
seenReasoningEffort=""
outputPath=""
prev=""
for arg in "$@"; do
  case "$prev" in
    --image) if [ -n "$arg" ]; then seenImage=1; fi ;;
    --config)
      case "$arg" in
        service_tier=*) seenServiceTier="$arg" ;;
        model_reasoning_effort=*) seenReasoningEffort="$arg" ;;
      esac
      ;;
    --output-last-message) outputPath="$arg" ;;
  esac
  prev="$arg"
done

stdinContent=$(cat)

if [ -n "$REQUIRE_ARG" ]; then
  case "$originalArgs" in
    *" $REQUIRE_ARG "*) ;;
    *) echo "missing arg: $REQUIRE_ARG" >&2; exit 8 ;;
  esac
fi
if [ -n "$FORBID_ARG" ]; then
  case "$originalArgs" in
    *" $FORBID_ARG "*) echo "forbidden arg: $FORBID_ARG" >&2; exit 9 ;;
  esac
fi
if [ "$REQUIRE_IMAGE" = 1 ] && [ "$seenImage" != 1 ]; then
  echo "missing --image input" >&2
  exit 2
fi
if [ -n "$REQUIRE_SERVICE_TIER" ] && [ "$seenServiceTier" != "service_tier=\"$REQUIRE_SERVICE_TIER\"" ]; then
  echo "unexpected service tier config: $seenServiceTier" >&2
  exit 5
fi
if [ -n "$REQUIRE_REASONING_EFFORT" ] && [ "$seenReasoningEffort" != "model_reasoning_effort=\"$REQUIRE_REASONING_EFFORT\"" ]; then
  echo "unexpected reasoning effort config: $seenReasoningEffort" >&2
  exit 6
fi
if [ "$FORBID_REASONING_EFFORT" = 1 ] && [ -n "$seenReasoningEffort" ]; then
  echo "reasoning effort config should be omitted: $seenReasoningEffort" >&2
  exit 7
fi
if [ -n "$STDIN_MUST_CONTAIN" ]; then
  case "$stdinContent" in
    *"$STDIN_MUST_CONTAIN"*) ;;
    *) echo "stdin missing expected content" >&2; exit 3 ;;
  esac
fi
if [ -n "$STDIN_MUST_NOT_CONTAIN" ]; then
  case "$stdinContent" in
    *"$STDIN_MUST_NOT_CONTAIN"*) echo "stdin contained forbidden content" >&2; exit 4 ;;
  esac
fi
if [ -n "$STDERR_TEXT" ]; then printf '%s\n' "$STDERR_TEXT" >&2; fi
if [ -n "$outputPath" ]; then printf '%s\n' "$OUTPUT_TEXT" > "$outputPath"; fi
exit "$EXIT_CODE"
"#
        .to_owned(),
    ]
    .join("\n")
}

/// Writes the fake binary into `dir/bin/codex` and returns its path.
fn write_fake_codex(dir: &Path, input: &FakeCodexInput) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let path = bin_dir.join("codex");
    std::fs::write(&path, fake_codex_script(input)).expect("fake codex");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

struct StaticModels(Vec<&'static str>);

#[async_trait]
impl CodexModelSource for StaticModels {
    async fn models(&self) -> Vec<CodexModel> {
        self.0
            .iter()
            .map(|slug| CodexModel {
                slug: (*slug).to_owned(),
                is_custom: false,
            })
            .collect()
    }
}

/// Upstream's `resolveRuntime` test double: the same config, the chosen environment.
struct FakeManagedRuntime {
    settings: CodexSettings,
    environment: BTreeMap<String, String>,
}

#[async_trait]
impl CodexManagedRuntimeSource for FakeManagedRuntime {
    async fn resolve(&self) -> Result<CodexEffectiveRuntime, String> {
        Ok(CodexEffectiveRuntime {
            settings: self.settings.clone(),
            environment: self.environment.clone(),
        })
    }
}

fn process_environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// Upstream's `withFakeCodexEnv`: builds the closure over a fake binary, then runs `check`.
async fn with_fake_codex<F, Fut>(input: FakeCodexInput, check: F)
where
    F: FnOnce(Arc<dyn ProviderTextGeneration>, PathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let dir = tempfile::tempdir().expect("temp dir");
    let codex_path = write_fake_codex(dir.path(), &input);
    let mut settings_json = json!({ "binaryPath": codex_path.to_string_lossy() });
    if let Some(launch_args) = &input.launch_args {
        settings_json["launchArgs"] = json!(launch_args);
    }
    let settings: CodexSettings = serde_json::from_value(settings_json).expect("codex settings");

    let attachments_dir = dir.path().join("attachments");
    let attachments_for_resolver = attachments_dir.clone();
    let options = CodexTextGenerationOptions {
        settings: settings.clone(),
        environment: input.environment.clone().map(|mut environment| {
            let mut merged = process_environment();
            merged.append(&mut environment);
            merged
        }),
        models: Arc::new(StaticModels(input.models.clone())),
        managed_runtime: input.managed_runtime.then(|| {
            Arc::new(FakeManagedRuntime {
                settings,
                environment: input
                    .environment
                    .clone()
                    .map(|mut environment| {
                        let mut merged = process_environment();
                        merged.append(&mut environment);
                        merged
                    })
                    .unwrap_or_else(process_environment),
            }) as Arc<dyn CodexManagedRuntimeSource>
        }),
        resolve_attachment_path: Arc::new(move |attachment| match attachment {
            ChatAttachment::Image(image) => {
                Some(attachments_for_resolver.join(format!("{}.png", image.id.as_str())))
            }
            _ => None,
        }),
        home_dir: dir.path().to_string_lossy().into_owned(),
    };
    let text_generation: Arc<dyn ProviderTextGeneration> = Arc::new(codex_text_generation(options));
    check(text_generation, attachments_dir).await;
}

fn commit_input(
    branch: &str,
    include_branch: bool,
    model: ModelSelection,
) -> CommitMessageGenerationInput {
    CommitMessageGenerationInput {
        cwd: std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned(),
        branch: Some(branch.to_owned()),
        staged_summary: "M README.md".to_owned(),
        staged_patch: "diff --git a/README.md b/README.md".to_owned(),
        include_branch,
        model_selection: model,
    }
}

fn cwd() -> String {
    std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .into_owned()
}

fn title_input(message: &str, model: ModelSelection) -> ThreadTitleGenerationInput {
    ThreadTitleGenerationInput {
        linked_context: None,
        cwd: cwd(),
        message: message.to_owned(),
        previous_title: None,
        attachments: Vec::new(),
        model_selection: model,
    }
}

fn branch_input(message: &str, model: ModelSelection) -> BranchNameGenerationInput {
    BranchNameGenerationInput {
        naming: None,
        cwd: cwd(),
        message: message.to_owned(),
        attachments: Vec::new(),
        model_selection: model,
    }
}

fn sample_commit_input() -> CommitMessageGenerationInput {
    commit_input("feature/codex-effect", false, default_selection())
}

#[tokio::test]
async fn dispatches_the_qualified_live_model_for_each_selected_model() {
    for selected_model in ["gpt-5.6-luna", "openai.gpt-5.6-luna"] {
        let mut input = FakeCodexInput::new(json!({ "title": "Bedrock title" }));
        input.models = vec!["openai.gpt-5.6-luna"];
        input.require_arg = Some("--model openai.gpt-5.6-luna".to_owned());
        input.forbid_arg = Some("--model gpt-5.6-luna".to_owned());
        with_fake_codex(input, |text_generation, _| async move {
            let result = text_generation
                .generate_thread_title(title_input(
                    "Describe this change",
                    selection(selected_model, Value::Null),
                ))
                .await
                .expect("title");
            assert_eq!(result.title, "Bedrock title");
        })
        .await;
    }
}

#[tokio::test]
async fn generates_and_sanitizes_commit_messages_without_branch_by_default() {
    let mut input = FakeCodexInput::new(json!({
        "subject": "  Add important change to the system with too much detail and a trailing period.\nsecondary line",
        "body": "\n- added migration\n- updated tests\n",
    }));
    input.stdin_must_not_contain =
        Some("branch must be a short semantic git branch fragment".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_commit_message(sample_commit_input())
            .await
            .expect("commit");
        assert!(generated.subject.chars().count() <= 72);
        assert!(!generated.subject.ends_with('.'));
        assert_eq!(generated.body, "- added migration\n- updated tests");
        assert_eq!(generated.branch, None);
    })
    .await;
}

#[tokio::test]
async fn forwards_codex_service_tier_and_non_default_reasoning_effort_into_codex_exec_config() {
    let mut input = FakeCodexInput::new(json!({ "subject": "Add important change", "body": "" }));
    input.require_service_tier = Some("priority".to_owned());
    input.require_reasoning_effort = Some("xhigh".to_owned());
    input.stdin_must_not_contain =
        Some("branch must be a short semantic git branch fragment".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let model = selection(
            "gpt-5.4",
            json!([
                { "id": "reasoningEffort", "value": "xhigh" },
                { "id": "serviceTier", "value": "priority" },
            ]),
        );
        text_generation
            .generate_commit_message(commit_input("feature/codex-effect", false, model))
            .await
            .expect("commit");
    })
    .await;
}

#[tokio::test]
async fn omits_a_persisted_service_tier_for_managed_chatgpt_text_generation() {
    let mut input = FakeCodexInput::new(json!({ "subject": "Update project", "body": "" }));
    input.managed_runtime = true;
    input.forbid_arg = Some("service_tier=\"priority\"".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let model = selection(
            "gpt-5.4",
            json!([{ "id": "serviceTier", "value": "priority" }]),
        );
        text_generation
            .generate_commit_message(commit_input("feature/chatgpt", false, model))
            .await
            .expect("commit");
    })
    .await;
}

#[tokio::test]
async fn generates_titles_with_the_users_mcp_servers_and_plugins_off() {
    let mut input = FakeCodexInput::new(json!({ "title": "Remove default shapes" }));
    input.mcp = McpAnswer::Servers(vec![
        ("augmenter", true),
        ("my.server", true),
        ("already-off", false),
    ]);
    input.require_arg = Some(
        "--config features.plugins=false --config features.apps=false --config mcp_servers={\"augmenter\"={enabled=false},\"my.server\"={enabled=false}}".to_owned(),
    );
    with_fake_codex(input, |text_generation, _| async move {
        let result = text_generation
            .generate_thread_title(title_input(
                "Augmenter task 1. Use the Augmenter MCP: link_thread when you start.",
                default_selection(),
            ))
            .await
            .expect("title");
        assert_eq!(result.title, "Remove default shapes");
    })
    .await;
}

#[tokio::test]
async fn still_generates_when_codex_cannot_list_its_mcp_servers() {
    let mut input = FakeCodexInput::new(json!({ "branch": "remove-default-shapes" }));
    input.mcp = McpAnswer::ExitCode(2);
    input.require_arg =
        Some("--config features.plugins=false --config features.apps=false --ephemeral".to_owned());
    input.forbid_arg = Some("mcp_servers=".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let result = text_generation
            .generate_branch_name(branch_input(
                "Remove default shapes from characters",
                default_selection(),
            ))
            .await
            .expect("branch");
        assert_eq!(result.branch, "remove-default-shapes");
    })
    .await;
}

#[tokio::test]
async fn keeps_plugins_and_apps_off_over_launch_args_that_turn_them_on() {
    let mut input = FakeCodexInput::new(json!({ "title": "Remove default shapes" }));
    input.launch_args =
        Some("--config features.plugins=true --config features.apps=true".to_owned());
    input.require_arg = Some(
        "--config features.plugins=true --config features.apps=true --config features.plugins=false --config features.apps=false --ephemeral".to_owned(),
    );
    with_fake_codex(input, |text_generation, _| async move {
        let result = text_generation
            .generate_thread_title(title_input(
                "Remove default shapes from characters",
                default_selection(),
            ))
            .await
            .expect("title");
        assert_eq!(result.title, "Remove default shapes");
    })
    .await;
}

#[tokio::test]
async fn passes_exec_safe_launch_args_into_codex_exec() {
    let mut input = FakeCodexInput::new(json!({ "subject": "Add important change", "body": "" }));
    input.launch_args = Some("--strict-config --listen off".to_owned());
    input.require_arg = Some("--strict-config".to_owned());
    input.forbid_arg = Some("--listen".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        text_generation
            .generate_commit_message(sample_commit_input())
            .await
            .expect("commit");
    })
    .await;
}

#[tokio::test]
async fn uses_t3code_codex_launch_args_for_codex_exec_over_settings() {
    let mut input = FakeCodexInput::new(json!({ "subject": "Add important change", "body": "" }));
    input.launch_args = Some("--enable settings-feature".to_owned());
    let mut environment = BTreeMap::new();
    environment.insert(
        "T3CODE_CODEX_LAUNCH_ARGS".to_owned(),
        " --strict-config --listen off ".to_owned(),
    );
    input.environment = Some(environment);
    input.require_arg = Some("--strict-config".to_owned());
    input.forbid_arg = Some("settings-feature".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        text_generation
            .generate_commit_message(sample_commit_input())
            .await
            .expect("commit");
    })
    .await;
}

#[tokio::test]
async fn defaults_git_text_generation_codex_effort_to_low() {
    let mut input = FakeCodexInput::new(json!({ "subject": "Add important change", "body": "" }));
    input.require_reasoning_effort = Some("low".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        text_generation
            .generate_commit_message(sample_commit_input())
            .await
            .expect("commit");
    })
    .await;
}

#[tokio::test]
async fn generates_commit_message_with_branch_when_include_branch_is_true() {
    let mut input = FakeCodexInput::new(json!({
        "subject": "Add important change",
        "body": "",
        "branch": "fix/important-system-change",
    }));
    input.stdin_must_contain =
        Some("branch must be a short semantic git branch fragment".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_commit_message(commit_input(
                "feature/codex-effect",
                true,
                default_selection(),
            ))
            .await
            .expect("commit");
        assert_eq!(generated.subject, "Add important change");
        assert_eq!(
            generated.branch.as_deref(),
            Some("fix/important-system-change")
        );
    })
    .await;
}

#[tokio::test]
async fn generates_pr_content_and_trims_markdown_body() {
    let input = FakeCodexInput::new(json!({
        "title": "  Improve orchestration flow\nwith ignored suffix",
        "body": "\n## Summary\n- improve flow\n\n## Testing\n- bun test\n\n",
    }));
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_pr_content(PrContentGenerationInput {
                cwd: cwd(),
                base_branch: "main".to_owned(),
                head_branch: "feature/codex-effect".to_owned(),
                commit_summary: "feat: improve orchestration flow".to_owned(),
                diff_summary: "2 files changed".to_owned(),
                diff_patch: "diff --git a/a.ts b/a.ts".to_owned(),
                change_request_template: None,
                model_selection: default_selection(),
            })
            .await
            .expect("pr content");
        assert_eq!(generated.title, "Improve orchestration flow");
        assert!(generated.body.starts_with("## Summary"));
        assert!(!generated.body.ends_with("\n\n"));
    })
    .await;
}

#[tokio::test]
async fn generates_branch_names_and_normalizes_branch_fragments() {
    let mut input = FakeCodexInput::new(json!({ "branch": "  Feat/Session  " }));
    input.stdin_must_not_contain = Some("Image attachments supplied to the model".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_branch_name(branch_input(
                "Please update session handling.",
                default_selection(),
            ))
            .await
            .expect("branch");
        assert_eq!(generated.branch, "feat/session");
    })
    .await;
}

#[tokio::test]
async fn generates_a_branch_using_each_naming_mode() {
    let cases = [
        (
            BranchNamingMode::Static,
            "Add Search",
            "team/add-search",
            "without a prefix or namespace",
        ),
        (
            BranchNamingMode::Semantic,
            "feat/add-search",
            "feat/add-search",
            "semantic prefix",
        ),
        (
            BranchNamingMode::Custom,
            "Julius/ABC-123.v2",
            "Julius/ABC-123.v2",
            "Preserve the issue ID and capitalization.",
        ),
    ];
    for (mode, output, expected, instruction) in cases {
        let mut input = FakeCodexInput::new(json!({ "branch": output }));
        input.stdin_must_contain = Some(instruction.to_owned());
        with_fake_codex(input, |text_generation, _| async move {
            let generated = text_generation
                .generate_branch_name(BranchNameGenerationInput {
                    naming: Some(BranchNamingOptions {
                        mode,
                        prefix: "team/".to_owned(),
                        instructions: "Preserve the issue ID and capitalization.".to_owned(),
                    }),
                    cwd: cwd(),
                    message: "Add search".to_owned(),
                    attachments: Vec::new(),
                    model_selection: default_selection(),
                })
                .await
                .expect("branch");
            assert_eq!(generated.branch, expected);
        })
        .await;
    }
}

/// Upstream runs this with a closed Effect scope to prove temp files are not tied to it.
/// Rust has no ambient scope; each request owns its temp workspace, so this checks the same
/// result without the scope.
#[tokio::test]
async fn generates_branch_names_without_an_ambient_scope() {
    let input = FakeCodexInput::new(json!({ "branch": "feat/background-generation" }));
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_branch_name(branch_input(
                "Please update session handling.",
                default_selection(),
            ))
            .await
            .expect("branch");
        assert_eq!(generated.branch, "feat/background-generation");
    })
    .await;
}

#[tokio::test]
async fn generates_thread_titles_and_trims_them_for_sidebar_use() {
    let input = FakeCodexInput::new(json!({
        "title": "  \"Investigate websocket reconnect regressions after worktree restore\"  \nignored line",
    }));
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_thread_title(title_input(
                "Please investigate websocket reconnect regressions after a worktree restore.",
                default_selection(),
            ))
            .await
            .expect("title");
        assert_eq!(
            generated.title,
            "Investigate websocket reconnect regressions after worktree restore"
        );
    })
    .await;
}

#[tokio::test]
async fn returns_the_refinement_signal_for_an_unresolved_subject() {
    let input =
        FakeCodexInput::new(json!({ "title": "Investigate issue", "needsRefinement": true }));
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_thread_title(title_input("Fix this", default_selection()))
            .await
            .expect("title");
        assert_eq!(generated.title, "Investigate issue");
        assert_eq!(generated.needs_refinement, Some(true));
    })
    .await;
}

#[tokio::test]
async fn falls_back_when_thread_title_normalization_becomes_whitespace_only() {
    let input = FakeCodexInput::new(json!({ "title": "  \"\"\"   \"\"\"  " }));
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_thread_title(title_input("Name this thread.", default_selection()))
            .await
            .expect("title");
        assert_eq!(generated.title, "New thread");
    })
    .await;
}

#[tokio::test]
async fn trims_whitespace_exposed_after_quote_removal_in_thread_titles() {
    let input = FakeCodexInput::new(json!({ "title": "  \"' hello world '\"  " }));
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_thread_title(title_input("Name this thread.", default_selection()))
            .await
            .expect("title");
        assert_eq!(generated.title, "hello world");
    })
    .await;
}

#[tokio::test]
async fn omits_attachment_metadata_section_when_no_attachments_are_provided() {
    let mut input = FakeCodexInput::new(json!({ "branch": "fix/session-timeout" }));
    input.stdin_must_not_contain = Some("Attachment metadata:".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let generated = text_generation
            .generate_branch_name(branch_input("Fix timeout behavior.", default_selection()))
            .await
            .expect("branch");
        assert_eq!(generated.branch, "fix/session-timeout");
    })
    .await;
}

#[tokio::test]
async fn passes_image_attachments_through_as_codex_image_inputs() {
    let mut input = FakeCodexInput::new(json!({ "branch": "fix/ui-regression" }));
    input.require_image = true;
    input.stdin_must_contain = Some("Attachment metadata:".to_owned());
    with_fake_codex(input, |text_generation, attachments_dir| async move {
        let attachment_id = "thread-branch-image-attachment";
        std::fs::create_dir_all(&attachments_dir).expect("attachments dir");
        std::fs::write(
            attachments_dir.join(format!("{attachment_id}.png")),
            b"hello",
        )
        .expect("attachment file");
        let generated = text_generation
            .generate_branch_name(BranchNameGenerationInput {
                naming: None,
                cwd: cwd(),
                message: "Fix layout bug from screenshot.".to_owned(),
                attachments: vec![image_attachment(attachment_id, "bug.png")],
                model_selection: default_selection(),
            })
            .await
            .expect("branch");
        assert_eq!(generated.branch, "fix/ui-regression");
    })
    .await;
}

#[tokio::test]
async fn resolves_persisted_attachment_ids_to_files_for_codex_image_inputs() {
    let mut input = FakeCodexInput::new(json!({ "branch": "fix/ui-regression" }));
    input.require_image = true;
    with_fake_codex(input, |text_generation, attachments_dir| async move {
        let attachment_id = "thread-1-attachment";
        let image_path = attachments_dir.join(format!("{attachment_id}.png"));
        std::fs::create_dir_all(&attachments_dir).expect("attachments dir");
        std::fs::write(&image_path, b"hello").expect("attachment file");
        let generated = text_generation
            .generate_branch_name(BranchNameGenerationInput {
                naming: None,
                cwd: cwd(),
                message: "Fix layout bug from screenshot.".to_owned(),
                attachments: vec![image_attachment(attachment_id, "bug.png")],
                model_selection: default_selection(),
            })
            .await;
        assert!(image_path.is_file());
        assert_eq!(generated.expect("branch").branch, "fix/ui-regression");
    })
    .await;
}

#[tokio::test]
async fn ignores_missing_attachment_ids_for_codex_image_inputs() {
    let mut input = FakeCodexInput::new(json!({ "branch": "fix/ui-regression" }));
    input.require_image = true;
    with_fake_codex(input, |text_generation, attachments_dir| async move {
        let missing_attachment_id = "thread-missing-attachment";
        let _ = std::fs::remove_file(attachments_dir.join(format!("{missing_attachment_id}.png")));
        let result = text_generation
            .generate_branch_name(BranchNameGenerationInput {
                naming: None,
                cwd: cwd(),
                message: "Fix layout bug from screenshot.".to_owned(),
                attachments: vec![image_attachment(missing_attachment_id, "outside.png")],
                model_selection: default_selection(),
            })
            .await;
        let error = result.expect_err("missing image must fail the fake");
        assert!(
            error_detail(&error).contains("missing --image input"),
            "{error:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn fails_with_typed_text_generation_error_when_codex_returns_wrong_branch_payload_shape() {
    let input = FakeCodexInput::new(json!({ "title": "This is not a branch payload" }));
    with_fake_codex(input, |text_generation, _| async move {
        let error = text_generation
            .generate_branch_name(branch_input(
                "Fix websocket reconnect flake",
                default_selection(),
            ))
            .await
            .expect_err("wrong payload must fail");
        assert!(
            error_detail(&error).contains("Codex returned invalid structured output"),
            "{error:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn returns_typed_text_generation_error_when_codex_exits_non_zero() {
    let mut input = FakeCodexInput::new(json!({ "subject": "ignored", "body": "" }));
    input.exit_code = 1;
    input.stderr = Some("codex execution failed".to_owned());
    with_fake_codex(input, |text_generation, _| async move {
        let error = text_generation
            .generate_commit_message(commit_input(
                "feature/codex-error",
                false,
                default_selection(),
            ))
            .await
            .expect_err("non-zero exit must fail");
        assert!(
            error_detail(&error).contains("Codex CLI command failed: codex execution failed"),
            "{error:?}"
        );
    })
    .await;
}

fn error_detail(error: &turin_contracts::git::TextGenerationError) -> String {
    let turin_contracts::git::TextGenerationError::TextGenerationError { detail, .. } = error;
    detail.clone()
}
