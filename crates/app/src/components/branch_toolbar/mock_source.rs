//! In-memory [`BranchBackend`] until T22 wires `turin-git`. Not an upstream
//! module. `TURIN_BRANCH_SCENARIO=worktree` makes the thread a worktree thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Task};
use serde_json::json;
use turin_contracts::git::VcsRef;
use turin_contracts::{EnvironmentId, ThreadId};

use super::backend::{BranchBackend, RefPage, SwitchRefResult};
use super::branch_toolbar_logic::EnvironmentOption;
use crate::utils::normalize_search_text;

pub struct MockBranchBackend {
    refs: Rc<RefCell<Vec<VcsRef>>>,
}

fn make_ref(
    name: &str,
    current: bool,
    is_default: bool,
    remote: Option<&str>,
    worktree: Option<&str>,
) -> VcsRef {
    serde_json::from_value(json!({
        "name": name,
        "isRemote": remote.is_some().then_some(true),
        "remoteName": remote,
        "current": current,
        "isDefault": is_default,
        "worktreePath": worktree,
    }))
    .expect("mock ref decodes")
}

impl MockBranchBackend {
    pub fn new() -> Self {
        let mut refs = vec![
            make_ref("task/U1-palette-git-toolbar", true, false, None, None),
            make_ref("main", false, true, None, None),
            make_ref(
                "task/T15-sidebar",
                false,
                false,
                None,
                Some("/Users/dev/code/turin-wt/T15-sidebar"),
            ),
            make_ref("task/T16-thread-view", false, false, None, None),
            make_ref("feature/hooks-trust-hash", false, false, None, None),
            make_ref("origin/main", false, false, Some("origin"), None),
            make_ref(
                "origin/feature/remote-only-change",
                false,
                false,
                Some("origin"),
                None,
            ),
        ];
        for index in 0..40 {
            refs.push(make_ref(
                &format!("archive/2026-0{}-experiment-{index}", index % 9 + 1),
                false,
                false,
                None,
                None,
            ));
        }
        Self {
            refs: Rc::new(RefCell::new(refs)),
        }
    }
}

impl Default for MockBranchBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl BranchBackend for MockBranchBackend {
    fn list_refs(&self, _: &str, query: &str, _: &App) -> RefPage {
        let needle = normalize_search_text(query);
        let refs: Vec<VcsRef> = self
            .refs
            .borrow()
            .iter()
            .filter(|r| needle.is_empty() || r.name.as_str().to_lowercase().contains(&needle))
            .cloned()
            .collect();
        RefPage {
            total_count: refs.len(),
            refs: super::super::branch_toolbar::branch_toolbar_logic::dedupe_remote_branches_with_local_matches(&refs),
            has_next_page: false,
            is_pending: false,
            is_fetching_next_page: false,
        }
    }

    fn load_next_refs(&self, _: &str, _: &str, _: &mut App) {}

    fn find_ref(&self, _: &str, name: &str, _: &App) -> Option<VcsRef> {
        self.refs
            .borrow()
            .iter()
            .find(|r| r.name.as_str() == name)
            .cloned()
    }

    fn switch_ref(
        &self,
        _: &str,
        ref_name: &str,
        cx: &mut App,
    ) -> Task<Result<SwitchRefResult, String>> {
        let refs = self.refs.clone();
        let ref_name = ref_name.to_owned();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let mut refs = refs.borrow_mut();
            let local = ref_name
                .strip_prefix("origin/")
                .unwrap_or(&ref_name)
                .to_owned();
            let mut found = false;
            for r in refs.iter_mut() {
                let is_target = r.name.as_str() == local || r.name.as_str() == ref_name;
                if r.is_remote != Some(true) {
                    r.current = is_target;
                    found |= is_target;
                }
            }
            if !found {
                refs.push(make_ref(&local, true, false, None, None));
            }
            Ok(SwitchRefResult {
                ref_name: Some(local),
            })
        })
    }

    fn create_ref(&self, _: &str, name: &str, cx: &mut App) -> Task<Result<String, String>> {
        let refs = self.refs.clone();
        let name = name.to_owned();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let mut refs = refs.borrow_mut();
            for r in refs.iter_mut() {
                r.current = false;
            }
            refs.insert(0, make_ref(&name, true, false, None, None));
            Ok(name)
        })
    }

    fn update_thread_metadata(
        &self,
        _: &EnvironmentId,
        _: &ThreadId,
        _: Option<String>,
        _: Option<String>,
        _: bool,
        _: &mut App,
    ) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }

    fn stop_thread_session(&self, _: &EnvironmentId, _: &ThreadId, _: &mut App) {}

    fn environments(&self, _: &App) -> Vec<EnvironmentOption> {
        Vec::new()
    }

    fn copy_text(&self, text: String, cx: &mut App) {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
    }
}
