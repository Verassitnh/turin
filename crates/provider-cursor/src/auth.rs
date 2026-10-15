//! SDK-independent Cursor auth status.
//!
//! Upstream `auth.ts` drives the `@cursor/sdk` browser login and an in-memory
//! credential store. ACP does not. `cursor-agent acp` reads a stored login at
//! process start. `authenticate` is an explicit `cursor_login` sign-in only:
//! it opens a browser and can block for about twenty minutes. This module
//! never calls it automatically (`authenticateOnAuthRequired` stays false).
//!
//! The macOS Keychain reader is not ported. A credential file is the
//! SDK-independent source for account usage; its token is not returned from
//! status helpers.

use std::fs;
use std::path::Path;

use serde_json::Value;
use turin_acp::AcpError;

use crate::error::{CursorError, CursorOperation};

/// The only authentication method ACP advertises.
pub const CURSOR_LOGIN_METHOD: &str = "cursor_login";

/// Cursor's method has no `type`. Upstream ACP auto-authenticates on
/// `authRequired`. The Cursor flavour must not.
pub const AUTHENTICATE_ON_AUTH_REQUIRED: bool = false;

pub const NOT_SIGNED_IN_MESSAGE: &str =
    "Not signed in. Run cursor-agent login, then sign in with cursor_login.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInPhase {
    /// No session has been opened yet.
    Idle,
    /// `session/new`, `session/load`, or `session/list` reported auth required.
    NotSignedIn,
    /// A stored credential file has an access token, or a session opened.
    Authenticated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorSignInStatus {
    pub phase: SignInPhase,
    pub method_id: Option<String>,
    pub can_authenticate: bool,
    pub message: Option<String>,
}

impl CursorSignInStatus {
    pub fn idle() -> Self {
        Self {
            phase: SignInPhase::Idle,
            method_id: Some(CURSOR_LOGIN_METHOD.to_owned()),
            can_authenticate: true,
            message: None,
        }
    }

    pub fn not_signed_in() -> Self {
        Self {
            phase: SignInPhase::NotSignedIn,
            method_id: Some(CURSOR_LOGIN_METHOD.to_owned()),
            can_authenticate: true,
            message: Some(NOT_SIGNED_IN_MESSAGE.to_owned()),
        }
    }
}

/// `authenticate` params. Any other method id is invalid for this CLI.
pub fn authenticate_params() -> Value {
    serde_json::json!({ "methodId": CURSOR_LOGIN_METHOD })
}

pub fn require_cursor_login_method(method_id: &str) -> Result<(), CursorError> {
    if method_id == CURSOR_LOGIN_METHOD {
        Ok(())
    } else {
        Err(CursorError::new(
            CursorOperation::Authenticate,
            format!("Cursor ACP only accepts methodId {CURSOR_LOGIN_METHOD}."),
        ))
    }
}

pub fn is_auth_required_message(message: &str) -> bool {
    message.to_ascii_lowercase().contains("authentication required")
}

pub fn is_auth_required_error(error: &AcpError) -> bool {
    match error {
        AcpError::Request(request) => is_auth_required_message(&request.error_message),
        _ => false,
    }
}

/// Auth methods from `initialize`. `cursor_login` is the explicit sign-in.
pub fn status_from_auth_methods(methods: &[Value]) -> CursorSignInStatus {
    let cursor_login = methods.iter().any(|method| {
        method.get("id").and_then(Value::as_str) == Some(CURSOR_LOGIN_METHOD)
            || method.as_str() == Some(CURSOR_LOGIN_METHOD)
    });
    CursorSignInStatus {
        phase: SignInPhase::Unknown,
        method_id: cursor_login.then(|| CURSOR_LOGIN_METHOD.to_owned()),
        can_authenticate: cursor_login || methods.is_empty(),
        message: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialFileStatus {
    Missing,
    Unreadable,
    Empty,
    Present,
}

/// Presence only. The access token stays in the file.
pub fn credential_file_status(path: &Path) -> CredentialFileStatus {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CredentialFileStatus::Missing,
        Err(_) => CredentialFileStatus::Unreadable,
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => match value.get("accessToken").and_then(Value::as_str) {
                Some(token) if !token.is_empty() => CredentialFileStatus::Present,
                _ => CredentialFileStatus::Empty,
            },
            Err(_) => CredentialFileStatus::Unreadable,
        },
    }
}

pub fn status_from_credential_file(path: &Path) -> CursorSignInStatus {
    match credential_file_status(path) {
        CredentialFileStatus::Present => CursorSignInStatus {
            phase: SignInPhase::Authenticated,
            method_id: Some(CURSOR_LOGIN_METHOD.to_owned()),
            can_authenticate: true,
            message: None,
        },
        CredentialFileStatus::Missing | CredentialFileStatus::Empty => {
            CursorSignInStatus::not_signed_in()
        }
        CredentialFileStatus::Unreadable => CursorSignInStatus {
            phase: SignInPhase::Unknown,
            method_id: Some(CURSOR_LOGIN_METHOD.to_owned()),
            can_authenticate: true,
            message: Some("Cursor credentials could not be read.".to_owned()),
        },
    }
}

/// Args for `cursor-agent acp`. Never `--force` or `--yolo`.
pub const ACP_ARGS: &[&str] = &["acp"];

pub fn launch_is_unforced(args: &[&str]) -> bool {
    !args
        .iter()
        .any(|arg| *arg == "--force" || *arg == "--yolo")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_acp::AcpError;

    #[test]
    fn authenticate_is_explicit_cursor_login_only() {
        assert!(!AUTHENTICATE_ON_AUTH_REQUIRED);
        assert_eq!(
            authenticate_params(),
            json!({"methodId": "cursor_login"})
        );
        assert!(require_cursor_login_method("cursor_login").is_ok());
        assert!(require_cursor_login_method("api_key").is_err());
        assert!(launch_is_unforced(ACP_ARGS));
        assert!(!launch_is_unforced(&["acp", "--force"]));
        assert!(!launch_is_unforced(&["acp", "--yolo"]));
    }

    #[test]
    fn auth_required_text_maps_to_not_signed_in() {
        assert!(is_auth_required_message(
            "Authentication required. Please run 'cursor-agent login' first, then call authenticate() with methodId 'cursor_login'."
        ));
        let error = AcpError::from(turin_acp::errors::AcpRequestError::internal(
            "Authentication required. Please run 'cursor-agent login' first.",
        ));
        let mapped = CursorError::from_acp(CursorOperation::SessionNew, &error);
        assert!(mapped.not_signed_in);
        assert!(!mapped.detail.to_ascii_lowercase().contains("api key"));
    }

    #[test]
    fn credential_file_status_hides_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        assert_eq!(credential_file_status(&path), CredentialFileStatus::Missing);
        fs::write(&path, r#"{"accessToken":"secret-token"}"#).unwrap();
        assert_eq!(credential_file_status(&path), CredentialFileStatus::Present);
        let status = status_from_credential_file(&path);
        assert_eq!(status.phase, SignInPhase::Authenticated);
        assert!(!format!("{status:?}").contains("secret-token"));
        fs::write(&path, r#"{"accessToken":""}"#).unwrap();
        assert_eq!(
            status_from_credential_file(&path).phase,
            SignInPhase::NotSignedIn
        );
    }

    #[test]
    fn initialize_methods_advertise_cursor_login() {
        let status = status_from_auth_methods(&[json!({"id": "cursor_login"})]);
        assert!(status.can_authenticate);
        assert_eq!(status.method_id.as_deref(), Some(CURSOR_LOGIN_METHOD));
    }
}
