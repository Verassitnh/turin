//! Live [`BranchBackend`] via `Commands::git_list_refs` / `git_switch_branch` /
//! `git_create_ref` and `thread.metadata.update`.
//!
//! The ref list is cached per `(checkout, query)`. The view asks for the first
//! page through [`BranchBackend::ensure_refs`] (it cannot start a load from the
//! read path) and for further pages on scroll; every landed page repaints the
//! windows. A switch or create drops the checkout's cached pages and refreshes
//! the git status the toolbar shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Task};
use turin_contracts::git::{VcsCreateRefInput, VcsListRefsInput, VcsRef, VcsSwitchRefInput};
use turin_contracts::{EnvironmentId, PositiveInt, ThreadId, TrimmedNonEmptyString};

use super::backend::{BranchBackend, RefPage, SwitchRefResult};
use super::branch_toolbar_logic::EnvironmentOption;
use crate::app_runtime::write::bus::CommandBus;
use crate::components::git_actions_control::backend::GitActionsBackend as _;
use crate::components::git_actions_control::live_source::LiveGitActionsBackend;
use crate::utils::normalize_search_text;

/// `VCS_REF_LIST_LIMIT`.
const REF_LIST_LIMIT: u64 = 100;

#[derive(Default)]
struct RefQuery {
    refs: Vec<VcsRef>,
    total_count: usize,
    next_cursor: Option<u64>,
    /// The first page is loading (or reloading; `refs` keeps the old list).
    pending: bool,
    /// A later page is loading.
    fetching_next: bool,
}

pub struct LiveBranchBackend {
    bus: CommandBus,
    git: LiveGitActionsBackend,
    pages: Rc<RefCell<HashMap<(String, String), RefQuery>>>,
}

