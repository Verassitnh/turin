//! What the Cursor driver hands the rest of the app for usage: the account
//! history reader the usage page scans (`CursorUsageAccounts`) and the plan limit
//! windows its status probe publishes (`readCursorUsageLimits`).
//!
//! Upstream builds both inside `driver.ts` from Effect services. Here every
//! network and credential read sits behind a trait ([`CursorUsageLimitsSource`],
//! `CursorUsageHttp`, `CursorKeychainBackend`, `DashboardClient`) so tests stand in
//! for each and never touch a real login.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::provider_usage_limits::ServerProviderUsageLimits;
use turin_contracts::usage::UsageProviderKind;
use turin_provider_core::host_process::{HostProcess, Platform, ProcessEnv};
use turin_provider_core::provider_host::ProviderHost;
use turin_provider_core::usage::ProviderUsageReader;

use crate::account_usage::ReqwestDashboard;
use crate::cursor_account_reader::CursorAccountReader;
use crate::cursor_keychain::CursorKeychain;
use crate::cursor_usage_accounts::{
    CursorPlatform, CursorUsageAccounts, CursorUsageEnvironment, CursorUsageScanInput,
    cursor_auth_path,
};
use crate::usage_limits::{
    CursorUsageHttp, CursorUsageLimitsHost, ReqwestCursorUsageHttp, read_cursor_usage_limits,
};

/// Reads the plan limit windows for one Cursor instance. The probe asks it only
/// for an enabled, installed, signed-in instance, as upstream's `checkProvider`
/// does.
#[async_trait]
pub trait CursorUsageLimitsSource: Send + Sync {
    async fn read(
        &self,
        api_endpoint: Option<&str>,
        environment: &ProcessEnv,
    ) -> ServerProviderUsageLimits;
}

/// The production source: the dashboard API, the CLI login file, and the Keychain
/// when the server setting `cursorKeychainUsageEnabled` allows it.
pub struct HostCursorUsageLimits {
    host: Arc<ProviderHost>,
    process: HostProcess,
    keychain: CursorKeychain,
    http: Arc<dyn CursorUsageHttp>,
}

impl HostCursorUsageLimits {
    pub fn new(host: Arc<ProviderHost>, process: HostProcess) -> Result<Self, reqwest::Error> {
        Ok(Self::with_parts(
            host,
            process,
            CursorKeychain::layer(),
            Arc::new(ReqwestCursorUsageHttp::new()?),
        ))
    }

    pub fn with_parts(
        host: Arc<ProviderHost>,
        process: HostProcess,
        keychain: CursorKeychain,
        http: Arc<dyn CursorUsageHttp>,
    ) -> Self {
        Self {
            host,
            process,
            keychain,
            http,
        }
    }
}

#[async_trait]
impl CursorUsageLimitsSource for HostCursorUsageLimits {
    async fn read(
        &self,
        api_endpoint: Option<&str>,
        environment: &ProcessEnv,
    ) -> ServerProviderUsageLimits {
        // An unreadable settings file leaves the Keychain off, the safe default.
        let allow_keychain = self
            .host
            .get_settings()
            .await
            .is_ok_and(|settings| settings.cursor_keychain_usage_enabled);
        read_cursor_usage_limits(
            api_endpoint,
            environment,
            allow_keychain,
            &CursorUsageLimitsHost {
                platform: self.process.platform,
                home_directory: &self.process.home_directory,
                keychain: &self.keychain,
                http: &*self.http,
            },
        )
        .await
    }
}

fn account_environment(process: &HostProcess) -> CursorUsageEnvironment {
    let variable = |name: &str| process.environment.get(name).cloned();
    CursorUsageEnvironment {
        home: variable("HOME"),
        userprofile: variable("USERPROFILE"),
        xdg_config_home: variable("XDG_CONFIG_HOME"),
        appdata: variable("APPDATA"),
        agent_cli_credential_store: variable("AGENT_CLI_CREDENTIAL_STORE"),
        cursor_auth_token: variable("CURSOR_AUTH_TOKEN"),
        cursor_api_key: variable("CURSOR_API_KEY"),
        platform: match process.platform {
            Platform::Darwin => CursorPlatform::Darwin,
            Platform::Win32 => CursorPlatform::Win32,
            Platform::Linux | Platform::Other => CursorPlatform::Linux,
        },
    }
}

/// The reader the usage page scans for Cursor's account history. Upstream's is
/// machine-scoped rather than one read per instance: `CursorUsageAccounts` handles
/// login availability, the Keychain opt-in, dashboard HTTP, cache freshness and
/// partial failures.
pub fn cursor_usage_reader(
    state_dir: &Path,
    process: &HostProcess,
) -> Result<ProviderUsageReader, reqwest::Error> {
    Ok(cursor_usage_reader_with(
        state_dir,
        process,
        CursorKeychain::layer(),
        Arc::new(ReqwestDashboard::new()?),
    ))
}

