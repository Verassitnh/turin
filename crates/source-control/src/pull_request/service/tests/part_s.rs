//! `PullRequestService.test.ts` lines 7001-7599: presses that land in the order they were made,
//! what the viewed-file caches keep and bound, which reader a mark belongs to, and how a press or a
//! listing waits out a host that is backing off. Two Azure cases (7112, 7613) are not ported.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{
    asked_paths, counter, environment_project, environment_ref, environment_viewed_provider,
    no_unreadable, press, revisions_of, settle, summary_of, viewed_sorted,
};
use crate::pull_request::service::test_support::*;
use crate::pull_request::viewed_files::{FILE_REVISIONS_CACHE_CAPACITY, MAX_FILE_REVISION_PATHS};
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::*;
use turin_contracts::pull_request::{
    PullRequestListInput, PullRequestSetFilesViewedFile, PullRequestSetFilesViewedInput,
};

/// The persisted cap on marks one read returns (`pull_request_files_viewed.rs` in the persistence
/// crate, which does not re-export it).
const MAX_FILES_VIEWED_ROWS: usize = 500;

/// A host that answers every asked path at `blob-a`, and backs off for an hour when it is asked
/// while `paused` holds.
fn rate_limited_host(
    clock: &Arc<TestClock>,
    paused: Arc<Mutex<bool>>,
) -> impl Fn(GetFileRevisionsInput) -> ProviderResult<ProviderFileRevisions> + Send + Sync + 'static
{
    let clock = clock.clone();
    move |input| {
        if *paused.lock() {
            return Err(PullRequestProviderError {
                retry_at: Some(clock.now_millis() + 3_600_000),
                ..provider_error(
                    "gitlab",
                    "getFileRevisions",
                    PullRequestProviderErrorReason::RateLimited,
                    "API rate limit exceeded.",
                )
            });
        }
        Ok(ProviderFileRevisions {
            revisions: input
                .paths
                .iter()
                .map(|path| (path.clone(), "blob-a".to_owned()))
                .collect(),
            complete: None,
        })
    }
}

#[tokio::test(start_paused = true)]
async fn finishes_two_presses_on_one_file_in_the_order_they_were_made() {
    // A tick asks the host what it has of the file before it stores anything, and an untick asks
    // nothing at all, so the second press would otherwise land first and be overwritten by the
    // first one finishing behind it.
    let held = Arc::new(Notify::new());
    let release = held.clone();
    let h = make_service(
        vec![environment_project()],
        vec![
            FakeProvider::new("gitlab")
                .capabilities_with(|capabilities| {
                    capabilities["viewedFiles"] = json!("environment");
                })
                .on_get_file_revisions_async(move |input| {
                    let release = release.clone();
                    async move {
                        release.notified().await;
                        Ok(ProviderFileRevisions {
                            revisions: input
                                .paths
                                .iter()
                                .map(|path| (path.clone(), "blob-a".to_owned()))
                                .collect(),
                            complete: None,
                        })
                    }
                }),
        ],
    )
    .await;
    let reference = environment_ref(1);

    let service = h.service.clone();
    let tick_ref = reference.clone();
    let tick = tokio::spawn(async move {
        press(&service, &tick_ref, &[("src/a.ts", true)]).await;
    });
    // Far enough for the tick to be waiting on the host rather than still on its way there.
    settle().await;
    h.clock.adjust(1_000).await;
    let service = h.service.clone();
    let untick_ref = reference.clone();
    let untick = tokio::spawn(async move {
        press(&service, &untick_ref, &[("src/a.ts", false)]).await;
    });
    settle().await;
    h.clock.adjust(1_000).await;
    held.notify_one();
    tick.await.unwrap();
    untick.await.unwrap();

    // The untick came second and stands: the file is open again.
    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([])
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_environment_marks_apart_from_another_change_requests() {
    let revisions = revisions_of(&[("src/a.ts", "blob-a")]);
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked_paths(),
            &no_unreadable(),
        )],
    )
    .await;

    press(&h.service, &environment_ref(1), &[("src/a.ts", true)]).await;
    let other = h.service.files_viewed(&environment_ref(2)).await.unwrap();

    assert_eq!(serde_json::to_value(other).unwrap()["files"], json!([]));
}

