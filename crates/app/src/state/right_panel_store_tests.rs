//! Tests for `right_panel_store.rs`, ported from upstream
//! `apps/web/src/rightPanelStore.test.ts`.
//!
//! Skipped upstream cases exercise only the cut surfaces (browser, device,
//! terminal, the files explorer). They are listed in PORTING.md's Deviations
//! table. Where a cut surface was one input among kept ones, the case is
//! ported with that input replaced by a kept surface (`device` by
//! `pull-requests`, `preview` by `diff`, `files` removed). Each adapted test
//! says so in a comment.

use serde_json::{Value, json};
use turin_contracts::chat_attachment::ChatFileAttachment;
use turin_contracts::environment::ScopedThreadRef;

use super::{
    ClosedPanelTab, FileSurface, PersistedRightPanelState, ProactiveSurface, PullRequestTarget,
    RightPanelKind, RightPanelStore, RightPanelSurface, SingletonKind, ThreadPanelPresentation,
    ThreadPanelVisibility, ThreadRightPanelState, migrate_persisted_right_panel_state,
    pull_request_surface, pull_request_surface_id,
};

fn thread(environment: &str, id: &str) -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: turin_contracts::EnvironmentId::new(environment).unwrap(),
        thread_id: turin_contracts::ThreadId::new(id).unwrap(),
    }
}

fn thread_a() -> ScopedThreadRef {
    thread("env-1", "thread-A")
}

fn thread_b() -> ScopedThreadRef {
    thread("env-1", "thread-B")
}

fn pr(number: u64) -> PullRequestTarget {
    PullRequestTarget {
        environment_id: None,
        project_id: "project-a".into(),
        host: None,
        repository: "pingdotgg/t3code".into(),
        number,
        url: None,
    }
}

fn file(path: &str, reveal_line: Option<f64>, reveal_request_id: u64) -> RightPanelSurface {
    RightPanelSurface::File(FileSurface {
        id: format!("file:{path}"),
        relative_path: path.into(),
        reveal_line,
        reveal_request_id,
        attachment: None,
    })
}

fn attachment(id: &str, name: &str) -> ChatFileAttachment {
    serde_json::from_value(json!({
        "type": "file",
        "id": id,
        "name": name,
        "mimeType": "application/pdf",
        "sizeBytes": 42,
    }))
    .unwrap()
}

fn attachment_surface(attachment: &ChatFileAttachment) -> RightPanelSurface {
    RightPanelSurface::File(FileSurface {
        id: format!("attachment:{}", attachment.id.as_str()),
        relative_path: attachment.name.as_str().into(),
        reveal_line: None,
        reveal_request_id: 0,
        attachment: Some(attachment.clone()),
    })
}

fn state(store: &RightPanelStore, thread: &ScopedThreadRef) -> ThreadRightPanelState {
    store.thread_state(thread)
}

fn surface_ids(store: &RightPanelStore, thread: &ScopedThreadRef) -> Vec<String> {
    state(store, thread)
        .surfaces
        .iter()
        .map(RightPanelSurface::id)
        .collect()
}

fn closed_ids(store: &RightPanelStore) -> Vec<String> {
    store
        .closed_panel_tabs
        .iter()
        .map(|entry: &ClosedPanelTab| entry.surface.id())
        .collect()
}

fn completed_diff() -> ProactiveSurface {
    ProactiveSurface::Diff
}

fn linked_pr() -> PullRequestTarget {
    pr(42)
}

// Upstream: "records single and bulk tab closes, newest first".
// Adapted: `device` is replaced by `pull-requests`.
#[test]
fn records_single_and_bulk_tab_closes_newest_first() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    let target = pr(42);
    let pr_id = pull_request_surface_id(&target);
    store.open_file(&a, "src/app.ts", None);
    store.open(&a, SingletonKind::PullRequests);
    store.open_pull_request(&a, &target);
    store.open(&a, SingletonKind::Diff);
    store.close_surface(&a, &pr_id);
    store.close_surfaces_to_right(&a, "pull-requests");
    store.close_other_surfaces(&a, "file:src/app.ts");
    assert_eq!(
        closed_ids(&store),
        ["pull-requests", "diff", pr_id.as_str()]
    );
}

// Upstream: "reopens the active tab first after a bulk close".
// Adapted: `files` and `device` are replaced by `pull-requests` and a PR tab.
#[test]
fn reopens_the_active_tab_first_after_a_bulk_close() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    let target = pr(42);
    let pr_id = pull_request_surface_id(&target);
    store.open(&a, SingletonKind::PullRequests);
    store.open_pull_request(&a, &target);
    store.open(&a, SingletonKind::Diff);
    store.activate_surface(&a, "diff");
    store.close_all_surfaces(&a);
    assert_eq!(
        closed_ids(&store),
        ["diff", pr_id.as_str(), "pull-requests"]
    );
}

