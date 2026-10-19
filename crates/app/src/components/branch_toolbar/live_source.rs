//! Live [`BranchBackend`] via `Commands::git_list_refs` / `git_switch_branch`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Task};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::git::{VcsListRefsInput, VcsRef};
use turin_contracts::{EnvironmentId, ThreadId};

use super::backend::{BranchBackend, RefPage, SwitchRefResult};
use super::branch_toolbar_logic::EnvironmentOption;
use crate::app_runtime::write::bus::CommandBus;
use crate::utils::normalize_search_text;

struct RefQuery {
    refs: Vec<VcsRef>,
    total_count: usize,
    pending: bool,
}

pub struct LiveBranchBackend {
    bus: CommandBus,
    pages: Rc<RefCell<HashMap<(String, String), RefQuery>>>,
}

impl LiveBranchBackend {
    pub fn new(bus: CommandBus) -> Self {
        Self {
            bus,
            pages: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn page_key(cwd: &str, query: &str) -> (String, String) {
        (cwd.to_owned(), normalize_search_text(query))
    }

    fn schedule_load(&self, cwd: &str, query: &str, cx: &mut App) {
        let key = Self::page_key(cwd, query);
        {
            let mut pages = self.pages.borrow_mut();
            let refs = pages
                .get(&key)
                .map(|page| page.refs.clone())
                .unwrap_or_default();
            pages.insert(
                key.clone(),
                RefQuery {
                    refs,
                    total_count: 0,
                    pending: true,
                },
            );
        }
        let cwd = cwd.to_owned();
        let query = query.to_owned();
        let pages = self.pages.clone();
        let bus = self.bus.clone();
        cx.spawn(async move |cx| {
            let list = match bus.commands() {
                Ok(commands) => {
                    let input = VcsListRefsInput {
                        cwd: TrimmedNonEmptyString::new(cwd.clone()).expect("cwd"),
                        query: (!query.is_empty())
                            .then(|| TrimmedNonEmptyString::new(query.clone()).expect("query")),
                        cursor: None,
                        include_matching_remote_refs: Some(true),
                        ref_kind: None,
                        refresh: Some(true),
                        limit: None,
                    };
                    Some(commands.git_list_refs(&input).await)
                }
                Err(_) => None,
            };
            let mut pages = pages.borrow_mut();
            let entry = pages.entry(key).or_insert(RefQuery {
                refs: Vec::new(),
                total_count: 0,
                pending: false,
            });
            entry.pending = false;
            match list {
                Some(Ok(result)) => {
                    entry.refs = result.refs;
                    entry.total_count = result.total_count as usize;
                }
                Some(Err(error)) => {
                    entry.refs.clear();
                    entry.total_count = 0;
                    tracing::warn!(%error.message, "list refs failed");
                }
                None => {}
            }
            cx.update(|_| ());
        })
        .detach();
    }
}

impl BranchBackend for LiveBranchBackend {
    fn list_refs(&self, cwd: &str, query: &str, _cx: &App) -> RefPage {
        let key = Self::page_key(cwd, query);
        if let Some(page) = self.pages.borrow().get(&key) {
            return RefPage {
                refs: page.refs.clone(),
                total_count: page.total_count,
                has_next_page: false,
                is_pending: page.pending,
                is_fetching_next_page: false,
            };
        }
        RefPage {
            refs: Vec::new(),
            total_count: 0,
            has_next_page: false,
            is_pending: true,
            is_fetching_next_page: false,
        }
    }

    fn load_next_refs(&self, cwd: &str, query: &str, cx: &mut App) {
        self.schedule_load(cwd, query, cx);
    }

    fn find_ref(&self, cwd: &str, name: &str, cx: &App) -> Option<VcsRef> {
        self.list_refs(cwd, name, cx)
            .refs
            .into_iter()
            .find(|r| r.name.as_str() == name)
    }

    fn switch_ref(&self, _: &str, _: &str, cx: &mut App) -> Task<Result<SwitchRefResult, String>> {
        cx.spawn(async move |_| Err("Switch branch is not wired yet.".into()))
    }

    fn create_ref(&self, _: &str, _: &str, cx: &mut App) -> Task<Result<String, String>> {
        cx.spawn(async move |_| Err("Create branch is not wired yet.".into()))
    }

    fn update_thread_metadata(
        &self,
        _: &EnvironmentId,
        _: &ThreadId,
        _: Option<String>,
        _: Option<String>,
        _: bool,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        cx.spawn(async move |_| Err("Thread branch update is not wired yet.".into()))
    }

    fn stop_thread_session(&self, _: &EnvironmentId, _: &ThreadId, _: &mut App) {}

    fn environments(&self, _: &App) -> Vec<EnvironmentOption> {
        Vec::new()
    }

    fn copy_text(&self, text: String, cx: &mut App) {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
    }
}