#[tokio::test(start_paused = true)]
async fn bounds_the_paths_one_change_requests_held_revisions_carry() {
    // The cache's count bounds how many change requests are held, not what any one of them holds: a
    // reader ticking a wide change request renews the same entry on every press and adds a path to
    // it each time. A press carries at most half the cap, so going one past it takes three.
    let asked = asked_paths();
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions_of(&[]),
            &asked,
            &no_unreadable(),
        )],
    )
    .await;
    let reference = environment_ref(1);
    let batch = |prefix: &str, count: usize| -> Vec<(String, bool)> {
        (0..count)
            .map(|index| (format!("{prefix}/{index:04}.ts"), true))
            .collect()
    };
    let press_batch = |files: Vec<(String, bool)>| {
        let service = h.service.clone();
        let reference = reference.clone();
        async move {
            let files: Vec<(&str, bool)> = files
                .iter()
                .map(|(path, viewed)| (path.as_str(), *viewed))
                .collect();
            press(&service, &reference, &files).await;
        }
    };

    press_batch(batch("a", MAX_FILE_REVISION_PATHS / 2)).await;
    press_batch(batch("b", MAX_FILE_REVISION_PATHS / 2)).await;
    press_batch(batch("c", 1)).await;
    let pressed = asked.lock().len();

    // The marks a read carries come first by path, so this one covers the earliest batch, which is
    // where the paths asked about longest ago are. Held short of them the entry no longer answers
    // the read, and the host is asked rather than the reader being told a version nothing holds any
    // more.
    viewed_sorted(&h.service, &reference).await;

    assert_eq!(asked.lock().len(), pressed + 1);
    assert!(
        asked
            .lock()
            .last()
            .unwrap()
            .contains(&"a/0000.ts".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_the_marked_paths_when_a_whole_change_answer_is_wider_than_the_cap() {
    // A host with no per-file version answers with the whole change, which on a wide review carries
    // more paths than one entry holds. What the trim reaches has to be the paths the answer threw in
    // rather than the ones the reader ticked: a mark stored with no baseline reports viewed however
    // far the head moves off it.
    let head = revisions_of(
        &(0..MAX_FILE_REVISION_PATHS + 178)
            .map(|index| (format!("src/f{index:04}.ts"), format!("blob-{index}")))
            .collect::<Vec<_>>()
            .iter()
            .map(|(path, version)| (path.as_str(), version.as_str()))
            .collect::<Vec<_>>(),
    );
    let whole = head.clone();
    let provider = FakeProvider::new("gitlab")
        .capabilities_with(|capabilities| {
            capabilities["viewedFiles"] = json!("environment");
        })
        .on_get_change_request_summary(|_| {
            Ok(summary_of(
                &change_request(1, "2026-07-02T00:00:00Z"),
                turin_contracts::pull_request::PullRequestState::Open,
            ))
        })
        .on_get_file_revisions(move |_| {
            Ok(ProviderFileRevisions {
                revisions: whole
                    .lock()
                    .iter()
                    .map(|(path, version)| (path.clone(), version.clone()))
                    .collect(),
                complete: Some(true),
            })
        });
    let h = make_service(vec![environment_project()], vec![provider]).await;
    let reference = environment_ref(1);
    let ticked = ["src/f0000.ts", "src/f0500.ts"];

    press(&h.service, &reference, &ticked.map(|path| (path, true))).await;
    for path in ticked {
        head.lock().insert(path.to_owned(), "blob-moved".to_owned());
    }
    // Past the stale window, so the read is answered by the host rather than from what the press
    // heard.
    h.clock.adjust(11 * 60_000).await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([
            { "path": "src/f0000.ts", "state": "dismissed" },
            { "path": "src/f0500.ts", "state": "dismissed" },
        ])
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_the_change_request_being_ticked_through_not_the_one_pressed_first() {
    // Ordered by insertion alone a hit does not renew its entry, so the review a reader is working
    // down is the first thing dropped once a cache's worth of other change requests have been
    // pressed, and the next press on it pays a host read for a version already held.
    let asked = asked_paths();
    let revisions = revisions_of(&[("src/a.ts", "blob-a")]);
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked,
            &no_unreadable(),
        )],
    )
    .await;
    let press_number = |number: u64| {
        let service = h.service.clone();
        async move { press(&service, &environment_ref(number), &[("src/a.ts", true)]).await }
    };

    press_number(1).await;
    // A cache's worth of other change requests, with the open one pressed in between each.
    for filled in 0..FILE_REVISIONS_CACHE_CAPACITY as u64 {
        press_number(2 + filled).await;
        press_number(1).await;
    }

    assert_eq!(asked.lock().len(), 1 + FILE_REVISIONS_CACHE_CAPACITY);
}