// Upstream: "does not save an incidental Files replacement when opening an
// existing file". Adapted: the `files` singleton is cut, so the case is the
// reopen itself.
#[test]
fn does_not_record_a_tab_when_reopening_an_existing_file() {
    let mut store = RightPanelStore::default();
    store.open_file(&thread_a(), "src/app.ts", None);
    store.open_file(&thread_a(), "src/app.ts", None);
    assert!(store.closed_panel_tabs.is_empty());
}

// Upstream: "records only closed tabs when a panel is hidden or a terminal tab
// closes". The terminal half is cut and dropped.
#[test]
fn records_only_closed_tabs_when_a_panel_is_hidden() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open(&a, SingletonKind::Diff);
    store.open_file(&a, "src/app.ts", None);
    store.close_surface(&a, "file:src/app.ts");
    store.close(&a);
    assert_eq!(
        store
            .closed_panel_tabs
            .iter()
            .map(|entry| (entry.thread_ref.clone(), entry.surface.id()))
            .collect::<Vec<_>>(),
        [(a.clone(), "file:src/app.ts".to_owned())]
    );
    store.toggle_visibility(&a);
    store.toggle(&a, SingletonKind::Diff);
    assert_eq!(store.closed_panel_tabs.len(), 1);
}

// Upstream: "ignores session tabs without browser snapshots and records the
// empty browser tab". Skipped: browser surfaces are cut.

// Upstream: "keeps a dismissed device hidden after another file opens". Skipped:
// device surfaces are cut.

// Upstream: "gives each host/device its own tab and preserves renamed tabs".
// Skipped: device surfaces are cut.

// Upstream: "does not collide when two hosts expose the same device id". Skipped:
// device surfaces are cut.

// Upstream: "keeps device tabs dismissed across reload after closing %s".
// Skipped: device surfaces are cut.

// Upstream: "prioritizes $surface.kind over browser and diff with $order
// delivery". Adapted: the browser tab that anchored the reconcile is dropped.
#[test]
fn prioritizes_pull_request_surfaces_over_a_completed_diff() {
    let linked = RightPanelSurface::PullRequest(pull_request_surface(&linked_pr()));
    let pull_requests = RightPanelSurface::PullRequests;
    let linked_proactive = ProactiveSurface::PullRequest(pull_request_surface(&linked_pr()));
    let cases = [
        (
            "linked pr, diff first",
            completed_diff(),
            linked_proactive.clone(),
            linked.clone(),
        ),
        (
            "linked pr, pr first",
            linked_proactive.clone(),
            completed_diff(),
            linked.clone(),
        ),
        (
            "pull-requests, diff first",
            completed_diff(),
            ProactiveSurface::PullRequests,
            pull_requests.clone(),
        ),
        (
            "pull-requests, pr first",
            ProactiveSurface::PullRequests,
            completed_diff(),
            pull_requests.clone(),
        ),
    ];
    for (label, first, second, expected) in cases {
        let mut store = RightPanelStore::default();
        let a = thread_a();
        let revision = store.user_action_revision(&a);
        // Delivery order does not matter. A diff arriving after a PR is refused.
        store.open_proactive(&a, first, revision);
        store.open_proactive(&a, second, revision);
        assert_eq!(store.active_panel_surface(&a), Some(expected), "{label}");
        store.open(&a, SingletonKind::Diff);
        assert_eq!(
            store.active_panel(&a),
            Some(RightPanelKind::Diff),
            "{label}"
        );
    }
}

// Upstream: "keeps a later $choice choice when automatic requests arrive".
// Browser and terminal choices are cut and dropped from the table.
#[test]
fn keeps_a_later_user_choice_when_automatic_requests_arrive() {
    type Choose = fn(&mut RightPanelStore, &ScopedThreadRef);
    let cases: [(&str, Choose); 6] = [
        ("file", |store, a| store.open_file(a, "src/app.ts", None)),
        ("pull request", |store, a| {
            store.open_pull_request(a, &pr(41))
        }),
        ("same tab", |store, a| store.activate_surface(a, "diff")),
        ("hide", |store, a| store.close(a)),
        ("toggle", |store, a| store.toggle(a, SingletonKind::Diff)),
        ("close all", |store, a| store.close_all_surfaces(a)),
    ];
    for (label, choose) in cases {
        let mut store = RightPanelStore::default();
        let a = thread_a();
        store.open(&a, SingletonKind::Diff);
        let revision = store.user_action_revision(&a);
        choose(&mut store, &a);
        let chosen = state(&store, &a);

        assert!(
            !store.open_proactive(&a, completed_diff(), revision),
            "{label}"
        );
        assert!(
            !store.open_proactive(
                &a,
                ProactiveSurface::PullRequest(pull_request_surface(&linked_pr())),
                revision
            ),
            "{label}"
        );
        assert!(
            !store.open_proactive(&a, ProactiveSurface::PullRequests, revision),
            "{label}"
        );
        assert_eq!(state(&store, &a), chosen, "{label}");
    }
}

