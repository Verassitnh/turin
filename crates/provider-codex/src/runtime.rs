//! Port of upstream/apps/server/src/provider/CodexProvider.ts
//! (`codexAppServerEnvironment`, launch/home setup) and `CodexManagedRuntime.ts`.
#![allow(clippy::result_large_err)]
use crate::{CodexAdapter, CodexSession};
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use turin_codex_app_server::{client::Client, errors::AppServerError};
use turin_contracts::provider_setup::ProviderSetupError;
use turin_provider_core::path_expansion::expand_home_path;

#[derive(Debug, Clone)]
pub struct CodexRuntimeConfig {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub home_path: Option<String>,
    pub launch_args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    /// Names taken out of the inherited process environment before `environment`
    /// applies. A managed launch uses it so ambient overrides cannot redirect its token.
    pub remove_environment: Vec<String>,
    pub client_version: String,
}

impl CodexRuntimeConfig {
    pub fn environment(&self, user_home: &Path) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([(
            "CODEX_INTERNAL_ORIGINATOR_OVERRIDE".into(),
            "T3 Code".into(),
        )]);
        environment.extend(self.environment.clone());
        if let Some(home) = &self.home_path {
            environment.insert(
                "CODEX_HOME".into(),
                expand_home_path(home, &user_home.to_string_lossy()),
            );
        }
        environment
    }

    pub fn spawn(&self, user_home: &Path) -> Result<Client, AppServerError> {
        let args = std::iter::once("app-server".to_owned())
            .chain(self.launch_args.iter().cloned())
            .collect::<Vec<_>>();
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let environment = self.environment(user_home);
        let pairs = environment
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>();
        let removed = self
            .remove_environment
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        CodexAdapter::spawn_removing(
            &self.binary.to_string_lossy(),
            &args,
            &self.cwd,
            &pairs,
            &removed,
        )
    }

    pub async fn connect(&self, user_home: &Path) -> Result<CodexSession, AppServerError> {
        CodexAdapter::initialize(self.spawn(user_home)?, &self.client_version).await
    }
}

/// One session's launch, with whatever must stay alive while it runs.
pub struct ResolvedCodexLaunch {
    pub config: CodexRuntimeConfig,
    /// The managed executable lease. Dropped when the session closes.
    pub keepalive: Box<dyn std::any::Any + Send + Sync>,
}

/// Upstream's `resolveRuntime` option of `createCodexAdapterV2`: launch settings
/// resolved per session from the signed-in token instead of fixed at create time.
#[async_trait]
pub trait CodexRuntimeResolver: Send + Sync {
    async fn resolve(
        &self,
        base: &CodexRuntimeConfig,
    ) -> Result<ResolvedCodexLaunch, ProviderSetupError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn home_expands_and_user_originator_wins() {
        let mut config = CodexRuntimeConfig {
            binary: "codex".into(),
            cwd: "/tmp".into(),
            home_path: Some("~/.codex-work".into()),
            launch_args: vec![],
            environment: BTreeMap::new(),
            remove_environment: vec![],
            client_version: "1".into(),
        };
        let env = config.environment(Path::new("/home/user"));
        assert_eq!(env["CODEX_HOME"], "/home/user/.codex-work");
        assert_eq!(env["CODEX_INTERNAL_ORIGINATOR_OVERRIDE"], "T3 Code");
        config
            .environment
            .insert("CODEX_INTERNAL_ORIGINATOR_OVERRIDE".into(), "custom".into());
        assert_eq!(
            config.environment(Path::new("/home/user"))["CODEX_INTERNAL_ORIGINATOR_OVERRIDE"],
            "custom"
        );
    }
}
