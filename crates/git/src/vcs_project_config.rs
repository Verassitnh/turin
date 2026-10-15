//! Port of upstream/apps/server/src/vcs/VcsProjectConfig.ts
//!
//! Walks from `cwd` toward the filesystem root for `.t3code/vcs.json`.
//! An explicit requested kind wins. Inspect, read, and decode failures are
//! logged and become `"auto"` so a broken config cannot block detection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use turin_contracts::vcs::VcsDriverKind;

/// `VcsDriverKind | "auto"` from the project config and resolve input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedVcsKind {
    Auto,
    Kind(VcsDriverKind),
}

impl RequestedVcsKind {
    pub fn cache_label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Kind(VcsDriverKind::Git) => "git",
            Self::Kind(VcsDriverKind::Jj) => "jj",
            Self::Kind(VcsDriverKind::Unknown) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsProjectConfigOperation {
    Inspect,
    Read,
    Decode,
}

impl VcsProjectConfigOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Read => "read",
            Self::Decode => "decode",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("Failed to {operation} VCS project config at {config_path}.")]
pub struct VcsProjectConfigError {
    pub operation: VcsProjectConfigOperation,
    pub cwd: String,
    pub config_path: String,
    /// Upstream `_tag` of the defect (`PlatformError` or `SchemaError`).
    pub cause_tag: &'static str,
    pub cause: String,
}

impl std::fmt::Display for VcsProjectConfigOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub struct VcsProjectConfigResolveInput {
    pub cwd: String,
    pub requested_kind: Option<RequestedVcsKind>,
}

#[derive(Debug, Deserialize)]
struct ProjectVcsConfigFile {
    #[serde(default)]
    vcs: Option<ProjectVcsSection>,
    #[serde(default, rename = "vcsKind")]
    vcs_kind: Option<VcsDriverKind>,
}

#[derive(Debug, Deserialize)]
struct ProjectVcsSection {
    #[serde(default)]
    kind: Option<VcsDriverKind>,
}

type WarningHook = Arc<dyn Fn(&VcsProjectConfigError) + Send + Sync>;

pub struct VcsProjectConfig {
    on_warning: Option<WarningHook>,
}

impl VcsProjectConfig {
    pub fn new() -> Self {
        Self { on_warning: None }
    }

    /// Test seam. Production callers use [`VcsProjectConfig::new`].
    pub fn with_warning_hook(hook: WarningHook) -> Self {
        Self {
            on_warning: Some(hook),
        }
    }

    pub async fn resolve_kind(
        &self,
        input: &VcsProjectConfigResolveInput,
    ) -> Result<RequestedVcsKind, VcsProjectConfigError> {
        if let Some(RequestedVcsKind::Kind(kind)) = input.requested_kind {
            return Ok(RequestedVcsKind::Kind(kind));
        }
        match self.lookup_kind(&input.cwd).await {
            Ok(kind) => Ok(kind),
            Err(error) => {
                self.report(&error);
                Ok(RequestedVcsKind::Auto)
            }
        }
    }

    async fn lookup_kind(&self, cwd: &str) -> Result<RequestedVcsKind, VcsProjectConfigError> {
        let Some(config_path) = self.find_config_path(cwd).await else {
            return Ok(RequestedVcsKind::Auto);
        };
        self.read_configured_kind(cwd, &config_path).await
    }

    async fn find_config_path(&self, cwd: &str) -> Option<PathBuf> {
        let mut current = PathBuf::from(cwd);
        loop {
            let candidate = current.join(".t3code").join("vcs.json");
            match tokio::fs::try_exists(&candidate).await {
                Ok(true) => return Some(candidate),
                Ok(false) => {}
                Err(error) => {
                    self.report(&VcsProjectConfigError {
                        operation: VcsProjectConfigOperation::Inspect,
                        cwd: cwd.to_owned(),
                        config_path: candidate.display().to_string(),
                        cause_tag: "PlatformError",
                        cause: error.to_string(),
                    });
                }
            }
            match current.parent() {
                Some(parent) if parent != current => current = parent.to_path_buf(),
                _ => return None,
            }
        }
    }

    async fn read_configured_kind(
        &self,
        cwd: &str,
        config_path: &Path,
    ) -> Result<RequestedVcsKind, VcsProjectConfigError> {
        let raw = tokio::fs::read_to_string(config_path).await.map_err(|error| {
            VcsProjectConfigError {
                operation: VcsProjectConfigOperation::Read,
                cwd: cwd.to_owned(),
                config_path: config_path.display().to_string(),
                cause_tag: "PlatformError",
                cause: error.to_string(),
            }
        })?;
        let parsed: ProjectVcsConfigFile =
            serde_json::from_str(&strip_lenient_json(&raw)).map_err(|error| {
                VcsProjectConfigError {
                    operation: VcsProjectConfigOperation::Decode,
                    cwd: cwd.to_owned(),
                    config_path: config_path.display().to_string(),
                    cause_tag: "SchemaError",
                    cause: error.to_string(),
                }
            })?;
        Ok(configured_kind(&parsed))
    }

    fn report(&self, error: &VcsProjectConfigError) {
        tracing::warn!(
            operation = error.operation.as_str(),
            cwd = error.cwd.as_str(),
            config_path = error.config_path.as_str(),
            error_tag = "VcsProjectConfigError",
            cause_tag = error.cause_tag,
            "{error}"
        );
        if let Some(hook) = &self.on_warning {
            hook(error);
        }
    }
}

impl Default for VcsProjectConfig {
    fn default() -> Self {
        Self::new()
    }
}

