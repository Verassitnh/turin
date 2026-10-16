//! Port of upstream/apps/web/src/composerHandleContext.ts
//!
//! A React context that carries a ref to the mounted `ChatComposer`'s handle,
//! so ChatView, the command palette and the file browser can focus it or
//! insert text. Here the ref is [`ComposerHandleRef`], generic over the
//! handle type because the handle is the composer view's own surface
//! (`ChatComposerHandle`), and the context is [`ComposerHandleContext`]:
//! whoever owns the composer provides it, consumers `use_handle` it.

use std::sync::Arc;

use parking_lot::RwLock;

/// `React.RefObject<ChatComposerHandle | null>`.
#[derive(Debug)]
pub struct ComposerHandleRef<H> {
    current: Arc<RwLock<Option<H>>>,
}

impl<H> Clone for ComposerHandleRef<H> {
    fn clone(&self) -> Self {
        Self {
            current: Arc::clone(&self.current),
        }
    }
}

impl<H> Default for ComposerHandleRef<H> {
    fn default() -> Self {
        Self {
            current: Arc::new(RwLock::new(None)),
        }
    }
}

impl<H: Clone> ComposerHandleRef<H> {
    /// `ref.current`.
    pub fn current(&self) -> Option<H> {
        self.current.read().clone()
    }
}

impl<H> ComposerHandleRef<H> {
    /// The composer sets its handle on mount and clears it on unmount.
    pub fn set(&self, handle: Option<H>) {
        *self.current.write() = handle;
    }

    pub fn is_same_ref(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.current, &other.current)
    }
}

/// `createContext<ComposerHandleRef | null>(null)`.
#[derive(Debug)]
pub struct ComposerHandleContext<H> {
    value: Option<ComposerHandleRef<H>>,
}

impl<H> Default for ComposerHandleContext<H> {
    fn default() -> Self {
        Self { value: None }
    }
}

impl<H> ComposerHandleContext<H> {
    pub fn provide(handle_ref: ComposerHandleRef<H>) -> Self {
        Self {
            value: Some(handle_ref),
        }
    }

    /// `useComposerHandleContext`: `None` outside a provider.
    pub fn use_composer_handle_context(&self) -> Option<&ComposerHandleRef<H>> {
        self.value.as_ref()
    }
}

#[cfg(test)]
mod tests {
    //! Upstream ships no test file.

    use super::*;

    #[test]
    fn no_provider_means_no_ref() {
        let context = ComposerHandleContext::<u32>::default();
        assert!(context.use_composer_handle_context().is_none());
    }

    #[test]
    fn a_provided_ref_sees_the_handle_the_composer_sets_and_clears() {
        let handle_ref = ComposerHandleRef::<u32>::default();
        let context = ComposerHandleContext::provide(handle_ref.clone());
        let used = context.use_composer_handle_context().unwrap();
        assert!(used.is_same_ref(&handle_ref));
        assert_eq!(used.current(), None);

        handle_ref.set(Some(7));
        assert_eq!(used.current(), Some(7));
        handle_ref.set(None);
        assert_eq!(used.current(), None);
    }
}