#[test]
fn allows_automatic_panels_for_a_later_turn_after_a_manual_choice() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    let first_turn_revision = store.user_action_revision(&a);
    store.open_file(&a, "src/app.ts", None);
    assert!(!store.open_proactive(&a, completed_diff(), first_turn_revision));

    let next_turn_revision = store.user_action_revision(&a);
    assert!(store.open_proactive(&a, completed_diff(), next_turn_revision));
    assert_eq!(store.active_panel(&a), Some(RightPanelKind::Diff));
}

#[test]
fn keeps_manual_choices_scoped_to_their_thread_and_environment() {
    let mut store = RightPanelStore::default();
    let other_environment = thread("env-2", "thread-A");
    let revision = store.user_action_revision(&thread_a());
    store.open_file(&thread_b(), "src/app.ts", None);
    store.open_file(&other_environment, "src/app.ts", None);

    assert!(store.open_proactive(&thread_a(), completed_diff(), revision));
    assert_eq!(store.active_panel(&thread_b()), Some(RightPanelKind::File));
    assert_eq!(
        store.active_panel(&other_environment),
        Some(RightPanelKind::File)
    );
}

// Upstream: "does not treat resource reconciliation as a manual choice".
// The browser reconcile is cut and dropped.
#[test]
fn does_not_treat_resource_reconciliation_as_a_manual_choice() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open_file(&a, "src/app.ts", None);
    let revision = store.user_action_revision(&a);
    store.reconcile_file_surfaces(&a, false);

    assert!(store.open_proactive(&a, completed_diff(), revision));
    assert_eq!(store.active_panel(&a), Some(RightPanelKind::Diff));
}

// Upstream: "keeps a maximized panel per thread without counting it as a manual
// choice". Adapted: `preview` is replaced by `pull-requests`.
#[test]
fn keeps_a_maximized_panel_per_thread_without_counting_it_as_a_manual_choice() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::PullRequests);
    store.open(&thread_b(), SingletonKind::Diff);
    let revision = store.user_action_revision(&thread_a());
    store.set_maximized(&thread_a(), true);
    store.set_maximized(&thread_b(), true);
    store.set_maximized(&thread_b(), false);

    assert!(state(&store, &thread_a()).maximized);
    assert!(!state(&store, &thread_b()).maximized);
    assert_eq!(store.user_action_revision(&thread_a()), revision);
}

// Upstream: "restores a saved maximized panel during migration". Adapted: the
// saved surface is `diff`, not `browser:new`.
#[test]
fn restores_a_saved_maximized_panel_during_migration() {
    let migrated = migrate_persisted_right_panel_state(&json!({
        "byThreadKey": {
            "env-1:thread-A": {
                "isOpen": true,
                "activeSurfaceId": "diff",
                "surfaces": [{ "id": "diff", "kind": "diff" }],
                "maximized": true,
            },
        },
    }));
    let key = "env-1:thread-A";
    assert!(migrated.by_thread_key[key].maximized);
}

// Upstream: "drops the legacy singleton terminal surface during migration" and
// "upgrades saved single-session terminal surfaces to split-capable surfaces".
// Skipped: terminal surfaces are cut.

#[test]
fn upgrades_saved_file_surfaces_with_neutral_reveal_state() {
    let migrated = migrate_persisted_right_panel_state(&json!({
        "byThreadKey": {
            "env-1:thread-A": {
                "isOpen": true,
                "activeSurfaceId": "file:src/index.ts",
                "surfaces": [{ "id": "file:src/index.ts", "kind": "file", "relativePath": "src/index.ts" }],
            },
        },
    }));
    assert_eq!(
        migrated,
        PersistedRightPanelState {
            by_thread_key: [(
                "env-1:thread-A".to_owned(),
                ThreadRightPanelState {
                    is_open: true,
                    active_surface_id: Some("file:src/index.ts".into()),
                    surfaces: vec![file("src/index.ts", None, 0)],
                    maximized: false,
                },
            )]
            .into_iter()
            .collect(),
            thread_panel_visibility_by_thread_key: Default::default(),
        }
    );
}

#[test]
fn upgrades_the_legacy_singleton_pull_request_surface_to_a_reference_keyed_tab() {
    let target = pr(4909);
    let id = pull_request_surface_id(&target);
    let migrated = migrate_persisted_right_panel_state(&json!({
        "byThreadKey": {
            "env-1:thread-A": {
                "isOpen": true,
                "activeSurfaceId": "pull-request",
                "surfaces": [{
                    "id": "pull-request",
                    "kind": "pull-request",
                    "projectId": "project-a",
                    "repository": "pingdotgg/t3code",
                    "number": 4909,
                }],
            },
        },
    }));
    assert_eq!(
        migrated.by_thread_key["env-1:thread-A"],
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some(id.clone()),
            surfaces: vec![RightPanelSurface::PullRequest(pull_request_surface(
                &target
            ))],
            maximized: false,
        }
    );
    assert_eq!(id, pull_request_surface(&target).id);
}

