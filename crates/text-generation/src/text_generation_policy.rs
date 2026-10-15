//! Port of upstream/packages/provider-core/src/server/textGenerationPolicy.ts
//!
//! The `customTextGenerationPolicy` helper lives here. The two presets are in
//! `text_generation_presets.rs`.

/// Upstream `TextGenerationPolicyKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextGenerationPolicyKind {
    Default,
    ConventionalCommits,
    RepoConventions,
    Custom,
}

/// Upstream `TextGenerationPolicy`. Instructions are appended to the matching prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextGenerationPolicy {
    pub kind: TextGenerationPolicyKind,
    pub commit_instructions: Option<String>,
    pub change_request_instructions: Option<String>,
    pub branch_instructions: Option<String>,
    pub thread_title_instructions: Option<String>,
    pub infer_repository_conventions: bool,
}

impl TextGenerationPolicy {
    /// `TextGenerationPolicy` with every instruction unset.
    pub fn new(kind: TextGenerationPolicyKind, infer_repository_conventions: bool) -> Self {
        Self {
            kind,
            commit_instructions: None,
            change_request_instructions: None,
            branch_instructions: None,
            thread_title_instructions: None,
            infer_repository_conventions,
        }
    }
}

/// Upstream `customTextGenerationPolicy(overrides)`. The kind is always `custom`.
pub fn custom_text_generation_policy(
    overrides: TextGenerationPolicyOverrides,
) -> TextGenerationPolicy {
    TextGenerationPolicy {
        commit_instructions: overrides.commit_instructions,
        change_request_instructions: overrides.change_request_instructions,
        branch_instructions: overrides.branch_instructions,
        thread_title_instructions: overrides.thread_title_instructions,
        ..TextGenerationPolicy::new(TextGenerationPolicyKind::Custom, false)
    }
}

/// `Omit<Partial<TextGenerationPolicy>, "kind">`. `inferRepositoryConventions` is
/// fixed to `false` for custom policies, so it is not an override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextGenerationPolicyOverrides {
    pub commit_instructions: Option<String>,
    pub change_request_instructions: Option<String>,
    pub branch_instructions: Option<String>,
    pub thread_title_instructions: Option<String>,
}
