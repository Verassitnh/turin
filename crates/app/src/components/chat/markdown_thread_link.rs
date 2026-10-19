//! Port of upstream/apps/web/src/components/chat/MarkdownThreadLink.tsx
//!
//! A `t3-thread://` link in chat. It shows the thread's current title, so a rename
//! reaches every message that links to it, and leads with a thread glyph the way a
//! web link leads with its favicon. The label is what the message wrote, shown only
//! when this client cannot see the thread. A click opens the thread in the app.
//!
//! Upstream reads `useThreadShell` and `useProject` atoms inside the React
//! component. Here the lookup is a [`ThreadLinkDirectory`] the shell installs as a
//! global, so the markdown renderer (which is not an entity) can resolve a link
//! while it builds the message.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Entity, Global};
use turin_contracts::ThreadId;
use turin_shared::markdown_links::extract_markdown_link_hrefs;
use turin_shared::thread_links::{
    format_thread_link, parse_thread_link_href, percent_decoded_thread_link_id,
};

use super::thread_navigation::request_open_thread;
use crate::state::store::ClientStore;

/// What a link needs from the thread it names (`useThreadShell` plus `useProject`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedThread {
    pub id: ThreadId,
    pub title: String,
    pub project_title: Option<String>,
}

/// Looks a thread up in this environment's shell index. Archived threads are not in
/// it; they still open.
pub trait ThreadLinkDirectory {
    fn thread(&self, thread_id: &ThreadId, cx: &App) -> Option<LinkedThread>;
}

/// The directory over the live client store.
pub struct ClientStoreThreadLinks {
    pub client: Entity<ClientStore>,
}

impl ThreadLinkDirectory for ClientStoreThreadLinks {
    fn thread(&self, thread_id: &ThreadId, cx: &App) -> Option<LinkedThread> {
        let state = self.client.read(cx).state();
        let view = state.shell_view.as_ref()?;
        let thread = view.thread(thread_id)?;
        Some(LinkedThread {
            id: thread.id.clone(),
            title: thread.title.clone(),
            project_title: view
                .project(&thread.project_id)
                .map(|project| project.title.clone()),
        })
    }
}

/// The app-wide directory (`cx.global::<ThreadLinkLookup>()`). Without one, links
/// show their written label.
#[derive(Clone)]
pub struct ThreadLinkLookup(pub Rc<dyn ThreadLinkDirectory>);

impl Global for ThreadLinkLookup {}

/// Installs the live directory once a client store exists.
pub fn install_thread_link_lookup(client: Entity<ClientStore>, cx: &mut App) {
    cx.set_global(ThreadLinkLookup(Rc::new(ClientStoreThreadLinks { client })));
}

/// The look and behaviour of one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedThreadLink {
    /// The id the link opens: the thread's own id when it was found, else the one written.
    pub thread_id: ThreadId,
    /// `thread?.title.trim() || props.label`.
    pub title: String,
    /// The `title` attribute: the project, or "Thread no longer available".
    pub tooltip: Option<String>,
    /// `data-markdown-copy`: how copying the rendered message writes the link.
    pub copy_markdown: String,
    /// The client can see the thread.
    pub found: bool,
}

