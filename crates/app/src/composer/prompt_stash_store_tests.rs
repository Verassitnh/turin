//! Port of upstream/apps/web/src/promptStashStore.test.ts

use serde_json::json;
use turin_contracts::assistant_citations::AssistantCitation;
use turin_contracts::composer_context::ComposerContextRecord;
use turin_shared::assistant_citations::{
    collect_assistant_citations, serialize_assistant_citation,
};

use super::*;

fn memory_store() -> PromptStashStore<MemoryStashStorage> {
    PromptStashStore::new(MemoryStashStorage::default(), false)
}

fn image(id: &str, name: &str, chars: usize) -> PersistedStashImage {
    PersistedStashImage {
        id: id.into(),
        name: name.into(),
        mime_type: "image/png".into(),
        size_bytes: chars as u64,
        source: None,
        data_url: "x".repeat(chars),
    }
}

fn make_entry(id: &str) -> PromptStashEntry {
    PromptStashEntry {
        id: id.into(),
        created_at: "2026-07-24T12:00:00.000Z".into(),
        prompt: format!("prompt {id}"),
        attachments: Vec::new(),
        files: Vec::new(),
        dropped_image_names: Vec::new(),
        unreadable_image_names: Vec::new(),
        pending_image_count: 0,
        records: Vec::new(),
    }
}

#[test]
fn keeps_attachments_within_the_budget_and_reports_dropped_names_in_order() {
    let small = image("a", "small.png", 10);
    let huge = image("b", "huge.png", MAX_STASH_ENTRY_ATTACHMENT_CHARS);
    let also_small = image("c", "also-small.png", 10);
    let partitioned = partition_stash_attachments(&[small, huge, also_small]);
    let kept: Vec<_> = partitioned
        .kept
        .iter()
        .map(|attachment| attachment.id.as_str())
        .collect();
    assert_eq!(kept, ["a", "c"]);
    assert_eq!(partitioned.dropped_names, ["huge.png"]);
}

#[test]
fn admits_a_single_attachment_that_exactly_fits_the_budget() {
    let exact = image("a", "exact.png", MAX_STASH_ENTRY_ATTACHMENT_CHARS);
    let partitioned = partition_stash_attachments(&[exact]);
    assert_eq!(partitioned.kept.len(), 1);
    assert!(partitioned.dropped_names.is_empty());
}

#[test]
fn prepends_entries_so_the_newest_stash_is_first() {
    let mut store = memory_store();
    store.stash_entry(make_entry("first"));
    store.stash_entry(make_entry("second"));
    let ids: Vec<_> = store
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, ["second", "first"]);
}

#[test]
fn restores_citations_and_bound_comments_from_a_persisted_stash() {
    let citation: AssistantCitation = serde_json::from_value(json!({
        "version": 1,
        "environmentId": "remote-source",
        "threadId": "source-thread",
        "messageId": "source-message",
        "text": "A quote to revisit later.",
        "comment": "Revisit this tomorrow.\nCheck whether the example still applies.",
        "start": 0,
        "end": 24,
        "prefix": "",
        "suffix": " Next.",
    }))
    .unwrap();
    let prompt = format!("Revisit {}", serialize_assistant_citation(&citation));
    let mut entry = make_entry("citation-stash");
    entry.prompt = prompt.clone();
    let raw = serde_json::to_string(&json!({
        "version": 2,
        "state": { "entries": [entry] },
    }))
    .unwrap();

    let mut store = memory_store();
    store.write_storage_for_test(&raw);
    let restored = store
        .take_entry("citation-stash")
        .entry
        .expect("stashed citation");
    assert_eq!(restored.prompt, prompt);
    let citations: Vec<_> = collect_assistant_citations(&restored.prompt)
        .into_iter()
        .map(|entry| entry.citation)
        .collect();
    assert_eq!(citations, [citation]);
    assert!(store.take_entry("citation-stash").entry.is_none());
}

#[test]
fn evicts_the_oldest_entry_past_the_cap_and_returns_it() {
    let mut store = memory_store();
    for index in 0..MAX_STASH_ENTRIES {
        assert!(
            store
                .stash_entry(make_entry(&format!("entry-{index}")))
                .evicted
                .is_none()
        );
    }
    let evicted = store.stash_entry(make_entry("overflow")).evicted;
    assert_eq!(
        evicted.as_ref().map(|entry| entry.id.as_str()),
        Some("entry-0")
    );
    assert_eq!(store.entries().len(), MAX_STASH_ENTRIES);
    assert_eq!(store.entries()[0].id, "overflow");
}

#[test]
fn distinguishes_a_memory_only_write_from_a_failed_one() {
    let mut store = memory_store();
    let result = store.stash_entry(make_entry("memory-only"));
    assert!(result.written);
    assert!(!result.durable);
    let ids: Vec<_> = store
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, ["memory-only"]);

    let mut failing = PromptStashStore::new(MemoryStashStorage::failing_writes(), true);
    let failed = failing.stash_entry(make_entry("quota"));
    assert!(!failed.written);
    assert!(!failed.durable);
    assert!(failed.evicted.is_none());
    assert!(failing.entries().is_empty());
}

#[test]
fn take_entry_removes_and_returns_the_entry() {
    let mut store = memory_store();
    store.stash_entry(make_entry("keep"));
    store.stash_entry(make_entry("take"));
    assert_eq!(
        store
            .take_entry("take")
            .entry
            .as_ref()
            .map(|entry| entry.id.as_str()),
        Some("take")
    );
    assert!(store.take_entry("take").entry.is_none());
    let ids: Vec<_> = store
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, ["keep"]);
}

