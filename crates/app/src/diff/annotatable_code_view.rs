//! Pure helpers from upstream/apps/web/src/components/diffs/AnnotatableCodeView.tsx
//! (annotation grouping; rendering stays in `panel_view.rs` / `rows.rs`).
//!
//! Port of upstream/apps/web/src/components/diffs/AnnotatableCodeView.tsx (pure helpers).

use super::diff_rendering::fnv1a32;
use super::pierre::types::{
    DiffLineAnnotation, FileDiffMetadata, SelectedLineRange, SelectionSide,
};
use super::review_comment_context::{ReviewCommentContext, restore_diff_review_comment_range};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffCommentAnnotationEntry {
    pub id: String,
    pub kind: DiffCommentAnnotationKind,
    pub range: SelectedLineRange,
    pub range_label: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffCommentAnnotationKind {
    Draft,
    Comment,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffCommentAnnotationGroup {
    pub entries: Vec<DiffCommentAnnotationEntry>,
}

pub type DiffCommentLineAnnotation = DiffLineAnnotation<DiffCommentAnnotationGroup>;

/// `annotationSide`.
pub fn annotation_side(range: &SelectedLineRange) -> SelectionSide {
    if range.end_side.or(range.side) == Some(SelectionSide::Deletions) {
        SelectionSide::Deletions
    } else {
        SelectionSide::Additions
    }
}

pub fn append_annotation_entry(
    annotations: &[DiffCommentLineAnnotation],
    range: SelectedLineRange,
    entry: DiffCommentAnnotationEntry,
) -> Vec<DiffCommentLineAnnotation> {
    let side = annotation_side(&range);
    if let Some(index) = annotations
        .iter()
        .position(|annotation| annotation.side == side && annotation.line_number == range.end)
    {
        return annotations
            .iter()
            .enumerate()
            .map(|(annotation_index, annotation)| {
                if annotation_index != index {
                    return annotation.clone();
                }
                let mut group = annotation.metadata.clone();
                group.entries.push(entry.clone());
                DiffLineAnnotation {
                    side: annotation.side,
                    line_number: annotation.line_number,
                    metadata: group,
                }
            })
            .collect();
    }
    let mut next = annotations.to_vec();
    next.push(DiffLineAnnotation {
        side,
        line_number: range.end,
        metadata: DiffCommentAnnotationGroup {
            entries: vec![entry],
        },
    });
    next
}

/// Review comments + optional draft annotation for one file (`AnnotatableCodeView` `items` reducer).
pub fn line_annotations_for_file(
    section_id: &str,
    file_path: &str,
    file: &FileDiffMetadata,
    comments: &[ReviewCommentContext],
    draft: Option<&DiffCommentLineAnnotation>,
) -> Vec<DiffCommentLineAnnotation> {
    let mut annotations = comments
        .iter()
        .filter(|comment| {
            comment.section_id == section_id
                && comment.file_path == file_path
                && comment.fence_language.as_deref().unwrap_or("diff") == "diff"
        })
        .fold(Vec::new(), |annotations, comment| {
            let Some(range) = restore_diff_review_comment_range(file, comment) else {
                return annotations;
            };
            append_annotation_entry(
                &annotations,
                range,
                DiffCommentAnnotationEntry {
                    id: comment.id.clone(),
                    kind: DiffCommentAnnotationKind::Comment,
                    range,
                    range_label: comment.range_label.clone(),
                    text: comment.text.clone(),
                },
            )
        });
    if let Some(draft) = draft {
        annotations.push(draft.clone());
    }
    annotations
}

/// Upstream `fnv1a32` cache-bust key over file version, collapse, and annotation text.
pub fn annotatable_item_version(
    file_version: u32,
    collapsed: bool,
    annotations: &[DiffCommentLineAnnotation],
) -> u32 {
    let signature = annotations
        .iter()
        .flat_map(|annotation| {
            annotation
                .metadata
                .entries
                .iter()
                .map(|entry| format!("{}:{}:{}", entry.id, entry.range_label, entry.text))
        })
        .collect::<Vec<_>>()
        .join(":");
    let payload = format!(
        "{file_version}:{}:{signature}",
        if collapsed { "1" } else { "0" }
    );
    fnv1a32(&payload, 0x811c_9dc5, 0x0100_0193)
}

#[cfg(test)]
mod tests {
    use super::super::pierre::types::SelectionSide;
    use super::*;

    fn range(end: usize, side: SelectionSide) -> SelectedLineRange {
        SelectedLineRange {
            start: end,
            side: Some(side),
            end,
            end_side: Some(side),
        }
    }

    fn entry(id: &str) -> DiffCommentAnnotationEntry {
        DiffCommentAnnotationEntry {
            id: id.into(),
            kind: DiffCommentAnnotationKind::Comment,
            range: range(3, SelectionSide::Additions),
            range_label: "L3".into(),
            text: "hi".into(),
        }
    }

    #[test]
    fn appends_new_line_annotation() {
        let range = range(3, SelectionSide::Additions);
        let annotations = append_annotation_entry(&[], range, entry("a"));
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].line_number, 3);
        assert_eq!(annotations[0].metadata.entries.len(), 1);
    }

    #[test]
    fn merges_entries_on_same_line() {
        let range = range(3, SelectionSide::Additions);
        let once = append_annotation_entry(&[], range, entry("a"));
        let twice = append_annotation_entry(&once, range, entry("b"));
        assert_eq!(twice.len(), 1);
        assert_eq!(twice[0].metadata.entries.len(), 2);
    }

    #[test]
    fn version_changes_when_annotation_text_changes() {
        let range = range(3, SelectionSide::Additions);
        let once = append_annotation_entry(&[], range, entry("a"));
        let twice = append_annotation_entry(&once, range, entry("b"));
        assert_ne!(
            annotatable_item_version(1, false, &once),
            annotatable_item_version(1, false, &twice)
        );
    }
}
