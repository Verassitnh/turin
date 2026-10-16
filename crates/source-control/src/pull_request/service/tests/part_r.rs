//! `PullRequestService.test.ts` lines 6712-6960: what a press, a push, a clear, an invalidation or
//! a merge leaves of a mark's recorded version, and what a read keeps when the host stops short.
//! The GitLab fixture is incidental; the store under test is this environment's.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{
    asked_paths, asked_sorted, environment_project, environment_ref, environment_viewed_provider,
    no_unreadable, press, revisions_of, settle, viewed_sorted,
};
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;

#[tokio::test(start_paused = true)]
async fn does_not_let_a_press_about_one_file_keep_another_files_version_alive() {
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
    h.clock.adjust(40_000).await;
    // This press asks about its own file and carries the other one forward untouched. Counting the
    // whole scope as heard from would put the first file's version back inside the window it had
    // almost aged out of, and a reader working down a long diff renews it press after press.
    press(&h.service, &reference, &[("src/b.ts", true)]).await;
    assert_eq!(
        asked_sorted(&asked),
        vec![vec!["src/a.ts".to_owned()], vec!["src/b.ts".to_owned()]]
    );

    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    h.clock.adjust(30_000).await;
    viewed_sorted(&h.service, &reference).await;
    settle().await;
    assert_eq!(asked.lock().len(), 3);

    h.clock.adjust(20_000).await;
    let caught = viewed_sorted(&h.service, &reference).await;

    assert_eq!(
        caught["files"],
        json!([
            { "path": "src/a.ts", "state": "dismissed" },
            { "path": "src/b.ts", "state": "viewed" },
        ])
    );
}

#[tokio::test(start_paused = true)]
async fn reports_a_file_pushed_to_since_it_was_cleared_as_changed() {
    let revisions = revisions_of(&[("src/a.ts", "blob-a"), ("src/b.ts", "blob-b")]);
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

    press(
        &h.service,
        &reference,
        &[("src/a.ts", true), ("src/b.ts", true)],
    )
    .await;
    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    // A push is not something the marks can hear about, so the reader asks to be re-answered.
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([
            { "path": "src/a.ts", "state": "dismissed" },
            { "path": "src/b.ts", "state": "viewed" },
        ])
    );
}

#[tokio::test(start_paused = true)]
async fn clears_a_mark_again_when_the_file_is_put_back() {
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
    press(&h.service, &reference, &[("src/a.ts", false)]).await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([])
    );
    // Unticking asks the host nothing: the row is going away whatever the head has.
    assert_eq!(asked_sorted(&asked), vec![vec!["src/a.ts".to_owned()]]);
}

#[tokio::test(start_paused = true)]
async fn keeps_a_deleted_file_cleared_which_the_head_has_no_version_of_at_all() {
    let revisions = revisions_of(&[]);
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

    press(&h.service, &reference, &[("src/gone.ts", true)]).await;
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/gone.ts", "state": "viewed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn leaves_a_mark_alone_when_the_host_could_not_say_what_the_head_has_of_it() {
    // A host answers for as much of a long change as it can read in one go. Reading the rest as
    // deleted would clear every file past the cut over a version nobody ever looked at.
    let revisions = revisions_of(&[("src/a.ts", "blob-a"), ("src/past-the-cut.ts", "blob-b")]);
    let unreadable = Arc::new(Mutex::new(HashSet::from(
        ["src/past-the-cut.ts".to_owned()],
    )));
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked_paths(),
            &unreadable,
        )],
    )
    .await;
    let reference = environment_ref(1);

    press(
        &h.service,
        &reference,
        &[("src/a.ts", true), ("src/past-the-cut.ts", true)],
    )
    .await;
    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([
            { "path": "src/a.ts", "state": "dismissed" },
            { "path": "src/past-the-cut.ts", "state": "viewed" },
        ])
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_a_file_cleared_that_the_press_could_not_learn_a_version_for() {
    // The press is the only moment a mark is given something to be measured against, and a host
    // reading as much of a long change as it can manage does not always reach the file being
    // ticked. Storing the empty version there reads as the head having nothing of the file, so the
    // first read that does reach it reports the reader's own press back to them as work to do.
    let revisions = revisions_of(&[("src/past-the-cut.ts", "blob-b")]);
    let unreadable = Arc::new(Mutex::new(HashSet::from(
        ["src/past-the-cut.ts".to_owned()],
    )));
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked_paths(),
            &unreadable,
        )],
    )
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/past-the-cut.ts", true)]).await;
    unreadable.lock().remove("src/past-the-cut.ts");
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/past-the-cut.ts", "state": "viewed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_the_version_it_last_heard_when_a_later_read_of_the_head_stops_short() {
    let revisions = revisions_of(&[("src/a.ts", "blob-a")]);
    let unreadable = no_unreadable();
    let h = make_service(
        vec![environment_project()],
        vec![environment_viewed_provider(
            &revisions,
            &asked_paths(),
            &unreadable,
        )],
    )
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "dismissed" }])
    );

    // The read behind the next answer has to stop before this file. Forgetting the version it was
    // last seen at would put the badge the reader has already been shown back to cleared, over an
    // answer that said nothing about the file either way.
    unreadable.lock().insert("src/a.ts".to_owned());
    h.clock.adjust(90_000).await;
    viewed_sorted(&h.service, &reference).await;
    settle().await;
    h.clock.adjust(20_000).await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "dismissed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn re_asks_what_the_head_has_of_a_marked_file_after_a_whole_workspace_refresh() {
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
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    // A push nobody told this environment about. No single reference has moved, so the held answer
    // goes only because the refresh is the reader asking for all of it to be read again.
    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    h.service.invalidate(&input(json!({})), false).await;

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "dismissed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn forgets_what_the_head_had_of_a_marked_file_once_a_mutation_moves_the_head() {
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
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    // Merging moves the head under the mark, and nobody asks for the refresh: the mutation is the
    // thing that knows, so it drops what it was holding rather than waiting to be told.
    *revisions.lock().get_mut("src/a.ts").unwrap() = "blob-a-again".to_owned();
    h.service
        .run_action(&input(with_ref(&reference, json!({ "action": "merge" }))))
        .await
        .unwrap();

    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "dismissed" }])
    );
}

#[tokio::test(start_paused = true)]
async fn still_reports_its_own_marks_when_the_host_will_not_say_what_the_head_has() {
    let answering = Arc::new(Mutex::new(true));
    let host_answering = answering.clone();
    let h = make_service(
        vec![environment_project()],
        vec![
            FakeProvider::new("gitlab")
                .capabilities_with(|capabilities| {
                    capabilities["viewedFiles"] = json!("environment");
                })
                .on_get_file_revisions(move |input| {
                    if !*host_answering.lock() {
                        return Err(request_failed());
                    }
                    Ok(ProviderFileRevisions {
                        revisions: input
                            .paths
                            .iter()
                            .map(|path| (path.clone(), "blob-a".to_owned()))
                            .collect(),
                        complete: None,
                    })
                }),
        ],
    )
    .await;
    let reference = environment_ref(1);

    press(&h.service, &reference, &[("src/a.ts", true)]).await;
    *answering.lock() = false;
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;

    // The rows are this environment's own. A rate limit or a signed-out CLI costs them the
    // staleness they would have carried, not the reader's whole record of what they have read.
    assert_eq!(
        viewed_sorted(&h.service, &reference).await["files"],
        json!([{ "path": "src/a.ts", "state": "viewed" }])
    );
}