#[test]
fn drops_the_pull_request_lists_shared_panel_so_a_restart_opens_the_page_fresh() {
    let panel_state = json!({
        "isOpen": true,
        "activeSurfaceId": pull_request_surface_id(&pr(4909)),
        "surfaces": [{
            "id": pull_request_surface_id(&pr(4909)),
            "kind": "pull-request",
            "projectId": "project-a",
            "repository": "pingdotgg/t3code",
            "number": 4909,
        }],
    });
    let migrated = migrate_persisted_right_panel_state(&json!({
        "byThreadKey": {
            "env-1:pull-requests-panel": panel_state.clone(),
            "env-1:thread-A": panel_state,
        },
    }));
    assert_eq!(migrated.by_thread_key.len(), 1);
    assert!(migrated.by_thread_key.contains_key("env-1:thread-A"));
}

// Upstream: "drops $kind with isOpen=$isOpen and falls back".
#[test]
fn drops_removed_surface_kinds_and_falls_back() {
    let cases = [("plan", true), ("agents", true), ("agents", false)];
    for (kind, is_open) in cases {
        let migrated = migrate_persisted_right_panel_state(&json!({
            "byThreadKey": {
                "env-1:thread-A": {
                    "isOpen": is_open,
                    "activeSurfaceId": kind,
                    "surfaces": [{ "id": kind, "kind": kind }],
                },
                "env-1:thread-B": {
                    "isOpen": is_open,
                    "activeSurfaceId": kind,
                    "surfaces": [{ "id": kind, "kind": kind }, { "id": "diff", "kind": "diff" }],
                },
            },
        }));
        assert_eq!(
            migrated.by_thread_key["env-1:thread-A"],
            ThreadRightPanelState::default(),
            "{kind} {is_open}"
        );
        assert_eq!(
            migrated.by_thread_key["env-1:thread-B"],
            ThreadRightPanelState {
                is_open,
                active_surface_id: Some("diff".into()),
                surfaces: vec![RightPanelSurface::Diff],
                maximized: false,
            },
            "{kind} {is_open}"
        );
        assert!(migrated.thread_panel_visibility_by_thread_key.is_empty());
    }
}

#[test]
fn persists_inline_preference_without_restoring_an_open_popover() {
    let migrated = migrate_persisted_right_panel_state(&json!({
        "threadPanelVisibilityByThreadKey": {
            "env-1:thread-A": { "inlineOpen": false, "popoverOpen": true },
            "env-1:thread-B": { "inlineOpen": true, "popoverOpen": true },
        },
    }));
    assert!(migrated.by_thread_key.is_empty());
    assert_eq!(
        migrated.thread_panel_visibility_by_thread_key,
        [(
            "env-1:thread-A".to_owned(),
            ThreadPanelVisibility {
                inline_open: false,
                popover_open: false,
            },
        )]
        .into_iter()
        .collect::<indexmap::IndexMap<_, _>>()
    );
}

#[test]
fn tracks_inline_and_popover_visibility_independently() {
    let mut store = RightPanelStore::default();
    assert!(store.thread_panel_open(&thread_a(), ThreadPanelPresentation::Inline));
    assert!(!store.thread_panel_open(&thread_a(), ThreadPanelPresentation::Popover));

    store.set_thread_panel_open(&thread_a(), ThreadPanelPresentation::Inline, false);
    store.toggle_thread_panel(&thread_a(), ThreadPanelPresentation::Popover);

    assert_eq!(
        store.thread_panel_visibility(&thread_a()),
        ThreadPanelVisibility {
            inline_open: false,
            popover_open: true,
        }
    );
    assert_eq!(
        store.thread_panel_visibility(&thread_b()),
        ThreadPanelVisibility::default()
    );
}

#[test]
fn closes_the_popover_atomically_when_the_real_right_panel_opens() {
    let mut store = RightPanelStore::default();
    store.set_thread_panel_open(&thread_a(), ThreadPanelPresentation::Popover, true);
    store.open(&thread_a(), SingletonKind::Diff);
    assert_eq!(
        store.thread_panel_visibility(&thread_a()),
        ThreadPanelVisibility::default()
    );
}

#[test]
fn keeps_an_open_popover_visible_by_promoting_it_to_inline_when_the_real_panel_closes() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open(&a, SingletonKind::Diff);
    store.set_thread_panel_open(&a, ThreadPanelPresentation::Inline, false);
    store.set_thread_panel_open(&a, ThreadPanelPresentation::Popover, true);
    store.close(&a);
    assert_eq!(
        store.thread_panel_visibility(&a),
        ThreadPanelVisibility {
            inline_open: true,
            popover_open: true,
        }
    );
}

// Upstream: "open sets the active panel for a thread". Adapted: `preview` is
// replaced by `pull-requests`.
#[test]
fn open_sets_the_active_panel_for_a_thread() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::PullRequests);
    assert_eq!(
        store.active_panel(&thread_a()),
        Some(RightPanelKind::PullRequests)
    );
    assert_eq!(store.active_panel(&thread_b()), None);
}

