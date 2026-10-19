//! Persists the composer's prompt stash through [`PromptStashStore`] (the v2
//! `t3code:prompt-stash:v2` payload), so stashed prompts survive a restart.
//!
//! [`ComposerDraftStore`](super::draft_store::ComposerDraftStore) keeps the
//! live entries (attachments as bytes). This module maps them to the persisted
//! shape (data URLs, uploaded file references) and back. Images over the stash
//! budget are re-encoded the way `compressImageForStash` does, and dropped by
//! name when they still do not fit. A file that is not uploaded yet has nothing
//! durable to point at, so it stays in this session's entry only.

use std::fmt;
use std::sync::Arc;

use crate::composer::draft_store::{
    ComposerFileAttachment, ComposerImageAttachment, PromptStashEntry,
};
use crate::composer::prompt_stash_store::{
    PersistedStashFile, PersistedStashImage, PromptStashEntry as PersistedEntry, PromptStashStore,
    StashStorage, StashStorageError, partition_stash_attachments,
};
use crate::hooks::use_local_storage::SharedLocalStorage;
use crate::state::helpers::image_compression::{
    ImageCrateCodec, ImageFile, MAX_STASH_IMAGE_DATA_URL_CHARS, compress_image_for_stash,
    data_url_to_file,
};

/// What [`ComposerDraftStore`](super::draft_store::ComposerDraftStore) calls on
/// every stash change.
pub trait StashPersistence: fmt::Debug {
    /// The persisted entries, newest first.
    fn load(&self) -> Vec<PromptStashEntry>;
    /// `stashEntry`. Returns whether the entry was written.
    fn stash(&mut self, entry: &PromptStashEntry) -> bool;
    /// `takeEntry`.
    fn take(&mut self, entry_id: &str);
}

/// [`StashStorage`] over a shared [`LocalStorage`](crate::hooks::use_local_storage::LocalStorage).
pub struct LocalStorageStash(pub SharedLocalStorage);

impl StashStorage for LocalStorageStash {
    fn get_item(&self, key: &str) -> Result<Option<String>, StashStorageError> {
        self.0.get_item(key).map_err(|_| StashStorageError)
    }
    fn set_item(&mut self, key: &str, value: &str) -> Result<(), StashStorageError> {
        self.0.set_item(key, value).map_err(|_| StashStorageError)
    }
    fn remove_item(&mut self, key: &str) -> Result<(), StashStorageError> {
        self.0.remove_item(key).map_err(|_| StashStorageError)
    }
}

pub struct PromptStashPersistence<S: StashStorage> {
    store: PromptStashStore<S>,
}

impl<S: StashStorage> PromptStashPersistence<S> {
    pub fn new(storage: S, durable_backend: bool) -> Self {
        Self {
            store: PromptStashStore::new(storage, durable_backend),
        }
    }
}

impl PromptStashPersistence<LocalStorageStash> {
    pub fn over(storage: SharedLocalStorage) -> Self {
        Self::new(LocalStorageStash(storage), true)
    }
}

impl<S: StashStorage> fmt::Debug for PromptStashPersistence<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptStashPersistence")
            .field("entries", &self.store.entries().len())
            .finish()
    }
}

impl<S: StashStorage> StashPersistence for PromptStashPersistence<S> {
    fn load(&self) -> Vec<PromptStashEntry> {
        self.store.entries().iter().map(from_persisted).collect()
    }

    fn stash(&mut self, entry: &PromptStashEntry) -> bool {
        self.store.stash_entry(to_persisted(entry)).written
    }

    fn take(&mut self, entry_id: &str) {
        self.store.take_entry(entry_id);
    }
}

fn to_persisted(entry: &PromptStashEntry) -> PersistedEntry {
    let mut dropped_image_names = entry.dropped_image_names.clone();
    let mut unreadable_image_names = entry.unreadable_image_names.clone();
    let mut images = Vec::new();
    for image in &entry.images {
        let file = ImageFile::new(&image.name, &image.mime_type, image.data.to_vec());
        match compress_image_for_stash(&ImageCrateCodec, &file, MAX_STASH_IMAGE_DATA_URL_CHARS) {
            Ok(compressed) => images.push(PersistedStashImage {
                id: image.id.clone(),
                name: image.name.clone(),
                mime_type: compressed.mime_type,
                size_bytes: compressed.size_bytes,
                source: None,
                data_url: compressed.data_url,
            }),
            Err(
                crate::state::helpers::image_compression::ImageCompressionFailureReason::Unreadable,
            ) => unreadable_image_names.push(image.name.clone()),
            Err(_) => dropped_image_names.push(image.name.clone()),
        }
    }
    let partitioned = partition_stash_attachments(&images);
    dropped_image_names.extend(partitioned.dropped_names);
    let files = entry
        .files
        .iter()
        .filter_map(|file| {
            Some(PersistedStashFile {
                id: file.id.clone(),
                name: file.name.clone(),
                mime_type: file.mime_type.clone(),
                size_bytes: file.size_bytes,
                attachment_id: file.uploaded_attachment_id.clone()?,
                environment_id: file.upload_environment_id.clone()?,
                source: None,
            })
        })
        .collect();
    PersistedEntry {
        id: entry.id.clone(),
        created_at: entry.created_at.clone(),
        prompt: entry.prompt.clone(),
        attachments: partitioned.kept,
        files,
        dropped_image_names,
        unreadable_image_names,
        pending_image_count: 0,
        records: Vec::new(),
    }
}

