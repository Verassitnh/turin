//! `PullRequestService.test.ts` lines 6528-6711: the environment-kept viewed files of a host that
//! keeps none of its own, and when a read is answered from what was last heard versus asked again.
//! The GitLab fixture is incidental; the store under test is this environment's.

use pretty_assertions::assert_eq;
use serde_json::json;

use super::{
    asked_paths, asked_sorted, environment_project, environment_ref, environment_viewed_provider,
    no_unreadable, press, revisions_of, settle, viewed_sorted,
};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn keeps_viewed_files_itself_for_a_host_that_keeps_none_of_its_own() {
    let asked = asked_paths();
    let revisions = revisions_of(&[("src/a.ts", "blob-a"), ("src/b.ts", "blob-b")]);
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked,
            &no_unreadable(),
        )],
    )
    .await;
    let reference = environment_ref(1);

    // Nothing marked is nothing to ask the host about.
    let empty = serde_json::to_value(h.service.files_viewed(&reference).await.unwrap()).unwrap();
    assert_eq!(empty, json!({ "files": [], "truncated": false }));
    assert_eq!(asked.lock().len(), 0);

    press(
        &h.service,
        &reference,
        &[("src/a.ts", true), ("src/b.ts", true)],
    )
    .await;
    let marked = viewed_sorted(&h.service, &reference).await;

    assert_eq!(
        marked["files"],
        json!([
            { "path": "src/a.ts", "state": "viewed" },
            { "path": "src/b.ts", "state": "viewed" },
        ])
    );
    assert_eq!(marked["truncated"], json!(false));
    // The marked paths alone, so the cost follows how much has been read rather than PR size, and
    // the read after the press is answered from what the press already heard.
    assert_eq!(
        asked_sorted(&asked),
        vec![vec!["src/a.ts".to_owned(), "src/b.ts".to_owned()]]
    );
}

#[tokio::test(start_paused = true)]
async fn holds_what_a_whole_change_answer_carried_so_the_next_tick_reads_nothing() {
    let asked = asked_paths();
    let head = revisions_of(&[("src/a.ts", "blob-a"), ("src/b.ts", "blob-b")]);
    let whole = head.clone();
    let asked_whole = asked.clone();
    // A host with no per-file version reads the whole change to answer for one file, which is what
    // Bitbucket's patch is, and says as much.
    let provider = environment_viewed_provider(&head, &asked, &no_unreadable())
        .on_get_file_revisions(move |input| {
            asked_whole.lock().push(input.paths.clone());
            let revisions = whole
                .lock()
                .iter()
                .map(|(path, version)| (path.clone(), version.clone()))
                .collect();
            Ok(ProviderFileRevisions {
                revisions,
                complete: Some(true),
            })
        });
    let h = make_service(vec![environment_project()], vec![provider]).await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    // A path nothing has asked about before, which is what every tick after the first names. Its
    // version came back with the first answer, so there is nothing left to read it for.
    press(&h.service, &reference, &[("src/b.ts", true)]).await;

    assert_eq!(asked_sorted(&asked), vec![vec!["src/a.ts".to_owned()]]);
    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([
            { "path": "src/a.ts", "state": "viewed" },
            { "path": "src/b.ts", "state": "viewed" },
        ])
    );

    // Still only as fresh as the read it came from: past that window the press reads again rather
    // than stamping a mark with a version the head may have moved off.
    h.clock.adjust(2 * 60_000).await;
    press(&h.service, &reference, &[("src/b.ts", true)]).await;
    assert_eq!(
        asked_sorted(&asked),
        vec![vec!["src/a.ts".to_owned()], vec!["src/b.ts".to_owned()]]
    );
}

#[tokio::test(start_paused = true)]
async fn reads_the_marks_without_asking_the_host_what_the_head_has_every_time() {
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
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    // Past the marks' own cache, so this read reaches the point where the host would be asked.
    h.clock.adjust(20_000).await;
    let marked = viewed_sorted(&h.service, &reference).await;

    assert_eq!(
        marked["files"],
        json!([{ "path": "src/a.ts", "state": "viewed" }])
    );
    assert_eq!(asked_sorted(&asked), vec![vec!["src/a.ts".to_owned()]]);
}

#[tokio::test(start_paused = true)]
async fn answers_the_marks_from_what_it_last_heard_while_it_asks_the_host_again() {
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
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    h.clock.adjust(90_000).await;
    let held = viewed_sorted(&h.service, &reference).await;
    settle().await;

    // The push is not in this answer, because waiting for the host is the thing being avoided.
    assert_eq!(
        held["files"],
        json!([{ "path": "src/a.ts", "state": "viewed" }])
    );
    assert_eq!(asked.lock().len(), 2);

    h.clock.adjust(20_000).await;
    let caught = viewed_sorted(&h.service, &reference).await;

    assert_eq!(
        caught["files"],
        json!([{ "path": "src/a.ts", "state": "dismissed" }])
    );
    // The refresh behind the previous answer is the one that heard about the push.
    assert_eq!(asked.lock().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn asks_the_host_about_a_file_it_has_not_been_asked_about_before() {
    let asked = asked_paths();
    let revisions = revisions_of(&[("src/a.ts", "blob-a"), ("src/b.ts", "blob-b")]);
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked,
            &no_unreadable(),
        )],
    )
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    h.clock.adjust(20_000).await;
    press(&h.service, &reference, &[("src/b.ts", true)]).await;
    h.clock.adjust(20_000).await;
    let marked = viewed_sorted(&h.service, &reference).await;

    assert_eq!(
        marked["files"],
        json!([
            { "path": "src/a.ts", "state": "viewed" },
            { "path": "src/b.ts", "state": "viewed" },
        ])
    );
    // The second press paid for its own file; the read that follows was already covered.
    assert_eq!(
        asked_sorted(&asked),
        vec![vec!["src/a.ts".to_owned()], vec!["src/b.ts".to_owned()]]
    );
}
