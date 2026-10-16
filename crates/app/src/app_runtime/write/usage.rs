//! Existing provider usage sources adapted to the usage service's scan port.
//! No transcript formats are synthesized here.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use turin_contracts::usage::UsageProviderKind;
use turin_provider_core::usage::ProviderUsageReader;
use turin_provider_cursor::{
    CursorAccountReader, CursorKeychain, CursorPlatform, CursorUsageAccounts,
    CursorUsageEnvironment, CursorUsageScanInput, ReqwestDashboard, cursor_auth_path,
};
use turin_usage::usage_service::UsageReader;

/// Upstream's account-history reader is machine-scoped, rather than one read
/// per Cursor instance. The existing scanner handles login availability,
/// keychain opt-in, real dashboard HTTP, cache freshness and partial failures.
pub fn cursor_reader(state_dir: &Path) -> anyhow::Result<UsageReader> {
    let environment = CursorUsageEnvironment {
        home: std::env::var("HOME").ok(),
        userprofile: std::env::var("USERPROFILE").ok(),
        xdg_config_home: std::env::var("XDG_CONFIG_HOME").ok(),
        appdata: std::env::var("APPDATA").ok(),
        agent_cli_credential_store: std::env::var("AGENT_CLI_CREDENTIAL_STORE").ok(),
        cursor_auth_token: std::env::var("CURSOR_AUTH_TOKEN").ok(),
        cursor_api_key: std::env::var("CURSOR_API_KEY").ok(),
        platform: if cfg!(target_os = "macos") {
            CursorPlatform::Darwin
        } else if cfg!(target_os = "windows") {
            CursorPlatform::Win32
        } else {
            CursorPlatform::Linux
        },
    };
    let accounts = Arc::new(CursorUsageAccounts::new(
        state_dir,
        cursor_auth_path(&environment),
        environment,
        Arc::new(CursorAccountReader::new(
            CursorKeychain::layer(),
            Arc::new(ReqwestDashboard::new().context("creating Cursor usage HTTP client")?),
        )),
    ));
    Ok(UsageReader {
        driver_kind: "cursor".to_owned(),
        reader: ProviderUsageReader::Scan {
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
        },
    })
}
