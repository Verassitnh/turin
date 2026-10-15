//! Port of the per-thread slice of upstream/apps/web/src/composerDraftStore.ts
//! that the composer uses: the prompt, image and file attachments, the model
//! selection per provider instance, runtime/interaction modes, and the stash.
//!
//! In memory only. Not ported: terminal contexts, preview annotations, review
//! comments and thread-context records (cut features or no producers yet),
//! draft-thread sessions (thread launch is replaced by hooks), localStorage
//! persistence (the persisted shape will be added with the settings task) and
//! sticky model selection.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use turin_contracts::chat_attachment::PROVIDER_SEND_TURN_MAX_ATTACHMENTS;

use crate::composer::provider_options::OptionSelection;

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerImageAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub data: Arc<Vec<u8>>,
}

impl ComposerImageAttachment {
    pub fn new(id: &str, name: &str, mime_type: &str, data: Vec<u8>) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            size_bytes: data.len() as u64,
            data: Arc::new(data),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerFileAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// `file !== null`. Only ever `None` after hydration.
    pub data: Option<Arc<Vec<u8>>>,
    pub uploaded_attachment_id: Option<String>,
    pub upload_environment_id: Option<String>,
}

impl ComposerFileAttachment {
    pub fn has_bytes(&self) -> bool {
        self.data.is_some()
    }

    /// `composerFileNeedsReattach`: neither bytes nor a server-side upload.
    pub fn needs_reattach(&self) -> bool {
        self.data.is_none() && self.uploaded_attachment_id.is_none()
    }
}

