//! Port of upstream/apps/web/src/components/chat/externalLinkContextMenu.ts
//!
//! The right-click menu on a link in chat: open in the integrated browser, open in the system
//! browser, copy the link, and (for pull request links) link it to or unlink it from the thread.

use url::Url;

use super::super::sidebar::sidebar_logic::ContextMenuItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalLinkContextMenuAction {
    OpenInPreview,
    OpenExternal,
    CopyLink,
    LinkToThread,
    UnlinkFromThread,
}

impl ExternalLinkContextMenuAction {
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenInPreview => "open-in-preview",
            Self::OpenExternal => "open-external",
            Self::CopyLink => "copy-link",
            Self::LinkToThread => "link-to-thread",
            Self::UnlinkFromThread => "unlink-from-thread",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [
            Self::OpenInPreview,
            Self::OpenExternal,
            Self::CopyLink,
            Self::LinkToThread,
            Self::UnlinkFromThread,
        ]
        .into_iter()
        .find(|action| action.id() == id)
    }

    fn failure_operation(self) -> ExternalLinkContextMenuFailureOperation {
        match self {
            Self::OpenInPreview => ExternalLinkContextMenuFailureOperation::OpenLinkInPreview,
            Self::OpenExternal => ExternalLinkContextMenuFailureOperation::OpenLinkExternal,
            Self::CopyLink => ExternalLinkContextMenuFailureOperation::CopyLink,
            Self::LinkToThread => ExternalLinkContextMenuFailureOperation::LinkPullRequestToThread,
            Self::UnlinkFromThread => {
                ExternalLinkContextMenuFailureOperation::UnlinkPullRequestFromThread
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalLinkContextMenuFailureOperation {
    ShowLinkContextMenu,
    OpenLinkInPreview,
    OpenLinkExternal,
    CopyLink,
    LinkPullRequestToThread,
    UnlinkPullRequestFromThread,
}

impl ExternalLinkContextMenuFailureOperation {
    pub fn id(self) -> &'static str {
        match self {
            Self::ShowLinkContextMenu => "show-link-context-menu",
            Self::OpenLinkInPreview => "open-link-in-preview",
            Self::OpenLinkExternal => "open-link-external",
            Self::CopyLink => "copy-link",
            Self::LinkPullRequestToThread => "link-pull-request-to-thread",
            Self::UnlinkPullRequestFromThread => "unlink-pull-request-from-thread",
        }
    }
}

/// The integrated browser is not always there to offer, but the other two answers hold wherever
/// a link does. Dropping the whole menu with the one item that cannot be honoured is what left a
/// right-click on a link showing the platform's cut-and-paste menu instead of a way to copy it.
pub fn external_link_context_menu_items(
    can_open_in_preview: bool,
    thread_link_action: Option<ExternalLinkContextMenuAction>,
) -> Vec<ContextMenuItem> {
    let mut items = Vec::new();
    if let Some(action) = thread_link_action {
        items.push(ContextMenuItem::new(
            action.id(),
            if action == ExternalLinkContextMenuAction::LinkToThread {
                "Link to thread"
            } else {
                "Unlink from thread"
            },
        ));
    }
    if can_open_in_preview {
        items.push(ContextMenuItem::new(
            "open-in-preview",
            "Open in integrated browser",
        ));
    }
    items.push(ContextMenuItem::new(
        "open-external",
        "Open in system browser",
    ));
    items.push(ContextMenuItem::new("copy-link", "Copy Link"));
    items
}

fn resolve_external_web_link(href: Option<&str>) -> Option<Url> {
    let href = href.filter(|href| !href.is_empty())?;
    let candidate = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    let url = Url::parse(&candidate).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// The link's normalized href, only for a web (`http:` / `https:`) destination.
pub fn resolve_external_web_link_href(href: Option<&str>) -> Option<String> {
    resolve_external_web_link(href).map(|url| url.as_str().to_owned())
}

pub fn resolve_external_web_link_host(href: Option<&str>) -> Option<String> {
    resolve_external_web_link(href)
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
}

/// What the menu needs from its host. Failures are `Err(cause)`.
#[allow(async_fn_in_trait)]
pub trait ExternalLinkContextMenuHost {
    type Error;
    /// The chosen action, or `None` when the menu was dismissed.
    async fn show_context_menu(
        &self,
        items: &[ContextMenuItem],
        position: (f32, f32),
    ) -> Result<Option<ExternalLinkContextMenuAction>, Self::Error>;
    async fn open_in_preview(&self, href: &str) -> Result<(), Self::Error>;
    async fn open_external(&self, href: &str) -> Result<(), Self::Error>;
    async fn copy_link(&self, href: &str) -> Result<(), Self::Error>;
    /// Absent upstream callers simply do nothing.
    async fn update_thread_link(&self, _href: &str, _linked: bool) -> Result<(), Self::Error> {
        Ok(())
    }
    fn report_failure(
        &self,
        operation: ExternalLinkContextMenuFailureOperation,
        cause: Self::Error,
    );
}

pub struct ShowExternalLinkContextMenuOptions<'a> {
    pub href: &'a str,
    pub position: (f32, f32),
    /// Absent means yes, which is what every caller before the browser could be missing meant.
    pub can_open_in_preview: bool,
    pub thread_link_action: Option<ExternalLinkContextMenuAction>,
}

/// `showExternalLinkContextMenu`.
pub async fn show_external_link_context_menu<H: ExternalLinkContextMenuHost>(
    host: &H,
    options: ShowExternalLinkContextMenuOptions<'_>,
) {
    let items =
        external_link_context_menu_items(options.can_open_in_preview, options.thread_link_action);
    let action = match host.show_context_menu(&items, options.position).await {
        Ok(action) => action,
        Err(cause) => {
            host.report_failure(
                ExternalLinkContextMenuFailureOperation::ShowLinkContextMenu,
                cause,
            );
            return;
        }
    };
    let href = options.href;
    let result = match action {
        Some(ExternalLinkContextMenuAction::OpenInPreview) => host.open_in_preview(href).await,
        Some(ExternalLinkContextMenuAction::OpenExternal) => host.open_external(href).await,
        Some(ExternalLinkContextMenuAction::CopyLink) => host.copy_link(href).await,
        Some(
            action @ (ExternalLinkContextMenuAction::LinkToThread
            | ExternalLinkContextMenuAction::UnlinkFromThread),
        ) => {
            host.update_thread_link(href, action == ExternalLinkContextMenuAction::LinkToThread)
                .await
        }
        None => Ok(()),
    };
    if let (Err(cause), Some(action)) = (result, action) {
        host.report_failure(action.failure_operation(), cause);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use futures::executor::block_on;

    type ShownMenu = (Vec<(String, String)>, (f32, f32));

    /// `createHarness`: records every call; `fail` makes the named call reject.
    #[derive(Default)]
    struct Harness {
        selection: Option<ExternalLinkContextMenuAction>,
        calls: RefCell<Vec<String>>,
        menus: RefCell<Vec<ShownMenu>>,
        failures: RefCell<Vec<(&'static str, &'static str)>>,
        fail: Option<&'static str>,
        menu_fails: bool,
    }

    impl Harness {
        fn selecting(selection: Option<ExternalLinkContextMenuAction>) -> Self {
            Self {
                selection,
                ..Default::default()
            }
        }

        fn call(&self, name: &str, arg: String) -> Result<(), &'static str> {
            self.calls.borrow_mut().push(format!("{name}:{arg}"));
            if self.fail == Some(name) {
                Err("rejected")
            } else {
                Ok(())
            }
        }

        fn called(&self, prefix: &str) -> bool {
            self.calls
                .borrow()
                .iter()
                .any(|call| call.starts_with(prefix))
        }
    }

    impl ExternalLinkContextMenuHost for Harness {
        type Error = &'static str;

        async fn show_context_menu(
            &self,
            items: &[ContextMenuItem],
            position: (f32, f32),
        ) -> Result<Option<ExternalLinkContextMenuAction>, Self::Error> {
            self.menus.borrow_mut().push((
                items
                    .iter()
                    .map(|item| (item.id.clone(), item.label.clone()))
                    .collect(),
                position,
            ));
            if self.menu_fails {
                return Err("menu unavailable");
            }
            Ok(self.selection)
        }

        async fn open_in_preview(&self, href: &str) -> Result<(), Self::Error> {
            self.call("openInPreview", href.to_owned())
        }

        async fn open_external(&self, href: &str) -> Result<(), Self::Error> {
            self.call("openExternal", href.to_owned())
        }

        async fn copy_link(&self, href: &str) -> Result<(), Self::Error> {
            self.call("copyLink", href.to_owned())
        }

        async fn update_thread_link(&self, href: &str, linked: bool) -> Result<(), Self::Error> {
            self.call("updateThreadLink", format!("{href},{linked}"))
        }

        fn report_failure(
            &self,
            operation: ExternalLinkContextMenuFailureOperation,
            cause: Self::Error,
        ) {
            self.failures.borrow_mut().push((operation.id(), cause));
        }
    }

    fn show(
        harness: &Harness,
        href: &str,
        position: (f32, f32),
        preview: bool,
        link: Option<ExternalLinkContextMenuAction>,
    ) {
        block_on(show_external_link_context_menu(
            harness,
            ShowExternalLinkContextMenuOptions {
                href,
                position,
                can_open_in_preview: preview,
                thread_link_action: link,
            },
        ));
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(id, label)| ((*id).to_owned(), (*label).to_owned()))
            .collect()
    }

    #[test]
    fn offers_both_open_actions_and_copy_link() {
        let harness = Harness::selecting(None);
        show(
            &harness,
            "https://example.com/docs?topic=menus#copy",
            (12.0, 24.0),
            true,
            None,
        );
        assert_eq!(
            harness.menus.borrow()[0],
            (
                pairs(&[
                    ("open-in-preview", "Open in integrated browser"),
                    ("open-external", "Open in system browser"),
                    ("copy-link", "Copy Link"),
                ]),
                (12.0, 24.0)
            )
        );
        assert!(harness.calls.borrow().is_empty());
    }

    #[test]
    fn still_offers_the_links_own_actions_where_the_integrated_browser_cannot_be_opened() {
        let harness = Harness::selecting(None);
        show(
            &harness,
            "https://github.com/pingdotgg/t3code/pull/6169",
            (4.0, 8.0),
            false,
            None,
        );
        assert_eq!(
            harness.menus.borrow()[0],
            (
                pairs(&[
                    ("open-external", "Open in system browser"),
                    ("copy-link", "Copy Link"),
                ]),
                (4.0, 8.0)
            )
        );
    }

    #[test]
    fn copies_the_exact_destination_without_opening_it() {
        let harness = Harness::selecting(Some(ExternalLinkContextMenuAction::CopyLink));
        let href = "https://example.com/docs?topic=menus#copy";
        show(&harness, href, (1.0, 2.0), true, None);
        assert_eq!(*harness.calls.borrow(), vec![format!("copyLink:{href}")]);
    }

    #[test]
    fn offers_and_runs_the_thread_link_actions() {
        for (action, label, linked) in [
            (
                ExternalLinkContextMenuAction::LinkToThread,
                "Link to thread",
                true,
            ),
            (
                ExternalLinkContextMenuAction::UnlinkFromThread,
                "Unlink from thread",
                false,
            ),
        ] {
            let harness = Harness::selecting(Some(action));
            let href = "https://github.com/pingdotgg/t3code/pull/42";
            show(&harness, href, (1.0, 2.0), true, Some(action));
            let (items, position) = harness.menus.borrow()[0].clone();
            assert!(items.contains(&(action.id().to_owned(), label.to_owned())));
            assert_eq!(items[0].0, action.id(), "the thread action leads the menu");
            assert_eq!(position, (1.0, 2.0));
            assert_eq!(
                *harness.calls.borrow(),
                vec![format!("updateThreadLink:{href},{linked}")]
            );
        }
    }

    #[test]
    fn preserves_the_open_actions() {
        for (selection, callback) in [
            (
                ExternalLinkContextMenuAction::OpenInPreview,
                "openInPreview",
            ),
            (ExternalLinkContextMenuAction::OpenExternal, "openExternal"),
        ] {
            let harness = Harness::selecting(Some(selection));
            let href = "https://example.com/docs";
            show(&harness, href, (1.0, 2.0), true, None);
            assert_eq!(*harness.calls.borrow(), vec![format!("{callback}:{href}")]);
            assert!(!harness.called("copyLink"));
        }
    }

    #[test]
    fn reports_the_selected_action_when_it_fails() {
        let harness = Harness {
            fail: Some("copyLink"),
            ..Harness::selecting(Some(ExternalLinkContextMenuAction::CopyLink))
        };
        show(&harness, "https://example.com/docs", (1.0, 2.0), true, None);
        assert_eq!(*harness.failures.borrow(), vec![("copy-link", "rejected")]);
    }

    #[test]
    fn reports_the_menu_operation_when_the_native_menu_cannot_be_shown() {
        let harness = Harness {
            menu_fails: true,
            ..Harness::selecting(None)
        };
        show(&harness, "https://example.com/docs", (1.0, 2.0), true, None);
        assert_eq!(
            *harness.failures.borrow(),
            vec![("show-link-context-menu", "menu unavailable")]
        );
        assert!(harness.calls.borrow().is_empty());
    }

    #[test]
    fn reports_a_failed_open_action() {
        for (selection, callback, operation) in [
            (
                ExternalLinkContextMenuAction::OpenInPreview,
                "openInPreview",
                "open-link-in-preview",
            ),
            (
                ExternalLinkContextMenuAction::OpenExternal,
                "openExternal",
                "open-link-external",
            ),
        ] {
            let harness = Harness {
                fail: Some(callback),
                ..Harness::selecting(Some(selection))
            };
            show(&harness, "https://example.com/docs", (1.0, 2.0), true, None);
            assert_eq!(*harness.failures.borrow(), vec![(operation, "rejected")]);
        }
    }

    #[test]
    fn reports_a_failed_thread_link_action() {
        let harness = Harness {
            fail: Some("updateThreadLink"),
            ..Harness::selecting(Some(ExternalLinkContextMenuAction::LinkToThread))
        };
        show(
            &harness,
            "https://github.com/pingdotgg/t3code/pull/42",
            (1.0, 2.0),
            true,
            Some(ExternalLinkContextMenuAction::LinkToThread),
        );
        assert_eq!(
            *harness.failures.borrow(),
            vec![("link-pull-request-to-thread", "rejected")]
        );
    }

    #[test]
    fn resolves_the_external_web_link_host() {
        for (href, expected) in [
            (Some("https://example.com"), Some("example.com")),
            (Some("http://localhost:3000/path"), Some("localhost")),
            (
                Some("//cdn.example.com/clip.mp4?signature=abc#t=2"),
                Some("cdn.example.com"),
            ),
            (Some("//"), None),
            (Some("#details"), None),
            (Some("mailto:hello@example.com"), None),
            (Some("file:///tmp/example.txt"), None),
            (Some("javascript:void(0)"), None),
            (Some("not a URL"), None),
            (None, None),
        ] {
            assert_eq!(
                resolve_external_web_link_host(href).as_deref(),
                expected,
                "{href:?}"
            );
        }
    }

    #[test]
    fn resolves_the_safe_external_web_link_href() {
        for (href, expected) in [
            (
                "https://example.com/docs?topic=security#links",
                "https://example.com/docs?topic=security#links",
            ),
            ("HTTP://EXAMPLE.COM", "http://example.com/"),
            ("//example.com/path", "https://example.com/path"),
        ] {
            assert_eq!(
                resolve_external_web_link_href(Some(href)).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn rejects_unsafe_external_web_link_hrefs() {
        for href in [
            "javascript:alert(document.domain)",
            " \nJaVaScRiPt:alert(document.domain)",
            "data:text/html,<script>alert(document.domain)</script>",
            "file:///tmp/example.txt",
            "mailto:hello@example.com",
            "/relative/path",
            "not a URL",
            "",
        ] {
            assert_eq!(resolve_external_web_link_href(Some(href)), None, "{href:?}");
        }
    }
}
