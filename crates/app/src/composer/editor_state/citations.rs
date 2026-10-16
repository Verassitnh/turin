//! Citation chips' comment state (ComposerPromptEditorTiptap.tsx:
//! `openCitation`, `citationDraftRef`, `citationRequestRef`,
//! `requestCitationComment`, `saveCitationDraft`, and the node view's
//! `onSaveComment` / `onRemove` / `onRestoreFocus`).
//!
//! The source anchor (the DOM range the popover points at in the timeline) is
//! not carried: Turin anchors the comment editor to the chip.

use turin_contracts::assistant_citations::AssistantCitation;
use turin_shared::assistant_citations::{
    serialize_assistant_citation, with_assistant_citation_comment,
};

use super::*;
use crate::composer::citation_comment_dismissal::{
    AssistantCitationCommentDismissal, resolve_assistant_citation_comment_dismissal,
};
use crate::composer::rich_doc::RunKind;

/// Spaces added around a freshly inserted citation, removed with it if its
/// comment is cancelled (`CitationInsertedSpaces`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CitationInsertedSpaces {
    pub before: bool,
    pub after: bool,
}

/// `ComposerCitationCommentRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationCommentRequest {
    pub previous_value: String,
    pub value: String,
    /// Markdown offset (UTF-16) of the citation's source in `value`.
    pub citation_start: usize,
    pub inserted_spaces: CitationInsertedSpaces,
}

/// `OpenCitationComment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCitationComment {
    pub key: String,
    pub remove_on_cancel: Option<CitationInsertedSpaces>,
}

/// `onCitationDraftSave(replacedValue, savedValue)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationDraftSave {
    pub replaced_value: String,
    pub saved_value: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CitationComments {
    open: Option<OpenCitationComment>,
    /// The open citation's unsaved comment, saved before a new value replaces
    /// the document.
    draft: Option<(String, String)>,
    request: Option<CitationCommentRequest>,
}

impl EditorState {
    /// Every citation chip with its position, in document order.
    pub fn citations(&self) -> Vec<(DocPos, String, AssistantCitation)> {
        let mut found = Vec::new();
        for (index, block) in self.doc.blocks.iter().enumerate() {
            let Some(inline) = block.inline() else {
                continue;
            };
            let mut offset = 0usize;
            for node in inline {
                if let InlineContent::Atom(Atom::Citation {
                    citation, cite_key, ..
                }) = &node.content
                {
                    found.push((
                        DocPos::new(index, offset),
                        cite_key.clone(),
                        citation.as_ref().clone(),
                    ));
                }
                offset += node.doc_len();
            }
        }
        found
    }

    fn citation_by_key(&self, key: &str) -> Option<(DocPos, AssistantCitation)> {
        self.citations()
            .into_iter()
            .find(|(_, cite_key, _)| cite_key == key)
            .map(|(pos, _, citation)| (pos, citation))
    }

    /// The citation whose source starts at markdown offset `md_start`
    /// (`run.kind === "token" && run.nodeName === "composer-citation"`).
    fn citation_key_at_markdown(&mut self, md_start: usize) -> Option<String> {
        let run = self
            .map()
            .runs
            .iter()
            .find(|run| run.kind == RunKind::Token && run.md_start == md_start)
            .cloned()?;
        let block = run.block?;
        let pos = DocPos::new(block, run.block_offset);
        self.citations()
            .into_iter()
            .find(|(at, _, _)| *at == pos)
            .map(|(_, key, _)| key)
    }

    /// The citation whose comment editor is open.
    pub fn open_citation_comment(&self) -> Option<&OpenCitationComment> {
        self.citation_comments.open.as_ref()
    }

    /// The open citation's position and data.
    pub fn open_citation(&self) -> Option<(DocPos, AssistantCitation)> {
        let key = self.citation_comments.open.as_ref()?.key.clone();
        self.citation_by_key(&key)
    }

