//! Port of upstream/packages/provider-cursor/src/server/CursorAccountReader.ts.
//!
//! Reads one range of Cursor account usage from the dashboard API. Tests can
//! substitute the HTTP client and Keychain backend.

use std::sync::Arc;

use crate::account_cache::CursorCredentialSource;
use crate::account_usage::{
    AccountUsageRead, CredentialSource, DashboardClient, KeychainOutcome, read_cursor_account_usage,
};
use crate::cursor_keychain::{CursorKeychain, CursorKeychainAccessError};

pub struct CursorAccountReader {
    keychain: CursorKeychain,
    client: Arc<dyn DashboardClient>,
}

impl CursorAccountReader {
    pub fn new(keychain: CursorKeychain, client: Arc<dyn DashboardClient>) -> Self {
        Self { keychain, client }
    }

    /// Reads Cursor's dashboard API with the saved CLI or Keychain login.
    pub async fn read(
        &self,
        credential_source: CursorCredentialSource,
        since_ms: f64,
        until_ms: f64,
    ) -> AccountUsageRead {
        let source = match credential_source {
            CursorCredentialSource::File(path) => CredentialSource::File(path),
            CursorCredentialSource::Keychain => {
                CredentialSource::Keychain(self.keychain_outcome().await)
            }
        };
        read_cursor_account_usage(source, since_ms, until_ms, Arc::clone(&self.client)).await
    }

    async fn keychain_outcome(&self) -> KeychainOutcome {
        match self.keychain.access_token().await {
            Ok(Some(token)) => KeychainOutcome::Token(token),
            Ok(None) => KeychainOutcome::Missing,
            Err(CursorKeychainAccessError::Timeout(_)) => KeychainOutcome::Timeout,
            Err(CursorKeychainAccessError::Read(_)) => KeychainOutcome::ReadFailed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::account_usage::{DashboardRequest, DashboardResponse};
    use crate::cursor_keychain::{
        CursorKeychainBackend, CursorKeychainReadError, KeychainReadFuture,
    };

    struct FixedKeychain(Option<String>);

    impl CursorKeychainBackend for FixedKeychain {
        fn read_access_token(&self) -> KeychainReadFuture {
            let token = self.0.clone();
            Box::pin(async move { Ok(token) })
        }
    }

    struct DenyClient(Arc<AtomicUsize>);

    #[async_trait]
    impl DashboardClient for DenyClient {
        async fn post(&self, _request: DashboardRequest) -> Result<DashboardResponse, ()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(DashboardResponse {
                status: 401,
                body: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn keychain_login_drives_the_dashboard_read() {
        let reader = CursorAccountReader::new(
            CursorKeychain::new(Arc::new(FixedKeychain(Some(
                "header.payload.sig".to_owned(),
            )))),
            Arc::new(DenyClient(Arc::new(AtomicUsize::new(0)))),
        );
        let result = reader
            .read(CursorCredentialSource::Keychain, 0.0, 1.0)
            .await;
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn missing_keychain_login_never_hits_the_dashboard() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reader = CursorAccountReader::new(
            CursorKeychain::new(Arc::new(FixedKeychain(None))),
            Arc::new(DenyClient(Arc::clone(&calls))),
        );
        let result = reader
            .read(CursorCredentialSource::Keychain, 0.0, 1.0)
            .await;
        assert!(result.missing);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn read_failure_surfaces_as_keychain_outcome() {
        let reader = CursorAccountReader::new(
            CursorKeychain::new(Arc::new(FailingKeychain)),
            Arc::new(DenyClient(Arc::new(AtomicUsize::new(0)))),
        );
        let result = reader
            .read(CursorCredentialSource::Keychain, 0.0, 1.0)
            .await;
        assert_eq!(
            result.error.as_deref(),
            Some("Cursor Keychain credentials could not be read.")
        );
    }

    struct FailingKeychain;

    impl CursorKeychainBackend for FailingKeychain {
        fn read_access_token(&self) -> KeychainReadFuture {
            Box::pin(async {
                Err(CursorKeychainReadError {
                    cause: Some("denied".to_owned()),
                })
            })
        }
    }
}