#[tokio::test(start_paused = true)]
async fn keeps_one_readers_marks_on_a_host_that_names_nobody() {
    let revisions = revisions_of(&[("src/a.ts", "blob-a")]);
    let h = make_service(
        vec![environment_project()],
        vec![
            environment_viewed_provider(&revisions, &asked_paths(), &no_unreadable())
                .on_get_viewer(|_, _| Ok(String::new())),
        ],
    )
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "viewed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn puts_a_listing_and_a_press_for_one_host_on_a_single_viewer_lookup() {
    let viewer_lookups = counter();
    let counted = viewer_lookups.clone();
    let revisions = revisions_of(&[("src/a.ts", "blob-a")]);
    let h = make_service(
        vec![environment_project()],
        vec![
            environment_viewed_provider(&revisions, &asked_paths(), &no_unreadable())
                .on_get_viewer(move |_, _| {
                    counted.fetch_add(1, SeqCst);
                    Ok("bilal".to_owned())
                })
                .on_list_change_requests(|_| Ok(page(Vec::new(), false))),
        ],
    )
    .await;

    // What a cold page load does: read the listing and the reader's own marks at the same time.
    // Nothing about which of them asked is in the lookup's key, so they wait on one CLI between them
    // rather than starting one each.
    let list_input = input::<PullRequestListInput>(json!({ "state": "open" }));
    let reference = environment_ref(1);
    let (listed, marks) = tokio::join!(
        h.service.list(&list_input),
        h.service.files_viewed(&reference),
    );
    listed.unwrap();
    marks.unwrap();

    assert_eq!(viewer_lookups.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn carries_a_bounded_number_of_its_own_marks_and_says_it_held_more() {
    let paths: Vec<String> = (0..MAX_FILES_VIEWED_ROWS + 40)
        .map(|at| format!("src/f{at:04}.ts"))
        .collect();
    let revisions = revisions_of(
        &paths
            .iter()
            .map(|path| (path.as_str(), "blob"))
            .collect::<Vec<_>>(),
    );
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked_paths(),
            &no_unreadable(),
        )],
    )
    .await;
    let reference = environment_ref(1);

    // The schema caps one write at 500 presses, and the service takes what it is handed (upstream's
    // service test passes all 540 straight in). Build within the cap, then extend past it.
    let files: Vec<Value> = paths
        .iter()
        .map(|path| json!({ "path": path, "viewed": true }))
        .collect();
    let mut write = input::<PullRequestSetFilesViewedInput>(with_ref(
        &reference,
        json!({ "files": &files[..MAX_FILES_VIEWED_ROWS] }),
    ));
    write
        .files
        .extend(
            paths[MAX_FILES_VIEWED_ROWS..]
                .iter()
                .map(|path| PullRequestSetFilesViewedFile {
                    path: path.clone(),
                    viewed: true,
                }),
        );
    h.service.set_files_viewed(&write).await.unwrap();
    let read = viewed_sorted(&h.service, &reference).await;

    // Every mark read is a path held for as long as the caller holds the read, so the rows are
    // bounded rather than however many a reader has ever ticked. The reader is told the count is
    // short rather than shown a quietly clipped list.
    assert_eq!(
        read["files"].as_array().unwrap().len(),
        MAX_FILES_VIEWED_ROWS
    );
    assert_eq!(read["truncated"], json!(true));
}