/// [`cursor_usage_reader`] over the given Keychain and dashboard client.
pub fn cursor_usage_reader_with(
    state_dir: &Path,
    process: &HostProcess,
    keychain: CursorKeychain,
    dashboard: Arc<dyn crate::account_usage::DashboardClient>,
) -> ProviderUsageReader {
    let environment = account_environment(process);
    let accounts = Arc::new(CursorUsageAccounts::new(
        state_dir,
        cursor_auth_path(&environment),
        environment,
        Arc::new(CursorAccountReader::new(keychain, dashboard)),
    ));
    ProviderUsageReader::Scan {
        provider: UsageProviderKind::Cursor,
        scan: Arc::new(move |input| {
            let accounts = accounts.clone();
            Box::pin(async move {
                Ok(accounts
                    .scan(CursorUsageScanInput {
                        keychain_usage_enabled: input.settings.cursor_keychain_usage_enabled,
                        window_start_ms: input.window_start_ms,
                        retention_cutoff_ms: input.retention_cutoff_ms,
                        await_refresh: input.await_refresh,
                    })
                    .await)
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use turin_provider_core::provider_host_live::{DesktopProviderHost, ShouldRunBackgroundWork};

    use super::*;
    use crate::cursor_keychain::{CursorKeychainBackend, KeychainReadFuture};

    /// A Keychain that counts reads and answers a fixed token.
    struct CountingKeychain {
        reads: Mutex<u32>,
    }

    impl CursorKeychainBackend for CountingKeychain {
        fn read_access_token(&self) -> KeychainReadFuture {
            *self.reads.lock().expect("reads") += 1;
            Box::pin(async { Ok(Some("keychain-token".to_owned())) })
        }
    }

    struct FixedHttp {
        tokens: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CursorUsageHttp for FixedHttp {
        async fn current_period_usage(&self, _endpoint: &str, token: &str) -> Result<Value, ()> {
            self.tokens.lock().expect("tokens").push(token.to_owned());
            Ok(json!({"planUsage": {"totalPercentUsed": 12.0}}))
        }
    }

    fn host(dir: &Path, keychain_usage_enabled: bool) -> Arc<ProviderHost> {
        let settings = serde_json::from_value(json!({
            "cursorKeychainUsageEnabled": keychain_usage_enabled
        }))
        .expect("server settings decode");
        let should_run: ShouldRunBackgroundWork = Arc::new(|_scope| Box::pin(async { false }));
        let desktop =
            DesktopProviderHost::new(dir, dir, settings, should_run).expect("desktop host opens");
        Arc::new(desktop.host)
    }

    fn darwin() -> HostProcess {
        HostProcess {
            platform: Platform::Darwin,
            home_directory: "/nonexistent-home".to_owned(),
            environment: HashMap::new(),
        }
    }

    fn source(
        keychain_usage_enabled: bool,
        dir: &Path,
    ) -> (HostCursorUsageLimits, Arc<CountingKeychain>, Arc<FixedHttp>) {
        let backend = Arc::new(CountingKeychain {
            reads: Mutex::new(0),
        });
        let http = Arc::new(FixedHttp {
            tokens: Mutex::new(Vec::new()),
        });
        let source = HostCursorUsageLimits::with_parts(
            host(dir, keychain_usage_enabled),
            darwin(),
            CursorKeychain::new(backend.clone()),
            http.clone(),
        );
        (source, backend, http)
    }

    #[tokio::test]
    async fn the_keychain_stays_unread_until_the_server_setting_allows_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, keychain, http) = source(false, dir.path());

        let limits = source.read(None, &ProcessEnv::new()).await;

        assert_eq!(*keychain.reads.lock().unwrap(), 0);
        assert!(http.tokens.lock().unwrap().is_empty());
        assert!(
            format!("{limits:?}").contains("Enable Cursor account usage in Turin"),
            "{limits:?}"
        );
    }

    #[tokio::test]
    async fn an_allowed_keychain_login_reads_the_plan_windows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, keychain, http) = source(true, dir.path());

        let limits = source.read(None, &ProcessEnv::new()).await;

        assert_eq!(*keychain.reads.lock().unwrap(), 1);
        assert_eq!(*http.tokens.lock().unwrap(), vec!["keychain-token"]);
        assert!(
            format!("{limits:?}").contains("totalPercentUsed"),
            "{limits:?}"
        );
    }

    #[tokio::test]
    async fn an_explicit_auth_token_never_touches_the_keychain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, keychain, http) = source(true, dir.path());
        let environment = HashMap::from([("CURSOR_AUTH_TOKEN".to_owned(), "env-token".to_owned())]);

        source.read(None, &environment).await;

        assert_eq!(*keychain.reads.lock().unwrap(), 0);
        assert_eq!(*http.tokens.lock().unwrap(), vec!["env-token"]);
    }

    #[test]
    fn the_account_environment_maps_the_process_variables_and_platform() {
        let process = HostProcess {
            platform: Platform::Win32,
            home_directory: "C:/Users/x".to_owned(),
            environment: HashMap::from([
                ("APPDATA".to_owned(), "C:/AppData".to_owned()),
                ("CURSOR_API_KEY".to_owned(), "key".to_owned()),
            ]),
        };
        let environment = account_environment(&process);
        assert_eq!(environment.platform, CursorPlatform::Win32);
        assert_eq!(environment.appdata.as_deref(), Some("C:/AppData"));
        assert_eq!(environment.cursor_api_key.as_deref(), Some("key"));
        assert_eq!(environment.home, None);
    }
}
