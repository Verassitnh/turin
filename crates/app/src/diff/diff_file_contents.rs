//! Port of upstream/apps/web/src/lib/diffFileContents.ts

use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;
use turin_contracts::review::ReviewDiffPreviewSourceKind;

use super::diff_rendering::resolve_file_diff_path;
use super::pierre::types::{ChangeType, FileContents, FileDiffMetadata};
use super::view_model::{DiffFileContents, DiffSource, GitSourceKind};

/// Git comparison identity for Pierre's hydrated render cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffFileContentsSource {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub source_kind: ReviewDiffPreviewSourceKind,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub cache_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestDiffFileContentsSource {
    pub environment_id: EnvironmentId,
    pub reference: PullRequestRef,
    pub commit: Option<String>,
    pub cache_key: String,
}

/// `(oldFile, newFile)` for hunk expansion (`FileDiffContentsLoader`).
pub fn load_diff_file_contents(
    source: &dyn DiffSource,
    kind: GitSourceKind,
    base_ref: Option<&str>,
    head_ref: Option<&str>,
    cache_key: &str,
    file: &FileDiffMetadata,
) -> Result<(Option<FileContents>, FileContents), String> {
    let new_path = resolve_file_diff_path(file);
    let old_path = file
        .prev_name
        .clone()
        .map(|name| {
            resolve_file_diff_path(&FileDiffMetadata {
                name,
                ..file.clone()
            })
        })
        .unwrap_or_else(|| new_path.clone());
    let DiffFileContents {
        old_contents,
        new_contents,
    } = source.diff_file_contents(kind, base_ref, head_ref, &old_path, &new_path, file.r#type)?;
    let new_file = FileContents {
        name: new_path.clone(),
        contents: new_contents,
        cache_key: Some(format!("{cache_key}:new:{new_path}")),
    };
    if file.r#type == ChangeType::RenamePure {
        return Ok((None, new_file));
    }
    Ok((
        Some(FileContents {
            name: old_path.clone(),
            contents: old_contents,
            cache_key: Some(format!("{cache_key}:old:{old_path}")),
        }),
        new_file,
    ))
}
