//! A click on a file chip or file link in chat markdown (`MarkdownFileLink`
//! in upstream's ChatMarkdown.tsx): the link is resolved against the thread's
//! workspace root and opened in the preferred editor
//! (`useOpenInPreferredEditor`). A bare path is not a URL, so it must never
//! reach `cx.open_url`.

use gpui_kit::{App, Global};

use super::file_link_meta::{
    resolve_markdown_file_link_meta, should_open_markdown_file_link_in_browser_by_default,
};
use crate::hooks::open_in_editor::EditorLauncherService;

/// The app's tokio runtime, which the editor launch runs on. Installed by the
/// live chat; without it a file link falls back to the system's default app.
#[derive(Clone)]
pub struct MarkdownFileLinkRuntime(pub tokio::runtime::Handle);

impl Global for MarkdownFileLinkRuntime {}

/// Where a file link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileLinkTarget {
    /// The editor opens `target` (`path`, `path:line` or `path:line:column`).
    Editor { target: String },
    /// The system's default app opens the file (PDFs stay browser first).
    System { path: String },
}

/// Resolves `href` against `cwd`; `None` when it names no file.
pub fn file_link_target(href: &str, cwd: Option<&str>) -> Option<FileLinkTarget> {
    let meta = resolve_markdown_file_link_meta(Some(href), cwd, None)?;
    if should_open_markdown_file_link_in_browser_by_default(&meta.file_path) {
        return Some(FileLinkTarget::System {
            path: meta.file_path,
        });
    }
    Some(FileLinkTarget::Editor {
        target: meta.target_path,
    })
}

/// Handles a click on `href`. Returns `false` when it is not a file link, so
/// the caller can treat it as an external URL.
pub fn open_markdown_file_link(href: &str, cwd: Option<&str>, cx: &mut App) -> bool {
    let Some(target) = file_link_target(href, cwd) else {
        return false;
    };
    match target {
        FileLinkTarget::System { path } => cx.open_url(&format!("file://{path}")),
        FileLinkTarget::Editor { target } => open_in_editor(target, cx),
    }
    true
}

fn open_in_editor(target: String, cx: &mut App) {
    let path = turin_shared::file_links::split_file_path_position(&target, "").path;
    let Some(runtime) = cx.try_global::<MarkdownFileLinkRuntime>().cloned() else {
        cx.open_url(&format!("file://{path}"));
        return;
    };
    let opened = EditorLauncherService::global().spawn_open(&runtime.0, target, cx);
    cx.spawn(async move |cx| {
        if let Err(message) = opened.await {
            tracing::warn!(%message, path, "could not open the file link in an editor");
            cx.update(|cx| {
                crate::components::chat::live_chat::push_error(
                    cx,
                    "Could not open the file",
                    &message,
                );
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/Users/me/proj";

    #[test]
    fn a_relative_path_with_a_line_opens_in_the_editor_under_the_workspace_root() {
        assert_eq!(
            file_link_target("src/main.rs:12", Some(ROOT)),
            Some(FileLinkTarget::Editor {
                target: "/Users/me/proj/src/main.rs:12".into()
            })
        );
    }

    #[test]
    fn an_absolute_link_with_a_line_hash_keeps_its_position() {
        assert_eq!(
            file_link_target("/Users/me/proj/foo.rs#L10", Some(ROOT)),
            Some(FileLinkTarget::Editor {
                target: "/Users/me/proj/foo.rs:10".into()
            })
        );
    }

    #[test]
    fn a_relative_path_without_a_workspace_is_not_resolvable() {
        assert_eq!(file_link_target("src/main.rs", None), None);
    }

    #[test]
    fn external_urls_are_not_file_links() {
        assert_eq!(
            file_link_target("https://example.com/a.rs", Some(ROOT)),
            None
        );
        assert_eq!(file_link_target("mailto:me@example.com", Some(ROOT)), None);
    }

    #[test]
    fn pdfs_open_in_the_system_viewer() {
        assert_eq!(
            file_link_target("/Users/me/report.pdf", Some(ROOT)),
            Some(FileLinkTarget::System {
                path: "/Users/me/report.pdf".into()
            })
        );
    }
}
