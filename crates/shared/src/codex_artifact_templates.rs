//! Port of upstream/packages/shared/src/codexArtifactTemplates.ts

use turin_contracts::base_schemas::js_trim;

pub const CODEX_ARTIFACT_TEMPLATE_KINDS: &[&str] = &[
    "document",
    "presentation",
    "spreadsheet",
    "site",
    "google-docs",
    "google-slides",
    "google-sheets",
    "image",
    "email",
    "slack",
];

pub const CODEX_ARTIFACT_TEMPLATE_GALLERY_KINDS: &[&str] = &["imagegen", "product-design"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexArtifactTemplate {
    pub artifact_kind: String,
    pub display_name: String,
    pub gallery_kind: Option<String>,
    pub skill_directory: String,
    pub skill_name: String,
}

/// `None` is an absent key. `Some(None)` is JSON null, which upstream rejects.
#[derive(Debug, Clone, Default)]
pub struct CodexArtifactTemplateAttributes<'a> {
    pub artifact_kind: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub gallery_kind: Option<Option<&'a str>>,
    pub skill_directory: Option<&'a str>,
    pub skill_name: Option<&'a str>,
}

fn is_kind(value: &str) -> bool {
    CODEX_ARTIFACT_TEMPLATE_KINDS.contains(&value)
}

fn is_gallery_kind(value: &str) -> bool {
    CODEX_ARTIFACT_TEMPLATE_GALLERY_KINDS.contains(&value)
}

fn is_absolute_skill_directory(value: &str) -> bool {
    static DRIVE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^[A-Za-z]:[\\/]").expect("drive"));
    static UNC: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:\\\\[^\\]+\\[^\\]+|//[^/]+/[^/]+)").expect("unc")
    });
    (value.starts_with('/') && !value.starts_with("//"))
        || DRIVE.is_match(value)
        || UNC.is_match(value)
}

/// Mirrors the Codex result-card schema so malformed directives remain literal Markdown.
pub fn resolve_codex_artifact_template(
    attributes: Option<&CodexArtifactTemplateAttributes<'_>>,
) -> Option<CodexArtifactTemplate> {
    let attributes = attributes?;
    let artifact_kind = attributes.artifact_kind?;
    let display_name = attributes
        .display_name
        .map(js_trim)
        .filter(|value| !value.is_empty())?;
    let skill_directory = attributes.skill_directory?;
    let skill_name = attributes.skill_name?;
    let gallery_kind = match attributes.gallery_kind {
        None => None,
        Some(None) => return None,
        Some(Some(kind)) if is_gallery_kind(kind) => Some(kind.to_string()),
        Some(Some(_)) => return None,
    };
    if !is_kind(artifact_kind)
        || !is_absolute_skill_directory(skill_directory)
        || !skill_name.starts_with("artifact-template-")
    {
        return None;
    }
    Some(CodexArtifactTemplate {
        artifact_kind: artifact_kind.to_string(),
        display_name: display_name.to_string(),
        gallery_kind,
        skill_directory: skill_directory.to_string(),
        skill_name: skill_name.to_string(),
    })
}

pub fn codex_artifact_template_use_prompt(template: &CodexArtifactTemplate) -> String {
    let skill = format!("${}", template.skill_name);
    match template.artifact_kind.as_str() {
        "document" => format!("Create a document using this {skill} about…"),
        "presentation" => format!("Create a presentation using the {skill} template about…"),
        "spreadsheet" => format!("Create a spreadsheet using this {skill} about…"),
        "site" => format!("Create a Site using this {skill} about…"),
        "google-docs" => format!("Create a Google Doc using this {skill} about…"),
        "google-slides" => format!("Create a Google Slides presentation using this {skill} about…"),
        "google-sheets" => format!("Create a Google Sheet using this {skill} about…"),
        "image" => format!("Create an image using this {skill} of…"),
        "email" => format!("Draft an email using this {skill} about…"),
        "slack" => format!("Draft a Slack message using this {skill} about…"),
        _ => String::new(),
    }
}

pub fn codex_artifact_template_presentation_label(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "document" => "Document template",
        "presentation" => "Presentation template",
        "spreadsheet" => "Spreadsheet template",
        "site" => "Site template",
        "google-docs" => "Google Doc template",
        "google-slides" => "Google Slides template",
        "google-sheets" => "Google Sheet template",
        "image" => "Image template",
        "email" => "Email template",
        "slack" => "Slack template",
        _ => return None,
    })
}

