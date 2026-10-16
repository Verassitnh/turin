//! Port of `resolveThreadMetadataUpdateForNextTurn` and
//! `shouldWriteThreadErrorToCurrentServerThread`.

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::model::ProviderOptionSelection;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::{EnvironmentId, ThreadId};

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMetadataUpdate {
    pub model_selection: Option<ModelSelection>,
    /// Present when the checkout branch replaced the thread branch.
    pub branch: Option<String>,
    /// `None` omits the key. `Some(None)` is the `worktreePath: null` upstream
    /// writes in the same patch as `branch`.
    pub worktree_path: Option<Option<String>>,
}

pub fn resolve_thread_metadata_update_for_next_turn(
    current_model_selection: &ModelSelection,
    next_model_selection: Option<&ModelSelection>,
    current_branch: Option<&str>,
    next_branch: Option<&str>,
) -> Option<ThreadMetadataUpdate> {
    let next_model = next_model_selection.filter(|next| {
        next.model != current_model_selection.model
            || next.instance_id != current_model_selection.instance_id
            || options_json(&next.options) != options_json(&current_model_selection.options)
    });
    let branch = next_branch
        .filter(|next| Some(*next) != current_branch)
        .map(str::to_owned);
    if next_model.is_none() && branch.is_none() {
        return None;
    }
    Some(ThreadMetadataUpdate {
        model_selection: next_model.cloned(),
        worktree_path: branch.as_ref().map(|_| None),
        branch,
    })
}

fn options_json(options: &Option<Vec<ProviderOptionSelection>>) -> String {
    match options {
        None => "null".to_owned(),
        Some(options) => serde_json::to_string(options).expect("model options json"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerThreadIdentity<'a> {
    pub environment_id: &'a EnvironmentId,
    pub id: &'a ThreadId,
}

pub fn should_write_thread_error_to_current_server_thread(
    server_thread: Option<ServerThreadIdentity<'_>>,
    route_thread_ref: &ScopedThreadRef,
    target_thread_id: &ThreadId,
) -> bool {
    let Some(server_thread) = server_thread else {
        return false;
    };
    target_thread_id == &route_thread_ref.thread_id
        && server_thread.environment_id == &route_thread_ref.environment_id
        && server_thread.id == target_thread_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::fixtures;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::provider_instance::ProviderInstanceId;

    fn selection() -> ModelSelection {
        ModelSelection {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            model: TrimmedNonEmptyString::new("gpt-5.4").unwrap(),
            options: None,
        }
    }

    #[test]
    fn updates_a_stale_local_thread_branch_to_the_active_checkout() {
        assert_eq!(
            resolve_thread_metadata_update_for_next_turn(
                &selection(),
                None,
                Some("feature/thread"),
                Some("feature/checkout"),
            ),
            Some(ThreadMetadataUpdate {
                model_selection: None,
                branch: Some("feature/checkout".to_owned()),
                worktree_path: Some(None),
            })
        );
    }

    #[test]
    fn does_not_write_metadata_when_the_model_and_branch_are_unchanged() {
        let model = selection();
        assert_eq!(
            resolve_thread_metadata_update_for_next_turn(
                &model,
                Some(&model),
                Some("feature/current"),
                Some("feature/current"),
            ),
            None
        );
    }

    #[test]
    fn requires_the_environment_route_thread_and_target_thread_to_match() {
        let environment_id = fixtures::env("environment-local");
        let thread_id = fixtures::thread_id("thread-1");
        let route = ScopedThreadRef {
            environment_id: environment_id.clone(),
            thread_id: thread_id.clone(),
        };
        assert!(should_write_thread_error_to_current_server_thread(
            Some(ServerThreadIdentity {
                environment_id: &environment_id,
                id: &thread_id,
            }),
            &route,
            &thread_id,
        ));
        assert!(!should_write_thread_error_to_current_server_thread(
            None, &route, &thread_id,
        ));
    }
}