    /// `requestCitationComment`: open the comment editor of the citation just
    /// inserted at `citation_start`, now or once the document holds
    /// `request.value`.
    pub fn request_citation_comment(&mut self, request: CitationCommentRequest) {
        let value = request.value.clone();
        let start = request.citation_start;
        let spaces = request.inserted_spaces;
        self.citation_comments.request = Some(request);
        if self.value() != value {
            return;
        }
        if let Some(key) = self.citation_key_at_markdown(start) {
            self.citation_comments.request = None;
            self.citation_comments.open = Some(OpenCitationComment {
                key,
                remove_on_cancel: Some(spaces),
            });
        }
    }

    /// `onOpenChange(citeKey, open)`.
    pub fn set_citation_comment_open(&mut self, key: &str, open: bool) {
        let comments = &mut self.citation_comments;
        if !open && comments.draft.as_ref().is_some_and(|(k, _)| k == key) {
            comments.draft = None;
        }
        if open {
            if comments.open.as_ref().is_some_and(|o| o.key == key) {
                return;
            }
            comments.open = Some(OpenCitationComment {
                key: key.to_owned(),
                remove_on_cancel: None,
            });
        } else if comments.open.as_ref().is_some_and(|o| o.key == key) {
            comments.open = None;
        }
    }

    /// `onDraftChange(citeKey, comment)`.
    pub fn set_citation_comment_draft(&mut self, key: &str, comment: &str) {
        self.citation_comments.draft = Some((key.to_owned(), comment.to_owned()));
    }

    /// Rewrite the citation's comment and source in place.
    fn write_citation_comment(&mut self, pos: DocPos, comment: &str) -> bool {
        let Some(inline) = self.doc.blocks[pos.block].inline_mut() else {
            return false;
        };
        let mut cells = to_cells(inline);
        let index = cell_index(&cells, pos.offset);
        let Some(Cell {
            kind:
                CellKind::Atom(Atom::Citation {
                    citation, source, ..
                }),
            ..
        }) = cells.get_mut(index)
        else {
            return false;
        };
        let next = with_assistant_citation_comment(citation, comment);
        *source = serialize_assistant_citation(&next);
        **citation = next;
        *inline = from_cells(&cells);
        true
    }

    /// `onSaveComment`: an undoable edit of the chip.
    pub fn save_citation_comment(&mut self, key: &str, comment: &str) -> bool {
        let Some((pos, _)) = self.citation_by_key(key) else {
            return false;
        };
        let before = self.entry();
        self.begin_change(ChangeKind::Other);
        if !self.write_citation_comment(pos, comment) {
            self.abort_change(&before, ChangeKind::Other);
            return false;
        }
        self.finish_change();
        true
    }

    /// `onRemove`: cancelling a fresh insertion drops the chip and the spaces
    /// inserted with it. Only while the comment was opened by that insertion.
    pub fn remove_citation_on_cancel(&mut self, key: &str) -> bool {
        let Some(spaces) = self
            .citation_comments
            .open
            .as_ref()
            .filter(|open| open.key == key)
            .and_then(|open| open.remove_on_cancel)
        else {
            return false;
        };
        let Some((pos, _)) = self.citation_by_key(key) else {
            return false;
        };
        let line: Vec<Cell> = self.doc.blocks[pos.block]
            .inline()
            .map(|inline| to_cells(inline))
            .unwrap_or_default();
        let index = cell_index(&line, pos.offset);
        let is_space = |cell: Option<&Cell>| {
            matches!(
                cell,
                Some(Cell {
                    kind: CellKind::Char(' '),
                    ..
                })
            )
        };
        let from = if spaces.before && index > 0 && is_space(line.get(index - 1)) {
            pos.offset - 1
        } else {
            pos.offset
        };
        let to = if spaces.after && is_space(line.get(index + 1)) {
            pos.offset + 2
        } else {
            pos.offset + 1
        };
        self.begin_change(ChangeKind::Other);
        let at = self.delete_range_raw(DocPos::new(pos.block, from), DocPos::new(pos.block, to));
        self.selection = Selection::caret(at);
        self.finish_change();
        self.citation_comments.open = None;
        self.citation_comments.draft = None;
        true
    }

