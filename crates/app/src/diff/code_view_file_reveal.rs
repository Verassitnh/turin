//! Port of upstream/apps/web/src/components/diffs/useCodeViewFileReveal.ts

/// Scroll target for a file row in the code view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeViewScrollTarget {
    pub file_key: String,
}

/// Whether the viewer is mounted and can scroll.
pub trait FileRevealHandle {
    fn is_ready(&self) -> bool;
    fn scroll_to(&self, target: CodeViewScrollTarget);
}

/// Pending reveal request (the hook's `request` state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeViewFileRevealRequest<TScope: Clone> {
    pub file_key: String,
    pub scope: TScope,
}

/// Tracks pending file reveals until the viewer and expanded rows are ready.
#[derive(Clone, Debug)]
pub struct CodeViewFileReveal<TScope: Clone + PartialEq> {
    request: Option<CodeViewFileRevealRequest<TScope>>,
    handled: Option<CodeViewFileRevealRequest<TScope>>,
}

impl<TScope: Clone + PartialEq> Default for CodeViewFileReveal<TScope> {
    fn default() -> Self {
        Self {
            request: None,
            handled: None,
        }
    }
}

impl<TScope: Clone + PartialEq> CodeViewFileReveal<TScope> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_reveal(&mut self, file_key: impl Into<String>, scope: TScope)
    where
        TScope: Clone,
    {
        self.request = Some(CodeViewFileRevealRequest {
            file_key: file_key.into(),
            scope,
        });
    }

    /// Call when `scope`, the viewer, or `ready_file_keys` change (the hook's effect).
    pub fn sync(
        &mut self,
        scope: &TScope,
        viewer: Option<&dyn FileRevealHandle>,
        ready_file_keys: Option<&[String]>,
    ) {
        let Some(request) = self.request.clone() else {
            return;
        };
        if self.handled.as_ref() == Some(&request) {
            return;
        }
        if request.scope != *scope {
            self.handled = Some(request);
            return;
        }
        let Some(viewer) = viewer else {
            return;
        };
        if !viewer.is_ready() {
            return;
        }
        if ready_file_keys.is_some_and(|keys| !keys.iter().any(|key| key == &request.file_key)) {
            return;
        }
        viewer.scroll_to(CodeViewScrollTarget {
            file_key: request.file_key.clone(),
        });
        self.handled = Some(request);
    }

    pub fn reset_on_scope_change(&mut self) {
        self.request = None;
        self.handled = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    struct MockViewer {
        ready: bool,
        scrolled: RefCell<Vec<String>>,
    }

    impl FileRevealHandle for MockViewer {
        fn is_ready(&self) -> bool {
            self.ready
        }

        fn scroll_to(&self, target: CodeViewScrollTarget) {
            self.scrolled.borrow_mut().push(target.file_key);
        }
    }

    #[test]
    fn scrolls_once_when_viewer_and_file_are_ready() {
        let mut reveal = CodeViewFileReveal::new();
        reveal.request_reveal("file-a", "scope-1");
        let viewer = MockViewer {
            ready: true,
            scrolled: RefCell::new(vec![]),
        };
        reveal.sync(&"scope-1", Some(&viewer), Some(&["file-a".into()]));
        assert_eq!(&*viewer.scrolled.borrow(), &["file-a".to_string()]);
        reveal.sync(&"scope-1", Some(&viewer), Some(&["file-a".into()]));
        assert_eq!(viewer.scrolled.borrow().len(), 1);
    }

    #[test]
    fn waits_for_ready_file_keys() {
        let mut reveal = CodeViewFileReveal::new();
        reveal.request_reveal("file-b", "scope-1");
        let viewer = MockViewer {
            ready: true,
            scrolled: RefCell::new(vec![]),
        };
        reveal.sync(&"scope-1", Some(&viewer), Some(&["file-a".into()]));
        assert!(viewer.scrolled.borrow().is_empty());
        reveal.sync(&"scope-1", Some(&viewer), Some(&["file-b".into()]));
        assert_eq!(&*viewer.scrolled.borrow(), &["file-b".to_string()]);
    }
}
