//! Port of upstream/apps/web/src/sourceControlPresentation.ts

#![allow(dead_code)]

use crate::state::helpers::source_control_clients::{
    ChangeRequestTerminology, SOURCE_CONTROL_CLIENTS, SourceControlClientDefinition,
};
use crate::ui::IconName;
use turin_contracts::source_control::SourceControlProviderInfo;

#[allow(unused_imports)]
pub use crate::state::helpers::source_control_clients::ChangeRequestTerminology as ChangeRequestTerminologyExport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceControlIcon {
    Lucide(IconName),
    PullRequestGlyph,
}

pub struct SourceControlPresentation {
    pub definition: &'static SourceControlClientDefinition,
    pub provider_name: String,
    pub terminology: &'static ChangeRequestTerminology,
    pub icon: SourceControlIcon,
}

fn source_control_icon(definition: &SourceControlClientDefinition) -> SourceControlIcon {
    match definition.icon {
        "github" => SourceControlIcon::Lucide(IconName::Github),
        "gitlab" => SourceControlIcon::Lucide(IconName::GitPullRequest),
        "forgejo" => SourceControlIcon::Lucide(IconName::GitFork),
        "bitbucket" => SourceControlIcon::Lucide(IconName::GitPullRequest),
        "azure-devops" => SourceControlIcon::Lucide(IconName::GitPullRequest),
        "gitcafe" => SourceControlIcon::Lucide(IconName::GitPullRequest),
        _ => SourceControlIcon::PullRequestGlyph,
    }
}

fn present_definition(
    definition: &'static SourceControlClientDefinition,
    provider_name: String,
) -> SourceControlPresentation {
    SourceControlPresentation {
        definition,
        provider_name,
        terminology: &definition.change_request,
        icon: source_control_icon(definition),
    }
}

pub fn get_source_control_presentation(
    provider: Option<&SourceControlProviderInfo>,
) -> SourceControlPresentation {
    let definition = SOURCE_CONTROL_CLIENTS.get(provider.map(|info| info.kind.as_str()));
    let name = provider
        .map(|info| info.name.as_str().to_owned())
        .unwrap_or_else(|| definition.label.to_owned());
    present_definition(definition, name)
}

pub fn get_source_control_presentation_for_kind(kind: &str) -> SourceControlPresentation {
    let definition = SOURCE_CONTROL_CLIENTS.get(Some(kind));
    present_definition(definition, definition.label.to_owned())
}