    /// `onRestoreFocus`: the caret right after the chip, so Enter sends and
    /// typing continues the prompt.
    pub fn caret_after_citation(&mut self, key: &str) -> bool {
        let Some((pos, _)) = self.citation_by_key(key) else {
            return false;
        };
        self.set_caret(DocPos::new(pos.block, pos.offset + 1));
        true
    }

    /// `saveCitationDraft`: a new value is about to replace the document while
    /// a comment is unsaved. Saves it the way dismissing the popover would,
    /// outside the undo history, and reports the saved value.
    fn save_citation_draft(&mut self, replaced_value: &str) -> Option<CitationDraftSave> {
        let (key, comment) = self.citation_comments.draft.take()?;
        let (pos, citation) = self.citation_by_key(&key)?;
        let dismissal = resolve_assistant_citation_comment_dismissal(
            "none",
            Some(&comment),
            citation.comment.as_deref(),
        );
        let AssistantCitationCommentDismissal::Commit { comment } = dismissal else {
            return None;
        };
        // `addToHistory: false`.
        if !self.write_citation_comment(pos, &comment) {
            return None;
        }
        self.map_cache = None;
        Some(CitationDraftSave {
            replaced_value: replaced_value.to_owned(),
            saved_value: self.value(),
        })
    }

    /// The controlled value/cursor effect: a value from the store (history
    /// recall, chip insertion, a question taking the composer) replaces the
    /// document. An unsaved citation comment is saved first and reported, and
    /// a pending `requestCitationComment` opens once its value lands.
    pub fn apply_controlled_value(
        &mut self,
        value: &str,
        collapsed_cursor: usize,
        skill_label_for: &dyn Fn(&str) -> SkillMeta,
    ) -> Option<CitationDraftSave> {
        let previous = self.value();
        let pending = self
            .citation_comments
            .request
            .clone()
            .filter(|request| request.value == value);
        let mut saved = None;
        if previous != value {
            saved = self.save_citation_draft(&previous);
            // The old document's chips are gone, and their comment editor with them.
            self.citation_comments.open = None;
        }
        self.set_value(value, Some(collapsed_cursor), skill_label_for);
        if let Some(request) = pending {
            self.citation_comments.request = None;
            if let Some(key) = self.citation_key_at_markdown(request.citation_start) {
                self.citation_comments.open = Some(OpenCitationComment {
                    key,
                    remove_on_cancel: Some(request.inserted_spaces),
                });
            }
        }
        saved
    }
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/web/src/components/ComposerPromptEditorTiptap.citationDraft.test.tsx,
    //! plus the comment actions of `ComposerCitationNodeView`.

    use super::*;
    use turin_shared::assistant_citations::serialize_assistant_citation;

