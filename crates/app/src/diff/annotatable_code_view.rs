//! Pure helpers from upstream/apps/web/src/components/diffs/AnnotatableCodeView.tsx
//! (annotation grouping; rendering stays in `panel_view.rs` / `rows.rs`).

use super::panel::annotation_side;
use super::pierre::types::{DiffLineAnnotation, SelectedLineRange};

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
}
