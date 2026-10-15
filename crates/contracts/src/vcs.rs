//! Port of upstream/packages/contracts/src/vcs.ts

use crate::base_schemas::{EffectOption, NonNegativeInt, TrimmedNonEmptyString};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VcsDriverKind {
    Git,
    Jj,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VcsFreshnessSource {
    LiveLocal,
    CachedLocal,
    CachedRemote,
    ExplicitRemote,
}

/// `VcsFreshness.observedAt` / `expiresAt` are `Schema.DateTimeUtc` /
/// `Schema.Option(Schema.DateTimeUtc)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsFreshness {
    pub source: VcsFreshnessSource,
    pub observed_at: crate::base_schemas::UtcDateTime,
    pub expires_at: EffectOption<crate::base_schemas::UtcDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsDriverCapabilities {
    pub kind: VcsDriverKind,
    pub supports_worktrees: bool,
    pub supports_bookmarks: bool,
    pub supports_atomic_snapshot: bool,
    pub supports_push_default_remote: bool,
    pub ignore_classifier: VcsIgnoreClassifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VcsIgnoreClassifier {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "git-compatible-fallback")]
    GitCompatibleFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRepositoryIdentity {
    pub kind: VcsDriverKind,
    pub root_path: TrimmedNonEmptyString,
    pub metadata_path: Option<TrimmedNonEmptyString>,
    pub freshness: VcsFreshness,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsListWorkspaceFilesResult {
    pub paths: Vec<TrimmedNonEmptyString>,
    pub truncated: bool,
    pub freshness: VcsFreshness,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRemote {
    pub name: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
    pub push_url: EffectOption<TrimmedNonEmptyString>,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsListRemotesResult {
    pub remotes: Vec<VcsRemote>,
    pub freshness: VcsFreshness,
}

/// Context every process-boundary error carries (upstream `VcsProcessErrorContext`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsProcessErrorContext {
    pub operation: String,
    pub command: String,
    pub cwd: String,
    pub argument_count: Option<NonNegativeInt>,
}

/// Upstream `VcsProcessSpawnFailure`.
#[derive(Debug, Clone)]
pub struct VcsProcessSpawnFailure {
    pub cause: serde_json::Value,
}

/// Upstream `VcsProcessTimeoutFailure`.
#[derive(Debug, Clone)]
pub struct VcsProcessTimeoutFailure {
    pub timeout_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VcsProcessExitFailureKind {
    Authentication,
    NotFound,
    RateLimited,
    CommandFailed,
}

/// Upstream `VcsProcessExitFailure`.
#[derive(Debug, Clone)]
pub struct VcsProcessExitFailure {
    pub exit_code: f64,
    pub stderr: String,
    pub stderr_truncated: bool,
}

/// `VcsProcessSpawnError` (Schema.TaggedError): `{"_tag": …, …fields}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessSpawnError {
    #[serde(rename = "VcsProcessSpawnError")]
    #[error("VCS process failed to spawn in {operation}: {command} ({cwd})")]
    VcsProcessSpawnError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
        cause: serde_json::Value,
    },
}

impl VcsProcessSpawnError {
    pub fn from_process_spawn_error(
        context: VcsProcessErrorContext,
        error: VcsProcessSpawnFailure,
    ) -> Self {
        Self::VcsProcessSpawnError {
            operation: context.operation,
            command: context.command,
            cwd: context.cwd,
            argument_count: context.argument_count,
            cause: error.cause,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessExitError {
    #[serde(rename = "VcsProcessExitError")]
    #[error(
        "VCS process failed in {operation}: {command} ({cwd}) exited with {exit_code} - {detail}"
    )]
    VcsProcessExitError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
        exit_code: f64,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_kind: Option<VcsProcessExitFailureKind>,
        /// Process-boundary hint for a recognized transient failure; absence is not retryable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retryable: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_length: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_truncated: Option<bool>,
    },
}

impl VcsProcessExitError {
    pub fn from_process_exit(
        context: VcsProcessErrorContext,
        error: VcsProcessExitFailure,
        failure_kind: VcsProcessExitFailureKind,
        retryable: Option<bool>,
    ) -> Self {
        let detail = match failure_kind {
            VcsProcessExitFailureKind::Authentication => "Authentication failed.".to_owned(),
            VcsProcessExitFailureKind::RateLimited => "API rate limit exceeded.".to_owned(),
            // Upstream also special-cases `glab` ("Merge request not found.") and
            // `az`; both hosts are cut (see PORTING.md Deviations).
            VcsProcessExitFailureKind::NotFound => if context.command == "gh" {
                "Pull request not found."
            } else {
                "VCS resource not found."
            }
            .to_owned(),
            VcsProcessExitFailureKind::CommandFailed => {
                "Process exited with a non-zero status.".to_owned()
            }
        };
        Self::VcsProcessExitError {
            operation: context.operation,
            command: context.command,
            cwd: context.cwd,
            argument_count: context.argument_count,
            exit_code: error.exit_code,
            detail,
            failure_kind: Some(failure_kind),
            retryable: if retryable == Some(true) {
                Some(true)
            } else {
                None
            },
            // JS `string.length`: UTF-16 code units, not bytes.
            stderr_length: Some(error.stderr.encode_utf16().count() as u64),
            stderr_truncated: Some(error.stderr_truncated),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessTimeoutError {
    #[serde(rename = "VcsProcessTimeoutError")]
    #[error("VCS process timed out in {operation}: {command} ({cwd}) after {timeout_ms}ms")]
    VcsProcessTimeoutError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
        timeout_ms: f64,
    },
}

impl VcsProcessTimeoutError {
    pub fn from_process_timeout_error(
        context: VcsProcessErrorContext,
        error: VcsProcessTimeoutFailure,
    ) -> Self {
        Self::VcsProcessTimeoutError {
            operation: context.operation,
            command: context.command,
            cwd: context.cwd,
            argument_count: context.argument_count,
            timeout_ms: error.timeout_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessStdinWriteError {
    #[serde(rename = "VcsProcessStdinWriteError")]
    #[error(
        "VCS process failed to write {stdin_bytes} bytes to stdin in {operation}: {command} ({cwd})"
    )]
    VcsProcessStdinWriteError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
        stdin_bytes: NonNegativeInt,
        cause: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VcsProcessStream {
    Stdout,
    Stderr,
    ExitCode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessOutputReadError {
    #[serde(rename = "VcsProcessOutputReadError")]
    #[error("VCS process failed to read {stream} in {operation}: {command} ({cwd})")]
    VcsProcessOutputReadError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
        stream: VcsProcessStream,
        cause: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VcsProcessOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessOutputLimitError {
    #[serde(rename = "VcsProcessOutputLimitError")]
    #[error(
        "VCS process {stream} produced {observed_bytes} bytes in {operation}: {command} ({cwd}), exceeding the {max_bytes} byte limit"
    )]
    VcsProcessOutputLimitError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
        stream: VcsProcessOutputStream,
        max_bytes: NonNegativeInt,
        observed_bytes: NonNegativeInt,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsProcessMissingExitCodeError {
    #[serde(rename = "VcsProcessMissingExitCodeError")]
    #[error("VCS process completed without an exit code in {operation}: {command} ({cwd})")]
    VcsProcessMissingExitCodeError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_count: Option<NonNegativeInt>,
    },
}

/// `VcsOutputDecodeError`: union of the stream decode failures. Untagged like
/// upstream's `Schema.Union`; each member validates its own `_tag`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(untagged)]
pub enum VcsOutputDecodeError {
    #[error(transparent)]
    StdinWrite(VcsProcessStdinWriteError),
    #[error(transparent)]
    OutputRead(VcsProcessOutputReadError),
    #[error(transparent)]
    OutputLimit(VcsProcessOutputLimitError),
    #[error(transparent)]
    MissingExitCode(VcsProcessMissingExitCodeError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsRepositoryDetectionError {
    #[serde(rename = "VcsRepositoryDetectionError")]
    #[error("VCS repository detection failed in {operation}: {cwd} - {detail}")]
    VcsRepositoryDetectionError {
        operation: String,
        cwd: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VcsUnsupportedOperationError {
    #[serde(rename = "VcsUnsupportedOperationError")]
    #[error("VCS operation is unsupported for {kind} in {operation}: {detail}")]
    VcsUnsupportedOperationError {
        operation: String,
        kind: VcsDriverKind,
        detail: String,
    },
}

/// `VcsError`: the full process-boundary union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(untagged)]
pub enum VcsError {
    #[error(transparent)]
    Spawn(VcsProcessSpawnError),
    #[error(transparent)]
    Exit(VcsProcessExitError),
    #[error(transparent)]
    Timeout(VcsProcessTimeoutError),
    #[error(transparent)]
    StdinWrite(VcsProcessStdinWriteError),
    #[error(transparent)]
    OutputRead(VcsProcessOutputReadError),
    #[error(transparent)]
    OutputLimit(VcsProcessOutputLimitError),
    #[error(transparent)]
    MissingExitCode(VcsProcessMissingExitCodeError),
    #[error(transparent)]
    RepositoryDetection(VcsRepositoryDetectionError),
    #[error(transparent)]
    UnsupportedOperation(VcsUnsupportedOperationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_error_detail_by_failure_kind() {
        let context = VcsProcessErrorContext {
            operation: "status".to_owned(),
            command: "gh".to_owned(),
            cwd: "/repo".to_owned(),
            argument_count: None,
        };
        let error = VcsProcessExitError::from_process_exit(
            context,
            VcsProcessExitFailure {
                exit_code: 1.0,
                stderr: "err".to_owned(),
                stderr_truncated: false,
            },
            VcsProcessExitFailureKind::NotFound,
            None,
        );
        let VcsProcessExitError::VcsProcessExitError { detail, .. } = &error;
        assert_eq!(detail, "Pull request not found.");

        // The wire form carries _tag first, then fields, like upstream TaggedError.
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["_tag"], "VcsProcessExitError");
        assert_eq!(json["failureKind"], "not-found");
        assert_eq!(json["exitCode"], 1.0);
        let back: VcsError = serde_json::from_value(json).unwrap();
        assert!(matches!(back, VcsError::Exit(_)));
    }

    #[test]
    fn freshness_round_trips_option_shapes() {
        let json = serde_json::json!({
            "source": "live-local",
            "observedAt": "2026-10-14T00:00:00.000Z",
            "expiresAt": {"_tag": "Some", "value": "2026-10-14T00:05:00.000Z"},
        });
        let freshness: VcsFreshness = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&freshness).unwrap(), json);

        let none = serde_json::json!({
            "source": "live-local",
            "observedAt": "2026-10-14T00:00:00.000Z",
            "expiresAt": {"_tag": "None"},
        });
        let freshness: VcsFreshness = serde_json::from_value(none.clone()).unwrap();
        assert_eq!(serde_json::to_value(&freshness).unwrap(), none);
    }

    use serde_json::{Value, json};

    fn context(command: &str) -> VcsProcessErrorContext {
        VcsProcessErrorContext {
            operation: "status".to_owned(),
            command: command.to_owned(),
            cwd: "/repo".to_owned(),
            argument_count: Some(3),
        }
    }

    #[test]
    fn exit_error_details_cover_every_failure_kind() {
        let exit = |command: &str, kind, retryable, stderr: &str| {
            VcsProcessExitError::from_process_exit(
                context(command),
                VcsProcessExitFailure {
                    exit_code: 2.0,
                    stderr: stderr.to_owned(),
                    stderr_truncated: true,
                },
                kind,
                retryable,
            )
        };
        let detail = |error: &VcsProcessExitError| {
            let VcsProcessExitError::VcsProcessExitError { detail, .. } = error;
            detail.clone()
        };
        use VcsProcessExitFailureKind::*;
        assert_eq!(
            detail(&exit("gh", Authentication, None, "")),
            "Authentication failed."
        );
        assert_eq!(
            detail(&exit("gh", RateLimited, None, "")),
            "API rate limit exceeded."
        );
        assert_eq!(
            detail(&exit("gh", NotFound, None, "")),
            "Pull request not found."
        );
        assert_eq!(
            detail(&exit("git", NotFound, None, "")),
            "VCS resource not found."
        );
        assert_eq!(
            detail(&exit("git", CommandFailed, None, "")),
            "Process exited with a non-zero status."
        );

        // `retryable` is only ever written when true; stderrLength counts UTF-16 units.
        let retried = serde_json::to_value(exit("gh", RateLimited, Some(true), "😀a")).unwrap();
        assert_eq!(retried["retryable"], json!(true));
        assert_eq!(retried["stderrLength"], json!(3));
        assert_eq!(retried["stderrTruncated"], json!(true));
        assert_eq!(retried["argumentCount"], json!(3));
        let not_retried = serde_json::to_value(exit("gh", RateLimited, Some(false), "")).unwrap();
        assert!(not_retried.get("retryable").is_none());
        let unspecified = serde_json::to_value(exit("gh", RateLimited, None, "")).unwrap();
        assert!(unspecified.get("retryable").is_none());
    }

    #[test]
    fn spawn_and_timeout_constructors_copy_the_context() {
        let spawn = VcsProcessSpawnError::from_process_spawn_error(
            context("git"),
            VcsProcessSpawnFailure {
                cause: json!({"message": "ENOENT"}),
            },
        );
        assert_eq!(
            serde_json::to_value(&spawn).unwrap(),
            json!({
                "_tag": "VcsProcessSpawnError", "operation": "status", "command": "git",
                "cwd": "/repo", "argumentCount": 3, "cause": {"message": "ENOENT"},
            })
        );
        assert_eq!(
            spawn.to_string(),
            "VCS process failed to spawn in status: git (/repo)"
        );
        let timeout = VcsProcessTimeoutError::from_process_timeout_error(
            VcsProcessErrorContext {
                argument_count: None,
                ..context("git")
            },
            VcsProcessTimeoutFailure { timeout_ms: 1500.5 },
        );
        assert_eq!(
            timeout.to_string(),
            "VCS process timed out in status: git (/repo) after 1500.5ms"
        );
        let json = serde_json::to_value(&timeout).unwrap();
        assert!(json.get("argumentCount").is_none());
        assert_eq!(json["timeoutMs"], json!(1500.5));
    }

    fn error_samples() -> Vec<Value> {
        let base =
            |tag: &str| json!({"_tag": tag, "operation": "op", "command": "git", "cwd": "/r"});
        let with = |tag: &str, extra: Value| {
            let mut value = base(tag);
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            value
        };
        vec![
            with("VcsProcessSpawnError", json!({"cause": null})),
            with(
                "VcsProcessExitError",
                json!({"exitCode": 128, "detail": "d"}),
            ),
            with("VcsProcessTimeoutError", json!({"timeoutMs": 10})),
            with(
                "VcsProcessStdinWriteError",
                json!({"stdinBytes": 7, "cause": "x"}),
            ),
            with(
                "VcsProcessOutputReadError",
                json!({"stream": "exitCode", "cause": 1}),
            ),
            with(
                "VcsProcessOutputLimitError",
                json!({"stream": "stderr", "maxBytes": 1, "observedBytes": 2}),
            ),
            base("VcsProcessMissingExitCodeError"),
            json!({"_tag": "VcsRepositoryDetectionError", "operation": "o", "cwd": "/r", "detail": "d"}),
            json!({"_tag": "VcsUnsupportedOperationError", "operation": "o", "kind": "jj", "detail": "d"}),
        ]
    }

    #[test]
    fn vcs_error_union_decodes_every_member_by_tag_and_round_trips() {
        for sample in error_samples() {
            let error: VcsError = serde_json::from_value(sample.clone()).unwrap();
            assert_eq!(serde_json::to_value(&error).unwrap(), sample);
        }
        // Not forward compatible: an unknown tag fails the union.
        assert!(
            serde_json::from_value::<VcsError>(json!({"_tag": "VcsFutureError", "operation": "o"}))
                .is_err()
        );
        // A known tag with a broken payload fails too (no cause, bad stream).
        assert!(
            serde_json::from_value::<VcsError>(json!({
                "_tag": "VcsProcessSpawnError", "operation": "o", "command": "c", "cwd": "/"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<VcsError>(json!({
                "_tag": "VcsProcessOutputLimitError", "operation": "o", "command": "c", "cwd": "/",
                "stream": "exitCode", "maxBytes": 1, "observedBytes": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn output_decode_error_union_excludes_other_vcs_errors() {
        let samples = error_samples();
        for (index, sample) in samples.iter().enumerate() {
            let decoded = serde_json::from_value::<VcsOutputDecodeError>(sample.clone());
            assert_eq!(decoded.is_ok(), (3..=6).contains(&index), "{sample}");
        }
    }

    #[test]
    fn error_messages_match_upstream() {
        let messages: Vec<String> = error_samples()
            .into_iter()
            .map(|sample| {
                serde_json::from_value::<VcsError>(sample)
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            messages[1],
            "VCS process failed in op: git (/r) exited with 128 - d"
        );
        assert_eq!(
            messages[3],
            "VCS process failed to write 7 bytes to stdin in op: git (/r)"
        );
        assert_eq!(
            messages[4],
            "VCS process failed to read exitCode in op: git (/r)"
        );
        assert_eq!(
            messages[5],
            "VCS process stderr produced 2 bytes in op: git (/r), exceeding the 1 byte limit"
        );
        assert_eq!(
            messages[6],
            "VCS process completed without an exit code in op: git (/r)"
        );
        assert_eq!(messages[7], "VCS repository detection failed in o: /r - d");
        assert_eq!(messages[8], "VCS operation is unsupported for jj in o: d");
    }

    #[test]
    fn freshness_normalises_instants_and_keeps_option_wire_shape() {
        let freshness: VcsFreshness = serde_json::from_value(json!({
            "source": "cached-remote",
            "observedAt": "2026-10-14T02:00:00+02:00",
            "expiresAt": {"_tag": "None"},
        }))
        .unwrap();
        let encoded = serde_json::to_value(&freshness).unwrap();
        // DateTimeUtc always encodes as YYYY-MM-DDTHH:MM:SS.sssZ.
        assert_eq!(encoded["observedAt"], json!("2026-10-14T00:00:00.000Z"));
        // `expiresAt` is required: absence is not None.
        assert!(
            serde_json::from_value::<VcsFreshness>(json!({
                "source": "live-local", "observedAt": "2026-10-14T00:00:00Z",
            }))
            .is_err()
        );
        for source in [
            "live-local",
            "cached-local",
            "cached-remote",
            "explicit-remote",
        ] {
            let parsed: VcsFreshnessSource = serde_json::from_value(json!(source)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(source));
        }
        assert!(serde_json::from_value::<VcsFreshnessSource>(json!("stale")).is_err());
    }

    #[test]
    fn identity_remotes_and_capabilities_round_trip() {
        let fresh = json!({
            "source": "live-local", "observedAt": "2026-10-14T00:00:00.000Z",
            "expiresAt": {"_tag": "None"},
        });
        let identity = json!({
            "kind": "git", "rootPath": "/repo", "metadataPath": null, "freshness": fresh,
        });
        let decoded: VcsRepositoryIdentity = serde_json::from_value(identity.clone()).unwrap();
        assert!(decoded.metadata_path.is_none());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), identity);
        assert!(
            serde_json::from_value::<VcsRepositoryIdentity>(
                json!({"kind": "git", "rootPath": "  ", "metadataPath": null, "freshness": fresh})
            )
            .is_err()
        );

        let remotes = json!({
            "remotes": [
                {"name": "origin", "url": "git@github.com:o/r.git",
                 "pushUrl": {"_tag": "Some", "value": "git@github.com:o/r.git"}, "isPrimary": true},
                {"name": "up", "url": "https://x/y.git", "pushUrl": {"_tag": "None"}, "isPrimary": false},
            ],
            "freshness": fresh,
        });
        let decoded: VcsListRemotesResult = serde_json::from_value(remotes.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), remotes);

        let files = json!({"paths": ["a", "b"], "truncated": true, "freshness": fresh});
        let decoded: VcsListWorkspaceFilesResult = serde_json::from_value(files.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), files);

        let capabilities = json!({
            "kind": "unknown", "supportsWorktrees": false, "supportsBookmarks": false,
            "supportsAtomicSnapshot": true, "supportsPushDefaultRemote": false,
            "ignoreClassifier": "git-compatible-fallback",
        });
        let decoded: VcsDriverCapabilities = serde_json::from_value(capabilities.clone()).unwrap();
        assert_eq!(
            decoded.ignore_classifier,
            VcsIgnoreClassifier::GitCompatibleFallback
        );
        assert_eq!(serde_json::to_value(&decoded).unwrap(), capabilities);
        let mut bad = capabilities;
        bad["ignoreClassifier"] = json!("fallback");
        assert!(serde_json::from_value::<VcsDriverCapabilities>(bad).is_err());
    }
}

impl std::fmt::Display for VcsProcessStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::ExitCode => "exitCode",
        })
    }
}

impl std::fmt::Display for VcsProcessOutputStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

impl std::fmt::Display for VcsDriverKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Git => "git",
            Self::Jj => "jj",
            Self::Unknown => "unknown",
        })
    }
}