/// A model selection stored in a draft (`ModelSelection` with plain strings).
#[derive(Debug, Clone, PartialEq)]
pub struct DraftModelSelection {
    pub instance_id: String,
    pub model: String,
    pub options: Vec<OptionSelection>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposerThreadDraft {
    pub prompt: String,
    pub images: Vec<ComposerImageAttachment>,
    pub files: Vec<ComposerFileAttachment>,
    /// Per-instance model selection, keyed by provider instance id so a
    /// default `codex` instance and a custom one each keep their own model.
    pub model_selection_by_provider: IndexMap<String, DraftModelSelection>,
    /// Routing key of the last picked instance.
    pub active_provider: Option<String>,
    /// True only when a human picked the active selection in the composer.
    pub model_selection_explicit: bool,
    pub runtime_mode: Option<String>,
    pub interaction_mode: Option<String>,
}

impl ComposerThreadDraft {
    /// `shouldRemoveDraft`.
    fn is_empty(&self) -> bool {
        self.prompt.is_empty()
            && self.images.is_empty()
            && self.files.is_empty()
            && self.model_selection_by_provider.is_empty()
            && self.active_provider.is_none()
            && self.runtime_mode.is_none()
            && self.interaction_mode.is_none()
    }
}

/// `composerDraftHasUserContent`: typed text or any attachment. Model and mode
/// choices alone are ambient defaults, not work in progress.
pub fn composer_draft_has_user_content(draft: Option<&ComposerThreadDraft>) -> bool {
    draft.is_some_and(|draft| !draft.prompt.trim().is_empty() || !draft.images.is_empty() || !draft.files.is_empty())
}

fn image_dedup_key(image: &ComposerImageAttachment) -> String {
    format!("{}\0{}\0{}", image.mime_type, image.size_bytes, image.name)
}

/// `composerFileDedupKey`.
pub fn composer_file_dedup_key(file: &ComposerFileAttachment) -> String {
    format!("{}\0{}\0{}", file.mime_type, file.size_bytes, file.name)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AddAttachmentsOptions {
    pub allow_duplicates: bool,
}

/// A stashed prompt (`PromptStashEntry`).
#[derive(Debug, Clone, PartialEq)]
pub struct PromptStashEntry {
    pub id: String,
    pub created_at: String,
    pub prompt: String,
    pub images: Vec<ComposerImageAttachment>,
    pub files: Vec<ComposerFileAttachment>,
    pub dropped_image_names: Vec<String>,
    pub unreadable_image_names: Vec<String>,
    pub pending_image_count: usize,
}

pub const MAX_STASH_ENTRIES: usize = 20;

#[derive(Debug, Default)]
pub struct ComposerDraftStore {
    drafts: HashMap<String, ComposerThreadDraft>,
    stash: Vec<PromptStashEntry>,
}

impl ComposerDraftStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_composer_draft(&self, thread_key: &str) -> Option<&ComposerThreadDraft> {
        self.drafts.get(thread_key)
    }

    fn update(&mut self, thread_key: &str, edit: impl FnOnce(&mut ComposerThreadDraft)) {
        if thread_key.is_empty() {
            return;
        }
        let mut draft = self.drafts.remove(thread_key).unwrap_or_default();
        edit(&mut draft);
        if !draft.is_empty() {
            self.drafts.insert(thread_key.to_owned(), draft);
        }
    }

    pub fn set_prompt(&mut self, thread_key: &str, prompt: &str) {
        self.update(thread_key, |draft| draft.prompt = prompt.to_owned());
    }

    /// `setModelSelection`.
    pub fn set_model_selection(&mut self, thread_key: &str, selection: DraftModelSelection, explicit: bool, replace_options: bool) {
        self.update(thread_key, |draft| {
            let current_options = draft
                .model_selection_by_provider
                .get(&selection.instance_id)
                .map(|current| current.options.clone())
                .unwrap_or_default();
            let options = if !selection.options.is_empty() || replace_options {
                selection.options.clone()
            } else {
                current_options
            };
            draft.active_provider = Some(selection.instance_id.clone());
            draft.model_selection_by_provider.insert(
                selection.instance_id.clone(),
                DraftModelSelection { options, ..selection },
            );
            draft.model_selection_explicit = explicit;
        });
    }

    /// `setProviderModelOptions`: stores the options for one instance, keeping
    /// its model (or `fallback_model`).
    pub fn set_provider_model_options(
        &mut self,
        thread_key: &str,
        instance_id: &str,
        options: Option<Vec<OptionSelection>>,
        fallback_model: &str,
    ) {
        self.update(thread_key, |draft| {
            let model = draft
                .model_selection_by_provider
                .get(instance_id)
                .map_or_else(|| fallback_model.to_owned(), |current| current.model.clone());
            match options.filter(|options| !options.is_empty()) {
                Some(options) => {
                    draft.model_selection_by_provider.insert(
                        instance_id.to_owned(),
                        DraftModelSelection {
                            instance_id: instance_id.to_owned(),
                            model,
                            options,
                        },
                    );
                }
                None => {
                    if let Some(current) = draft.model_selection_by_provider.get_mut(instance_id) {
                        current.options.clear();
                    }
                }
            }
            draft.active_provider = Some(instance_id.to_owned());
            draft.model_selection_explicit = true;
        });
    }

    pub fn set_runtime_mode(&mut self, thread_key: &str, mode: Option<&str>) {
        let mode = mode.filter(|mode| ["approval-required", "auto-accept-edits", "full-access"].contains(mode));
        self.update(thread_key, |draft| draft.runtime_mode = mode.map(str::to_owned));
    }

    pub fn set_interaction_mode(&mut self, thread_key: &str, mode: Option<&str>) {
        let mode = mode.filter(|mode| *mode == "plan" || *mode == "default");
        self.update(thread_key, |draft| draft.interaction_mode = mode.map(str::to_owned));
    }

    /// `addImages`: deduplicated, capped at the provider's attachment limit.
    /// Returns the ids that were accepted.
    pub fn add_images(&mut self, thread_key: &str, images: Vec<ComposerImageAttachment>, options: AddAttachmentsOptions) -> Vec<String> {
        let mut accepted = Vec::new();
        self.update(thread_key, |draft| {
            let mut ids: std::collections::HashSet<String> = draft.images.iter().map(|i| i.id.clone()).collect();
            let mut keys: std::collections::HashSet<String> = draft.images.iter().map(image_dedup_key).collect();
            for image in images {
                let key = image_dedup_key(&image);
                if ids.contains(&image.id) || (!options.allow_duplicates && keys.contains(&key)) {
                    continue;
                }
                if draft.images.len() + draft.files.len() >= PROVIDER_SEND_TURN_MAX_ATTACHMENTS {
                    continue;
                }
                ids.insert(image.id.clone());
                keys.insert(key);
                accepted.push(image.id.clone());
                draft.images.push(image);
            }
        });
        accepted
    }

    pub fn remove_image(&mut self, thread_key: &str, image_id: &str) {
        if !self.drafts.contains_key(thread_key) {
            return;
        }
        self.update(thread_key, |draft| draft.images.retain(|image| image.id != image_id));
    }

    /// `addFiles`: same rules as images; returns the accepted ids.
    pub fn add_files(&mut self, thread_key: &str, files: Vec<ComposerFileAttachment>, options: AddAttachmentsOptions) -> Vec<String> {
        let mut accepted = Vec::new();
        self.update(thread_key, |draft| {
            let mut ids: std::collections::HashSet<String> = draft.files.iter().map(|f| f.id.clone()).collect();
            let mut keys: std::collections::HashSet<String> = draft.files.iter().map(composer_file_dedup_key).collect();
            for file in files {
                let key = composer_file_dedup_key(&file);
                if ids.contains(&file.id) || (!options.allow_duplicates && keys.contains(&key)) {
                    continue;
                }
                if draft.images.len() + draft.files.len() >= PROVIDER_SEND_TURN_MAX_ATTACHMENTS {
                    continue;
                }
                ids.insert(file.id.clone());
                keys.insert(key);
                accepted.push(file.id.clone());
                draft.files.push(file);
            }
        });
        accepted
    }

    pub fn remove_file(&mut self, thread_key: &str, file_id: &str) {
        if !self.drafts.contains_key(thread_key) {
            return;
        }
        self.update(thread_key, |draft| draft.files.retain(|file| file.id != file_id));
    }

    /// `clearComposerContent`: prompt and attachments go, choices stay.
    pub fn clear_composer_content(&mut self, thread_key: &str) {
        if !self.drafts.contains_key(thread_key) {
            return;
        }
        self.update(thread_key, |draft| {
            draft.prompt.clear();
            draft.images.clear();
            draft.files.clear();
        });
    }

    /// `moveComposerPromptAndImages`: the prompt and what fits of the
    /// attachments move to another thread's draft.
    pub fn move_composer_prompt_and_images(&mut self, from: &str, to: &str) {
        if from.is_empty() || to.is_empty() || from == to {
            return;
        }
        let Some(source) = self.drafts.get(from).cloned() else {
            return;
        };
        let destination = self.drafts.get(to).cloned().unwrap_or_default();
        let destination_file_ids: std::collections::HashSet<_> = destination.files.iter().map(|f| f.id.clone()).collect();
        let destination_file_keys: std::collections::HashSet<_> = destination.files.iter().map(composer_file_dedup_key).collect();
        let transferable: Vec<ComposerFileAttachment> = source
            .files
            .iter()
            .filter(|file| {
                file.has_bytes()
                    && !destination_file_ids.contains(&file.id)
                    && !destination_file_keys.contains(&composer_file_dedup_key(file))
            })
            .cloned()
            .collect();
        let slots = PROVIDER_SEND_TURN_MAX_ATTACHMENTS.saturating_sub(destination.images.len() + destination.files.len());
        let moved_images: Vec<_> = source.images.iter().take(slots).cloned().collect();
        let moved_files: Vec<_> = transferable.into_iter().take(slots - moved_images.len()).collect();
        let moved_image_ids: std::collections::HashSet<_> = moved_images.iter().map(|i| i.id.clone()).collect();
        let moved_file_ids: std::collections::HashSet<_> = moved_files.iter().map(|f| f.id.clone()).collect();
        let mut next_destination = destination;
        next_destination.prompt = source.prompt.clone();
        next_destination.images.extend(moved_images);
        next_destination.files.extend(moved_files);
        let mut next_source = source;
        next_source.prompt.clear();
        next_source.images.retain(|image| !moved_image_ids.contains(&image.id));
        next_source.files.retain(|file| !moved_file_ids.contains(&file.id));
        for (key, draft) in [(to, next_destination), (from, next_source)] {
            self.drafts.remove(key);
            if !draft.is_empty() {
                self.drafts.insert(key.to_owned(), draft);
            }
        }
    }

    // ── Stash ──────────────────────────────────────────────────────────────

    pub fn stash_entries(&self) -> &[PromptStashEntry] {
        &self.stash
    }

    /// Newest first, capped at [`MAX_STASH_ENTRIES`].
    pub fn push_stash_entry(&mut self, entry: PromptStashEntry) {
        self.stash.insert(0, entry);
        self.stash.truncate(MAX_STASH_ENTRIES);
    }

    pub fn remove_stash_entry(&mut self, id: &str) -> Option<PromptStashEntry> {
        let index = self.stash.iter().position(|entry| entry.id == id)?;
        Some(self.stash.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: &str, name: &str) -> ComposerImageAttachment {
        ComposerImageAttachment::new(id, name, "image/png", vec![1, 2, 3])
    }

    fn file(id: &str, name: &str, bytes: bool) -> ComposerFileAttachment {
        ComposerFileAttachment {
            id: id.into(),
            name: name.into(),
            mime_type: "text/plain".into(),
            size_bytes: 4,
            data: bytes.then(|| Arc::new(vec![1])),
            uploaded_attachment_id: None,
            upload_environment_id: None,
        }
    }

    #[test]
    fn drops_a_draft_that_holds_nothing() {
        let mut store = ComposerDraftStore::new();
        store.set_prompt("t", "hello");
        assert!(store.get_composer_draft("t").is_some());
        store.set_prompt("t", "");
        assert!(store.get_composer_draft("t").is_none());
        store.set_interaction_mode("t", Some("plan"));
        assert!(store.get_composer_draft("t").is_some());
        store.set_interaction_mode("t", Some("bogus"));
        assert!(store.get_composer_draft("t").is_none());
        store.set_prompt("", "ignored");
        assert!(store.get_composer_draft("").is_none());
    }

    #[test]
    fn user_content_ignores_ambient_choices() {
        let mut store = ComposerDraftStore::new();
        store.set_runtime_mode("t", Some("full-access"));
        assert!(!composer_draft_has_user_content(store.get_composer_draft("t")));
        store.set_prompt("t", "  ");
        assert!(!composer_draft_has_user_content(store.get_composer_draft("t")));
        store.set_prompt("t", "hi");
        assert!(composer_draft_has_user_content(store.get_composer_draft("t")));
        assert!(!composer_draft_has_user_content(None));
    }

    #[test]
    fn dedupes_images_by_id_and_metadata() {
        let mut store = ComposerDraftStore::new();
        let accepted = store.add_images(
            "t",
            vec![image("a", "x.png"), image("a", "y.png"), image("b", "x.png"), image("c", "z.png")],
            AddAttachmentsOptions::default(),
        );
        assert_eq!(accepted, ["a", "c"]);
        let accepted = store.add_images("t", vec![image("d", "x.png")], AddAttachmentsOptions { allow_duplicates: true });
        assert_eq!(accepted, ["d"]);
        store.remove_image("t", "a");
        let ids: Vec<_> = store.get_composer_draft("t").unwrap().images.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["c", "d"]);
    }

    #[test]
    fn caps_attachments_at_the_provider_limit() {
        let mut store = ComposerDraftStore::new();
        let many: Vec<_> = (0..PROVIDER_SEND_TURN_MAX_ATTACHMENTS + 5)
            .map(|index| image(&format!("i{index}"), &format!("{index}.png")))
            .collect();
        let accepted = store.add_images("t", many, AddAttachmentsOptions::default());
        assert_eq!(accepted.len(), PROVIDER_SEND_TURN_MAX_ATTACHMENTS);
        assert!(store.add_files("t", vec![file("f", "f.txt", true)], AddAttachmentsOptions::default()).is_empty());
    }

    #[test]
    fn clears_content_but_keeps_choices() {
        let mut store = ComposerDraftStore::new();
        store.set_prompt("t", "hi");
        store.add_images("t", vec![image("a", "a.png")], AddAttachmentsOptions::default());
        store.set_interaction_mode("t", Some("plan"));
        store.clear_composer_content("t");
        let draft = store.get_composer_draft("t").unwrap();
        assert_eq!((draft.prompt.as_str(), draft.images.len(), draft.interaction_mode.as_deref()), ("", 0, Some("plan")));
    }

    #[test]
    fn model_selection_keeps_options_per_instance() {
        let mut store = ComposerDraftStore::new();
        let selection = |model: &str, options: Vec<OptionSelection>| DraftModelSelection {
            instance_id: "codex".into(),
            model: model.into(),
            options,
        };
        store.set_provider_model_options("t", "codex", Some(vec![OptionSelection::string("effort", "high")]), "gpt-5.4");
        let draft = store.get_composer_draft("t").unwrap();
        assert_eq!(draft.model_selection_by_provider["codex"].model, "gpt-5.4");
        assert!(draft.model_selection_explicit);
        // A model pick without options keeps the instance's options.
        store.set_model_selection("t", selection("gpt-5.3", vec![]), true, false);
        let draft = store.get_composer_draft("t").unwrap();
        assert_eq!(draft.model_selection_by_provider["codex"].model, "gpt-5.3");
        assert_eq!(draft.model_selection_by_provider["codex"].options, vec![OptionSelection::string("effort", "high")]);
        // Replacing options with none clears them.
        store.set_model_selection("t", selection("gpt-5.3", vec![]), true, true);
        assert!(store.get_composer_draft("t").unwrap().model_selection_by_provider["codex"].options.is_empty());
        assert_eq!(store.get_composer_draft("t").unwrap().active_provider.as_deref(), Some("codex"));
    }

    #[test]
    fn moves_the_prompt_and_what_fits_to_another_thread() {
        let mut store = ComposerDraftStore::new();
        store.set_prompt("from", "moved prompt");
        store.add_images("from", vec![image("a", "a.png")], AddAttachmentsOptions::default());
        store.add_files(
            "from",
            vec![file("keep", "gone.txt", false), file("move", "move.txt", true)],
            AddAttachmentsOptions::default(),
        );
        store.move_composer_prompt_and_images("from", "to");
        let to = store.get_composer_draft("to").unwrap();
        assert_eq!(to.prompt, "moved prompt");
        assert_eq!(to.images.len(), 1);
        assert_eq!(to.files.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["move"]);
        let from = store.get_composer_draft("from").unwrap();
        assert_eq!(from.prompt, "");
        assert_eq!(from.files.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["keep"]);
    }

    #[test]
    fn stash_is_newest_first_and_capped() {
        let mut store = ComposerDraftStore::new();
        for index in 0..MAX_STASH_ENTRIES + 3 {
            store.push_stash_entry(PromptStashEntry {
                id: format!("s{index}"),
                created_at: "2026-10-15T00:00:00.000Z".into(),
                prompt: format!("p{index}"),
                images: vec![],
                files: vec![],
                dropped_image_names: vec![],
                unreadable_image_names: vec![],
                pending_image_count: 0,
            });
        }
        assert_eq!(store.stash_entries().len(), MAX_STASH_ENTRIES);
        assert_eq!(store.stash_entries()[0].id, format!("s{}", MAX_STASH_ENTRIES + 2));
        let removed = store.remove_stash_entry("s22").unwrap();
        assert_eq!(removed.prompt, "p22");
        assert!(store.remove_stash_entry("s22").is_none());
    }
}
