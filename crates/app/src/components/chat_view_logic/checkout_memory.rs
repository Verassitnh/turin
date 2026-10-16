//! Port of upstream/apps/web/src/components/ChatView.logic.ts (checkout Git memory).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::state::contracts::EnvironmentId;

static SESSION_CHECKOUT_IS_REPO: LazyLock<Mutex<HashMap<String, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn checkout_is_repo_key(environment_id: &EnvironmentId, cwd: &str) -> String {
    serde_json::to_string(&(environment_id.to_string(), cwd)).expect("checkout key json")
}

pub fn remember_checkout_is_repo(environment_id: &EnvironmentId, cwd: &str, is_repo: bool) {
    SESSION_CHECKOUT_IS_REPO
        .lock()
        .expect("checkout is repo lock")
        .insert(checkout_is_repo_key(environment_id, cwd), is_repo);
}

pub fn recall_checkout_is_repo(environment_id: &EnvironmentId, cwd: Option<&str>) -> Option<bool> {
    let cwd = cwd?;
    SESSION_CHECKOUT_IS_REPO
        .lock()
        .expect("checkout is repo lock")
        .get(&checkout_is_repo_key(environment_id, cwd))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_confuse_an_environment_id_containing_the_separator_with_a_path() {
        let env = EnvironmentId::new("env").unwrap();
        remember_checkout_is_repo(&env, "a:b", false);
        assert_eq!(
            recall_checkout_is_repo(&EnvironmentId::new("env:a").unwrap(), Some("b")),
            None
        );
    }
}
