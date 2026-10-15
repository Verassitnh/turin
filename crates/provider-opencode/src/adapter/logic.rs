//! Pure OpenCode adapter mappings shared by both generations.
//!
//! Port of the pure functions in
//! `upstream/packages/provider-opencode/src/server/adapter.ts`,
//! `server/toolItems.ts`, `server/status.ts`, and `server/v2/adapter.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use turin_contracts::{
    NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId, TrimmedNonEmptyString, TurnItemId,
    UtcDateTime,
    model::{
        ModelCapabilities, ProviderOptionChoice, ProviderOptionDescriptor, SelectOptionType,
        SelectProviderOptionDescriptor,
    },
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ApprovalCapabilities, OrchestrationV2CheckpointCapabilities,
        OrchestrationV2ContextCapabilities, OrchestrationV2FileSearchResult,
        OrchestrationV2IdentityCapabilities, OrchestrationV2NativeRefStrength,
        OrchestrationV2PlanningCapabilities, OrchestrationV2ProviderCapabilities,
        OrchestrationV2ProviderFailure, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderFailureMessage, OrchestrationV2ProviderRef,
        OrchestrationV2ProviderTurn, OrchestrationV2RuntimePolicyCapabilities,
        OrchestrationV2RuntimePolicyEnforcement, OrchestrationV2SessionCapabilities,
        OrchestrationV2StreamingCapabilities, OrchestrationV2SubagentCapabilities,
        OrchestrationV2ThreadCapabilities, OrchestrationV2ToolCapabilities,
        OrchestrationV2TurnCapabilities, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
        OrchestrationV2UserInputQuestion, OrchestrationV2UserInputQuestionOption,
        OrchestrationV2WebSearchResult,
    },
    provider_instance::ProviderDriverKind,
    provider_policy::{ProviderApprovalDecision, ProviderInteractionMode, RuntimeMode},
    provider_runtime::{
        MainAgentUsageScope, TurnTokenUsage, TurnTokenUsageCompleteStatus,
        TurnTokenUsagePartialStatus,
    },
};
use turin_provider_core::server_provider::{
    ServerProviderAuth, ServerProviderAuthStatus, ServerProviderModel, ServerProviderSkill,
    ServerProviderSlashCommand, ServerProviderSlashCommandInput, ServerProviderState,
};

use crate::permission_rules::SessionPermissionRule;

pub const DECLINED: &str = "The user declined this request.";
pub const INSTRUCTIONS_KEY: &str = "t3-code";
pub const OPENCODE_2_STILL_STOPPING: &str =
    "OpenCode is still stopping the previous turn. Send the message again in a moment.";
pub const RECONNECT_ATTEMPTS: u32 = 5;
pub const SEARCH_PREVIEW_MAX_CHARS: usize = 8_000;

static COMMAND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^/([^\s/]+)(?:\s+(.*))?$").expect("command pattern"));

pub fn opencode_driver() -> ProviderDriverKind {
    ProviderDriverKind::new("opencode").expect("opencode slug")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeAdmissionSignal {
    Accepted,
    AssistantCompleted,
    Busy,
    Idle,
    UserMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeAdmissionAction {
    Hold,
    ReconcileIdle,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdmissionFlags {
    pub admission_accepted: bool,
    pub admission_message_observed: bool,
    pub admission_pending: bool,
    pub idle_during_admission: bool,
}

/// Port of `advanceOpenCodePromptAdmission`.
pub fn advance_open_code_prompt_admission(
    admission: &mut AdmissionFlags,
    signal: OpenCodeAdmissionSignal,
) -> OpenCodeAdmissionAction {
    if !admission.admission_pending {
        return OpenCodeAdmissionAction::Release;
    }
    if signal == OpenCodeAdmissionSignal::AssistantCompleted {
        admission.admission_accepted = true;
        admission.admission_message_observed = true;
        admission.admission_pending = false;
        return OpenCodeAdmissionAction::Release;
    }
    if signal == OpenCodeAdmissionSignal::Idle {
        admission.idle_during_admission = true;
        return OpenCodeAdmissionAction::Hold;
    }
    if signal == OpenCodeAdmissionSignal::Accepted {
        admission.admission_accepted = true;
    }
    if signal == OpenCodeAdmissionSignal::Busy || signal == OpenCodeAdmissionSignal::UserMessage {
        admission.admission_message_observed = true;
    }
    if !admission.admission_accepted || !admission.admission_message_observed {
        return OpenCodeAdmissionAction::Hold;
    }
    if admission.idle_during_admission {
        return OpenCodeAdmissionAction::ReconcileIdle;
    }
    admission.admission_pending = false;
    OpenCodeAdmissionAction::Release
}

pub fn cancel_open_code_prompt_admission(admission: &mut AdmissionFlags) {
    admission.admission_pending = false;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V1PermissionRule {
    pub permission: String,
    pub pattern: String,
    pub action: String,
}

fn v1_rule(permission: &str, pattern: &str, action: &str) -> V1PermissionRule {
    V1PermissionRule {
        permission: permission.to_owned(),
        pattern: pattern.to_owned(),
        action: action.to_owned(),
    }
}

const ALWAYS_ALLOWED: [&str; 8] = [
    "question",
    "read",
    "glob",
    "grep",
    "lsp",
    "todowrite",
    "task",
    "skill",
];

const RESTRICTED: [&str; 7] = [
    "bash",
    "edit",
    "webfetch",
    "websearch",
    "codesearch",
    "external_directory",
    "doom_loop",
];

fn record_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

/// Port of `openCodePermissionRules` in `server/adapter.ts`.
pub fn open_code_permission_rules(
    runtime_mode: RuntimeMode,
    approval_policy: Option<&Value>,
    sandbox_policy: Option<&Value>,
) -> Vec<V1PermissionRule> {
    let sandbox_type = sandbox_policy.and_then(|policy| record_string(policy, "type"));
    let requires_approval = match approval_policy {
        None => {
            sandbox_policy.is_some_and(|policy| !policy.is_null())
                || runtime_mode != RuntimeMode::FullAccess
        }
        Some(Value::String(policy)) => !policy.trim().is_empty() && policy != "never",
        Some(_) => true,
    };
    let externally_sandboxed = sandbox_type == Some("externalSandbox");
    let danger_full_access = sandbox_type == Some("dangerFullAccess");
    let implicit_full_access = sandbox_type.is_none() && runtime_mode == RuntimeMode::FullAccess;
    if !requires_approval && (externally_sandboxed || danger_full_access || implicit_full_access) {
        return vec![v1_rule("*", "*", "allow")];
    }
    let mut rules = vec![v1_rule("*", "*", "deny")];
    for permission in RESTRICTED {
        rules.push(v1_rule(permission, "*", "deny"));
    }
    if requires_approval {
        rules.push(v1_rule("*", "*", "ask"));
        for permission in RESTRICTED {
            rules.push(v1_rule(permission, "*", "ask"));
        }
    }
    for permission in ALWAYS_ALLOWED {
        rules.push(v1_rule(permission, "*", "allow"));
    }
    let env_action = if requires_approval { "ask" } else { "deny" };
    rules.push(v1_rule("read", "*.env", env_action));
    rules.push(v1_rule("read", "*.env.*", env_action));
    rules.push(v1_rule("read", "*.env.example", "allow"));
    if runtime_mode == RuntimeMode::AutoAcceptEdits
        || (!requires_approval && sandbox_type == Some("workspaceWrite"))
    {
        rules.push(v1_rule("edit", "*", "allow"));
    }
    if !requires_approval
        && sandbox_policy
            .and_then(|p| p.get("networkAccess"))
            .and_then(Value::as_bool)
            == Some(true)
    {
        for permission in ["webfetch", "websearch", "codesearch"] {
            rules.push(v1_rule(permission, "*", "allow"));
        }
    }
    if !requires_approval && sandbox_type == Some("readOnly") {
        let access = sandbox_policy.and_then(|p| p.get("access"));
        if access.and_then(|a| record_string(a, "type")) == Some("fullAccess") {
            rules.push(v1_rule("external_directory", "*", "allow"));
        }
    }
    if !requires_approval
        && sandbox_type == Some("workspaceWrite")
        && let Some(roots) = sandbox_policy
            .and_then(|p| p.get("writableRoots"))
            .and_then(Value::as_array)
    {
        for root in roots {
            if let Some(root) = root.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                let pattern = format!("{}/*", root.trim_end_matches('/'));
                rules.push(v1_rule("external_directory", &pattern, "allow"));
            }
        }
    }
    rules
}

pub fn permission_rule_equals(left: &V1PermissionRule, right: &V1PermissionRule) -> bool {
    left.permission == right.permission
        && left.pattern == right.pattern
        && left.action == right.action
}

/// Port of `openCodeChildPermissionRules`.
pub fn open_code_child_permission_rules(
    parent: &[V1PermissionRule],
    native_child: &[V1PermissionRule],
) -> Vec<V1PermissionRule> {
    let inherited: Vec<_> = parent
        .iter()
        .filter(|rule| rule.permission == "external_directory" || rule.action == "deny")
        .cloned()
        .collect();
    let mut rules = parent.to_vec();
    for child in native_child {
        if !inherited
            .iter()
            .any(|inherited| permission_rule_equals(child, inherited))
        {
            rules.push(child.clone());
        }
    }
    rules
}

pub fn open_code_permission_request_kind(
    permission: &str,
    tool_name: Option<&str>,
) -> &'static str {
    let normalized = permission.to_ascii_lowercase();
    let tool = tool_name.unwrap_or("").to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "edit" | "write" | "patch" | "apply_patch"
    ) || matches!(tool.as_str(), "edit" | "write" | "patch" | "apply_patch")
    {
        return "file-change";
    }
    if matches!(
        normalized.as_str(),
        "read" | "glob" | "grep" | "lsp" | "external_directory"
    ) || tool == "read"
        || tool.contains("glob")
        || tool.contains("grep")
        || tool.contains("search")
    {
        return "file-read";
    }
    "command"
}

/// v1 maps accept-for-session onto OpenCode's `always`. v2 must not: `always`
/// persists a project-wide grant, so the v2 adapter writes a session rule and
/// replies `once`.
pub fn v1_permission_reply(decision: ProviderApprovalDecision) -> &'static str {
    match decision {
        ProviderApprovalDecision::Accept => "once",
        ProviderApprovalDecision::AcceptForSession | ProviderApprovalDecision::AcceptAlways => {
            "always"
        }
        ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel => "reject",
    }
}

