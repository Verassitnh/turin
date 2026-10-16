//! `PullRequestService.test.ts` lines 6118-6243: the diff cache across a file being ticked off,
//! and what it keeps of large slices. The releases-an-expired-diff case at 6244 depends on V8
//! garbage collection through WeakRef and is not ported.

use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;

use super::{counter, ref_json_value};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;
use turin_contracts::pull_request::PullRequestDiffInput;

fn diff_input(
    reference: &turin_contracts::pull_request::PullRequestRef,
    extra: serde_json::Value,
) -> PullRequestDiffInput {
    input(with_ref(reference, extra))
}

#[tokio::test(start_paused = true)]
async fn keeps_the_diff_cached_across_a_file_being_ticked_off() {
    let diff_reads = counter();
    let viewed_reads = counter();
    let state = Arc::new(Mutex::new("viewed"));
    let (counted_diffs, counted_views, current) =
        (diff_reads.clone(), viewed_reads.clone(), state.clone());
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["viewedFiles"] = json!("host");
                })
                .on_get_diff(move |_| {
                    counted_diffs.fetch_add(1, SeqCst);
                    Ok(ProviderDiffSlice {
                        patch: "@@".to_owned(),
                        truncated: false,
                        next_cursor: None,
                        omitted_file_stats: None,
                    })
                })
                .on_get_files_viewed(move |_| {
                    counted_views.fetch_add(1, SeqCst);
                    Ok(ProviderFilesViewed {
                        files: vec![
                            serde_json::from_value(json!({
                                "path": "src/a.ts",
                                "state": *current.lock(),
                            }))
                            .unwrap(),
                        ],
                        truncated: false,
                    })
                })
                .on_set_files_viewed(|_| Ok(())),
        ],
    )
    .await;
    let reference = pr_ref("p1", "pingdotgg/t3code", 1);

    h.service
        .diff(&diff_input(&reference, json!({})))
        .await
        .unwrap();
    h.service.files_viewed(&reference).await.unwrap();
    h.service
        .set_files_viewed(&input(with_ref(
            &reference,
            json!({ "files": [{ "path": "src/a.ts", "viewed": false }] }),
        )))
        .await
        .unwrap();
    h.service
        .diff(&diff_input(&reference, json!({})))
        .await
        .unwrap();
    h.service.files_viewed(&reference).await.unwrap();

    // The press forgets only the reader's own ticks; cached diffs survive it.
    assert_eq!(diff_reads.load(SeqCst), 1);
    assert_eq!(viewed_reads.load(SeqCst), 2);

    *state.lock() = "dismissed";
    h.service
        .invalidate(
            &input(json!({ "reference": ref_json_value(&reference), "filesViewedOnly": true })),
            false,
        )
        .await;
    h.service
        .diff(&diff_input(&reference, json!({})))
        .await
        .unwrap();
    let files = serde_json::to_value(h.service.files_viewed(&reference).await.unwrap()).unwrap();
    assert_eq!(
        files["files"],
        json!([{ "path": "src/a.ts", "state": "dismissed" }])
    );
    assert_eq!(diff_reads.load(SeqCst), 1);
    assert_eq!(viewed_reads.load(SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn returns_large_diff_slices_intact_without_retaining_them_in_either_cache() {
    let reads = counter();
    let counted = reads.clone();
    let patch = "\u{1f4bb}".repeat(140_000);
    let returned = patch.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_get_diff(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(ProviderDiffSlice {
                patch: returned.clone(),
                truncated: false,
                next_cursor: Some("2".to_owned()),
                omitted_file_stats: None,
            })
        })],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    for extra in [
        json!({}),
        json!({ "cursor": "2" }),
        json!({ "commit": "a".repeat(40) }),
    ] {
        let before = reads.load(SeqCst);
        for _ in 0..2 {
            let result = serde_json::to_value(
                h.service
                    .diff(&diff_input(&reference, extra.clone()))
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["patch"], json!(patch));
            assert_eq!(result["truncated"], json!(false));
            assert_eq!(result["nextCursor"], json!("2"));
        }
        assert_eq!(reads.load(SeqCst), before + 2);
    }
}

#[tokio::test(start_paused = true)]
async fn caches_a_small_replacement_after_releasing_a_large_diff() {
    let reads = counter();
    let counted = reads.clone();
    let large_patch = "x".repeat(300_000);
    let returned = large_patch.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_get_diff(move |_| {
            let call = counted.fetch_add(1, SeqCst) + 1;
            Ok(ProviderDiffSlice {
                patch: if call == 1 {
                    returned.clone()
                } else {
                    "@@ small replacement".to_owned()
                },
                truncated: false,
                next_cursor: None,
                omitted_file_stats: None,
            })
        })],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let mut patches = Vec::new();
    for _ in 0..3 {
        let result = h
            .service
            .diff(&diff_input(&reference, json!({})))
            .await
            .unwrap();
        patches.push(serde_json::to_value(result).unwrap()["patch"].clone());
    }
    assert_eq!(patches[0], json!(large_patch));
    assert_eq!(patches[1], json!("@@ small replacement"));
    assert_eq!(patches[2], json!("@@ small replacement"));
    assert_eq!(reads.load(SeqCst), 2);
}
