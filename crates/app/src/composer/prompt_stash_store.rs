//! Port of upstream/apps/web/src/promptStashStore.ts
//!
//! The persisted prompt queue. Upstream is a zustand singleton over
//! `localStorage`. Turin keeps the same queue on [`PromptStashStore`]; the
//! backing store is [`StashStorage`], so tests can use memory and a blocked
//! or quota-failing store. There is no module global.
//!
//! `draft_store::PromptStashEntry` is the in-memory composer slice. This
//! module is the persisted v2 payload, including data URLs and context records.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use turin_contracts::composer_context::ComposerContextRecord;
use turin_shared::js_string::utf16_len;

pub const PROMPT_STASH_STORAGE_KEY: &str = "t3code:prompt-stash:v2";
const LEGACY_PROMPT_STASH_STORAGE_KEY: &str = "t3code:prompt-stash:v1";
const PROMPT_STASH_STORAGE_VERSION: u64 = 2;

pub const MAX_STASH_ENTRIES: usize = 20;
pub const MAX_STASH_ENTRY_ATTACHMENT_CHARS: usize = 2_700_000;

#[derive(Debug)]
pub struct StashStorageError;

pub trait StashStorage {
    fn get_item(&self, key: &str) -> Result<Option<String>, StashStorageError>;
    fn set_item(&mut self, key: &str, value: &str) -> Result<(), StashStorageError>;
    fn remove_item(&mut self, key: &str) -> Result<(), StashStorageError>;
}

#[derive(Debug, Default)]
pub struct MemoryStashStorage {
    items: HashMap<String, String>,
    fail_writes: bool,
}

impl MemoryStashStorage {
    pub fn failing_writes() -> Self {
        Self {
            fail_writes: true,
            ..Self::default()
        }
    }
}

impl StashStorage for MemoryStashStorage {
    fn get_item(&self, key: &str) -> Result<Option<String>, StashStorageError> {
        Ok(self.items.get(key).cloned())
    }