impl LiveBranchBackend {
    pub fn new(bus: CommandBus, git: LiveGitActionsBackend) -> Self {
        Self {
            bus,
            git,
            pages: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn page_key(cwd: &str, query: &str) -> (String, String) {
        (cwd.to_owned(), normalize_search_text(query))
    }

    /// Loads the first page (`cursor == None`, replacing the list) or the page
    /// at `cursor` (appending). A load already in flight for the key wins.
    fn schedule_load(&self, cwd: &str, query: &str, cursor: Option<u64>, cx: &mut App) {
        let key = Self::page_key(cwd, query);
        {
            let mut pages = self.pages.borrow_mut();
            let entry = pages.entry(key.clone()).or_default();
            if entry.pending || entry.fetching_next {
                return;
            }
            if cursor.is_some() {
                entry.fetching_next = true;
            } else {
                entry.pending = true;
            }
        }
        let input = (|| {
            Some(VcsListRefsInput {
                cwd: TrimmedNonEmptyString::new(cwd).ok()?,
                query: TrimmedNonEmptyString::new(query).ok(),
                cursor,
                include_matching_remote_refs: Some(true),
                ref_kind: None,
                refresh: Some(cursor.is_none()),
                limit: PositiveInt::new(REF_LIST_LIMIT),
            })
        })();
        let pages = self.pages.clone();
        let Some(input) = input else {
            let mut pages = pages.borrow_mut();
            if let Some(entry) = pages.get_mut(&key) {
                entry.pending = false;
                entry.fetching_next = false;
            }
            return;
        };
        let task = self.bus.task(cx, move |commands| async move {
            commands.git_list_refs(&input).await
        });
        cx.spawn(async move |cx| {
            let outcome = task.await;
            {
                let mut pages = pages.borrow_mut();
                // A switch may have dropped the entry while the load ran; its
                // result is stale then.
                if let Some(entry) = pages.get_mut(&key) {
                    entry.pending = false;
                    entry.fetching_next = false;
                    match outcome {
                        Ok(result) => {
                            if cursor.is_none() {
                                entry.refs.clear();
                            }
                            for incoming in result.refs {
                                if !entry.refs.iter().any(|known| known.name == incoming.name) {
                                    entry.refs.push(incoming);
                                }
                            }
                            entry.total_count = result.total_count as usize;
                            entry.next_cursor = result.next_cursor;
                        }
                        Err(error) => {
                            if cursor.is_none() {
                                entry.refs.clear();
                                entry.total_count = 0;
                            }
                            entry.next_cursor = None;
                            tracing::warn!(message = %error.message, "list refs failed");
                        }
                    }
                }
            }
            cx.update(|cx| cx.refresh_windows());
        })
        .detach();
    }
}

/// What the view reads for one cached query (`usePaginatedBranches`).
fn ref_page(page: Option<&RefQuery>) -> RefPage {
    match page {
        Some(page) => RefPage {
            refs: page.refs.clone(),
            total_count: page.total_count,
            has_next_page: page.next_cursor.is_some(),
            // A reload keeps the old list usable.
            is_pending: page.pending && page.refs.is_empty(),
            is_fetching_next_page: page.fetching_next,
        },
        // Not asked for yet: pending until `ensure_refs` runs.
        None => RefPage {
            is_pending: true,
            ..RefPage::default()
        },
    }
}

impl BranchBackend for LiveBranchBackend {
    fn list_refs(&self, cwd: &str, query: &str, _cx: &App) -> RefPage {
        ref_page(self.pages.borrow().get(&Self::page_key(cwd, query)))
    }

    fn ensure_refs(&self, cwd: &str, query: &str, cx: &mut App) {
        if !self
            .pages
            .borrow()
            .contains_key(&Self::page_key(cwd, query))
        {
            self.schedule_load(cwd, query, None, cx);
        }
    }

    fn reload_refs(&self, cwd: &str, query: &str, cx: &mut App) {
        self.schedule_load(cwd, query, None, cx);
    }

    fn load_next_refs(&self, cwd: &str, query: &str, cx: &mut App) {
        let cursor = self
            .pages
            .borrow()
            .get(&Self::page_key(cwd, query))
            .and_then(|page| page.next_cursor);
        if let Some(cursor) = cursor {
            self.schedule_load(cwd, query, Some(cursor), cx);
        }
    }

    fn find_ref(&self, cwd: &str, name: &str, _cx: &App) -> Option<VcsRef> {
        // Any cached page of the checkout may know the ref (the active ref is
        // found through the unfiltered list).
        self.pages
            .borrow()
            .iter()
            .filter(|((key, _), _)| key == cwd)
            .find_map(|(_, page)| page.refs.iter().find(|r| r.name.as_str() == name).cloned())
    }

    fn switch_ref(
        &self,
        cwd: &str,
        ref_name: &str,
        cx: &mut App,
    ) -> Task<Result<SwitchRefResult, String>> {
        let Ok(input) = (|| -> Result<VcsSwitchRefInput, String> {
            Ok(VcsSwitchRefInput {
                cwd: TrimmedNonEmptyString::new(cwd).map_err(|error| error.to_string())?,
                ref_name: TrimmedNonEmptyString::new(ref_name)
                    .map_err(|error| error.to_string())?,
            })
        })() else {
            return Task::ready(Err("A ref name is required.".into()));
        };
        let task = self.bus.task(cx, move |commands| async move {
            commands.git_switch_branch(input).await
        });
        let (pages, git, cwd) = (self.pages.clone(), self.git.clone(), cwd.to_owned());
        cx.spawn(async move |cx| {
            let outcome = task.await;
            pages.borrow_mut().retain(|(key, _), _| *key != cwd);
            cx.update(|cx| {
                git.refresh_status(&cwd, cx);
                cx.refresh_windows();
            });
            outcome
                .map(|result| SwitchRefResult {
                    ref_name: result.ref_name.map(|name| name.as_str().to_owned()),
                })
                .map_err(|error| error.message)
        })
    }

    fn create_ref(&self, cwd: &str, name: &str, cx: &mut App) -> Task<Result<String, String>> {
        let Ok(input) = (|| -> Result<VcsCreateRefInput, String> {
            Ok(VcsCreateRefInput {
                cwd: TrimmedNonEmptyString::new(cwd).map_err(|error| error.to_string())?,
                ref_name: TrimmedNonEmptyString::new(name).map_err(|error| error.to_string())?,
                switch_ref: Some(true),
            })
        })() else {
            return Task::ready(Err("A ref name is required.".into()));
        };
        let task = self.bus.task(cx, move |commands| async move {
            commands.git_create_ref(input).await
        });
        let (pages, git, cwd) = (self.pages.clone(), self.git.clone(), cwd.to_owned());
        cx.spawn(async move |cx| {
            let outcome = task.await;
            pages.borrow_mut().retain(|(key, _), _| *key != cwd);
            cx.update(|cx| {
                git.refresh_status(&cwd, cx);
                cx.refresh_windows();
            });
            outcome
                .map(|result| result.ref_name.as_str().to_owned())
                .map_err(|error| error.message)
        })
    }

    fn update_thread_metadata(
        &self,
        _: &EnvironmentId,
        thread_id: &ThreadId,
        branch: Option<String>,
        worktree_path: Option<String>,
        expected_empty: bool,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        let thread_id = thread_id.clone();
        let task = self.bus.task(cx, move |commands| async move {
            commands
                .update_thread_branch(&thread_id, branch, Some(worktree_path), expected_empty)
                .await
        });
        cx.spawn(async move |_| task.await.map_err(|error| error.message))
    }

    fn stop_thread_session(&self, _: &EnvironmentId, thread_id: &ThreadId, _: &mut App) {
        let thread_id = thread_id.clone();
        self.bus.spawn(
            move |commands| async move { commands.stop_thread(&thread_id).await },
            |result| {
                if let Err(error) = result {
                    tracing::warn!(message = %error.message, "stopping the thread session failed");
                }
            },
        );
    }

    fn environments(&self, _: &App) -> Vec<EnvironmentOption> {
        Vec::new()
    }

    fn copy_text(&self, text: String, cx: &mut App) {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn make_ref(name: &str) -> VcsRef {
        serde_json::from_value(json!({
            "name": name, "current": false, "isDefault": false,
        }))
        .expect("ref decodes")
    }

    #[test]
    fn an_unrequested_query_is_pending_so_the_view_asks_for_it() {
        let page = ref_page(None);
        assert!(page.is_pending);
        assert!(page.refs.is_empty());
        assert!(!page.has_next_page);
    }

    #[test]
    fn a_loaded_page_reports_its_cursor_as_the_next_page() {
        let loaded = RefQuery {
            refs: vec![make_ref("main")],
            total_count: 150,
            next_cursor: Some(100),
            ..RefQuery::default()
        };
        let page = ref_page(Some(&loaded));
        assert!(!page.is_pending);
        assert!(page.has_next_page);
        assert_eq!(page.total_count, 150);

        let last = RefQuery {
            next_cursor: None,
            ..loaded
        };
        assert!(!ref_page(Some(&last)).has_next_page);
    }

    #[test]
    fn a_reload_keeps_the_old_list_open_but_a_first_load_does_not() {
        let reloading = RefQuery {
            refs: vec![make_ref("main")],
            pending: true,
            ..RefQuery::default()
        };
        assert!(!ref_page(Some(&reloading)).is_pending);
        let first = RefQuery {
            pending: true,
            ..RefQuery::default()
        };
        assert!(ref_page(Some(&first)).is_pending);
    }
}