#[tokio::test(start_paused = true)]
async fn records_a_press_while_the_host_is_backing_off() {
    let clock = TestClock::new();
    let viewer_lookups = counter();
    let counted = viewer_lookups.clone();
    let paused = Arc::new(Mutex::new(true));
    let host = rate_limited_host(&clock, paused.clone());
    let h = Harness::new(ServiceOptions {
        clock: Some(clock.clone()),
        projects: vec![environment_project().build()],
        providers: vec![
            FakeProvider::new("gitlab")
                .capabilities_with(|capabilities| {
                    capabilities["viewedFiles"] = json!("environment");
                })
                .on_get_viewer(move |_, _| {
                    counted.fetch_add(1, SeqCst);
                    Ok("bilal".to_owned())
                })
                .on_get_file_revisions(host),
        ],
        ..ServiceOptions::default()
    })
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    h.clock.adjust(11 * 60_000).await;
    press(&h.service, &reference, &[("src/b.ts", true)]).await;

    // Nothing about a host holding its reads off says the reader did not press these, and these rows
    // are this environment's own. They keep no baseline, because none was read, so they hold until
    // they are pressed again rather than reporting a staleness nobody looked up.
    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([
            { "path": "src/a.ts", "state": "viewed" },
            { "path": "src/b.ts", "state": "viewed" },
        ])
    );
    assert_eq!(viewer_lookups.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn asks_who_is_reading_through_a_pause_only_for_the_press_that_is_waiting_on_it() {
    let clock = TestClock::new();
    let viewer_lookups = counter();
    let counted = viewer_lookups.clone();
    let paused = Arc::new(Mutex::new(true));
    let host = rate_limited_host(&clock, paused);
    let h = Harness::new(ServiceOptions {
        clock: Some(clock.clone()),
        projects: vec![environment_project().build()],
        providers: vec![
            FakeProvider::new("gitlab")
                .capabilities_with(|capabilities| {
                    capabilities["viewedFiles"] = json!("environment");
                })
                .on_get_viewer(move |_, _| {
                    counted.fetch_add(1, SeqCst);
                    Ok("bilal".to_owned())
                })
                .on_list_change_requests(|_| Ok(page(Vec::new(), false)))
                .on_get_file_revisions(host),
        ],
        ..ServiceOptions::default()
    })
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    assert_eq!(viewer_lookups.load(SeqCst), 1);
    h.clock.adjust(11 * 60_000).await;

    // A listing is not the reader waiting on this lookup, and a failed one is held nowhere, so
    // letting it through would spawn the host's CLI on every refresh for as long as the pause lasts
    // and re-extend it each time.
    let listed = h
        .service
        .list(&input(json!({ "state": "open", "involvement": "all" })))
        .await
        .unwrap_err();
    assert!(is_operation_error(&listed));
    assert_eq!(viewer_lookups.load(SeqCst), 1);

    // The press is bounded by what the reader does, and its rows are keyed by who they are, so it is
    // asked rather than refused.
    press(&h.service, &reference, &[("src/b.ts", true)]).await;
    assert_eq!(viewer_lookups.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn does_not_ask_a_paused_host_who_is_reading_again_after_the_ask_failed() {
    let clock = TestClock::new();
    let viewer_lookups = counter();
    let counted = viewer_lookups.clone();
    let h = Harness::new(ServiceOptions {
        clock: Some(clock.clone()),
        projects: vec![environment_project().build()],
        providers: vec![
            environment_viewed_provider(
                &revisions_of(&[("src/a.ts", "blob-a")]),
                &asked_paths(),
                &no_unreadable(),
            )
            .on_get_viewer(move |_, _| {
                counted.fetch_add(1, SeqCst);
                Err(provider_error(
                    "gitlab",
                    "getViewer",
                    PullRequestProviderErrorReason::Failed,
                    "glab exited with status 1",
                ))
            })
            .on_list_change_requests(|_| Ok(page(Vec::new(), false)))
            .on_run_action(move |_| {
                Err(PullRequestProviderError {
                    retry_at: Some(clock.now_millis() + 3_600_000),
                    ..provider_error(
                        "gitlab",
                        "runAction",
                        PullRequestProviderErrorReason::RateLimited,
                        "API rate limit exceeded.",
                    )
                })
            }),
        ],
        ..ServiceOptions::default()
    })
    .await;
    let reference = environment_ref(1);

    assert!(h.service.files_viewed(&reference).await.is_err());
    assert_eq!(viewer_lookups.load(SeqCst), 1);
    assert!(
        h.service
            .run_action(&input(with_ref(&reference, json!({ "action": "merge" }))))
            .await
            .is_err()
    );

    // A failed lookup is held nowhere, so a background read let through the pause would spawn the
    // host's CLI on every refresh for as long as the pause lasted, and re-extend it each time.
    assert!(
        h.service
            .list(&input(json!({ "state": "open", "involvement": "all" })))
            .await
            .is_err()
    );
    h.clock.adjust(11 * 60_000).await;
    assert!(
        h.service
            .list(&input(json!({ "state": "open", "involvement": "all" })))
            .await
            .is_err()
    );
    assert_eq!(viewer_lookups.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn refuses_the_marks_when_the_host_could_not_be_asked_who_is_reading() {
    let answering = Arc::new(Mutex::new(true));
    let host_answering = answering.clone();
    let clock = TestClock::new();
    let h = Harness::new(ServiceOptions {
        clock: Some(clock.clone()),
        projects: vec![environment_project().build()],
        providers: vec![
            environment_viewed_provider(
                &revisions_of(&[("src/a.ts", "blob-a")]),
                &asked_paths(),
                &no_unreadable(),
            )
            .on_get_viewer(move |_, _| {
                if *host_answering.lock() {
                    Ok("bilal".to_owned())
                } else {
                    Err(provider_error(
                        "gitlab",
                        "getViewer",
                        PullRequestProviderErrorReason::Failed,
                        "glab exited with status 1",
                    ))
                }
            }),
        ],
        ..ServiceOptions::default()
    })
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    // Who is signed in is held for ten minutes, so the lookup has to come round again before a
    // failing CLI can reach the read at all.
    *answering.lock() = false;
    h.clock.adjust(11 * 60_000).await;

    // Answering these from the unnamed reader's rows would show the reader none of their own ticks,
    // and file the next press where the recovered CLI will never look for it again.
    assert!(h.service.files_viewed(&reference).await.is_err());
    assert!(
        h.service
            .set_files_viewed(&input(with_ref(
                &reference,
                json!({ "files": [{ "path": "src/b.ts", "viewed": true }] }),
            )))
            .await
            .is_err()
    );

    *answering.lock() = true;
    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "viewed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn refuses_to_track_viewed_files_on_a_host_that_does_not() {
    let h = make_service(
        vec![environment_project()],
        vec![FakeProvider::new("gitlab").on_get_files_viewed(|_| panic!("must not be called"))],
    )
    .await;
    let reference = environment_ref(1);

    let read = h.service.files_viewed(&reference).await.unwrap_err();
    let write = h
        .service
        .set_files_viewed(&input(with_ref(
            &reference,
            json!({ "files": [{ "path": "a.ts", "viewed": true }] }),
        )))
        .await
        .unwrap_err();

    assert!(is_operation_error(&read));
    assert!(is_operation_error(&write));
}
