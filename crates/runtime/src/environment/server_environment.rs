//! Port of upstream/apps/server/src/environment/ServerEnvironment.ts
//!
//! Identity and descriptor of this machine. The environment ID is persisted
//! in `environment-id` under the state dir, so it survives restarts. Creation
//! races with another process are resolved by hard-linking a temp file, so
//! the first writer wins and later ones read its ID.

use std::path::{Path, PathBuf};

use thiserror::Error;
use turin_contracts::base_schemas::{EnvironmentId, PositiveInt, TrimmedNonEmptyString};
use turin_contracts::chat_attachment::PROVIDER_SEND_TURN_MAX_FILE_BYTES;
use turin_contracts::environment::{
    ExecutionEnvironmentCapabilities, ExecutionEnvironmentDescriptor, ExecutionEnvironmentPlatform,
    ExecutionEnvironmentPlatformArch, ExecutionEnvironmentPlatformOs, FileAttachmentCapability,
    ORCHESTRATION_PROTOCOL_VERSION,
};

use super::server_environment_label::{SystemHost, resolve_server_environment_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentIdOperation {
    Check,
    Read,
    Write,
    Initialize,
}

/// `ServerEnvironmentIdPersistenceError`.
#[derive(Debug, Error)]
pub enum ServerEnvironmentIdPersistenceError {
    #[error("Server environment ID initialize failed at '{path}'.")]
    Initialize { path: PathBuf },
    #[error("Server environment ID {} failed at '{}'.", operation_name(.operation), .path.display())]
    Io {
        operation: EnvironmentIdOperation,
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
}

fn operation_name(operation: &EnvironmentIdOperation) -> &'static str {
    match operation {
        EnvironmentIdOperation::Check => "check",
        EnvironmentIdOperation::Read => "read",
        EnvironmentIdOperation::Write => "write",
        EnvironmentIdOperation::Initialize => "initialize",
    }
}

fn io_error(
    operation: EnvironmentIdOperation,
    path: &Path,
) -> impl FnOnce(std::io::Error) -> ServerEnvironmentIdPersistenceError + '_ {
    move |cause| ServerEnvironmentIdPersistenceError::Io {
        operation,
        path: path.to_path_buf(),
        cause,
    }
}

/// `readPersistedEnvironmentId`: the trimmed file contents, or `None` when the
/// file is absent or blank.
fn read_persisted_environment_id(
    path: &Path,
) -> Result<Option<String>, ServerEnvironmentIdPersistenceError> {
    let exists = path
        .try_exists()
        .map_err(io_error(EnvironmentIdOperation::Check, path))?;
    if !exists {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(path).map_err(io_error(EnvironmentIdOperation::Read, path))?;
    let trimmed = raw.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    })
}

