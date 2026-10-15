//! Ports of upstream `AttachmentClaims.test.ts`.

use super::*;
use crate::pipeline::attachment_store::create_pending_attachment_id;
use serde_json::json;
use std::path::Path;
use tokio::sync::Notify;

struct Fixture {
    dir: tempfile::TempDir,
    claims: AttachmentClaims,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let claims = AttachmentClaims::new(dir.path());
    Fixture { dir, claims }
}

fn attachment(kind: &str, id: &str, name: &str, mime: &str, size: u64) -> ChatAttachment {
    serde_json::from_value(json!({
        "type": kind, "id": id, "name": name, "mimeType": mime, "sizeBytes": size,
    }))
    .unwrap()
}

fn stage_pending_upload(dir: &Path, name: &str, bytes: &[u8], mime: &str) -> ChatAttachment {
    let pending_id = create_pending_attachment_id(None);
    std::fs::write(dir.join(format!("{pending_id}.png")), bytes).unwrap();
    attachment("image", &pending_id, name, mime, bytes.len() as u64)
}

fn entries(dir: &Path, prefix: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with(prefix))
        .collect()
}

#[tokio::test]
async fn accepts_100_existing_files_without_claiming_new_copies() {
    let f = fixture();
    let attachments: Vec<_> = (0..100)
        .map(|i| {
            attachment(
                "file",
                &format!("existing-file-{i}"),
                &format!("{i}.txt"),
                "text/plain",
                1,
            )
        })
        .collect();
    let claimed = f
        .claims
        .claim_pending_attachments("thread-many", &attachments)
        .await
        .unwrap();
    assert_eq!(claimed.attachments, attachments);
    assert!(claimed.claimed_paths.is_empty());
}

