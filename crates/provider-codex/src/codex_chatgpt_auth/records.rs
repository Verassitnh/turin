//! Port of the persisted record schemas in upstream/apps/server/src/provider/CodexChatGptAuth.ts
//! (`Record`, `Sessions`, `Registration`, `RegistrationProfiles`).
//!
//! Field order is the upstream struct order: encoded bytes must keep opening
//! under a Node server sharing the same secret directory.

use serde::{Deserialize, Serialize};
use turin_contracts::base_schemas::{double_option, js_number, js_number_nullable};

/// `Record` in the upstream file: one token set bound to one OAuth client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexChatGptCredentials {
    pub client_id: String,
    pub access_token: String,
    /// `NullOr` with a required key: encodes `null`.
    pub refresh_token: Option<String>,
    #[serde(with = "js_number")]
    pub expires_at: f64,
    /// `NullOr` with a required key: encodes `null`.
    #[serde(with = "js_number_nullable")]
    pub earliest_refresh_at: Option<f64>,
    pub scopes: Vec<String>,
    pub subject: String,
    /// `NullOr` with a required key: encodes `null`.
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

/// `Sessions`: every saved token set plus the active client id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sessions {
    pub active_client_id: Option<String>,
    pub sessions: Vec<CodexChatGptCredentials>,
}

/// `Registration`: the OAuth client a profile was registered with. Stored
/// without the transient `idTokenHint` of `ChatGptReconnectProfile`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRegistration {
    #[serde(deserialize_with = "deserialize_client_id")]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub email: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_redirect_uri"
    )]
    pub redirect_uri: Option<String>,
}

/// `RegistrationProfiles`: every saved registration, most recent first on read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationProfiles {
    pub profiles: Vec<StoredRegistration>,
    pub last_client_id: Option<String>,
}

/// Decode `Schema.Union([Sessions, Record])`: the legacy single-record shape
/// is accepted until the next save.
pub fn decode_sessions(text: &str) -> Result<Sessions, DecodeError> {
    if let Ok(sessions) = serde_json::from_str::<Sessions>(text) {
        return Ok(sessions);
    }
    let record = serde_json::from_str::<CodexChatGptCredentials>(text)?;
    Ok(Sessions {
        active_client_id: Some(record.client_id.clone()),
        sessions: vec![record],
    })
}

/// Decode `Schema.Union([RegistrationProfiles, Registration])` into the list
/// form. A legacy single registration becomes the only, last-used profile.
pub fn decode_registrations(text: &str) -> Result<RegistrationProfiles, DecodeError> {
    if let Ok(profiles) = serde_json::from_str::<RegistrationProfiles>(text) {
        return Ok(profiles);
    }
    let registration = serde_json::from_str::<StoredRegistration>(text)?;
    Ok(RegistrationProfiles {
        last_client_id: Some(registration.client_id.clone()),
        profiles: vec![registration],
    })
}

/// Only the decoder's failure matters to callers; they map it to a safe
/// user-facing message and never echo the stored text.
#[derive(Debug, thiserror::Error)]
#[error("invalid stored ChatGPT record")]
pub struct DecodeError(#[from] serde_json::Error);

/// `Schema.String.check(Schema.isPattern(/^oaiapp_/u))`.
fn deserialize_client_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.starts_with("oaiapp_") {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("clientId must start with oaiapp_"))
    }
}

/// `Schema.isPattern(/^http:\/\/(?:127\.0\.0\.1|localhost):[1-9]\d{0,4}\/auth\/callback$/u)`.
/// `\d` is ASCII-only in JS with the `u` flag, so the digit class is spelled out.
fn deserialize_redirect_uri<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        Some(value) if is_loopback_redirect_uri(&value) => Ok(Some(value)),
        Some(_) => Err(serde::de::Error::custom(
            "redirectUri must be a loopback callback",
        )),
        None => Ok(None),
    }
}

pub fn is_loopback_redirect_uri(value: &str) -> bool {
    let rest = value
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| value.strip_prefix("http://localhost:"));
    let Some(port_and_path) = rest else {
        return false;
    };
    let Some(port) = port_and_path.strip_suffix("/auth/callback") else {
        return false;
    };
    let digits = port.as_bytes();
    (1..=5).contains(&digits.len())
        && digits[0] != b'0'
        && digits.iter().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = r#"{"clientId":"oaiapp_a","accessToken":"tok","refreshToken":null,"expiresAt":1,"earliestRefreshAt":null,"scopes":["chatgpt.tokens.use.direct"],"subject":"s","email":null}"#;

    #[test]
    fn legacy_record_decodes_as_the_only_active_session() {
        let sessions = decode_sessions(RECORD).unwrap();
        assert_eq!(sessions.active_client_id.as_deref(), Some("oaiapp_a"));
        assert_eq!(sessions.sessions.len(), 1);
    }

    #[test]
    fn saved_sessions_keep_upstream_field_order_and_null_keys() {
        let sessions = Sessions {
            active_client_id: None,
            sessions: vec![decode_sessions(RECORD).unwrap().sessions.remove(0)],
        };
        let json = serde_json::to_string(&sessions).unwrap();
        assert!(json.starts_with(r#"{"activeClientId":null,"sessions":[{"clientId":"oaiapp_a","accessToken":"tok","refreshToken":null,"expiresAt":1,"earliestRefreshAt":null,"scopes":["chatgpt.tokens.use.direct"],"subject":"s","email":null}]}"#), "{json}");
    }

    #[test]
    fn registration_client_ids_and_redirects_are_checked() {
        let bad_client = r#"{"clientId":"client_x"}"#;
        assert!(decode_registrations(bad_client).is_err());
        let bad_port =
            r#"{"clientId":"oaiapp_a","redirectUri":"http://127.0.0.1:01234/auth/callback"}"#;
        assert!(decode_registrations(bad_port).is_err());
        let ok = r#"{"clientId":"oaiapp_a","redirectUri":"http://localhost:7001/auth/callback"}"#;
        assert!(decode_registrations(ok).is_ok());
    }

    #[test]
    fn loopback_redirect_uris_match_the_upstream_pattern() {
        assert!(is_loopback_redirect_uri("http://127.0.0.1:1/auth/callback"));
        assert!(is_loopback_redirect_uri(
            "http://localhost:65535/auth/callback"
        ));
        assert!(!is_loopback_redirect_uri(
            "http://localhost:123456/auth/callback"
        ));
        assert!(!is_loopback_redirect_uri(
            "http://127.0.0.1:1/auth/callback/"
        ));
        assert!(!is_loopback_redirect_uri(
            "https://localhost:1/auth/callback"
        ));
        assert!(!is_loopback_redirect_uri(
            "http://127.0.0.1:\u{0661}/auth/callback"
        ));
    }

    #[test]
    fn a_legacy_registration_becomes_the_last_used_profile() {
        let profiles = decode_registrations(r#"{"clientId":"oaiapp_a"}"#).unwrap();
        assert_eq!(profiles.last_client_id.as_deref(), Some("oaiapp_a"));
        assert_eq!(profiles.profiles[0].client_id, "oaiapp_a");
    }
}