// Upstream: "opening a different kind keeps both surfaces and activates the new
// one". Adapted: `device` is replaced by `pull-requests`, `preview` by `diff`.
#[test]
fn opening_a_different_kind_keeps_both_surfaces_and_activates_the_new_one() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::PullRequests);
    store.open(&thread_a(), SingletonKind::Diff);
    assert_eq!(store.active_panel(&thread_a()), Some(RightPanelKind::Diff));
    assert_eq!(state(&store, &thread_a()).surfaces.len(), 2);
}

#[test]
fn reopening_an_inactive_singleton_activates_its_existing_surface() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open(&a, SingletonKind::Diff);
    store.open(&a, SingletonKind::PullRequests);
    store.open(&a, SingletonKind::Diff);

    assert_eq!(
        state(&store, &a),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("diff".into()),
            surfaces: vec![RightPanelSurface::Diff, RightPanelSurface::PullRequests],
            maximized: false,
        }
    );
}

// Upstream: "keeps files as a singleton surface" and "opens workspace-root links
// as the singleton files explorer". Skipped: the files explorer is cut. A `.`
// path is a no-op.

#[test]
fn a_workspace_root_link_changes_nothing() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open_file(&a, "README.md", None);
    store.open_file(&a, ".", None);
    assert_eq!(
        state(&store, &a).active_surface_id.as_deref(),
        Some("file:README.md")
    );
    assert_eq!(surface_ids(&store, &a), ["file:README.md"]);
}

// Upstream: "replaces the standalone explorer with peer file surfaces". Adapted:
// the `files` singleton is cut, so the case starts at the file opens.
#[test]
fn file_surfaces_are_peers_and_reopen_bumps_the_reveal_request() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open_file(&a, "src/index.ts", None);
    store.open_file(&a, "src/index.ts", None);
    store.open_file(&a, "README.md", None);

    assert_eq!(
        state(&store, &a),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("file:README.md".into()),
            surfaces: vec![file("src/index.ts", None, 2), file("README.md", None, 1),],
            maximized: false,
        }
    );
}

#[test]
fn keeps_distinct_file_paths_in_separate_tabs() {
    let cases = [
        ("generated\\", "generated"),
        ("notes/meeting ", "notes/meeting"),
        (" notes/meeting", "notes/meeting"),
    ];
    for (first, second) in cases {
        let mut store = RightPanelStore::default();
        store.open_file(&thread_a(), first, None);
        store.open_file(&thread_a(), second, None);
        assert_eq!(
            state(&store, &thread_a()).surfaces,
            vec![file(first, None, 1), file(second, None, 1)],
            "{first:?} and {second:?}"
        );
    }
}

#[test]
fn reuses_the_folder_tab_for_trailing_slash_and_drive_root_paths() {
    let cases = [
        ("docs/", "docs"),
        ("docs///", "docs"),
        ("/", "/"),
        ("C:/", "C:/"),
    ];
    for (link_path, tree_path) in cases {
        let mut store = RightPanelStore::default();
        store.open_file(&thread_a(), link_path, None);
        store.open_file(&thread_a(), tree_path, None);
        assert_eq!(
            state(&store, &thread_a()).surfaces,
            vec![file(tree_path, None, 2)],
            "{link_path:?} and {tree_path:?}"
        );
    }
}

// Upstream: "opens an attachment as a file surface without the standalone
// explorer". Adapted: the `files` singleton is cut.
#[test]
fn opens_an_attachment_as_a_file_surface() {
    let a = thread_a();
    let attached = attachment("thread-A-attachment-pdf", "report.pdf");
    let mut store = RightPanelStore::default();
    store.open_attachment(&a, attached.clone());

    assert_eq!(
        state(&store, &a),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("attachment:thread-A-attachment-pdf".into()),
            surfaces: vec![attachment_surface(&attached)],
            maximized: false,
        }
    );
}

#[test]
fn keeps_attachment_and_workspace_file_ids_disjoint() {
    let mut store = RightPanelStore::default();
    store.open_file(&thread_a(), "attachment:shared-id", None);
    store.open_attachment(&thread_a(), attachment("shared-id", "report.pdf"));
    assert_eq!(
        surface_ids(&store, &thread_a()),
        ["file:attachment:shared-id", "attachment:shared-id"]
    );
}

#[test]
fn updates_line_reveal_requests_when_reopening_a_file_surface() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open_file(&a, "src/index.ts", Some(42.0));
    store.open_file(&a, "src/index.ts", Some(87.0));
    assert_eq!(
        state(&store, &a),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("file:src/index.ts".into()),
            surfaces: vec![file("src/index.ts", Some(87.0), 2)],
            maximized: false,
        }
    );

    store.open_file(&a, "src/index.ts", None);
    assert_eq!(
        state(&store, &a),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("file:src/index.ts".into()),
            surfaces: vec![file("src/index.ts", None, 3)],
            maximized: false,
        }
    );
}

