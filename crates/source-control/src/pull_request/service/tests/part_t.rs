//! `PullRequestService.test.ts` lines 7650-7707: agent credits on a merge or an armed merge. The
//! table upstream resolves the environment and project settings into one value; that resolution
//! belongs to the settings layer, so only the service's part is ported here: the resolved value it
//! is handed is the one the provider gets.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;
use turin_contracts::pull_request::PullRequestViewerPermissions;

use super::summary_of;
use crate::pull_request::service::test_support::*;

#[tokio::test(start_paused = true)]
async fn hands_a_merge_and_an_armed_merge_the_credit_setting_the_project_resolves_to() {
    for (action, resolved) in [
        ("merge", true),
        ("merge", false),
        ("enable-auto-merge", true),
        ("enable-auto-merge", false),
    ] {
        let calls = Arc::new(Mutex::new(Vec::<bool>::new()));
        let recorded = calls.clone();
        let h = Harness::new(ServiceOptions {
            projects: vec![project("p1", "web", "/w", "acme/web").build()],
            remove_agent_credits_on_merge: HashMap::from([("p1".to_owned(), resolved)]),
            providers: vec![
                FakeProvider::new("github")
                    .capabilities_with(|capabilities| {
                        capabilities["actions"] = json!(["merge", "enable-auto-merge"]);
                    })
                    .on_get_change_request_summary(|_| {
                        Ok(summary_of(
                            &change_request(1, "2026-07-02T00:00:00Z"),
                            turin_contracts::pull_request::PullRequestState::Open,
                        ))
                    })
                    .on_get_viewer_permissions(|_| {
                        Ok(
                            serde_json::from_value::<PullRequestViewerPermissions>(json!({
                                "actions": ["merge", "enable-auto-merge"],
                                "comment": true,
                                "resolve": true,
                                "verdicts": ["comment"],
                                "requestReviewers": false,
                            }))
                            .unwrap(),
                        )
                    })
                    .on_run_action(move |input| {
                        recorded.lock().push(input.remove_agent_credits_on_merge);
                        Ok(())
                    }),
            ],
            ..ServiceOptions::default()
        })
        .await;

        h.service
            .run_action(&input(json!({
                "projectId": "p1",
                "repository": "acme/web",
                "number": 1,
                "action": action,
            })))
            .await
            .unwrap();

        assert_eq!(
            *calls.lock(),
            vec![resolved],
            "{action} with setting {resolved}"
        );
    }
}