/// `persistEnvironmentId`. `create` publishes at `environment-id`; `recover`
/// publishes at `environment-id.recovery` and then copies that over the
/// primary path. The hard link never replaces an existing file.
fn persist_environment_id(
    state_dir: &Path,
    environment_id_path: &Path,
    value: &str,
    recover: bool,
) -> Result<(), ServerEnvironmentIdPersistenceError> {
    let write = |cause: std::io::Error| ServerEnvironmentIdPersistenceError::Io {
        operation: EnvironmentIdOperation::Write,
        path: environment_id_path.to_path_buf(),
        cause,
    };
    let destination = if recover {
        PathBuf::from(format!("{}.recovery", environment_id_path.display()))
    } else {
        environment_id_path.to_path_buf()
    };
    let temp_path = state_dir.join(format!(".environment-id-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temp_path, format!("{value}\n")).map_err(write)?;
    // Scoped cleanup: the temp file is removed whatever the outcome.
    let result = (|| {
        match std::fs::hard_link(&temp_path, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(write(error)),
        }
        if recover {
            // Reuse the recovery ID so delayed initializers publish the same winner.
            std::fs::remove_file(&temp_path).map_err(write)?;
            std::fs::copy(&destination, &temp_path).map_err(write)?;
            std::fs::rename(&temp_path, environment_id_path).map_err(write)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&temp_path);
    result
}

/// Reads the persisted environment ID, or creates and persists a new UUIDv4.
/// Port of `makeIdentity`'s `environmentIdRaw` effect.
pub fn load_or_create_environment_id(
    state_dir: &Path,
    environment_id_path: &Path,
) -> Result<EnvironmentId, ServerEnvironmentIdPersistenceError> {
    if let Some(persisted) = read_persisted_environment_id(environment_id_path)? {
        return environment_id_from_persisted(persisted, environment_id_path);
    }

    let generated = uuid::Uuid::new_v4().to_string();
    persist_environment_id(state_dir, environment_id_path, &generated, false)?;
    let mut winner = read_persisted_environment_id(environment_id_path)?;
    if winner.is_none() {
        persist_environment_id(state_dir, environment_id_path, &generated, true)?;
        winner = read_persisted_environment_id(environment_id_path)?;
    }
    match winner {
        Some(winner) => environment_id_from_persisted(winner, environment_id_path),
        None => Err(ServerEnvironmentIdPersistenceError::Initialize {
            path: environment_id_path.to_path_buf(),
        }),
    }
}

/// A persisted value that fails `EnvironmentId` validation is reported as an
/// initialization failure, since the file cannot be used as-is.
fn environment_id_from_persisted(
    value: String,
    environment_id_path: &Path,
) -> Result<EnvironmentId, ServerEnvironmentIdPersistenceError> {
    EnvironmentId::new(value).map_err(|_| ServerEnvironmentIdPersistenceError::Initialize {
        path: environment_id_path.to_path_buf(),
    })
}

/// `platformOs`, keyed by `std::env::consts::OS`.
fn platform_os(os: &str) -> ExecutionEnvironmentPlatformOs {
    match os {
        "macos" => ExecutionEnvironmentPlatformOs::Darwin,
        "linux" => ExecutionEnvironmentPlatformOs::Linux,
        "windows" => ExecutionEnvironmentPlatformOs::Windows,
        _ => ExecutionEnvironmentPlatformOs::Unknown,
    }
}

/// `platformArch`, keyed by `std::env::consts::ARCH`.
fn platform_arch(arch: &str) -> ExecutionEnvironmentPlatformArch {
    match arch {
        "aarch64" => ExecutionEnvironmentPlatformArch::Arm64,
        "x86_64" => ExecutionEnvironmentPlatformArch::X64,
        _ => ExecutionEnvironmentPlatformArch::Other,
    }
}

/// This machine as a T3 environment: identity, label and the capability
/// descriptor the clients read.
#[derive(Debug, Clone)]
pub struct ServerEnvironment {
    descriptor: ExecutionEnvironmentDescriptor,
}

impl ServerEnvironment {
    /// `make` for the local host. `state_dir`, `environment_id_path` and `cwd`
    /// come from [`crate::config::ServerDerivedPaths`] and the process config.
    pub fn load(
        state_dir: &Path,
        environment_id_path: &Path,
        cwd: &Path,
    ) -> Result<Self, ServerEnvironmentIdPersistenceError> {
        let environment_id = load_or_create_environment_id(state_dir, environment_id_path)?;
        let cwd_base_name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().trim().to_owned())
            .unwrap_or_default();
        let label = resolve_server_environment_label(&SystemHost, &cwd_base_name);
        Ok(Self::from_parts(environment_id, &label))
    }

    fn from_parts(environment_id: EnvironmentId, label: &str) -> Self {
        let file_attachments = FileAttachmentCapability {
            max_upload_bytes: PositiveInt::new(PROVIDER_SEND_TURN_MAX_FILE_BYTES)
                .expect("upload limit is positive"),
        };
        // Capability flags for features Turin keeps. Cut features (self-update,
        // desktop app update, cloud agent activity publishing, browser/preview)
        // are left unset.
        let capabilities = ExecutionEnvironmentCapabilities {
            repository_identity: true,
            connection_probe: Some(true),
            attachment_uploads: Some(true),
            question_attachments: Some(true),
            file_attachments: Some(file_attachments),
            pull_requests: Some(true),
            pull_request_checks: Some(true),
            inline_message_context: Some(true),
            required_worktree_bootstrap: Some(true),
            thread_settlement: Some(true),
            thread_auto_settlement: Some(true),
            storage_cleanup: Some(true),
            storage_cleanup_run: Some(true),
            otlp_endpoint_check: Some(true),
            project_worktree_cleanup: Some(true),
            worktrees_directory: Some(true),
            thread_auto_settlement_scope: Some(true),
            thread_restart_continuation: Some(true),
            project_settings_overrides: Some(true),
            project_provider_instance_overrides: Some(true),
            thread_snooze: Some(true),
            environment_themes: Some(true),
            usage_limit_sources: Some(true),
            usage_price_overrides: Some(true),
            usage_model_aliases: Some(true),
            thread_pinning: Some(true),
            thread_pin_reorder: Some(true),
            thread_active_reorder: Some(true),
            thread_auto_settle_opt_out: Some(true),
            thread_title_regeneration: Some(true),
            thread_visited_tracking: Some(true),
            thread_pull_request_linking: Some(true),
            server_resolved_command_context: Some(true),
            thread_pull_requests: Some(true),
            thread_pull_request_watch: Some(true),
            pull_request_stack_actions: Some(true),
            environment_icon: Some(true),
            project_clone_tracking: Some(true),
            ..Default::default()
        };
        Self {
            descriptor: ExecutionEnvironmentDescriptor {
                environment_id,
                label: TrimmedNonEmptyString::new(label).expect("label is non-empty"),
                platform: ExecutionEnvironmentPlatform {
                    os: platform_os(std::env::consts::OS),
                    arch: platform_arch(std::env::consts::ARCH),
                    machine: None,
                },
                server_version: TrimmedNonEmptyString::new(env!("CARGO_PKG_VERSION"))
                    .expect("crate version is non-empty"),
                orchestration_protocol_version: Some(ORCHESTRATION_PROTOCOL_VERSION),
                capabilities,
            },
        }
    }

    pub fn environment_id(&self) -> &EnvironmentId {
        &self.descriptor.environment_id
    }

    /// `getDescriptor`. Upstream also reads the cloud publish opt-in here;
    /// Turin has no cloud publish, so the descriptor is static.
    pub fn descriptor(&self) -> &ExecutionEnvironmentDescriptor {
        &self.descriptor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_then_reuses_the_environment_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environment-id");
        let first = load_or_create_environment_id(dir.path(), &path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.trim(), first.as_str());
        assert!(written.ends_with('\n'));
        let second = load_or_create_environment_id(dir.path(), &path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_blank_file_is_replaced_by_a_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environment-id");
        std::fs::write(&path, "  \n").unwrap();
        let id = load_or_create_environment_id(dir.path(), &path).unwrap();
        assert!(!id.as_str().is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), id.as_str());
    }

    #[test]
    fn an_existing_id_is_read_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environment-id");
        std::fs::write(&path, "  env-123 \n").unwrap();
        let id = load_or_create_environment_id(dir.path(), &path).unwrap();
        assert_eq!(id.as_str(), "env-123");
    }

    #[test]
    fn an_unwritable_state_dir_is_a_structured_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("missing");
        let path = state_dir.join("environment-id");
        let error = load_or_create_environment_id(&state_dir, &path).unwrap_err();
        assert!(matches!(
            error,
            ServerEnvironmentIdPersistenceError::Io {
                operation: EnvironmentIdOperation::Write,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            format!(
                "Server environment ID write failed at '{}'.",
                path.display()
            )
        );
    }

    #[test]
    fn the_descriptor_carries_the_protocol_version_and_label() {
        let environment =
            ServerEnvironment::from_parts(EnvironmentId::new("env-1").unwrap(), "Studio");
        let descriptor = environment.descriptor();
        assert_eq!(descriptor.label.as_str(), "Studio");
        assert_eq!(
            descriptor.orchestration_protocol_version,
            Some(ORCHESTRATION_PROTOCOL_VERSION)
        );
        assert!(descriptor.capabilities.repository_identity);
        assert_eq!(descriptor.capabilities.server_browser, None);
    }
}