pub fn v2_permission_decision(decision: ProviderApprovalDecision) -> &'static str {
    match decision {
        ProviderApprovalDecision::Accept
        | ProviderApprovalDecision::AcceptForSession
        | ProviderApprovalDecision::AcceptAlways => "once",
        ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel => "reject",
    }
}

pub fn approves(decision: ProviderApprovalDecision) -> bool {
    matches!(
        decision,
        ProviderApprovalDecision::Accept
            | ProviderApprovalDecision::AcceptForSession
            | ProviderApprovalDecision::AcceptAlways
    )
}

pub fn declined_note(action: &str, resources: &[String]) -> String {
    if resources.is_empty() {
        format!(
            "The user declined the {action} request. Do not retry it; continue without it or ask the user how to proceed."
        )
    } else {
        format!(
            "The user declined the {action} request ({}). Do not retry it; continue without it or ask the user how to proceed.",
            resources.join(", ")
        )
    }
}

pub fn session_grant_label(action: &str, save: &[String]) -> String {
    if save.iter().all(|pattern| pattern == "*") {
        format!("Allow every {action} request this session")
    } else {
        format!("Allow {} this session", save.join(", "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelSlug {
    pub provider_id: String,
    pub model_id: String,
}

pub fn parse_open_code_model_slug(slug: &str) -> Option<ParsedModelSlug> {
    let trimmed = slug.trim();
    let separator = trimmed.find('/')?;
    if separator == 0 || separator + 1 == trimmed.len() {
        return None;
    }
    Some(ParsedModelSlug {
        provider_id: trimmed[..separator].to_owned(),
        model_id: trimmed[separator + 1..].to_owned(),
    })
}

pub fn malformed_model(model: &str) -> String {
    format!("OpenCode model '{model}' must use provider/model format")
}

pub fn model_option(selection: &ModelSelection, id: &str) -> Option<String> {
    selection.options.as_ref()?.iter().find_map(|option| {
        if option.id.as_str() != id {
            return None;
        }
        match &option.value {
            turin_contracts::model::ProviderOptionSelectionValue::Str(value) => {
                Some(value.as_str().to_owned())
            }
            turin_contracts::model::ProviderOptionSelectionValue::Bool(_) => None,
        }
    })
}

pub fn agent_for(interaction: ProviderInteractionMode, selection: &ModelSelection) -> String {
    model_option(selection, "agent").unwrap_or_else(|| {
        if interaction == ProviderInteractionMode::Plan {
            "plan".to_owned()
        } else {
            "build".to_owned()
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub text: String,
}

pub fn command_of(text: &str) -> Option<SlashCommand> {
    let captures = COMMAND_RE.captures(text.trim())?;
    Some(SlashCommand {
        name: captures.get(1)?.as_str().to_owned(),
        text: captures
            .get(2)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default(),
    })
}

/// `$skill` tokens. The Rust `regex` crate has no lookaround, so this walks
/// the same rules as `SKILL_MENTION_PATTERN`: a currency sigil after a
/// boundary, a name that contains a letter, and not a bare amount (`$20`,
/// `$20k`, `$1e6`).
pub fn skills_named(text: &str, known: &HashSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || chars[i - 1].is_whitespace();
        if boundary && is_currency(chars[i]) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_skill_char(chars[end]) {
                end += 1;
            }
            let name: String = chars[start..end].iter().collect();
            let followed = end == chars.len() || chars[end].is_whitespace();
            if followed && skill_name_ok(&name) && known.contains(&name) && !found.contains(&name) {
                found.push(name);
            }
            i = end.max(i + 1);
            continue;
        }
        i += 1;
    }
    found
}

fn is_currency(c: char) -> bool {
    c == '$' || c == '€' || c == '£' || c == '¥' || unicode_general_category_currency(c)
}

fn unicode_general_category_currency(c: char) -> bool {
    matches!(c, '\u{00A2}'..='\u{00A5}' | '\u{20A0}'..='\u{20CF}')
}

fn is_skill_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-'
}

fn skill_name_ok(name: &str) -> bool {
    if name.is_empty() || !name.contains(|c: char| c.is_ascii_alphabetic()) {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes[0].is_ascii_digit() {
        let rest = name.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_');
        if rest.is_empty() {
            return false;
        }
        let suffix = rest.to_ascii_lowercase();
        if matches!(suffix.as_str(), "k" | "m" | "b" | "t") {
            return false;
        }
        if suffix.starts_with('e')
            && suffix[1..].chars().all(|c| c.is_ascii_digit())
            && suffix.len() > 1
        {
            return false;
        }
    }
    true
}

pub fn turn_prompt_id(session_id: &str, attempt_id: &str) -> String {
    format!("msg_t3_turn_{session_id}:{attempt_id}")
}

pub fn steer_prompt_id(session_id: &str, message_id: &str) -> String {
    format!("msg_t3_steer_{session_id}:{message_id}")
}

pub fn prompt_of(turn: &OrchestrationV2ProviderTurn) -> Option<&str> {
    turn.native_turn_ref
        .as_ref()
        .and_then(|native| native.native_id.as_ref())
        .map(|id| id.as_str())
        .filter(|id| id.starts_with("msg_"))
}

pub fn is_wake_turn(turn: &OrchestrationV2ProviderTurn) -> bool {
    turn.native_turn_ref
        .as_ref()
        .and_then(|native| native.native_id.as_ref())
        .is_some_and(|id| id.as_str().contains(":wake:"))
}

/// v1 exclusive boundary: the next native user message after the selected turn.
pub fn open_code_boundary_after_provider_turn(
    turns: &[OrchestrationV2ProviderTurn],
    selected_id: &ProviderTurnId,
) -> Option<String> {
    let selected = turns.iter().find(|turn| &turn.id == selected_id)?;
    turns
        .iter()
        .filter(|turn| turn.ordinal > selected.ordinal)
        .min_by_key(|turn| turn.ordinal)
        .and_then(|turn| {
            turn.native_turn_ref
                .as_ref()
                .and_then(|native| native.native_id.as_ref())
                .map(|id| id.as_str().to_owned())
        })
}

/// v2 boundary. `Err` when a later turn has no known prompt id.
pub fn boundary_after(
    turns: &[OrchestrationV2ProviderTurn],
    provider_thread_id: &ProviderThreadId,
    kept_ordinal: Option<u64>,
    prompts: &HashSet<String>,
) -> Result<Option<String>, String> {
    let mut later: Vec<_> = turns
        .iter()
        .filter(|turn| {
            &turn.provider_thread_id == provider_thread_id
                && kept_ordinal.is_none_or(|kept| turn.ordinal.get() > kept)
        })
        .collect();
    later.sort_by_key(|turn| turn.ordinal);
    if later
        .iter()
        .any(|turn| prompt_of(turn).is_none() && !is_wake_turn(turn))
    {
        return Err(
            "This OpenCode conversation has turns from an earlier T3 Code version, so it can't be cut there."
                .to_owned(),
        );
    }
    Ok(later.into_iter().find_map(|turn| {
        let id = prompt_of(turn)?;
        prompts.contains(id).then(|| id.to_owned())
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormOption {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormField {
    pub key: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub custom: Option<bool>,
    #[serde(default)]
    pub when: Option<Vec<Value>>,
    #[serde(default)]
    pub options: Option<Vec<FormOption>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NativeForm {
    pub id: String,
    #[serde(default)]
    pub fields: Vec<FormField>,
}

fn text_or(value: Option<&str>, fallback: &str) -> String {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

pub fn form_questions(form: &NativeForm) -> Result<Vec<OrchestrationV2UserInputQuestion>, String> {
    let mut questions = Vec::new();
    for (index, field) in form.fields.iter().enumerate() {
        if field.field_type == "external" {
            return Err("a link to open".to_owned());
        }
        if field.field_type != "string" && field.field_type != "multiselect" {
            return Err(format!("a {} value", field.field_type));
        }
        if field.hidden == Some(true) {
            return Err("a hidden field".to_owned());
        }
        if field.when.as_ref().is_some_and(|when| !when.is_empty()) {
            return Err("a field that depends on another".to_owned());
        }
        let header = text_or(field.title.as_deref(), &format!("Question {}", index + 1));
        let options = field
            .options
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|option| {
                let label = text_or(
                    option.label.as_deref(),
                    &text_or(option.value.as_deref(), "Option"),
                );
                let description = text_or(option.description.as_deref(), &label);
                OrchestrationV2UserInputQuestionOption {
                    label: TrimmedNonEmptyString::new(&label)
                        .unwrap_or_else(|_| TrimmedNonEmptyString::new("Option").expect("literal")),
                    description: TrimmedNonEmptyString::new(&description)
                        .unwrap_or_else(|_| TrimmedNonEmptyString::new("Option").expect("literal")),
                    value: option.value.clone(),
                }
            })
            .collect::<Vec<_>>();
        let allow_custom = field.custom == Some(true) || options.is_empty();
        questions.push(OrchestrationV2UserInputQuestion {
            id: TrimmedNonEmptyString::new(&field.key)
                .unwrap_or_else(|_| TrimmedNonEmptyString::new("question").expect("literal")),
            header: TrimmedNonEmptyString::new(&header)
                .unwrap_or_else(|_| TrimmedNonEmptyString::new("Question").expect("literal")),
            question: TrimmedNonEmptyString::new(text_or(field.description.as_deref(), &header))
                .unwrap_or_else(|_| TrimmedNonEmptyString::new("Question").expect("literal")),
            options,
            multi_select: Some(field.field_type == "multiselect"),
            allow_custom_answer: Some(allow_custom),
            initial_answer: None,
            required: None,
        });
    }
    Ok(questions)
}

pub fn form_answer(form: &NativeForm, answers: &HashMap<String, Value>) -> Value {
    let mut answer = serde_json::Map::new();
    for field in &form.fields {
        let Some(raw) = answers.get(&field.key) else {
            continue;
        };
        let values: Vec<String> = match raw {
            Value::Array(items) => items
                .iter()
                .filter_map(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .collect(),
            Value::String(value) if !value.trim().is_empty() => vec![value.clone()],
            _ => Vec::new(),
        };
        if values.is_empty() {
            continue;
        }
        if field.field_type == "multiselect" {
            answer.insert(field.key.clone(), json!(values));
        } else {
            answer.insert(field.key.clone(), json!(values.join(", ")));
        }
    }
    Value::Object(answer)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StepTokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UsageAccum {
    part_ids: HashSet<String>,
    pub steps: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub has_subagents: bool,
    pub complete: bool,
}

impl UsageAccum {
    pub fn new() -> Self {
        Self {
            complete: true,
            ..Self::default()
        }
    }

    pub fn add_step(&mut self, id: &str, tokens: StepTokens) {
        if !self.part_ids.insert(id.to_owned()) {
            return;
        }
        self.steps += 1;
        self.input_tokens += tokens.input + tokens.cache_read + tokens.cache_write;
        self.cached_input_tokens += tokens.cache_read;
        self.cache_creation_tokens += tokens.cache_write;
        self.output_tokens += tokens.output + tokens.reasoning;
        self.reasoning_tokens += tokens.reasoning;
    }

    pub fn to_turn_usage(&self, completed: bool) -> TurnTokenUsage {
        if self.steps == 0 {
            return TurnTokenUsage::Partial {
                usage_scope: MainAgentUsageScope,
                cached_input_tokens: None,
                cache_creation_tokens: None,
                reasoning_tokens: None,
                has_subagents: self.has_subagents,
                usage_status: TurnTokenUsagePartialStatus::Unavailable,
                input_tokens: None,
                output_tokens: None,
            };
        }
        if completed && self.complete {
            TurnTokenUsage::Complete {
                usage_scope: MainAgentUsageScope,
                cached_input_tokens: Some(self.cached_input_tokens),
                cache_creation_tokens: Some(self.cache_creation_tokens),
                reasoning_tokens: Some(self.reasoning_tokens),
                has_subagents: self.has_subagents,
                usage_status: TurnTokenUsageCompleteStatus,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            }
        } else {
            TurnTokenUsage::Partial {
                usage_scope: MainAgentUsageScope,
                cached_input_tokens: Some(self.cached_input_tokens),
                cache_creation_tokens: Some(self.cache_creation_tokens),
                reasoning_tokens: Some(self.reasoning_tokens),
                has_subagents: self.has_subagents,
                usage_status: TurnTokenUsagePartialStatus::Partial,
                input_tokens: Some(self.input_tokens),
                output_tokens: Some(self.output_tokens),
            }
        }
    }
}

pub fn parse_step_tokens(value: &Value) -> Option<StepTokens> {
    let tokens = value.get("tokens").unwrap_or(value);
    Some(StepTokens {
        input: json_u64(tokens, "input")?,
        output: json_u64(tokens, "output").unwrap_or(0),
        reasoning: json_u64(tokens, "reasoning").unwrap_or(0),
        cache_read: tokens
            .get("cache")
            .and_then(|cache| json_u64(cache, "read"))
            .unwrap_or(0),
        cache_write: tokens
            .get("cache")
            .and_then(|cache| json_u64(cache, "write"))
            .unwrap_or(0),
    })
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|n| {
        n.as_u64()
            .or_else(|| n.as_i64().filter(|n| *n >= 0).map(|n| n as u64))
            .or_else(|| {
                n.as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(|n| n as u64)
            })
    })
}

#[derive(Debug, Clone)]
pub struct ToolItemBase {
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<OrchestrationV2ProviderRef>,
    pub ordinal: u64,
    pub status: OrchestrationV2TurnItemStatus,
    pub title: Option<String>,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
}

pub fn open_code_tool_projection_kind(tool_name: &str) -> &'static str {
    let normalized = tool_name.to_ascii_lowercase();
    if normalized == "todowrite" {
        return "dynamic_tool";
    }
    if normalized.contains("bash") || normalized.contains("shell") {
        return "command_execution";
    }
    if normalized.contains("edit") || normalized.contains("write") || normalized.contains("patch") {
        return "file_change";
    }
    if normalized.contains("web") || normalized == "codesearch" || normalized == "code_search" {
        return "web_search";
    }
    if normalized == "read" {
        return "dynamic_tool";
    }
    if normalized.contains("glob")
        || normalized.contains("grep")
        || normalized.contains("search")
        || normalized.contains("lsp")
    {
        return "file_search";
    }
    "dynamic_tool"
}

fn value_string(input: &Value, keys: &[&str]) -> Option<String> {
    let object = input.as_object()?;
    for key in keys {
        if let Some(value) = object.get(*key).and_then(Value::as_str)
            && !value.trim().is_empty()
        {
            return Some(value.to_owned());
        }
    }
    None
}

fn value_i64(input: &Value, keys: &[&str]) -> Option<i64> {
    let object = input.as_object()?;
    for key in keys {
        if let Some(value) = object.get(*key).and_then(Value::as_i64) {
            return Some(value);
        }
        if let Some(value) = object.get(*key).and_then(Value::as_u64) {
            return Some(value as i64);
        }
    }
    None
}

fn stable_json(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn format_read_tool_label(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        "Read file".to_owned()
    } else {
        format!("Read {trimmed}")
    }
}

fn format_search_tool_label(input: &Value, pattern: Option<&str>) -> Option<String> {
    let query = value_string(input, &["pattern", "query", "searchTerm", "regex"])
        .or_else(|| pattern.map(str::to_owned));
    let glob = value_string(input, &["glob", "globPattern", "include"]);
    let target = value_string(input, &["path", "filePath", "directory"]).and_then(|path| {
        path.rsplit(['/', '\\'])
            .find(|part| !part.is_empty() && *part != ".")
            .map(str::to_owned)
    });
    match (query.as_deref(), glob.as_deref(), target.as_deref()) {
        (Some(query), _, Some(target)) => Some(format!("Searched {query} in {target}")),
        (_, Some(glob), Some(target)) => Some(format!("Searched files {glob} in {target}")),
        (_, Some(glob), None) => Some(format!("Searched files {glob}")),
        (Some(query), _, None) => Some(format!("Searched {query}")),
        (_, _, Some(target)) => Some(format!("Searched in {target}")),
        _ => None,
    }
}

macro_rules! tool_item {
    ($base:expr, $variant:ident { $($field:ident: $value:expr),* $(,)? }) => {
        OrchestrationV2TurnItem::$variant {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: $base.id.clone(),
            thread_id: $base.thread_id.clone(),
            run_id: $base.run_id.clone(),
            node_id: $base.node_id.clone(),
            provider_thread_id: $base.provider_thread_id.clone(),
            provider_turn_id: $base.provider_turn_id.clone(),
            native_item_ref: $base.native_item_ref.clone(),
            parent_item_id: None,
            ordinal: $base.ordinal,
            status: $base.status,
            title: $base.title.clone(),
            started_at: $base.started_at,
            completed_at: $base.completed_at,
            updated_at: $base.updated_at,
            $($field: $value,)*
        }
    };
}

pub fn open_code_tool_turn_item(
    base: ToolItemBase,
    name: &str,
    input: &Value,
    output: Option<&str>,
    completed_metadata: Option<&Value>,
) -> OrchestrationV2TurnItem {
    let metadata = completed_metadata.unwrap_or(&Value::Null);
    match open_code_tool_projection_kind(name) {
        "command_execution" => {
            let exit_code = value_i64(metadata, &["exit", "exitCode"]);
            let command =
                value_string(input, &["command", "cmd"]).unwrap_or_else(|| stable_json(input));
            tool_item!(
                base,
                CommandExecution {
                    input: command,
                    output: output.map(str::to_owned),
                    output_omitted: None,
                    output_indicates_failure: None,
                    exit_code: exit_code,
                }
            )
        }
        "file_change" => {
            let diff = value_string(metadata, &["diff", "patch"]).or_else(|| {
                (base.status == OrchestrationV2TurnItemStatus::Failed)
                    .then(|| {
                        output
                            .filter(|text| !text.trim().is_empty())
                            .map(str::to_owned)
                    })
                    .flatten()
            });
            let raw_name = value_string(input, &["filePath", "path", "file"])
                .unwrap_or_else(|| name.to_owned());
            let file_name = TrimmedNonEmptyString::new(raw_name).unwrap_or_else(|_| {
                TrimmedNonEmptyString::new(name)
                    .unwrap_or_else(|_| TrimmedNonEmptyString::new("file").expect("literal"))
            });
            tool_item!(
                base,
                FileChange {
                    file_name: file_name,
                    additions: None,
                    deletions: None,
                    diff_str: diff,
                    old_str: value_string(input, &["oldString", "oldText"]),
                    new_str: value_string(input, &["newString", "content", "newText"]),
                    changes: None,
                }
            )
        }
        "file_search" => {
            let pattern = value_string(input, &["pattern", "query", "path", "filePath"]);
            let search_root =
                value_string(input, &["path", "filePath"]).or_else(|| pattern.clone());
            let title = format_search_tool_label(input, pattern.as_deref()).or(base.title.clone());
            let mut titled = base;
            titled.title = title;
            let results = match (
                output.map(str::trim).filter(|text| !text.is_empty()),
                search_root,
            ) {
                (Some(output), Some(root)) => Some(vec![OrchestrationV2FileSearchResult {
                    file_name: TrimmedNonEmptyString::new(root)
                        .unwrap_or_else(|_| TrimmedNonEmptyString::new("search").expect("literal")),
                    line: None,
                    column: None,
                    preview: Some(output.chars().take(SEARCH_PREVIEW_MAX_CHARS).collect()),
                }]),
                _ => None,
            };
            tool_item!(
                titled,
                FileSearch {
                    pattern: pattern,
                    results: results
                }
            )
        }
        "web_search" => {
            let pattern = value_string(input, &["query", "url", "pattern"]);
            let url = value_string(input, &["url"]);
            let results = output
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|output| {
                    vec![OrchestrationV2WebSearchResult {
                        title: None,
                        url: url.and_then(|url| TrimmedNonEmptyString::new(url).ok()),
                        snippet: Some(output.chars().take(SEARCH_PREVIEW_MAX_CHARS).collect()),
                    }]
                });
            let patterns = pattern.map(|pattern| vec![pattern]);
            tool_item!(
                base,
                WebSearch {
                    patterns: patterns,
                    results: results,
                }
            )
        }
        _ => {
            let read_path = value_string(input, &["filePath", "path", "file"]);
            let mut titled = base;
            if name.eq_ignore_ascii_case("read")
                && let Some(path) = &read_path
            {
                titled.title = Some(format_read_tool_label(path));
            }
            tool_item!(
                titled,
                DynamicTool {
                    tool_name: TrimmedNonEmptyString::new(name).ok(),
                    viewed_image_path: None,
                    input: input.clone(),
                    output: output.map(|text| Value::String(text.to_owned())),
                    output_omitted: None,
                }
            )
        }
    }
}

pub fn subagent_output(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("<subagent")
        && let Some(body_at) = rest.find('>')
    {
        let body = &rest[body_at + 1..];
        if let Some(end) = body.rfind("</subagent>") {
            return body[..end].trim_matches('\n').to_owned();
        }
    }
    text.to_owned()
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    variants: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    models: HashMap<String, ProviderModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderList {
    #[serde(default)]
    all: Vec<ProviderEntry>,
    #[serde(default)]
    connected: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentEntry {
    name: String,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    mode: Option<String>,
}

fn title_case_slug(value: &str) -> String {
    value
        .split(['-', '_', '/'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn infer_default_variant(provider_id: &str, variants: &[String]) -> Option<String> {
    if variants.len() == 1 {
        return variants.first().cloned();
    }
    if provider_id == "anthropic" || provider_id.starts_with("google") {
        return variants.iter().find(|v| v.as_str() == "high").cloned();
    }
    if provider_id == "openai" || provider_id == "opencode" {
        return variants
            .iter()
            .find(|v| v.as_str() == "medium")
            .or_else(|| variants.iter().find(|v| v.as_str() == "high"))
            .cloned();
    }
    None
}

fn select_descriptor(
    id: &str,
    label: &str,
    options: Vec<(String, bool)>,
    current: Option<&str>,
) -> ProviderOptionDescriptor {
    ProviderOptionDescriptor::Select(SelectProviderOptionDescriptor {
        id: TrimmedNonEmptyString::new(id).expect("descriptor id"),
        label: TrimmedNonEmptyString::new(label).expect("descriptor label"),
        description: None,
        type_: SelectOptionType,
        options: options
            .into_iter()
            .map(|(value, is_default)| ProviderOptionChoice {
                id: TrimmedNonEmptyString::new(&value)
                    .unwrap_or_else(|_| TrimmedNonEmptyString::new("option").expect("literal")),
                label: TrimmedNonEmptyString::new(title_case_slug(&value)).unwrap_or_else(|_| {
                    TrimmedNonEmptyString::new(&value)
                        .unwrap_or_else(|_| TrimmedNonEmptyString::new("Option").expect("literal"))
                }),
                description: None,
                is_default: is_default.then_some(true),
            })
            .collect(),
        current_value: current.and_then(|value| TrimmedNonEmptyString::new(value).ok()),
        prompt_injected_values: None,
    })
}

fn capabilities_for(
    provider_id: &str,
    model: &ProviderModel,
    agents: &[AgentEntry],
) -> ModelCapabilities {
    let mut variants: Vec<String> = model
        .variants
        .as_ref()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    if variants.is_empty() {
        variants = ["low", "medium", "high", "xhigh"]
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    let default_variant = infer_default_variant(provider_id, &variants);
    let variant_options = variants
        .iter()
        .map(|value| (value.clone(), default_variant.as_deref() == Some(value)))
        .collect();
    let primary: Vec<_> = agents
        .iter()
        .filter(|agent| agent.hidden != Some(true))
        .filter(|agent| matches!(agent.mode.as_deref(), Some("primary") | Some("all") | None))
        .collect();
    let default_agent = primary
        .iter()
        .find(|agent| agent.name == "build")
        .or_else(|| primary.first())
        .map(|agent| agent.name.clone());
    let agent_options = primary
        .iter()
        .map(|agent| {
            (
                agent.name.clone(),
                default_agent.as_deref() == Some(agent.name.as_str()),
            )
        })
        .collect();
    let mut descriptors = Vec::new();
    if !variants.is_empty() {
        descriptors.push(select_descriptor(
            "variant",
            "Reasoning",
            variant_options,
            default_variant.as_deref(),
        ));
    }
    if !primary.is_empty() {
        descriptors.push(select_descriptor(
            "agent",
            "Agent",
            agent_options,
            default_agent.as_deref(),
        ));
    }
    ModelCapabilities {
        option_descriptors: Some(descriptors),
    }
}

/// Connected providers from `GET /provider` or v2 `{data: ...}`.
pub fn models_from_provider_list(payload: &Value, agents: &[Value]) -> Vec<ServerProviderModel> {
    let list: ProviderList = if payload.get("data").is_some() {
        serde_json::from_value(payload["data"].clone()).unwrap_or(ProviderList {
            all: Vec::new(),
            connected: Vec::new(),
        })
    } else {
        serde_json::from_value(payload.clone()).unwrap_or(ProviderList {
            all: Vec::new(),
            connected: Vec::new(),
        })
    };
    let agents: Vec<AgentEntry> = agents
        .iter()
        .filter_map(|agent| serde_json::from_value(agent.clone()).ok())
        .collect();
    let connected: HashSet<_> = list.connected.iter().cloned().collect();
    let mut models = Vec::new();
    for provider in list.all {
        if !connected.is_empty() && !connected.contains(&provider.id) {
            continue;
        }
        for (key, mut model) in provider.models {
            if model.id.is_empty() {
                model.id = key;
            }
            let Some(name) = TrimmedNonEmptyString::new(model.name.trim()).ok() else {
                continue;
            };
            models.push(ServerProviderModel {
                slug: TrimmedNonEmptyString::new(format!("{}/{}", provider.id, model.id))
                    .unwrap_or_else(|_| TrimmedNonEmptyString::new("model").expect("literal")),
                name,
                short_name: None,
                sub_provider: provider
                    .name
                    .as_deref()
                    .and_then(|name| TrimmedNonEmptyString::new(name.trim()).ok()),
                aliases: None,
                badge: None,
                is_custom: false,
                is_default: None,
                is_legacy: None,
                capabilities: Some(capabilities_for(&provider.id, &model, &agents)),
            });
        }
    }
    models.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    models
}

pub fn skills_from_list(skills: &[Value]) -> Vec<ServerProviderSkill> {
    let mut out = Vec::new();
    for skill in skills {
        let Some(name) = skill
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| TrimmedNonEmptyString::new(name.trim()).ok())
        else {
            continue;
        };
        let path = skill
            .get("location")
            .or_else(|| skill.get("path"))
            .and_then(Value::as_str)
            .and_then(|path| TrimmedNonEmptyString::new(path.trim()).ok());
        let Some(path) = path else { continue };
        let description = skill
            .get("description")
            .and_then(Value::as_str)
            .and_then(|text| TrimmedNonEmptyString::new(text.trim()).ok());
        out.push(ServerProviderSkill {
            name,
            description: description.clone(),
            path,
            scope: None,
            enabled: true,
            display_name: None,
            short_description: description,
            user_invocation_only: None,
            user_invocable: None,
        });
    }
    out.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    out
}

pub fn commands_from_list(commands: &[Value]) -> Vec<ServerProviderSlashCommand> {
    let compact = ServerProviderSlashCommand {
        name: TrimmedNonEmptyString::new("compact").expect("compact"),
        description: Some(
            TrimmedNonEmptyString::new("Compact the conversation").expect("compact description"),
        ),
        input: None,
    };
    let mut names = HashSet::from(["compact".to_owned()]);
    let mut out = vec![compact];
    for command in commands {
        if command.get("source").and_then(Value::as_str) == Some("skill") {
            continue;
        }
        let Some(name) = command
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        if !names.insert(name.to_owned()) {
            continue;
        }
        let hint = command
            .get("hints")
            .and_then(Value::as_array)
            .map(|hints| {
                hints
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|hint| !hint.trim().is_empty());
        out.push(ServerProviderSlashCommand {
            name: TrimmedNonEmptyString::new(name).expect("command name"),
            description: command
                .get("description")
                .and_then(Value::as_str)
                .and_then(|text| TrimmedNonEmptyString::new(text.trim()).ok()),
            input: hint.and_then(|hint| {
                TrimmedNonEmptyString::new(hint.trim())
                    .ok()
                    .map(|hint| ServerProviderSlashCommandInput { hint })
            }),
        });
    }
    out
}

pub fn format_probe_error(
    detail: Option<&str>,
    external: bool,
    phase: &str,
    server_url: &str,
) -> (bool, String) {
    let lower = detail.unwrap_or("").to_ascii_lowercase();
    if external {
        if lower.contains("401")
            || lower.contains("403")
            || lower.contains("unauthorized")
            || lower.contains("forbidden")
        {
            return (
                true,
                "OpenCode server rejected authentication. Check the server URL and password."
                    .to_owned(),
            );
        }
        if [
            "econnrefused",
            "enotfound",
            "fetch failed",
            "timed out",
            "timeout",
            "socket hang up",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            return (
                true,
                format!(
                    "Couldn't reach the configured OpenCode server at {server_url}. Check that the server is running and the URL is correct."
                ),
            );
        }
        return (
            true,
            detail
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("Failed to connect to the configured OpenCode server.")
                .to_owned(),
        );
    }
    if lower.contains("enoent") || lower.contains("notfound") {
        return (
            false,
            "OpenCode CLI (`opencode`) is not installed or not on PATH.".to_owned(),
        );
    }
    if lower.contains("quarantine") {
        return (
            true,
            "macOS is blocking the OpenCode binary (quarantine). Run `xattr -d com.apple.quarantine $(which opencode)` to fix this.".to_owned(),
        );
    }
    let label = if phase == "inventory" {
        "Failed to load OpenCode provider inventory"
    } else {
        "Failed to execute OpenCode CLI health check"
    };
    (
        true,
        match detail.filter(|text| !text.trim().is_empty()) {
            Some(detail) => format!("{label}: {detail}"),
            None => format!("{label}."),
        },
    )
}

pub fn provider_failure(
    class: OrchestrationV2ProviderFailureClass,
    message: &str,
) -> OrchestrationV2ProviderFailure {
    let trimmed = message.trim();
    let message = if trimmed.is_empty() {
        "OpenCode request failed".to_owned()
    } else {
        trimmed.chars().take(2_000).collect()
    };
    OrchestrationV2ProviderFailure {
        class,
        message: OrchestrationV2ProviderFailureMessage::new(message).unwrap_or_else(|_| {
            OrchestrationV2ProviderFailureMessage::new("OpenCode request failed").expect("literal")
        }),
        code: None,
        retryable: None,
        reset_at: None,
    }
}

pub fn native_ref(native_id: &str, weak: bool) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: opencode_driver(),
        native_id: TrimmedNonEmptyString::new(native_id).ok(),
        strength: if weak {
            OrchestrationV2NativeRefStrength::Weak
        } else {
            OrchestrationV2NativeRefStrength::Strong
        },
        fingerprint: None,
        ordinal: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn caps(
    sessions: OrchestrationV2SessionCapabilities,
    threads: OrchestrationV2ThreadCapabilities,
    turns: OrchestrationV2TurnCapabilities,
    streaming: OrchestrationV2StreamingCapabilities,
    tools: OrchestrationV2ToolCapabilities,
    approvals: OrchestrationV2ApprovalCapabilities,
    planning: OrchestrationV2PlanningCapabilities,
    subagents: OrchestrationV2SubagentCapabilities,
    context: OrchestrationV2ContextCapabilities,
) -> OrchestrationV2ProviderCapabilities {
    OrchestrationV2ProviderCapabilities {
        sessions,
        threads,
        turns,
        streaming,
        tools,
        approvals,
        planning,
        subagents,
        context,
        checkpointing: OrchestrationV2CheckpointCapabilities {
            app_can_checkpoint_filesystem: true,
            supports_nested_checkpoint_scopes: true,
            provider_can_rollback_conversation: true,
            provider_rollback_returns_snapshot: true,
            provider_can_read_conversation_snapshot: true,
        },
        identity: OrchestrationV2IdentityCapabilities {
            native_thread_ids: OrchestrationV2NativeRefStrength::Strong,
            native_turn_ids: OrchestrationV2NativeRefStrength::Weak,
            native_item_ids: OrchestrationV2NativeRefStrength::Strong,
            native_request_ids: OrchestrationV2NativeRefStrength::Strong,
        },
        runtime_policy: OrchestrationV2RuntimePolicyCapabilities {
            enforcement: OrchestrationV2RuntimePolicyEnforcement::Native,
        },
    }
}

pub fn v1_capabilities() -> OrchestrationV2ProviderCapabilities {
    caps(
        OrchestrationV2SessionCapabilities {
            supports_multiple_provider_threads_per_session: false,
            supports_model_switch_in_session: true,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: false,
            pending_requests_survive_restart: false,
        },
        OrchestrationV2ThreadCapabilities {
            can_create_empty_thread: true,
            can_read_thread_snapshot: true,
            can_rollback_thread: true,
            can_fork_thread: true,
            can_fork_from_turn: true,
            can_fork_from_subagent_thread: true,
            exposes_native_thread_id: true,
        },
        OrchestrationV2TurnCapabilities {
            exposes_native_turn_id: false,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_active_steering: true,
            active_steering_interrupts_tools: None,
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: OrchestrationV2NativeRefStrength::Strong,
        },
        OrchestrationV2StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: true,
            streams_plan_text: false,
            emits_message_completed: true,
        },
        OrchestrationV2ToolCapabilities {
            exposes_tool_item_ids: true,
            emits_tool_started: true,
            emits_tool_completed: true,
            emits_tool_output: true,
            supports_mcp_tools: true,
            supports_dynamic_tool_callbacks: false,
        },
        OrchestrationV2ApprovalCapabilities {
            supports_command_approval: true,
            supports_file_read_approval: true,
            supports_file_change_approval: true,
            supports_apply_patch_approval: true,
            approvals_have_native_request_ids: true,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: true,
        },
        OrchestrationV2PlanningCapabilities {
            emits_plan_updated: true,
            emits_todo_list: true,
            emits_proposed_plan: false,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: false,
        },
        OrchestrationV2SubagentCapabilities {
            supports_subagents: true,
            exposes_subagent_thread_ids: true,
            emits_subagent_lifecycle: true,
            can_wait_for_subagents: true,
            can_close_subagents: false,
            can_fork_subagent_thread: true,
        },
        OrchestrationV2ContextCapabilities {
            accepts_system_context: false,
            accepts_developer_context: false,
            accepts_synthetic_user_context: true,
            can_generate_summaries: true,
            can_consume_handoff_summaries: true,
            supports_delta_handoff: true,
            supports_full_thread_handoff: true,
            max_recommended_handoff_chars: None,
        },
    )
}

pub fn v2_capabilities() -> OrchestrationV2ProviderCapabilities {
    caps(
        OrchestrationV2SessionCapabilities {
            supports_multiple_provider_threads_per_session: true,
            supports_model_switch_in_session: true,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: true,
            pending_requests_survive_restart: false,
        },
        OrchestrationV2ThreadCapabilities {
            can_create_empty_thread: true,
            can_read_thread_snapshot: true,
            can_rollback_thread: true,
            can_fork_thread: true,
            can_fork_from_turn: true,
            can_fork_from_subagent_thread: false,
            exposes_native_thread_id: true,
        },
        OrchestrationV2TurnCapabilities {
            exposes_native_turn_id: false,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_active_steering: true,
            active_steering_interrupts_tools: None,
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: OrchestrationV2NativeRefStrength::Strong,
        },
        OrchestrationV2StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: false,
            streams_plan_text: false,
            emits_message_completed: true,
        },
        OrchestrationV2ToolCapabilities {
            exposes_tool_item_ids: true,
            emits_tool_started: true,
            emits_tool_completed: true,
            emits_tool_output: true,
            supports_mcp_tools: false,
            supports_dynamic_tool_callbacks: false,
        },
        OrchestrationV2ApprovalCapabilities {
            supports_command_approval: true,
            supports_file_read_approval: true,
            supports_file_change_approval: true,
            supports_apply_patch_approval: true,
            approvals_have_native_request_ids: true,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: true,
        },
        OrchestrationV2PlanningCapabilities {
            emits_plan_updated: false,
            emits_todo_list: false,
            emits_proposed_plan: false,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: false,
        },
        OrchestrationV2SubagentCapabilities {
            supports_subagents: true,
            exposes_subagent_thread_ids: true,
            emits_subagent_lifecycle: true,
            can_wait_for_subagents: true,
            can_close_subagents: false,
            can_fork_subagent_thread: false,
        },
        OrchestrationV2ContextCapabilities {
            accepts_system_context: false,
            accepts_developer_context: false,
            accepts_synthetic_user_context: true,
            can_generate_summaries: false,
            can_consume_handoff_summaries: true,
            supports_delta_handoff: true,
            supports_full_thread_handoff: true,
            max_recommended_handoff_chars: None,
        },
    )
}

/// A failed probe keeps 1.x. Only an explicit v2 probe selects the 2.x adapter.
pub fn select_generation(probed: Option<crate::OpenCodeGeneration>) -> crate::OpenCodeGeneration {
    match probed {
        Some(crate::OpenCodeGeneration::V2) => crate::OpenCodeGeneration::V2,
        _ => crate::OpenCodeGeneration::V1,
    }
}

pub fn per_turn_instructions(model: &str) -> String {
    turin_provider_core::runtime_instructions::build_runtime_instructions(
        &turin_provider_core::runtime_instructions::RuntimeInfo {
            harness: "OpenCode",
            model: Some(model),
            ..Default::default()
        },
    )
}

pub fn context_window_of(model: &Value) -> Option<u64> {
    let limit = model.get("limit")?;
    json_u64(limit, "input").or_else(|| json_u64(limit, "context"))
}

pub fn pending_status(
    enabled: bool,
    server_url: &str,
) -> (ServerProviderState, ServerProviderAuth, String) {
    let message = if !enabled {
        if server_url.trim().is_empty() {
            "OpenCode is disabled in T3 Code settings.".to_owned()
        } else {
            "OpenCode is disabled in T3 Code settings. A server URL is configured.".to_owned()
        }
    } else {
        "OpenCode provider status has not been checked in this session yet.".to_owned()
    };
    (
        ServerProviderState::Warning,
        ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
        message,
    )
}

pub fn v1_rules_as_session(rules: &[V1PermissionRule]) -> Vec<SessionPermissionRule> {
    rules
        .iter()
        .map(|rule| SessionPermissionRule {
            action: rule.permission.clone(),
            resource: rule.pattern.clone(),
            effect: rule.action.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::PositiveInt;

    #[test]
    fn admission_holds_stale_idle_until_the_user_message_is_observed() {
        let mut admission = AdmissionFlags {
            admission_pending: true,
            ..AdmissionFlags::default()
        };
        assert_eq!(
            advance_open_code_prompt_admission(&mut admission, OpenCodeAdmissionSignal::Idle),
            OpenCodeAdmissionAction::Hold
        );
        assert!(admission.idle_during_admission);
        assert_eq!(
            advance_open_code_prompt_admission(&mut admission, OpenCodeAdmissionSignal::Accepted),
            OpenCodeAdmissionAction::Hold
        );
        assert_eq!(
            advance_open_code_prompt_admission(
                &mut admission,
                OpenCodeAdmissionSignal::UserMessage
            ),
            OpenCodeAdmissionAction::ReconcileIdle
        );
    }

    #[test]
    fn admission_releases_after_acceptance_and_evidence() {
        let mut admission = AdmissionFlags {
            admission_pending: true,
            ..AdmissionFlags::default()
        };
        assert_eq!(
            advance_open_code_prompt_admission(&mut admission, OpenCodeAdmissionSignal::Accepted),
            OpenCodeAdmissionAction::Hold
        );
        assert_eq!(
            advance_open_code_prompt_admission(&mut admission, OpenCodeAdmissionSignal::Busy),
            OpenCodeAdmissionAction::Release
        );
        assert!(!admission.admission_pending);
    }

    #[test]
    fn assistant_completion_releases_admission() {
        let mut admission = AdmissionFlags {
            admission_pending: true,
            ..AdmissionFlags::default()
        };
        assert_eq!(
            advance_open_code_prompt_admission(
                &mut admission,
                OpenCodeAdmissionSignal::AssistantCompleted
            ),
            OpenCodeAdmissionAction::Release
        );
    }

    #[test]
    fn permission_families_map_to_request_kinds() {
        assert_eq!(open_code_permission_request_kind("bash", None), "command");
        assert_eq!(
            open_code_permission_request_kind("edit", None),
            "file-change"
        );
        assert_eq!(open_code_permission_request_kind("read", None), "file-read");
        assert_eq!(
            open_code_permission_request_kind("external_directory", None),
            "file-read"
        );
    }

    #[test]
    fn tools_map_to_turn_item_families() {
        assert_eq!(open_code_tool_projection_kind("bash"), "command_execution");
        assert_eq!(open_code_tool_projection_kind("edit"), "file_change");
        assert_eq!(open_code_tool_projection_kind("grep"), "file_search");
        assert_eq!(open_code_tool_projection_kind("webfetch"), "web_search");
        assert_eq!(open_code_tool_projection_kind("read"), "dynamic_tool");
        assert_eq!(open_code_tool_projection_kind("todowrite"), "dynamic_tool");
    }

    #[test]
    fn full_access_without_approval_is_a_star_allow() {
        let rules = open_code_permission_rules(RuntimeMode::FullAccess, None, None);
        assert_eq!(rules, vec![v1_rule("*", "*", "allow")]);
    }

    #[test]
    fn auto_keeps_edit_asking() {
        let rules = open_code_permission_rules(RuntimeMode::Auto, None, None);
        assert!(rules.iter().any(|rule| {
            rule.permission == "edit" && rule.action == "ask" && rule.pattern == "*"
        }));
        assert!(!rules.iter().any(|rule| {
            rule.permission == "edit" && rule.action == "allow" && rule.pattern == "*"
        }));
    }

    #[test]
    fn auto_accept_edits_allows_edit() {
        let rules = open_code_permission_rules(RuntimeMode::AutoAcceptEdits, None, None);
        assert!(rules.iter().any(|rule| {
            rule.permission == "edit" && rule.action == "allow" && rule.pattern == "*"
        }));
    }

    #[test]
    fn child_rules_keep_parent_denies_and_extra_child_rules() {
        let parent = open_code_permission_rules(RuntimeMode::ApprovalRequired, None, None);
        let child = open_code_child_permission_rules(
            &parent,
            &[v1_rule("task", "*", "deny"), v1_rule("*", "*", "deny")],
        );
        assert!(
            child
                .iter()
                .any(|rule| rule.permission == "task" && rule.action == "deny")
        );
        let deny_star = child
            .iter()
            .filter(|rule| rule.permission == "*" && rule.pattern == "*" && rule.action == "deny")
            .count();
        assert_eq!(deny_star, 1);
    }

    #[test]
    fn boundary_is_the_next_native_user_message() {
        let turns = vec![turn("a", 1, Some("msg_a")), turn("b", 2, Some("msg_b"))];
        assert_eq!(
            open_code_boundary_after_provider_turn(&turns, &turns[0].id).as_deref(),
            Some("msg_b")
        );
        assert_eq!(
            open_code_boundary_after_provider_turn(&turns, &turns[1].id),
            None
        );
    }

    #[test]
    fn declined_note_names_the_action_and_resources() {
        assert!(declined_note("shell", &[]).contains("declined the shell request."));
        assert!(declined_note("shell", &["pwd".into()]).contains("(pwd)"));
        assert_eq!(
            v2_permission_decision(ProviderApprovalDecision::Decline),
            "reject"
        );
        assert_eq!(
            v2_permission_decision(ProviderApprovalDecision::AcceptForSession),
            "once"
        );
        assert_eq!(
            v1_permission_reply(ProviderApprovalDecision::AcceptForSession),
            "always"
        );
    }

    #[test]
    fn command_and_skills_parse_prompt_text() {
        let command = command_of("/review branch").unwrap();
        assert_eq!(command.name, "review");
        assert_eq!(command.text, "branch");
        assert!(command_of("hello").is_none());
        let known = HashSet::from(["commit".to_owned()]);
        assert_eq!(
            skills_named("use $commit please", &known),
            vec!["commit".to_owned()]
        );
        assert!(skills_named("costs $20 today", &known).is_empty());
    }

    #[test]
    fn step_usage_adds_cache_into_input_and_reasoning_into_output() {
        let mut usage = UsageAccum::new();
        usage.add_step(
            "step-1",
            StepTokens {
                input: 10,
                output: 4,
                reasoning: 1,
                cache_read: 3,
                cache_write: 2,
            },
        );
        usage.add_step(
            "step-1",
            StepTokens {
                input: 100,
                ..StepTokens::default()
            },
        );
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.steps, 1);
    }

    #[test]
    fn forms_reject_hidden_fields_and_shape_answers() {
        let form: NativeForm = serde_json::from_value(json!({
            "id": "f1",
            "fields": [{
                "key": "name",
                "type": "string",
                "title": "Name",
                "options": [{"label": "Ada", "value": "ada"}]
            }]
        }))
        .unwrap();
        assert!(form_questions(&form).is_ok());
        let mut answers = HashMap::new();
        answers.insert("name".into(), json!("ada"));
        assert_eq!(form_answer(&form, &answers)["name"], "ada");
        let hidden: NativeForm = serde_json::from_value(json!({
            "id": "f2",
            "fields": [{"key": "x", "type": "string", "hidden": true}]
        }))
        .unwrap();
        assert_eq!(form_questions(&hidden).unwrap_err(), "a hidden field");
    }

    #[test]
    fn catalog_keeps_connected_models_and_compact() {
        let models = models_from_provider_list(
            &json!({
                "all": [{
                    "id": "openai",
                    "name": "OpenAI",
                    "models": { "gpt": { "id": "gpt", "name": "GPT" } }
                }],
                "connected": ["openai"]
            }),
            &[json!({"name": "build", "mode": "primary"})],
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug.as_str(), "openai/gpt");
        let commands = commands_from_list(&[json!({"name": "review", "description": "Review"})]);
        assert_eq!(commands[0].name.as_str(), "compact");
        assert_eq!(commands[1].name.as_str(), "review");
    }

    #[test]
    fn a_failed_probe_stays_on_v1() {
        assert_eq!(select_generation(None), crate::OpenCodeGeneration::V1);
        assert_eq!(
            select_generation(Some(crate::OpenCodeGeneration::V2)),
            crate::OpenCodeGeneration::V2
        );
    }

    fn turn(id: &str, ordinal: u64, native: Option<&str>) -> OrchestrationV2ProviderTurn {
        OrchestrationV2ProviderTurn {
            id: ProviderTurnId::new(id).unwrap(),
            provider_thread_id: ProviderThreadId::new("thread").unwrap(),
            node_id: NodeId::new("node").unwrap(),
            run_attempt_id: None,
            native_turn_ref: native.map(|native| native_ref(native, true)),
            ordinal: PositiveInt::new(ordinal).expect("turn ordinal"),
            status: turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Completed,
            started_at: None,
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        }
    }
}