    fn set_item(&mut self, key: &str, value: &str) -> Result<(), StashStorageError> {
        if self.fail_writes {
            return Err(StashStorageError);
        }
        self.items.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove_item(&mut self, key: &str) -> Result<(), StashStorageError> {
        self.items.remove(key);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedStashImage {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,
    pub data_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedStashFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub attachment_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptStashEntry {
    pub id: String,
    pub created_at: String,
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<PersistedStashImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<PersistedStashFile>,
    #[serde(default)]
    pub dropped_image_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_image_names: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pending_image_count: u64,
    /// `ForwardCompatibleArray(ComposerContextRecord)`: a record that does not
    /// decode is dropped and the entry's other records still restore.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "turin_contracts::base_schemas::forward_compatible_array"
    )]
    pub records: Vec<ComposerContextRecord>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq)]
pub struct StashWrite {
    pub evicted: Option<PromptStashEntry>,
    pub written: bool,
    pub durable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StashTake {
    pub entry: Option<PromptStashEntry>,
    pub durable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashFinalize {
    pub attached: bool,
    pub durable: bool,
}

pub struct FinalizedStashImages {
    pub attachments: Vec<PersistedStashImage>,
    pub dropped_image_names: Vec<String>,
    pub unreadable_image_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PartitionedStashAttachments {
    pub kept: Vec<PersistedStashImage>,
    pub dropped_names: Vec<String>,
}

pub fn partition_stash_attachments(
    attachments: &[PersistedStashImage],
) -> PartitionedStashAttachments {
    let mut kept = Vec::new();
    let mut dropped_names = Vec::new();
    let mut used_chars = 0usize;
    for attachment in attachments {
        let chars = utf16_len(&attachment.data_url);
        if used_chars + chars > MAX_STASH_ENTRY_ATTACHMENT_CHARS {
            dropped_names.push(attachment.name.clone());
            continue;
        }
        used_chars += chars;
        kept.push(attachment.clone());
    }
    PartitionedStashAttachments {
        kept,
        dropped_names,
    }
}

fn clear_orphaned_pending_images(entries: Vec<PromptStashEntry>) -> Vec<PromptStashEntry> {
    entries
        .into_iter()
        .map(|mut entry| {
            if entry.pending_image_count == 0 {
                return entry;
            }
            let lost = entry.pending_image_count;
            entry.pending_image_count = 0;
            for index in 0..lost {
                entry
                    .unreadable_image_names
                    .push(format!("image {} (not saved before reload)", index + 1));
            }
            entry
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct PersistedPromptStashState {
    entries: Vec<PromptStashEntry>,
}

pub struct PromptStashStore<S> {
    storage: S,
    durable_backend: bool,
    entries: Vec<PromptStashEntry>,
}

impl<S: StashStorage> PromptStashStore<S> {
    pub fn new(storage: S, durable_backend: bool) -> Self {
        let mut store = Self {
            storage,
            durable_backend,
            entries: Vec::new(),
        };
        let _ = store.storage.remove_item(LEGACY_PROMPT_STASH_STORAGE_KEY);
        store.entries = store.read_persisted().unwrap_or_default();
        store
    }

    pub fn entries(&self) -> &[PromptStashEntry] {
        &self.entries
    }

    pub fn stash_entry(&mut self, entry: PromptStashEntry) -> StashWrite {
        let mut next = self.entries.clone();
        next.insert(0, entry);
        let evicted = if next.len() > MAX_STASH_ENTRIES {
            next.pop()
        } else {
            None
        };
        let persisted = self.persist(&next);
        if !persisted.written {
            return StashWrite {
                evicted: None,
                written: false,
                durable: false,
            };
        }
        self.entries = next;
        StashWrite {
            evicted,
            written: true,
            durable: persisted.durable,
        }
    }

    pub fn take_entry(&mut self, entry_id: &str) -> StashTake {
        let Some(entry) = self
            .entries
            .iter()
            .find(|candidate| candidate.id == entry_id)
            .cloned()
        else {
            return StashTake {
                entry: None,
                durable: true,
            };
        };
        let next: Vec<_> = self
            .entries
            .iter()
            .filter(|candidate| candidate.id != entry_id)
            .cloned()
            .collect();
        let persisted = self.persist(&next);
        self.entries = next;
        StashTake {
            entry: Some(entry),
            durable: persisted.durable,
        }
    }

    pub fn finalize_entry_images(
        &mut self,
        entry_id: &str,
        images: FinalizedStashImages,
    ) -> StashFinalize {
        let Some(index) = self
            .entries
            .iter()
            .position(|candidate| candidate.id == entry_id)
        else {
            return StashFinalize {
                attached: false,
                durable: true,
            };
        };
        let mut next = self.entries.clone();
        let existing = &mut next[index];
        existing.attachments = images.attachments;
        existing.dropped_image_names = images.dropped_image_names;
        existing.unreadable_image_names = images.unreadable_image_names;
        existing.pending_image_count = 0;
        let persisted = self.persist(&next);
        self.entries = next;
        StashFinalize {
            attached: true,
            durable: persisted.durable,
        }
    }

    /// Seeds the persisted payload and rehydrates. An empty string clears.
    pub fn write_storage_for_test(&mut self, raw: &str) {
        let _ = self.storage.set_item(PROMPT_STASH_STORAGE_KEY, raw);
        self.entries = self.read_persisted().unwrap_or_default();
    }

    fn persist(&mut self, entries: &[PromptStashEntry]) -> PersistResult {
        let payload = serde_json::json!({
            "version": PROMPT_STASH_STORAGE_VERSION,
            "state": { "entries": entries },
        });
        match serde_json::to_string(&payload) {
            Ok(encoded) => match self.storage.set_item(PROMPT_STASH_STORAGE_KEY, &encoded) {
                Ok(()) => PersistResult {
                    written: true,
                    durable: self.durable_backend,
                },
                Err(_) => PersistResult {
                    written: false,
                    durable: false,
                },
            },
            Err(_) => PersistResult {
                written: false,
                durable: false,
            },
        }
    }

    fn read_persisted(&self) -> Option<Vec<PromptStashEntry>> {
        let raw = self.storage.get_item(PROMPT_STASH_STORAGE_KEY).ok()??;
        if raw.is_empty() {
            return None;
        }
        let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let state = parsed.get("state")?.clone();
        let decoded: PersistedPromptStashState = serde_json::from_value(state).ok()?;
        Some(clear_orphaned_pending_images(decoded.entries))
    }
}

struct PersistResult {
    written: bool,
    durable: bool,
}

#[cfg(test)]
#[path = "prompt_stash_store_tests.rs"]
mod tests;