#[tokio::test]
async fn rejects_oversized_image_batches_before_copying_a_pending_upload() {
    let f = fixture();
    let pending = stage_pending_upload(f.dir.path(), "pending.png", &[1], "image/png");
    let mut attachments = vec![pending];
    attachments.extend((0..9).map(|i| {
        attachment(
            "image",
            &format!("existing-image-{i}"),
            &format!("{i}.png"),
            "image/png",
            10 * 1024 * 1024,
        )
    }));
    let error = f
        .claims
        .claim_pending_attachments("thread-image-budget", &attachments)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("80 MiB"));
    assert_eq!(std::fs::read_dir(f.dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn claims_a_pending_upload_into_the_thread_store_and_rewrites_the_id() {
    let f = fixture();
    let pending = stage_pending_upload(f.dir.path(), "screenshot.png", &[1, 2, 3, 4], "image/png");
    let claimed = f
        .claims
        .claim_pending_attachments("thread-claims-1", &[pending])
        .await
        .unwrap();
    assert_eq!(claimed.attachments.len(), 1);
    let claimed_attachment = &claimed.attachments[0];
    assert!(!attachment_is_pending_upload(claimed_attachment));
    assert_eq!(
        parse_thread_segment_from_attachment_id(attachment_fields(claimed_attachment).id)
            .as_deref(),
        Some("thread-claims-1")
    );
    assert_eq!(claimed.claimed_paths.len(), 1);
    assert!(claimed.claimed_paths[0].exists());
    // The pending copy stays behind as the retry source.
    assert_eq!(entries(f.dir.path(), "pending-").len(), 1);
}

#[tokio::test]
async fn passes_already_claimed_attachments_through_untouched() {
    let f = fixture();
    let stored = attachment(
        "image",
        "thread-claims-2-00000000-0000-4000-8000-000000000001",
        "existing.png",
        "image/png",
        4,
    );
    let claimed = f
        .claims
        .claim_pending_attachments("thread-claims-2", std::slice::from_ref(&stored))
        .await
        .unwrap();
    assert_eq!(claimed.attachments[0], stored);
    assert!(claimed.claimed_paths.is_empty());
}

#[tokio::test]
async fn rejects_a_pending_ref_whose_staged_file_is_missing() {
    let f = fixture();
    let missing = attachment(
        "image",
        &create_pending_attachment_id(None),
        "gone.png",
        "image/png",
        4,
    );
    assert!(
        f.claims
            .claim_pending_attachments("thread-claims-3", &[missing])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejects_a_size_mismatch_and_cleans_up_earlier_claims_from_the_batch() {
    let f = fixture();
    let good = stage_pending_upload(f.dir.path(), "one.png", &[1, 2, 3, 4], "image/png");
    let staged = stage_pending_upload(f.dir.path(), "two.png", &[1, 2, 3, 4], "image/png");
    let mismatched = attachment(
        "image",
        attachment_fields(&staged).id,
        "two.png",
        "image/png",
        999,
    );
    let error = f
        .claims
        .claim_pending_attachments("thread-claims-4", &[good, mismatched])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("stored size does not match"));
    // The successfully claimed first attachment was rolled back.
    assert!(entries(f.dir.path(), "thread-claims-4-").is_empty());
}

#[tokio::test]
async fn rejects_duplicate_attachment_ids_before_copying_anything() {
    let f = fixture();
    let pending = stage_pending_upload(f.dir.path(), "dupe.png", &[5, 5, 5], "image/png");
    let error = f
        .claims
        .claim_pending_attachments("thread-claims-dupe", &[pending.clone(), pending])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Duplicate attachment ids"));
    assert!(entries(f.dir.path(), "thread-claims-dupe-").is_empty());
    // The pending source survives for a corrected retry.
    assert_eq!(entries(f.dir.path(), "pending-").len(), 1);
}

#[tokio::test]
async fn rolls_back_a_copy_that_finishes_after_the_claim_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (gate_reached, gate_release) = (reached.clone(), release.clone());
    let claims = AttachmentClaims::new(dir.path()).with_copy_gate(Arc::new(move || {
        let (reached, release) = (gate_reached.clone(), gate_release.clone());
        Box::pin(async move {
            reached.notify_one();
            release.notified().await;
        })
    }));
    let pending = stage_pending_upload(dir.path(), "interrupt.png", &[7, 7, 7], "image/png");
    let claim = {
        let claims = claims.clone();
        tokio::spawn(async move {
            claims
                .claim_pending_attachments("thread-claims-interrupt", &[pending])
                .await
        })
    };
    reached.notified().await;
    // The caller goes away while the copy is about to run.
    claim.abort();
    let _ = claim.await;
    release.notify_one();
    // The task finishes its copy, then the cancelled caller's guard releases it.
    for _ in 0..200 {
        if entries(dir.path(), "thread-claims-interrupt-").is_empty()
            && entries(dir.path(), "pending-").len() == 1
        {
            // Make sure the copy really ran before it was rolled back.
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(entries(dir.path(), "thread-claims-interrupt-").is_empty());
    // ...and the pending source survived for a retry.
    assert_eq!(entries(dir.path(), "pending-").len(), 1);
}

#[tokio::test]
async fn release_claimed_attachments_removes_claimed_copies_best_effort() {
    let f = fixture();
    let pending = stage_pending_upload(f.dir.path(), "release.png", &[9, 9], "image/png");
    let claimed = f
        .claims
        .claim_pending_attachments("thread-claims-5", &[pending])
        .await
        .unwrap();
    assert!(claimed.claimed_paths[0].exists());
    release_claimed_attachments(&claimed.claimed_paths).await;
    assert!(!claimed.claimed_paths[0].exists());
    // Releasing again is a no-op, not an error.
    release_claimed_attachments(&claimed.claimed_paths).await;
}

#[tokio::test]
async fn treats_malformed_ids_as_non_pending_and_passes_them_through() {
    let f = fixture();
    let bogus = attachment("image", "pending-not-a-uuid", "weird.png", "image/png", 1);
    let claimed = f
        .claims
        .claim_pending_attachments("thread-claims-6", std::slice::from_ref(&bogus))
        .await
        .unwrap();
    assert_eq!(claimed.attachments[0], bogus);
    assert!(claimed.claimed_paths.is_empty());
}

#[tokio::test]
async fn rewrites_context_bindings_to_the_claimed_attachment_ids() {
    let f = fixture();
    let pending = stage_pending_upload(f.dir.path(), "ctx.png", &[1, 2], "image/png");
    let before_id = attachment_fields(&pending).id.to_owned();
    let context: OrchestrationMessageContext = serde_json::from_value(json!({
        "version": 1,
        "records": [
            {"version": 1, "contextId": "ctx-1", "label": "shot", "kind": "image",
             "attachmentId": before_id, "name": "ctx.png", "mimeType": "image/png", "sizeBytes": 2},
            {"version": 1, "contextId": "ctx-2", "label": "other", "kind": "image",
             "attachmentId": "untouched", "name": "o.png", "mimeType": "image/png", "sizeBytes": 2},
        ],
    }))
    .unwrap();
    let claimed = f
        .claims
        .claim_pending_attachments("thread-claims-7", std::slice::from_ref(&pending))
        .await
        .unwrap();
    let remapped = remap_composer_context_attachments(&context, &[pending], &claimed.attachments);
    let value = serde_json::to_value(&remapped).unwrap();
    assert_eq!(
        value["records"][0]["attachmentId"],
        attachment_fields(&claimed.attachments[0]).id
    );
    assert_eq!(value["records"][1]["attachmentId"], "untouched");
}