fn configured_kind(config: &ProjectVcsConfigFile) -> RequestedVcsKind {
    if let Some(kind) = config.vcs.as_ref().and_then(|section| section.kind) {
        return RequestedVcsKind::Kind(kind);
    }
    if let Some(kind) = config.vcs_kind {
        return RequestedVcsKind::Kind(kind);
    }
    RequestedVcsKind::Auto
}

/// `fromLenientJson`: drop `//` and `/* */` comments and trailing commas, keeping
/// those characters when they sit inside a JSON string.
fn strip_lenient_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaping = false;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            out.push(ch);
            if escaping {
                escaping = false;
            } else if ch == '\\' {
                escaping = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            index += 1;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            index += 2;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            if index + 1 < chars.len() {
                index += 2;
            } else {
                index = chars.len();
            }
            continue;
        }
        if ch == ',' {
            let mut ahead = index + 1;
            while ahead < chars.len() && chars[ahead].is_whitespace() {
                ahead += 1;
            }
            if matches!(chars.get(ahead), Some('}' | ']')) {
                index += 1;
                continue;
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    fn recording() -> (
        VcsProjectConfig,
        Arc<Mutex<Vec<VcsProjectConfigError>>>,
    ) {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&logs);
        let config = VcsProjectConfig::with_warning_hook(Arc::new(move |error| {
            sink.lock().push(error.clone());
        }));
        (config, logs)
    }

    async fn write_config(root: &Path, body: &str) -> PathBuf {
        let dir = root.join(".t3code");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("vcs.json");
        tokio::fs::write(&path, body).await.unwrap();
        path
    }

    #[tokio::test]
    async fn explicit_requested_kind_wins_over_config() {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), r#"{"vcs":{"kind":"git"}}"#).await;
        let config = VcsProjectConfig::new();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: Some(RequestedVcsKind::Kind(VcsDriverKind::Jj)),
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Kind(VcsDriverKind::Jj));
    }

    #[tokio::test]
    async fn discovers_vcs_json_from_nested_workspaces() {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), r#"{"vcs":{"kind":"jj"}}"#).await;
        let nested = root.path().join("packages").join("app");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        let config = VcsProjectConfig::new();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: nested.display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Kind(VcsDriverKind::Jj));
    }

    #[tokio::test]
    async fn top_level_vcs_kind_is_used_when_vcs_kind_is_absent() {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), r#"{"vcsKind":"git"}"#).await;
        let config = VcsProjectConfig::new();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Kind(VcsDriverKind::Git));
    }

    #[tokio::test]
    async fn continues_to_parent_configs_after_a_candidate_inspect_failure() {
        let root = tempfile::tempdir().unwrap();
        let config_path = write_config(root.path(), r#"{"vcs":{"kind":"jj"}}"#).await;
        let cwd = root.path().join("invalid\0child");
        let (config, logs) = recording();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: cwd.display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Kind(VcsDriverKind::Jj));
        let failed = cwd.join(".t3code").join("vcs.json");
        let error = logs.lock()[0].clone();
        assert_eq!(
            error.to_string(),
            format!("Failed to inspect VCS project config at {}.", failed.display())
        );
        assert_eq!(error.operation, VcsProjectConfigOperation::Inspect);
        assert_eq!(error.cwd, cwd.display().to_string());
        assert_eq!(error.config_path, failed.display().to_string());
        assert_eq!(error.cause_tag, "PlatformError");
        assert!(config_path.exists());
    }

    #[tokio::test]
    async fn falls_back_to_auto_when_no_config_exists() {
        let root = tempfile::tempdir().unwrap();
        let config = VcsProjectConfig::new();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Auto);
    }

    #[tokio::test]
    async fn falls_back_to_auto_when_config_json_is_malformed() {
        let root = tempfile::tempdir().unwrap();
        let path = write_config(root.path(), "{not json").await;
        let (config, logs) = recording();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Auto);
        let error = logs.lock()[0].clone();
        assert_eq!(
            error.to_string(),
            format!("Failed to decode VCS project config at {}.", path.display())
        );
        assert_eq!(error.operation, VcsProjectConfigOperation::Decode);
        assert_eq!(error.cause_tag, "SchemaError");
        assert_eq!(error.cwd, root.path().display().to_string());
        assert_eq!(error.config_path, path.display().to_string());
    }

    #[tokio::test]
    async fn falls_back_to_auto_when_the_config_path_cannot_be_read() {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join(".t3code").join("vcs.json");
        tokio::fs::create_dir_all(&config_path).await.unwrap();
        let (config, logs) = recording();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Auto);
        let error = logs.lock()[0].clone();
        assert_eq!(
            error.to_string(),
            format!(
                "Failed to read VCS project config at {}.",
                config_path.display()
            )
        );
        assert_eq!(error.operation, VcsProjectConfigOperation::Read);
        assert_eq!(error.cause_tag, "PlatformError");
        assert_eq!(error.config_path, config_path.display().to_string());
    }

    #[tokio::test]
    async fn falls_back_to_auto_when_config_kind_is_invalid() {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), r#"{"vcs":{"kind":"svn"}}"#).await;
        let config = VcsProjectConfig::new();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: None,
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Auto);
    }

    #[tokio::test]
    async fn accepts_trailing_commas_and_comments() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "{\n  // driver\n  \"vcs\": { \"kind\": \"jj\", },\n}\n",
        )
        .await;
        let config = VcsProjectConfig::new();
        let kind = config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: root.path().display().to_string(),
                requested_kind: Some(RequestedVcsKind::Auto),
            })
            .await
            .unwrap();
        assert_eq!(kind, RequestedVcsKind::Kind(VcsDriverKind::Jj));
    }

    #[test]
    fn lenient_json_keeps_commas_inside_strings() {
        let stripped = strip_lenient_json(r#"{"note":"a,]","ok":true,}"#);
        let value: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["note"], "a,]");
        assert_eq!(value["ok"], true);
    }
}