/// Agents often percent-encode the id, so when the id as written names no thread the
/// decoded form is tried.
pub fn resolve_thread_link(
    lookup: &dyn Fn(&ThreadId) -> Option<LinkedThread>,
    written: &ThreadId,
    label: &str,
) -> ResolvedThreadLink {
    let thread = lookup(written).or_else(|| {
        percent_decoded_thread_link_id(written)
            .as_ref()
            .and_then(lookup)
    });
    let thread_id = thread
        .as_ref()
        .map_or_else(|| written.clone(), |t| t.id.clone());
    let title = thread
        .as_ref()
        .map(|thread| thread.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or(label)
        .to_owned();
    ResolvedThreadLink {
        copy_markdown: format_thread_link(thread_id.as_str(), &title),
        tooltip: match &thread {
            None => Some("Thread no longer available".to_owned()),
            Some(thread) => thread.project_title.clone(),
        },
        found: thread.is_some(),
        thread_id,
        title,
    }
}

/// [`resolve_thread_link`] over the installed global directory.
pub fn resolve_thread_link_in(cx: &App, written: &ThreadId, label: &str) -> ResolvedThreadLink {
    match cx.try_global::<ThreadLinkLookup>() {
        Some(directory) => resolve_thread_link(&|id| directory.0.thread(id, cx), written, label),
        None => resolve_thread_link(&|_| None, written, label),
    }
}

/// Every thread link in `markdown`, resolved with the label its text wrote. The
/// renderer builds this once per message, before it lays the links out.
pub fn resolve_thread_links(cx: &App, markdown: &str) -> HashMap<String, ResolvedThreadLink> {
    let mut links = HashMap::new();
    if !turin_shared::thread_links::has_thread_links(markdown) {
        return links;
    }
    for href in extract_markdown_link_hrefs(markdown) {
        if let Some(written) = parse_thread_link_href(&href) {
            links
                .entry(href)
                .or_insert_with(|| resolve_thread_link_in(cx, &written, written.as_str()));
        }
    }
    links
}

/// A click on a thread link: opens the thread the link resolves to.
pub fn open_thread_link(href: &str, cx: &mut App) -> bool {
    let Some(written) = parse_thread_link_href(href) else {
        return false;
    };
    let resolved = resolve_thread_link_in(cx, &written, written.as_str());
    request_open_thread(resolved.thread_id.as_str(), cx);
    true
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A directory over a fixed list, counting lookups by id.
    struct Fixed {
        threads: Vec<LinkedThread>,
        asked: RefCell<Vec<String>>,
    }

    impl Fixed {
        fn find(&self, thread_id: &ThreadId) -> Option<LinkedThread> {
            self.asked.borrow_mut().push(thread_id.as_str().to_owned());
            self.threads
                .iter()
                .find(|thread| thread.id == *thread_id)
                .cloned()
        }
    }

    fn thread(id: &str, title: &str, project: Option<&str>) -> LinkedThread {
        LinkedThread {
            id: ThreadId::new(id).expect("thread id"),
            title: title.to_owned(),
            project_title: project.map(str::to_owned),
        }
    }

    fn id(value: &str) -> ThreadId {
        ThreadId::new(value).expect("thread id")
    }

    fn resolve(fixed: &Fixed, written: &str, label: &str) -> ResolvedThreadLink {
        resolve_thread_link(&|thread_id| fixed.find(thread_id), &id(written), label)
    }

    #[test]
    fn shows_the_threads_current_title_and_its_project() {
        let fixed = Fixed {
            threads: vec![thread("thread-1", "  Renamed thread  ", Some("Turin"))],
            asked: RefCell::default(),
        };

        let resolved = resolve(&fixed, "thread-1", "Old title");

        assert_eq!(resolved.title, "Renamed thread");
        assert_eq!(resolved.tooltip.as_deref(), Some("Turin"));
        assert!(resolved.found);
        assert_eq!(resolved.thread_id, id("thread-1"));
        assert_eq!(
            resolved.copy_markdown,
            "[Renamed thread](t3-thread://v1/thread-1)"
        );
    }

    #[test]
    fn falls_back_to_the_written_label_when_the_title_is_blank() {
        let fixed = Fixed {
            threads: vec![thread("thread-1", "   ", None)],
            asked: RefCell::default(),
        };

        let resolved = resolve(&fixed, "thread-1", "What the message said");

        assert_eq!(resolved.title, "What the message said");
        assert_eq!(resolved.tooltip, None);
        assert!(resolved.found);
    }

    #[test]
    fn a_thread_this_client_cannot_see_keeps_its_label_and_still_opens() {
        let fixed = Fixed {
            threads: Vec::new(),
            asked: RefCell::default(),
        };

        let resolved = resolve(&fixed, "thread-gone", "Archived work");

        assert_eq!(resolved.title, "Archived work");
        assert_eq!(
            resolved.tooltip.as_deref(),
            Some("Thread no longer available")
        );
        assert!(!resolved.found);
        assert_eq!(resolved.thread_id, id("thread-gone"));
    }

    #[test]
    fn tries_the_percent_decoded_id_when_the_written_one_names_no_thread() {
        let fixed = Fixed {
            threads: vec![thread("thread:1", "Colon thread", Some("Turin"))],
            asked: RefCell::default(),
        };

        let resolved = resolve(&fixed, "thread%3A1", "label");

        assert_eq!(*fixed.asked.borrow(), vec!["thread%3A1", "thread:1"]);
        assert!(resolved.found);
        // The link opens the thread's own id, not the escaped one.
        assert_eq!(resolved.thread_id, id("thread:1"));
        assert_eq!(resolved.title, "Colon thread");
    }

    #[test]
    fn the_written_id_wins_over_its_decoded_form() {
        let fixed = Fixed {
            threads: vec![
                thread("a%20b", "Written", None),
                thread("a b", "Decoded", None),
            ],
            asked: RefCell::default(),
        };

        let resolved = resolve(&fixed, "a%20b", "label");

        assert_eq!(resolved.title, "Written");
        assert_eq!(*fixed.asked.borrow(), vec!["a%20b"]);
    }

    #[test]
    fn without_a_directory_the_link_shows_its_label() {
        let resolved = resolve_thread_link(&|_| None, &id("thread-1"), "Label");
        assert_eq!(resolved.title, "Label");
        assert!(!resolved.found);
    }
}
