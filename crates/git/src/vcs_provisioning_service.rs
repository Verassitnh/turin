//! Port of upstream/apps/server/src/vcs/VcsProvisioningService.ts
//!
//! `init_repository` only. There is no thread-worktree creation here.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use turin_contracts::git::VcsInitInput;
use turin_contracts::vcs::{VcsDriverKind, VcsError, VcsUnsupportedOperationError};

use crate::vcs_driver_registry::VcsDriverRegistry;

pub struct VcsProvisioningService {
    registry: Arc<VcsDriverRegistry>,
}

impl VcsProvisioningService {
    pub fn new(registry: Arc<VcsDriverRegistry>) -> Self {
        Self { registry }
    }

    pub async fn init_repository(&self, input: &VcsInitInput) -> Result<(), VcsError> {
        let kind = resolve_requested_kind(input.kind)?;
        let driver = self.registry.get(kind).await?;
        driver.init_repository(input).await
    }
}

fn resolve_requested_kind(kind: Option<VcsDriverKind>) -> Result<VcsDriverKind, VcsError> {
    match kind {
        None => Ok(VcsDriverKind::Git),
        Some(VcsDriverKind::Unknown) => Err(VcsError::UnsupportedOperation(
            VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                operation: "VcsProvisioningService.resolveRequestedKind".to_owned(),
                kind: VcsDriverKind::Unknown,
                detail: "A concrete VCS driver kind is required for repository provisioning."
                    .to_owned(),
            },
        )),
        Some(kind) => Ok(kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_vcs_driver::GitVcsDriver;
    use crate::process::ProcessRunner;
    use crate::vcs_process::VcsProcess;
    use crate::vcs_project_config::VcsProjectConfig;
    use std::process::Command;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;

    fn service() -> VcsProvisioningService {
        let registry = VcsDriverRegistry::new(
            Arc::new(VcsProjectConfig::new()),
            Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
        );
        VcsProvisioningService::new(Arc::new(registry))
    }

    fn cwd(path: &std::path::Path) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(path.display().to_string()).unwrap()
    }

    fn is_git_repo(path: &std::path::Path) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--is-inside-work-tree"])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .map(|output| output.status.success() && output.stdout.starts_with(b"true"))
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn routes_repository_initialization_through_an_explicit_git_kind() {
        let dir = tempfile::tempdir().unwrap();
        service()
            .init_repository(&VcsInitInput {
                cwd: cwd(dir.path()),
                kind: Some(VcsDriverKind::Git),
            })
            .await
            .unwrap();
        assert!(is_git_repo(dir.path()));
    }

    #[tokio::test]
    async fn defaults_repository_initialization_to_git() {
        let dir = tempfile::tempdir().unwrap();
        service()
            .init_repository(&VcsInitInput {
                cwd: cwd(dir.path()),
                kind: None,
            })
            .await
            .unwrap();
        assert!(is_git_repo(dir.path()));
    }

    #[tokio::test]
    async fn unknown_kind_is_unsupported_and_does_not_init() {
        let dir = tempfile::tempdir().unwrap();
        let error = service()
            .init_repository(&VcsInitInput {
                cwd: cwd(dir.path()),
                kind: Some(VcsDriverKind::Unknown),
            })
            .await
            .unwrap_err();
        match error {
            VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation,
                    kind,
                    detail,
                },
            ) => {
                assert_eq!(operation, "VcsProvisioningService.resolveRequestedKind");
                assert_eq!(kind, VcsDriverKind::Unknown);
                assert_eq!(
                    detail,
                    "A concrete VCS driver kind is required for repository provisioning."
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(!dir.path().join(".git").exists());
    }
}