#[test]
fn normalizes_reveal_lines_to_whole_numbers_of_at_least_one() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open_file(&a, "a.ts", Some(0.0));
    assert_eq!(state(&store, &a).surfaces, vec![file("a.ts", Some(1.0), 1)]);
    store.open_file(&a, "a.ts", Some(7.9));
    assert_eq!(state(&store, &a).surfaces, vec![file("a.ts", Some(7.0), 2)]);
    store.open_file(&a, "a.ts", Some(f64::NAN));
    assert_eq!(state(&store, &a).surfaces, vec![file("a.ts", None, 3)]);
}

// Upstream: "removes persisted file surfaces when their workspace no longer
// exists". Adapted: `device` is replaced by `pull-requests`.
#[test]
fn removes_persisted_file_surfaces_when_their_workspace_no_longer_exists() {
    let mut store = RightPanelStore::default();
    store.open_file(&thread_a(), "src/index.ts", None);
    store.open(&thread_a(), SingletonKind::PullRequests);
    store.open_file(&thread_a(), "README.md", None);

    store.reconcile_file_surfaces(&thread_a(), false);

    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("pull-requests".into()),
            surfaces: vec![RightPanelSurface::PullRequests],
            maximized: false,
        }
    );

    store.open_file(&thread_b(), "conductor.json", None);
    store.reconcile_file_surfaces(&thread_b(), false);
    assert_eq!(state(&store, &thread_b()), ThreadRightPanelState::default());
}

#[test]
fn keeps_attachment_previews_when_their_workspace_is_unavailable() {
    let attached = attachment("thread-A-attachment-pdf", "report.pdf");
    let mut store = RightPanelStore::default();
    store.open_file(&thread_a(), "README.md", None);
    store.open_attachment(&thread_a(), attached.clone());

    store.reconcile_file_surfaces(&thread_a(), false);

    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("attachment:thread-A-attachment-pdf".into()),
            surfaces: vec![attachment_surface(&attached)],
            maximized: false,
        }
    );
}

// Upstream: "close hides the panel without clearing its selected surface".
// Adapted: `device` is replaced by `diff`.
#[test]
fn close_hides_the_panel_without_clearing_its_selected_surface() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::Diff);
    store.close(&thread_a());
    assert_eq!(store.active_panel(&thread_a()), None);
    assert_eq!(
        store.selected_panel_surface(&thread_a()),
        Some(RightPanelSurface::Diff)
    );
    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: false,
            active_surface_id: Some("diff".into()),
            surfaces: vec![RightPanelSurface::Diff],
            maximized: false,
        }
    );
}

#[test]
fn toggles_empty_panel_visibility_without_creating_a_surface() {
    let mut store = RightPanelStore::default();
    store.toggle_visibility(&thread_a());
    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: None,
            surfaces: vec![],
            maximized: false,
        }
    );

    store.toggle_visibility(&thread_a());
    assert!(store.by_thread_key.is_empty());
}

#[test]
fn toggle_hides_the_panel_without_discarding_the_active_surface() {
    let mut store = RightPanelStore::default();
    store.toggle(&thread_a(), SingletonKind::Diff);
    assert_eq!(store.active_panel(&thread_a()), Some(RightPanelKind::Diff));
    store.toggle(&thread_a(), SingletonKind::Diff);
    assert_eq!(store.active_panel(&thread_a()), None);
    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: false,
            active_surface_id: Some("diff".into()),
            surfaces: vec![RightPanelSurface::Diff],
            maximized: false,
        }
    );
}

// Upstream: "toggle to a different kind switches active". Adapted: `preview` and
// `device` are replaced by `pull-requests` and `diff`.
#[test]
fn toggle_to_a_different_kind_switches_active() {
    let mut store = RightPanelStore::default();
    store.toggle(&thread_a(), SingletonKind::PullRequests);
    store.toggle(&thread_a(), SingletonKind::Diff);
    assert_eq!(store.active_panel(&thread_a()), Some(RightPanelKind::Diff));
}

// Upstream: "removeThread clears persisted state". Adapted: `device` is replaced
// by `diff`.
#[test]
fn remove_thread_clears_persisted_state() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::Diff);
    store.remove_thread(&thread_a());
    assert_eq!(store.active_panel(&thread_a()), None);
}

#[test]
fn close_on_a_never_opened_thread_is_a_no_op() {
    let mut store = RightPanelStore::default();
    store.close(&thread_a());
    assert!(store.by_thread_key.is_empty());
}

// Upstream: "tracks one surface per browser session". Skipped: browser surfaces
// are cut.

