//! Port of `codexArtifactTemplatePromptToAppend`.

use turin_shared::codex_artifact_templates::{
    CodexArtifactTemplate, append_codex_artifact_template_use_prompt,
    codex_artifact_template_use_prompt,
};

pub fn codex_artifact_template_prompt_to_append(
    current_draft: &str,
    template: &CodexArtifactTemplate,
) -> Option<String> {
    if append_codex_artifact_template_use_prompt(current_draft, template) == current_draft {
        None
    } else {
        Some(codex_artifact_template_use_prompt(template))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_world() -> CodexArtifactTemplate {
        CodexArtifactTemplate {
            artifact_kind: "document".to_owned(),
            display_name: "Hello World".to_owned(),
            gallery_kind: None,
            skill_directory: "/Users/test/.codex/skills/artifact-template-hello-world".to_owned(),
            skill_name: "artifact-template-hello-world".to_owned(),
        }
    }

    #[test]
    fn does_not_insert_an_already_present_prompt() {
        let prompt = "Create a document using this $artifact-template-hello-world about…";
        assert_eq!(
            codex_artifact_template_prompt_to_append(prompt, &hello_world()),
            None
        );
    }
}
