//! Port of `upstream/packages/provider-cursor/src/server/CursorKeychain.ts`.
//!
//! macOS shows the Keychain access prompt on the server's own screen, which a
//! remote client cannot answer, so a read gives up after a timeout. Production
//! reads use the `security` CLI; tests inject a fake backend and never touch
//! the real Keychain.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Notify;
use tokio::time::Instant;

const CACHE_MS: u64 = 5 * 60_000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("Timed out waiting for Keychain access.")]
pub struct CursorKeychainTimeoutError;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("Cursor Keychain credentials could not be read.")]
pub struct CursorKeychainReadError {
    pub cause: Option<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CursorKeychainAccessError {
    #[error(transparent)]
    Timeout(#[from] CursorKeychainTimeoutError),
    #[error(transparent)]
    Read(#[from] CursorKeychainReadError),
}

pub type KeychainReadFuture =
    Pin<Box<dyn Future<Output = Result<Option<String>, CursorKeychainReadError>> + Send>>;

/// Reads the Cursor CLI macOS credential. Tests replace this; production uses
/// [`MacOsSecurityCliKeychain`].
pub trait CursorKeychainBackend: Send + Sync {
    fn read_access_token(&self) -> KeychainReadFuture;
}

/// `security find-generic-password` for Cursor's default Keychain entry.
pub struct MacOsSecurityCliKeychain;

impl CursorKeychainBackend for MacOsSecurityCliKeychain {
    fn read_access_token(&self) -> KeychainReadFuture {
        Box::pin(async move {
            let output = tokio::process::Command::new("security")
                .args([
                    "find-generic-password",
                    "-s",
                    "cursor-access-token",
                    "-a",
                    "cursor-user",
                    "-w",
                ])
                .output()
                .await
                .map_err(|error| CursorKeychainReadError {
                    cause: Some(error.to_string()),
                })?;
            if !output.status.success() {
                if output.status.code() == Some(44) {
                    return Ok(None);
                }
                return Err(CursorKeychainReadError {
                    cause: Some(String::from_utf8_lossy(&output.stderr).into_owned()),
                });
            }
            let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if token.is_empty() {
                Ok(None)
            } else {
                Ok(Some(token))
            }
        })
    }
}

type ReadResult = Result<Option<String>, CursorKeychainReadError>;

struct PendingRead {
    result: Mutex<Option<ReadResult>>,
    notify: Notify,
}

struct Inner {
    cached: Option<(String, Instant)>,
    pending: Option<Arc<PendingRead>>,
}

pub struct CursorKeychain {
    backend: Arc<dyn CursorKeychainBackend>,
    timeout: Duration,
    inner: Arc<Mutex<Inner>>,
}

impl CursorKeychain {
    pub fn new(backend: Arc<dyn CursorKeychainBackend>) -> Self {
        Self::with_timeout(backend, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(backend: Arc<dyn CursorKeychainBackend>, timeout: Duration) -> Self {
        Self {
            backend,
            timeout,
            inner: Arc::new(Mutex::new(Inner {
                cached: None,
                pending: None,
            })),
        }
    }

    pub fn layer() -> Self {
        Self::new(Arc::new(MacOsSecurityCliKeychain))
    }

    /// The access token, or `None` when the Keychain holds no Cursor login.
    pub async fn access_token(&self) -> Result<Option<String>, CursorKeychainAccessError> {
        if let Some(token) = self.cached_token() {
            return Ok(Some(token));
        }
        let pending = self.ensure_pending();
        let result = tokio::time::timeout(self.timeout, self.wait_pending(&pending))
            .await
            .map_err(|_| CursorKeychainAccessError::Timeout(CursorKeychainTimeoutError))??;
        Ok(result)
    }

    fn cached_token(&self) -> Option<String> {
        let inner = self.inner.lock().expect("keychain lock");
        inner
            .cached
            .as_ref()
            .filter(|(_, until)| *until > Instant::now())
            .map(|(token, _)| token.clone())
    }

    fn ensure_pending(&self) -> Arc<PendingRead> {
        let mut inner = self.inner.lock().expect("keychain lock");
        if let Some(pending) = &inner.pending {
            return Arc::clone(pending);
        }
        let pending = Arc::new(PendingRead {
            result: Mutex::new(None),
            notify: Notify::new(),
        });
        inner.pending = Some(Arc::clone(&pending));
        let backend = Arc::clone(&self.backend);
        let inner = Arc::clone(&self.inner);
        let pending_task = Arc::clone(&pending);
        tokio::spawn(async move {
            let read = backend.read_access_token().await;
            {
                let mut state = inner.lock().expect("keychain lock");
                match &read {
                    Ok(Some(token)) => {
                        state.cached = Some((
                            token.clone(),
                            Instant::now() + Duration::from_millis(CACHE_MS),
                        ));
                    }
                    Ok(None) => state.cached = None,
                    Err(_) => state.cached = None,
                }
                state.pending = None;
            }
            *pending_task.result.lock().expect("result lock") = Some(read.clone());
            pending_task.notify.notify_waiters();
        });
        pending
    }

    async fn wait_pending(
        &self,
        pending: &Arc<PendingRead>,
    ) -> Result<Option<String>, CursorKeychainAccessError> {
        loop {
            if let Some(result) = pending.result.lock().expect("result lock").clone() {
                return result.map_err(CursorKeychainAccessError::Read);
            }
            pending.notify.notified().await;
        }
    }
}

pub fn is_cursor_keychain_timeout_error(error: &CursorKeychainAccessError) -> bool {
    matches!(error, CursorKeychainAccessError::Timeout(_))
}

impl Clone for CursorKeychain {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            timeout: self.timeout,
            inner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptBackend {
        reads: Arc<AtomicUsize>,
        block: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    }

    impl CursorKeychainBackend for ScriptBackend {
        fn read_access_token(&self) -> KeychainReadFuture {
            let reads = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            let block = Arc::clone(&self.block);
            Box::pin(async move {
                if let Some(wait) = block.lock().await.take() {
                    let _ = wait.await;
                }
                Ok(Some(format!("token-{reads}")))
            })
        }
    }

    #[tokio::test]
    async fn shares_concurrent_reads_and_rechecks_after_the_cache_expires() {
        let reads = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn CursorKeychainBackend> = Arc::new(ScriptBackend {
            reads: Arc::clone(&reads),
            block: Arc::new(tokio::sync::Mutex::new(None)),
        });
        let keychain = CursorKeychain::new(Arc::clone(&backend));
        let (a, b) = tokio::join!(keychain.access_token(), keychain.access_token());
        assert_eq!(a.unwrap(), Some("token-1".to_owned()));
        assert_eq!(b.unwrap(), Some("token-1".to_owned()));
        assert_eq!(
            keychain.access_token().await.unwrap(),
            Some("token-1".to_owned())
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        {
            let mut inner = keychain.inner.lock().unwrap();
            inner.cached = Some(("token-1".to_owned(), Instant::now()));
        }
        assert_eq!(
            keychain.access_token().await.unwrap(),
            Some("token-2".to_owned())
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn gives_up_on_an_unanswered_prompt_and_reuses_it_on_the_next_read() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let reads = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn CursorKeychainBackend> = Arc::new(ScriptBackend {
            reads: Arc::clone(&reads),
            block: Arc::new(tokio::sync::Mutex::new(Some(wait))),
        });
        let keychain = CursorKeychain::with_timeout(Arc::clone(&backend), Duration::from_millis(1));
        let timed_out = tokio::spawn({
            let keychain = keychain.clone();
            async move { keychain.access_token().await }
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        let error = timed_out.await.unwrap().unwrap_err();
        assert!(is_cursor_keychain_timeout_error(&error));
        let retry = tokio::spawn({
            let keychain = keychain.clone();
            async move { keychain.access_token().await }
        });
        let _ = release.send(());
        assert_eq!(retry.await.unwrap().unwrap(), Some("token-1".to_owned()));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}