#[test]
fn tracks_one_surface_per_pull_request() {
    let first = pr(4909);
    let second = pr(4910);
    let url = "https://gitlab.example.com/pingdotgg/t3code/-/merge_requests/4909";
    let mut store = RightPanelStore::default();
    store.open_pull_request(&thread_a(), &first);
    store.open_pull_request(&thread_a(), &second);
    store.open_pull_request(
        &thread_a(),
        &PullRequestTarget {
            url: Some(url.into()),
            ..first.clone()
        },
    );
    store.open_pull_request(&thread_a(), &first);

    let current = state(&store, &thread_a());
    assert_eq!(
        current
            .surfaces
            .iter()
            .map(RightPanelSurface::id)
            .collect::<Vec<_>>(),
        [
            pull_request_surface_id(&first),
            pull_request_surface_id(&second)
        ]
    );
    assert_eq!(
        current.active_surface_id,
        Some(pull_request_surface_id(&first))
    );
    match store.active_panel_surface(&thread_a()) {
        Some(RightPanelSurface::PullRequest(surface)) => {
            assert_eq!(surface.target.url.as_deref(), Some(url));
        }
        other => panic!("expected a pull request surface, got {other:?}"),
    }
    match &current.surfaces[1] {
        RightPanelSurface::PullRequest(surface) => assert_eq!(surface.target.url, None),
        other => panic!("expected a pull request surface, got {other:?}"),
    }
}

#[test]
fn keeps_matching_repository_and_number_on_different_hosts_as_separate_tabs() {
    let first = PullRequestTarget {
        host: Some("github.com".into()),
        repository: "acme/api".into(),
        number: 7,
        ..pr(7)
    };
    let second = PullRequestTarget {
        host: Some("github.example.com".into()),
        ..first.clone()
    };
    let mut store = RightPanelStore::default();
    store.open_pull_request(&thread_a(), &first);
    store.open_pull_request(&thread_a(), &second);
    assert_eq!(
        state(&store, &thread_a()).surfaces,
        vec![
            RightPanelSurface::PullRequest(pull_request_surface(&first)),
            RightPanelSurface::PullRequest(pull_request_surface(&second)),
        ]
    );
    let upper = PullRequestTarget {
        host: Some("GITHUB.COM".into()),
        ..first.clone()
    };
    assert_eq!(
        pull_request_surface_id(&upper),
        pull_request_surface_id(&first)
    );
}

#[test]
fn keeps_one_pull_request_read_from_two_servers_as_two_tabs() {
    let local = PullRequestTarget {
        environment_id: Some("local".into()),
        ..pr(4909)
    };
    let remote = PullRequestTarget {
        environment_id: Some("remote".into()),
        ..pr(4909)
    };
    let mut store = RightPanelStore::default();
    store.open_pull_request(&thread_a(), &local);
    store.open_pull_request(&thread_a(), &remote);
    assert_eq!(
        surface_ids(&store, &thread_a()),
        [
            pull_request_surface_id(&local),
            pull_request_surface_id(&remote)
        ]
    );
}

#[test]
fn keeps_the_pages_panel_tabs_reachable_when_the_set_of_connected_servers_changes() {
    // The pull-requests page keys its one shared panel by a fixed sentinel
    // environment, not by whichever capable server sorts first. A server
    // disconnecting must not move open tabs to a key nobody wrote them under.
    let panel = crate::state::right_panel_store::pull_requests_panel_ref();
    let from_server_a = PullRequestTarget {
        environment_id: Some("server-a".into()),
        project_id: "project-a".into(),
        number: 1,
        ..pr(1)
    };
    let from_server_b = PullRequestTarget {
        environment_id: Some("server-b".into()),
        project_id: "project-b".into(),
        number: 2,
        ..pr(2)
    };

    let mut store = RightPanelStore::default();
    store.open_pull_request(&panel, &from_server_a);
    store.open_pull_request(&panel, &from_server_b);

    assert_eq!(
        surface_ids(&store, &panel),
        [
            pull_request_surface_id(&from_server_a),
            pull_request_surface_id(&from_server_b),
        ]
    );

    // A ref keyed by the first capable environment would change identity when
    // server A drops out, and its lookup would find nothing.
    let while_both_connected = thread("server-a", "pull-requests-panel");
    let after_a_disconnects = thread("server-b", "pull-requests-panel");
    assert_ne!(while_both_connected, after_a_disconnects);
    assert!(store.thread_state(&after_a_disconnects).surfaces.is_empty());
}

// Upstream: "tracks one surface per terminal session", "tracks split panes and
// the active pane within a terminal surface", "tracks vertical layout for a
// terminal surface", "closing the final terminal pane removes its surface and
// closes the panel". Skipped: terminal surfaces are cut.

// Upstream: "closing the active surface activates a neighboring surface".
// Adapted: the browser neighbor is replaced by `pull-requests`, the terminal by
// `diff`.
#[test]
fn closing_the_active_surface_activates_a_neighboring_surface() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::PullRequests);
    store.open(&thread_a(), SingletonKind::Diff);
    store.close_surface(&thread_a(), "diff");

    assert_eq!(
        store
            .active_panel_surface(&thread_a())
            .map(|surface| surface.id()),
        Some("pull-requests".into())
    );
}

// Upstream: "closing the final surface closes the panel". Adapted: the terminal
// is replaced by `diff`.
#[test]
fn closing_the_final_surface_closes_the_panel() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::Diff);
    store.close_surface(&thread_a(), "diff");

    assert_eq!(state(&store, &thread_a()), ThreadRightPanelState::default());
}