    fn citation() -> AssistantCitation {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "environmentId": "environment",
            "threadId": "thread",
            "messageId": "source",
            "text": "hello",
            "start": 0,
            "end": 5,
            "prefix": "",
            "suffix": "",
        }))
        .expect("citation")
    }

    fn no_skills(name: &str) -> SkillMeta {
        SkillMeta {
            label: name.to_owned(),
            description: None,
        }
    }

    fn prompt_before() -> String {
        format!("See {} ", serialize_assistant_citation(&citation()))
    }

    fn request(value: &str, spaces: CitationInsertedSpaces) -> CitationCommentRequest {
        CitationCommentRequest {
            previous_value: "See ".to_owned(),
            value: value.to_owned(),
            citation_start: "See ".len(),
            inserted_spaces: spaces,
        }
    }

    /// Mirrors ChatComposer: a pending question shows its answer in the same
    /// editor, and `onCitationDraftSave` writes back into the prompt it replaced.
    struct Composer {
        editor: EditorState,
        prompt: String,
        answer: String,
    }

    impl Composer {
        fn new() -> Self {
            let prompt = prompt_before();
            let mut editor = EditorState::from_value(&prompt, &no_skills, false, false);
            editor.set_collapsed_cursor(0);
            Self {
                editor,
                prompt,
                answer: String::new(),
            }
        }

        fn render(&mut self, question: bool) {
            let value = if question {
                self.answer.clone()
            } else {
                self.prompt.clone()
            };
            if let Some(save) = self.editor.apply_controlled_value(&value, 0, &no_skills)
                && save.replaced_value == self.prompt
            {
                self.prompt = save.saved_value;
            }
        }
    }

    #[test]
    fn keeps_an_unsaved_citation_comment_when_a_question_takes_the_composer() {
        let mut composer = Composer::new();
        composer.editor.request_citation_comment(request(
            &prompt_before(),
            CitationInsertedSpaces {
                before: false,
                after: true,
            },
        ));
        let open = composer
            .editor
            .open_citation_comment()
            .cloned()
            .expect("Comment editor did not open");
        composer
            .editor
            .set_citation_comment_draft(&open.key, "check this");

        composer.render(true);
        assert_eq!(composer.answer, "");
        composer.render(false);

        assert_eq!(
            composer.prompt,
            format!(
                "See {} ",
                serialize_assistant_citation(&with_assistant_citation_comment(
                    &citation(),
                    "check this"
                ))
            )
        );
        // The chip now offers "Edit citation comment".
        let (_, _, saved) = composer.editor.citations().remove(0);
        assert_eq!(saved.comment.as_deref(), Some("check this"));
    }

    #[test]
    fn a_request_waits_for_its_value_to_land() {
        let mut editor = EditorState::from_value("See ", &no_skills, false, false);
        editor
            .request_citation_comment(request(&prompt_before(), CitationInsertedSpaces::default()));
        assert!(editor.open_citation_comment().is_none());
        let cursor =
            crate::composer::logic::clamp_collapsed_composer_cursor(&prompt_before(), 99, false);
        editor.apply_controlled_value(&prompt_before(), cursor, &no_skills);
        assert!(editor.open_citation_comment().is_some());
    }

    #[test]
    fn saving_a_comment_rewrites_the_source_and_is_undoable() {
        let mut composer = Composer::new();
        let (_, key, _) = composer.editor.citations().remove(0);
        assert!(composer.editor.save_citation_comment(&key, "  note  "));
        assert_eq!(
            composer.editor.value(),
            format!(
                "See {} ",
                serialize_assistant_citation(&with_assistant_citation_comment(&citation(), "note"))
            )
        );
        assert!(composer.editor.undo());
        assert_eq!(composer.editor.value(), prompt_before());
    }

    #[test]
    fn cancelling_a_fresh_insertion_removes_the_chip_and_its_spaces() {
        let mut composer = Composer::new();
        composer.editor.request_citation_comment(request(
            &prompt_before(),
            CitationInsertedSpaces {
                before: false,
                after: true,
            },
        ));
        let key = composer.editor.open_citation_comment().unwrap().key.clone();
        assert!(composer.editor.remove_citation_on_cancel(&key));
        assert_eq!(composer.editor.value(), "See ");
        assert!(composer.editor.open_citation_comment().is_none());
    }

    #[test]
    fn cancelling_a_comment_opened_by_hand_keeps_the_chip() {
        let mut composer = Composer::new();
        let (_, key, _) = composer.editor.citations().remove(0);
        composer.editor.set_citation_comment_open(&key, true);
        assert!(!composer.editor.remove_citation_on_cancel(&key));
        assert_eq!(composer.editor.value(), prompt_before());
    }

    #[test]
    fn restoring_focus_puts_the_caret_after_the_chip() {
        let mut composer = Composer::new();
        let (pos, key, _) = composer.editor.citations().remove(0);
        assert!(composer.editor.caret_after_citation(&key));
        assert_eq!(
            composer.editor.selection().head,
            DocPos::new(pos.block, pos.offset + 1)
        );
    }

    #[test]
    fn closing_drops_the_unsaved_draft() {
        let mut composer = Composer::new();
        let (_, key, _) = composer.editor.citations().remove(0);
        composer.editor.set_citation_comment_open(&key, true);
        composer.editor.set_citation_comment_draft(&key, "draft");
        composer.editor.set_citation_comment_open(&key, false);
        composer.render(true);
        composer.render(false);
        assert_eq!(composer.prompt, prompt_before());
    }
}