#[test]
fn finalize_entry_images_attaches_images_and_clears_the_pending_count() {
    let mut store = memory_store();
    let mut pending = make_entry("pending");
    pending.pending_image_count = 2;
    store.stash_entry(pending);
    let result = store.finalize_entry_images(
        "pending",
        FinalizedStashImages {
            attachments: vec![PersistedStashImage {
                id: "img-1".into(),
                name: "a.webp".into(),
                mime_type: "image/webp".into(),
                size_bytes: 10,
                source: None,
                data_url: "data:image/webp;base64,AAAA".into(),
            }],
            dropped_image_names: vec!["big.png".into()],
            unreadable_image_names: Vec::new(),
        },
    );
    assert!(result.attached);
    let entry = &store.entries()[0];
    assert_eq!(entry.attachments.len(), 1);
    assert_eq!(entry.dropped_image_names, ["big.png"]);
    assert_eq!(entry.pending_image_count, 0);
}

#[test]
fn take_entry_returns_images_finalized_after_a_menu_snapshot() {
    let mut store = memory_store();
    let mut pending = make_entry("pending-restore");
    pending.pending_image_count = 1;
    store.stash_entry(pending);
    let menu_snapshot = store.entries()[0].clone();
    assert!(menu_snapshot.attachments.is_empty());

    store.finalize_entry_images(
        "pending-restore",
        FinalizedStashImages {
            attachments: vec![PersistedStashImage {
                id: "img-finalized".into(),
                name: "finalized.webp".into(),
                mime_type: "image/webp".into(),
                size_bytes: 12,
                source: None,
                data_url: "data:image/webp;base64,BBBB".into(),
            }],
            dropped_image_names: vec!["too-large.png".into()],
            unreadable_image_names: vec!["unreadable.png".into()],
        },
    );

    let taken = store
        .take_entry("pending-restore")
        .entry
        .expect("finalized entry");
    assert_ne!(taken, menu_snapshot);
    assert_eq!(taken.attachments[0].id, "img-finalized");
    assert_eq!(taken.attachments[0].name, "finalized.webp");
    assert_eq!(taken.dropped_image_names, ["too-large.png"]);
    assert_eq!(taken.unreadable_image_names, ["unreadable.png"]);
    assert_eq!(taken.pending_image_count, 0);
}

#[test]
fn preserves_uploaded_file_references_without_storing_file_contents() {
    let mut store = memory_store();
    let file = PersistedStashFile {
        id: "file-1".into(),
        name: "report.pdf".into(),
        mime_type: "application/pdf".into(),
        size_bytes: 42,
        attachment_id: "pending-report-pdf".into(),
        environment_id: "environment-1".into(),
        source: None,
    };
    let mut entry = make_entry("with-file");
    entry.files = vec![file.clone()];
    store.stash_entry(entry);
    store.finalize_entry_images(
        "with-file",
        FinalizedStashImages {
            attachments: Vec::new(),
            dropped_image_names: Vec::new(),
            unreadable_image_names: Vec::new(),
        },
    );
    assert_eq!(store.entries()[0].files, [file]);
}

#[test]
fn finalize_entry_images_reports_false_when_the_entry_was_already_taken() {
    let mut store = memory_store();
    let mut racing = make_entry("racing");
    racing.pending_image_count = 1;
    store.stash_entry(racing);
    store.take_entry("racing");
    let result = store.finalize_entry_images(
        "racing",
        FinalizedStashImages {
            attachments: Vec::new(),
            dropped_image_names: Vec::new(),
            unreadable_image_names: Vec::new(),
        },
    );
    assert!(!result.attached);
}

#[test]
fn settles_a_pending_count_left_behind_by_a_crashed_session() {
    let mut orphan = make_entry("orphan");
    orphan.pending_image_count = 2;
    let raw = serde_json::to_string(&json!({
        "version": 2,
        "state": { "entries": [orphan] },
    }))
    .unwrap();
    let mut store = memory_store();
    store.write_storage_for_test(&raw);
    let entry = &store.entries()[0];
    assert_eq!(entry.pending_image_count, 0);
    assert_eq!(
        entry.unreadable_image_names,
        [
            "image 1 (not saved before reload)",
            "image 2 (not saved before reload)",
        ]
    );
}

#[test]
fn ignores_an_unreadable_v1_payload_seeded_under_the_current_key() {
    let raw = serde_json::to_string(&json!({
        "version": 1,
        "state": { "queuesByScopeKey": { "provider:claudeAgent": [] } },
    }))
    .unwrap();
    let mut store = memory_store();
    store.write_storage_for_test(&raw);
    assert!(store.entries().is_empty());
}

#[test]
fn keeps_the_records_behind_a_stashed_prompts_chips() {
    let record: ComposerContextRecord = serde_json::from_value(json!({
        "version": 1,
        "contextId": "ctx-1",
        "kind": "terminal",
        "label": "Terminal 1 line 4",
        "terminalId": "default",
        "terminalLabel": "Terminal 1",
        "lineStart": 4,
        "lineEnd": 4,
        "text": "boom",
    }))
    .unwrap();
    let mut store = memory_store();
    let mut entry = make_entry("entry-records");
    entry.created_at = "2026-01-01T00:00:00.000Z".into();
    entry.prompt = "see [Terminal 1 line 4](t3-context://v1/terminal/ctx-1)".into();
    entry.records = vec![record.clone()];
    store.stash_entry(entry);
    let taken = store.take_entry("entry-records").entry.expect("records");
    assert_eq!(taken.records, [record]);
}
