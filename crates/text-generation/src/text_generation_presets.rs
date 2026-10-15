//! Port of upstream/apps/server/src/textGeneration/TextGenerationPresets.ts

use crate::text_generation_policy::{TextGenerationPolicy, TextGenerationPolicyKind};

/// Upstream `conventionalCommitsTextGenerationPolicy`.
pub fn conventional_commits_text_generation_policy() -> TextGenerationPolicy {
    TextGenerationPolicy {
        commit_instructions: Some(
            "Use Conventional Commits when generating commit subjects. Prefer the narrowest accurate type and include a scope only when it is obvious from the diff.".to_owned(),
        ),
        change_request_instructions: Some(
            "Keep the change request title concise. Do not force Conventional Commit syntax into the title unless the repository already uses it.".to_owned(),
        ),
        ..TextGenerationPolicy::new(TextGenerationPolicyKind::ConventionalCommits, false)
    }
}

/// Upstream `repositoryConventionsTextGenerationPolicy`.
pub fn repository_conventions_text_generation_policy() -> TextGenerationPolicy {
    TextGenerationPolicy {
        commit_instructions: Some(
            "Follow the repository's established commit message style when examples are available.".to_owned(),
        ),
        change_request_instructions: Some(
            "Follow the repository's established change request title and body style when examples are available.".to_owned(),
        ),
        ..TextGenerationPolicy::new(TextGenerationPolicyKind::RepoConventions, true)
    }
}