// Upstream: "closing other surfaces keeps the selected surface active". Adapted:
// the browser and terminal are replaced by `pull-requests` and `diff`.
#[test]
fn closing_other_surfaces_keeps_the_selected_surface_active() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::PullRequests);
    store.open_file(&thread_a(), "src/index.ts", None);
    store.open(&thread_a(), SingletonKind::Diff);

    store.close_other_surfaces(&thread_a(), "file:src/index.ts");

    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("file:src/index.ts".into()),
            surfaces: vec![file("src/index.ts", None, 1)],
            maximized: false,
        }
    );
}

// Upstream: "closing surfaces to the right activates the selected surface when
// active was removed". Adapted: the browser is replaced by `pull-requests`.
#[test]
fn closing_surfaces_to_the_right_activates_the_selected_surface_when_active_was_removed() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::PullRequests);
    store.open_file(&thread_a(), "src/index.ts", None);
    store.open(&thread_a(), SingletonKind::Diff);

    store.close_surfaces_to_right(&thread_a(), "pull-requests");

    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: true,
            active_surface_id: Some("pull-requests".into()),
            surfaces: vec![RightPanelSurface::PullRequests],
            maximized: false,
        }
    );
}

// Upstream: "closing all surfaces closes the panel". Adapted: the browser is
// replaced by `diff`.
#[test]
fn closing_all_surfaces_closes_the_panel() {
    let mut store = RightPanelStore::default();
    store.open(&thread_a(), SingletonKind::Diff);
    store.open_file(&thread_a(), "src/index.ts", None);

    store.close_all_surfaces(&thread_a());

    assert_eq!(
        state(&store, &thread_a()),
        ThreadRightPanelState {
            is_open: false,
            active_surface_id: None,
            surfaces: vec![],
            maximized: false,
        }
    );
}

// Upstream: "does not replace a stale surface with a hidden tab, but preserves
// explicit opens" and "reconciles browser surfaces without deleting other
// surface kinds". Skipped: browser reconciliation is cut.

// Upstream: "moves a surface to a new index and keeps it there through browser
// reconciliation". Adapted: the terminal and browser are replaced by a pull
// request tab and `pull-requests`. The revision check stays.
#[test]
fn moves_a_surface_to_a_new_index_without_counting_as_a_choice() {
    let mut store = RightPanelStore::default();
    let a = thread_a();
    store.open(&a, SingletonKind::PullRequests);
    store.open_pull_request(&a, &pr(1));
    store.open(&a, SingletonKind::Diff);
    let revision = store.user_action_revision(&a);

    store.move_surface(&a, "diff", 0);
    assert_eq!(
        surface_ids(&store, &a),
        [
            "diff",
            "pull-requests",
            pull_request_surface_id(&pr(1)).as_str()
        ]
    );
    store.move_surface(&a, "diff", 2);
    assert_eq!(
        surface_ids(&store, &a),
        [
            "pull-requests",
            pull_request_surface_id(&pr(1)).as_str(),
            "diff"
        ]
    );
    store.move_surface(&a, "diff", 0);
    store.reconcile_file_surfaces(&a, true);
    assert_eq!(
        surface_ids(&store, &a),
        [
            "diff",
            "pull-requests",
            pull_request_surface_id(&pr(1)).as_str()
        ]
    );
    // Reordering is not a choice about what the panel shows.
    assert_eq!(store.user_action_revision(&a), revision);
}

// --- persisted shape ------------------------------------------------------

#[test]
fn persisted_value_round_trips_kept_surfaces_and_drops_the_shared_panel() {
    let mut store = RightPanelStore::default();
    let attached = attachment("att-1", "notes.txt");
    store.open(&thread_a(), SingletonKind::Diff);
    store.open_file(&thread_a(), "src/index.ts", Some(12.0));
    store.open_attachment(&thread_a(), attached);
    store.open_pull_request(&thread_a(), &pr(9));
    store.open_pull_request(&pull_requests_panel(), &pr(3));
    store.set_thread_panel_open(&thread_b(), ThreadPanelPresentation::Inline, false);
    store.set_maximized(&thread_a(), true);

    let saved = store.to_persisted_value();
    let mut restored = RightPanelStore::default();
    restored.load_persisted(&saved);

    assert!(
        !restored
            .by_thread_key
            .contains_key("env-1:pull-requests-panel")
    );
    assert_eq!(
        restored.by_thread_key["env-1:thread-A"],
        store.by_thread_key["env-1:thread-A"]
    );
    assert_eq!(
        restored.thread_panel_visibility_by_thread_key["env-1:thread-B"],
        ThreadPanelVisibility {
            inline_open: false,
            popover_open: false,
        }
    );
}

fn pull_requests_panel() -> ScopedThreadRef {
    crate::state::right_panel_store::pull_requests_panel_ref()
}

#[test]
fn unreadable_persisted_values_load_as_empty_maps() {
    assert_eq!(
        migrate_persisted_right_panel_state(&Value::Null),
        PersistedRightPanelState::default()
    );
    assert_eq!(
        migrate_persisted_right_panel_state(&json!("nope")),
        PersistedRightPanelState::default()
    );
}