fn from_persisted(entry: &PersistedEntry) -> PromptStashEntry {
    let mut unreadable_image_names = entry.unreadable_image_names.clone();
    let mut images = Vec::new();
    for attachment in &entry.attachments {
        match data_url_to_file(
            &attachment.data_url,
            &attachment.name,
            &attachment.mime_type,
        ) {
            Ok(file) => images.push(ComposerImageAttachment {
                id: attachment.id.clone(),
                name: attachment.name.clone(),
                mime_type: attachment.mime_type.clone(),
                size_bytes: attachment.size_bytes,
                data: Arc::clone(&file.bytes),
            }),
            Err(_) => unreadable_image_names.push(attachment.name.clone()),
        }
    }
    PromptStashEntry {
        id: entry.id.clone(),
        created_at: entry.created_at.clone(),
        prompt: entry.prompt.clone(),
        images,
        files: entry
            .files
            .iter()
            .map(|file| ComposerFileAttachment {
                id: file.id.clone(),
                name: file.name.clone(),
                mime_type: file.mime_type.clone(),
                size_bytes: file.size_bytes,
                data: None,
                uploaded_attachment_id: Some(file.attachment_id.clone()),
                upload_environment_id: Some(file.environment_id.clone()),
            })
            .collect(),
        dropped_image_names: entry.dropped_image_names.clone(),
        unreadable_image_names,
        pending_image_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::use_local_storage::{FileLocalStorage, MemoryLocalStorage};

    fn entry(id: &str, prompt: &str) -> PromptStashEntry {
        PromptStashEntry {
            id: id.into(),
            created_at: "2026-10-19T10:00:00.000Z".into(),
            prompt: prompt.into(),
            images: Vec::new(),
            files: Vec::new(),
            dropped_image_names: Vec::new(),
            unreadable_image_names: Vec::new(),
            pending_image_count: 0,
        }
    }

    #[test]
    fn a_stashed_prompt_with_attachments_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-storage.json");
        let open = || PromptStashPersistence::over(Arc::new(FileLocalStorage::open(&path)));

        let mut first = open();
        let mut stashed = entry("stash-1", "fix the flaky test");
        stashed.images = vec![ComposerImageAttachment::new(
            "img-1",
            "shot.png",
            "image/png",
            vec![1, 2, 3, 4],
        )];
        stashed.files = vec![
            ComposerFileAttachment {
                id: "file-1".into(),
                name: "notes.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 9,
                data: None,
                uploaded_attachment_id: Some("att-1".into()),
                upload_environment_id: Some("local".into()),
            },
            // Not uploaded: nothing durable to point at.
            ComposerFileAttachment {
                id: "file-2".into(),
                name: "raw.bin".into(),
                mime_type: "application/octet-stream".into(),
                size_bytes: 2,
                data: Some(Arc::new(vec![0, 1])),
                uploaded_attachment_id: None,
                upload_environment_id: None,
            },
        ];
        assert!(first.stash(&stashed));
        assert!(first.stash(&entry("stash-2", "second")));

        let restored = open().load();
        assert_eq!(
            restored.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["stash-2", "stash-1"]
        );
        let back = &restored[1];
        assert_eq!(back.prompt, "fix the flaky test");
        assert_eq!(back.images.len(), 1);
        assert_eq!(back.images[0].name, "shot.png");
        assert_eq!(back.images[0].data.as_slice(), [1, 2, 3, 4]);
        assert_eq!(back.files.len(), 1);
        assert_eq!(
            back.files[0].uploaded_attachment_id.as_deref(),
            Some("att-1")
        );
        assert!(!back.files[0].needs_reattach());
    }

    #[test]
    fn taking_an_entry_removes_it_from_disk() {
        let storage = MemoryLocalStorage::shared();
        let mut persistence = PromptStashPersistence::over(storage.clone());
        persistence.stash(&entry("stash-1", "a"));
        persistence.take("stash-1");
        assert!(PromptStashPersistence::over(storage).load().is_empty());
    }

    #[test]
    fn a_failing_store_reports_the_entry_as_not_written() {
        let mut persistence = PromptStashPersistence::new(
            crate::composer::prompt_stash_store::MemoryStashStorage::failing_writes(),
            false,
        );
        assert!(!persistence.stash(&entry("stash-1", "a")));
    }
}