pub fn append_codex_artifact_template_use_prompt(
    draft: &str,
    template: &CodexArtifactTemplate,
) -> String {
    let prompt = codex_artifact_template_use_prompt(template);
    let trimmed = draft.trim_end();
    let already_ends = trimmed.strip_suffix(&prompt).is_some_and(|prefix| {
        prefix.is_empty() || prefix.chars().next_back().is_some_and(char::is_whitespace)
    });
    if already_ends {
        return draft.to_string();
    }
    let needs_space =
        !draft.is_empty() && !draft.chars().next_back().is_some_and(char::is_whitespace);
    if needs_space {
        format!("{draft} {prompt}")
    } else {
        format!("{draft}{prompt}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> CodexArtifactTemplate {
        CodexArtifactTemplate {
            artifact_kind: "document".into(),
            display_name: "Hello World".into(),
            gallery_kind: None,
            skill_directory: "/Users/test/.codex/skills/artifact-template-hello-world".into(),
            skill_name: "artifact-template-hello-world".into(),
        }
    }

    #[test]
    fn shares_labels() {
        assert_eq!(
            codex_artifact_template_presentation_label("document"),
            Some("Document template")
        );
    }

    #[test]
    fn accepts_template_metadata() {
        let resolved = resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
            artifact_kind: Some("document"),
            display_name: Some("  Hello World  "),
            gallery_kind: None,
            skill_directory: Some("/Users/test/.codex/skills/artifact-template-hello-world"),
            skill_name: Some("artifact-template-hello-world"),
        }))
        .expect("template");
        assert_eq!(resolved, hello());
    }

    #[test]
    fn accepts_absolute_windows_skill_directories() {
        for directory in [
            r"C:\Users\test\.codex\skills\artifact-template-hello-world",
            r"\\server\share\artifact-template-hello-world",
            "//server/share/artifact-template-hello-world",
        ] {
            let resolved =
                resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                    artifact_kind: Some("image"),
                    display_name: Some("Reference image"),
                    gallery_kind: Some(Some("product-design")),
                    skill_directory: Some(directory),
                    skill_name: Some("artifact-template-reference-image"),
                }))
                .expect(directory);
            assert_eq!(resolved.artifact_kind, "image");
            assert_eq!(resolved.gallery_kind.as_deref(), Some("product-design"));
            assert_eq!(resolved.skill_directory, directory);
        }
    }

    #[test]
    fn rejects_malformed_template_metadata() {
        let base = CodexArtifactTemplateAttributes {
            artifact_kind: Some("document"),
            display_name: Some("Hello World"),
            gallery_kind: None,
            skill_directory: Some("/templates/hello-world"),
            skill_name: Some("artifact-template-hello-world"),
        };
        assert!(
            resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                artifact_kind: Some("unknown"),
                ..base.clone()
            }))
            .is_none()
        );
        assert!(
            resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                display_name: Some(" "),
                ..base.clone()
            }))
            .is_none()
        );
        assert!(
            resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                skill_directory: Some("relative/template"),
                ..base.clone()
            }))
            .is_none()
        );
        assert!(
            resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                skill_name: Some("hello-world"),
                ..base.clone()
            }))
            .is_none()
        );
        assert!(
            resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                gallery_kind: Some(None),
                ..base.clone()
            }))
            .is_none()
        );
        assert!(
            resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
                gallery_kind: Some(Some("unknown")),
                ..base
            }))
            .is_none()
        );
    }

    #[test]
    fn builds_use_prompts() {
        let template = hello();
        assert_eq!(
            codex_artifact_template_use_prompt(&template),
            "Create a document using this $artifact-template-hello-world about…"
        );
        let mut image = template.clone();
        image.artifact_kind = "image".into();
        assert_eq!(
            codex_artifact_template_use_prompt(&image),
            "Create an image using this $artifact-template-hello-world of…"
        );
    }

    #[test]
    fn appends_the_use_prompt_once() {
        let template = hello();
        let prompt = "Create a document using this $artifact-template-hello-world about…";
        assert_eq!(
            append_codex_artifact_template_use_prompt("", &template),
            prompt
        );
        assert_eq!(
            append_codex_artifact_template_use_prompt("Write about otters", &template),
            format!("Write about otters {prompt}")
        );
        assert_eq!(
            append_codex_artifact_template_use_prompt(prompt, &template),
            prompt
        );
        assert_eq!(
            append_codex_artifact_template_use_prompt(&format!("{prompt}\n"), &template),
            format!("{prompt}\n")
        );
        let notes = format!("Notes\n\n{prompt}");
        assert_eq!(
            append_codex_artifact_template_use_prompt(&notes, &template),
            notes
        );
        let draft = format!("{prompt}\nAdditional instructions");
        assert_eq!(
            append_codex_artifact_template_use_prompt(&draft, &template),
            format!("{draft} {prompt}")
        );
    }
}
