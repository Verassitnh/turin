//! Port of upstream/packages/contracts/src/providerSetup.ts

use crate::base_schemas::{
    ContractError, EnvironmentId, IsoDateTime, NonNegativeInt, TrimmedNonEmptyString,
    double_option, forward_compatible_array,
};
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId, optional_unknown};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Decode-time `Schema.isMaxLength(n)` / `isBetween` helpers. Lengths count
/// UTF-16 code units, like JS `string.length`.
mod checks {
    use super::*;
    use serde::de::Error;

    fn utf16_len(value: &str) -> usize {
        value.encode_utf16().count()
    }

    /// `Schema.String.check(Schema.isMaxLength(N))`
    pub fn max_string<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<String, D::Error> {
        let value = String::deserialize(d)?;
        if utf16_len(&value) > N {
            return Err(D::Error::custom(format!(
                "expected a string of at most {N} characters"
            )));
        }
        Ok(value)
    }

    /// `Schema.optionalKey(Schema.String.check(Schema.isMaxLength(N)))`
    pub fn max_string_opt<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<Option<String>, D::Error> {
        let value = Option::<String>::deserialize(d)?;
        if let Some(value) = &value {
            if utf16_len(value) > N {
                return Err(D::Error::custom(format!(
                    "expected a string of at most {N} characters"
                )));
            }
        }
        Ok(value)
    }

    /// `Schema.NullOr(Schema.String.check(Schema.isMaxLength(N)))`
    pub fn max_string_nullable<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<Option<String>, D::Error> {
        max_string_opt::<D, N>(d)
    }

    /// `Schema.NonEmptyString.check(Schema.isMaxLength(N))` (not trimmed).
    pub fn non_empty_max_string<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<String, D::Error> {
        let value = max_string::<D, N>(d)?;
        if value.is_empty() {
            return Err(D::Error::custom("expected a non-empty string"));
        }
        Ok(value)
    }

    /// `TrimmedNonEmptyString.check(Schema.isMaxLength(N))`
    pub fn max_trimmed<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<TrimmedNonEmptyString, D::Error> {
        let value = TrimmedNonEmptyString::deserialize(d)?;
        if utf16_len(value.as_str()) > N {
            return Err(D::Error::custom(format!(
                "expected a string of at most {N} characters"
            )));
        }
        Ok(value)
    }

    /// `Schema.Array(X).check(Schema.isMaxLength(N))`
    pub fn max_items<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        let values = Vec::<T>::deserialize(d)?;
        if values.len() > N {
            return Err(D::Error::custom(format!(
                "expected an array of at most {N} items"
            )));
        }
        Ok(values)
    }

    /// `Schema.Int.check(Schema.isBetween({ minimum: MIN, maximum: MAX }))`
    pub fn int_between<'de, D: Deserializer<'de>, const MIN: i64, const MAX: i64>(
        d: D,
    ) -> Result<i64, D::Error> {
        let value = i64::deserialize(d)?;
        if !(MIN..=MAX).contains(&value) {
            return Err(D::Error::custom(format!(
                "expected an integer between {MIN} and {MAX}"
            )));
        }
        Ok(value)
    }

    /// `Schema.record(SetupOperationId, String.check(max 16_384)).check(isMaxProperties(16))`
    pub fn credential_values<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<indexmap::IndexMap<SetupOperationId, String>, D::Error> {
        let values = indexmap::IndexMap::<SetupOperationId, String>::deserialize(d)?;
        if values.len() > 16 {
            return Err(D::Error::custom("expected at most 16 credential values"));
        }
        if values.values().any(|value| utf16_len(value) > 16_384) {
            return Err(D::Error::custom(
                "expected credential values of at most 16384 characters",
            ));
        }
        Ok(values)
    }

    /// `Schema.String.check(Schema.isPattern(/^oaiapp_[\w-]+$/u))`
    pub fn oaiapp_client_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let value = String::deserialize(d)?;
        let valid = value.strip_prefix("oaiapp_").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
        if !valid {
            return Err(D::Error::custom(
                "expected a client id matching ^oaiapp_[\\w-]+$",
            ));
        }
        Ok(value)
    }

    /// `Schema.optionalKey(Schema.String.check(Schema.isPattern(
    /// /^http:\/\/(?:127\.0\.0\.1|localhost):[1-9]\d{0,4}\/auth\/callback$/u)))`
    pub fn loopback_redirect_uri<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<String>, D::Error> {
        let Some(value) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        if is_loopback_redirect_uri(&value) {
            Ok(Some(value))
        } else {
            Err(D::Error::custom(
                "expected a loopback /auth/callback redirect uri",
            ))
        }
    }

    pub(super) fn is_loopback_redirect_uri(value: &str) -> bool {
        let Some(rest) = value.strip_prefix("http://") else {
            return false;
        };
        let Some(rest) = rest
            .strip_prefix("127.0.0.1")
            .or_else(|| rest.strip_prefix("localhost"))
        else {
            return false;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            return false;
        };
        let Some(port) = rest.strip_suffix("/auth/callback") else {
            return false;
        };
        (1..=5).contains(&port.len())
            && port.bytes().all(|b| b.is_ascii_digit())
            && !port.starts_with('0')
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSetupInput {
    pub instance_id: ProviderInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAuthCallbackInput {
    #[serde(deserialize_with = "checks::max_string::<_, 16384>")]
    pub authorization_url: String,
    #[serde(deserialize_with = "checks::max_string::<_, 4096>")]
    pub return_url: String,
    pub environment_id: EnvironmentId,
    pub instance_id: ProviderInstanceId,
    #[serde(deserialize_with = "checks::max_trimmed::<_, 128>")]
    pub flow_id: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CodexAuthCallbackState {
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "finished")]
    Finished {
        #[serde(deserialize_with = "checks::max_string::<_, 16384>")]
        callback_url: String,
    },
}

/// `SetupOperationId`: trimmed non-empty, ≤ 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetupOperationId(TrimmedNonEmptyString);

impl SetupOperationId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        // `Schema.isMaxLength` counts UTF-16 code units.
        if value.as_str().encode_utf16().count() > 128 {
            return Err(ContractError {
                expected: "a setup operation id of at most 128 chars",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    pub fn into_inner(self) -> TrimmedNonEmptyString {
        self.0
    }
}

impl fmt::Display for SetupOperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl AsRef<str> for SetupOperationId {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl Serialize for SetupOperationId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for SetupOperationId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderAuthMethodType {
    Agent,
    Terminal,
    Credentials,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthMethod {
    pub id: SetupOperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<TrimmedNonEmptyString>,
    pub name: TrimmedNonEmptyString,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub type_: ProviderAuthMethodType,
}

// These describe client interactions, not OAuth grant types. The provider
// adapter remains responsible for credentials, callbacks, and refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderAuthInteraction {
    #[serde(rename = "browser")]
    Browser {
        id: SetupOperationId,
        #[serde(deserialize_with = "checks::max_trimmed::<_, 16384>")]
        url: TrimmedNonEmptyString,
        requires_consent: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accepts_callback: Option<bool>,
    },
    #[serde(rename = "deviceCode")]
    DeviceCode {
        id: SetupOperationId,
        #[serde(deserialize_with = "checks::max_trimmed::<_, 16384>")]
        url: TrimmedNonEmptyString,
        #[serde(deserialize_with = "checks::max_trimmed::<_, 256>")]
        user_code: TrimmedNonEmptyString,
    },
    #[serde(rename = "terminal")]
    Terminal {
        id: SetupOperationId,
        #[serde(deserialize_with = "checks::max_string::<_, 16384>")]
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_offset: Option<NonNegativeInt>,
    },
    #[serde(rename = "credentials")]
    Credentials {
        id: SetupOperationId,
        #[serde(deserialize_with = "checks::max_items::<_, _, 16>")]
        fields: Vec<ProviderAuthCredentialsField>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthCredentialsField {
    pub name: SetupOperationId,
    pub label: TrimmedNonEmptyString,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderAuthResponse {
    #[serde(rename = "browser")]
    Browser { action: ProviderAuthBrowserAction },
    #[serde(rename = "terminal")]
    Terminal {
        #[serde(deserialize_with = "checks::max_string::<_, 4096>")]
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ProviderAuthTerminalSize>,
    },
    #[serde(rename = "credentials")]
    Credentials {
        #[serde(deserialize_with = "checks::credential_values")]
        values: indexmap::IndexMap<SetupOperationId, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderAuthBrowserAction {
    Accept,
    Decline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthTerminalSize {
    #[serde(deserialize_with = "checks::int_between::<_, 1, 500>")]
    pub cols: i64,
    #[serde(deserialize_with = "checks::int_between::<_, 1, 200>")]
    pub rows: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthStartInput {
    pub instance_id: ProviderInstanceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_id: Option<SetupOperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_mode: Option<ProviderAuthCallbackMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderAuthCallbackMode {
    Server,
    Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthRespondInput {
    pub instance_id: ProviderInstanceId,
    pub flow_id: SetupOperationId,
    pub interaction_id: SetupOperationId,
    pub response: ProviderAuthResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderAuthPhase {
    Idle,
    Starting,
    Waiting,
    Verifying,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderCredentialOwner {
    #[serde(rename = "provider")]
    Provider,
    #[serde(rename = "t3")]
    T3,
}

/// Serde codec for `methods`: `optionalKey(ForwardCompatibleArray(X)
/// .check(isMaxLength(32)))`. Elements this build cannot decode are dropped;
/// the 32-entry cap applies to what remains. `null` is not a valid value.
mod auth_methods {
    use super::*;

    pub fn serialize<S: Serializer>(
        methods: &Option<Vec<ProviderAuthMethod>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match methods {
            Some(methods) => forward_compatible_array::serialize(methods, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<ProviderAuthMethod>>, D::Error> {
        let methods: Vec<ProviderAuthMethod> = forward_compatible_array::deserialize(d)?;
        if methods.len() > 32 {
            return Err(serde::de::Error::custom("auth methods are limited to 32"));
        }
        Ok(Some(methods))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthState {
    pub instance_id: ProviderInstanceId,
    pub phase: ProviderAuthPhase,
    pub flow_id: Option<SetupOperationId>,
    pub authorization_url: Option<String>,
    pub expires_at: Option<IsoDateTime>,
    pub message: Option<String>,
    // Newer servers may add method types, interactions, or owners; older
    // clients drop what they cannot decode instead of rejecting the state.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "auth_methods"
    )]
    pub methods: Option<Vec<ProviderAuthMethod>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::forward_compatible_optional_nullable"
    )]
    pub interaction: Option<Option<ProviderAuthInteraction>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::forward_compatible_optional"
    )]
    pub credential_owner: Option<ProviderCredentialOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthCompleteInput {
    pub instance_id: ProviderInstanceId,
    pub flow_id: SetupOperationId,
    #[serde(deserialize_with = "checks::max_trimmed::<_, 16384>")]
    pub callback_url: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthCancelInput {
    pub instance_id: ProviderInstanceId,
    pub flow_id: SetupOperationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderInstallPhase {
    Idle,
    Downloading,
    Extracting,
    Verifying,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderInstallSource {
    #[serde(rename = "managed")]
    Managed,
    #[serde(rename = "local")]
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstallState {
    pub driver: ProviderDriverKind,
    pub operation_id: Option<SetupOperationId>,
    pub phase: ProviderInstallPhase,
    pub downloaded_bytes: NonNegativeInt,
    pub total_bytes: Option<NonNegativeInt>,
    pub version: Option<TrimmedNonEmptyString>,
    pub installed_version: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub executable_path: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub source: Option<Option<ProviderInstallSource>>,
    pub can_remove: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstallCancelInput {
    pub instance_id: ProviderInstanceId,
    pub operation_id: SetupOperationId,
}

/// `ProviderSetupError` (Schema.TaggedError). Safe setup failure text: never
/// include OAuth codes, URLs, or native token data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderSetupError {
    #[serde(rename = "ProviderSetupError")]
    #[error("{detail}")]
    SetupError {
        instance_id: ProviderInstanceId,
        operation: String,
        detail: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "optional_unknown"
        )]
        cause: Option<serde_json::Value>,
    },
}

// A selected registration is reused on the primary without copying refresh ownership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptReconnectProfile {
    #[serde(deserialize_with = "checks::oaiapp_client_id")]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub email: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "checks::loopback_redirect_uri"
    )]
    pub redirect_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "checks::max_string_opt::<_, 16384>"
    )]
    pub id_token_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptTransferredProfileCredentials {
    pub client_id: String,
    #[serde(deserialize_with = "checks::non_empty_max_string::<_, 16384>")]
    pub access_token: String,
    #[serde(deserialize_with = "checks::max_string_nullable::<_, 16384>")]
    pub refresh_token: Option<String>,
    #[serde(deserialize_with = "checks::non_empty_max_string::<_, 16384>")]
    pub id_token: String,
    pub issuer: String,
    pub expires_at: f64,
    pub earliest_refresh_at: Option<f64>,
    pub scopes: Vec<String>,
    pub subject: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptTransferredProfile {
    pub registration: ChatGptReconnectProfile,
    pub credentials: ChatGptTransferredProfileCredentials,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptReconnectProfileInput {
    pub instance_id: ProviderInstanceId,
    pub method_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptImportProfileInput {
    pub instance_id: ProviderInstanceId,
    pub profile: ChatGptTransferredProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptHandoffInput {
    pub instance_id: ProviderInstanceId,
    pub environment_id: EnvironmentId,
    #[serde(deserialize_with = "checks::max_string::<_, 128>")]
    pub attempt_id: String,
    #[serde(deserialize_with = "checks::max_string::<_, 4096>")]
    pub return_url: String,
    pub profile: Option<ChatGptReconnectProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatGptHandoffState {
    #[serde(rename = "auth")]
    Auth { state: ProviderAuthState },
    #[serde(rename = "finished")]
    Finished { profile: ChatGptTransferredProfile },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode_response(
        value: serde_json::Value,
    ) -> Result<ProviderAuthResponse, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn decode_state(value: serde_json::Value) -> Result<ProviderAuthState, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn credential_responses_enforce_field_limit_and_value_checks() {
        let values: serde_json::Map<String, serde_json::Value> = (0..16)
            .map(|i| (format!("field_{i}"), json!("value")))
            .collect();
        let ok = decode_response(json!({"type": "credentials", "values": values})).unwrap();
        // Round trip keeps order and shape.
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"type": "credentials", "values": values})
        );
        let mut extra = values.clone();
        extra.insert("extra".into(), json!("value"));
        assert!(decode_response(json!({"type": "credentials", "values": extra})).is_err());
        assert!(decode_response(json!({"type": "credentials", "values": {"": "value"}})).is_err());
        assert!(
            decode_response(
                json!({"type": "credentials", "values": {"token": "x".repeat(16_385)}})
            )
            .is_err()
        );
        assert!(
            decode_response(
                json!({"type": "credentials", "values": {"token": "x".repeat(16_384)}})
            )
            .is_ok()
        );
    }

    #[test]
    fn terminal_and_browser_responses() {
        assert!(decode_response(json!({"type": "browser", "action": "accept"})).is_ok());
        assert!(decode_response(json!({"type": "browser", "action": "later"})).is_err());
        let terminal =
            json!({"type": "terminal", "data": "ls\n", "size": {"cols": 500, "rows": 1}});
        assert_eq!(
            serde_json::to_value(decode_response(terminal.clone()).unwrap()).unwrap(),
            terminal
        );
        for size in [
            json!({"cols": 0, "rows": 10}),
            json!({"cols": 501, "rows": 10}),
            json!({"cols": 10, "rows": 0}),
            json!({"cols": 10, "rows": 201}),
        ] {
            assert!(
                decode_response(json!({"type": "terminal", "data": "", "size": size})).is_err()
            );
        }
        assert!(decode_response(json!({"type": "terminal", "data": "x".repeat(4097)})).is_err());
        // An unknown response type is not forward compatible: it fails.
        assert!(decode_response(json!({"type": "passkey"})).is_err());
    }

    fn state_json() -> serde_json::Value {
        json!({
            "instanceId": "cursor", "phase": "waiting", "flowId": null,
            "authorizationUrl": null, "expiresAt": null, "message": null,
        })
    }

    #[test]
    fn auth_state_drops_variants_from_newer_servers() {
        let method =
            json!({"id": "browser", "name": "Browser", "description": null, "type": "agent"});
        let mut input = state_json();
        input["methods"] = json!([method, {"id": "passkey", "name": "Browser", "description": null, "type": "passkey"}]);
        input["interaction"] = json!({"type": "passkey", "id": "passkey"});
        input["credentialOwner"] = json!("keychain");
        let decoded = decode_state(input).unwrap();
        assert_eq!(decoded.methods.as_ref().unwrap().len(), 1);
        assert_eq!(decoded.interaction, None);
        assert_eq!(decoded.credential_owner, None);
        let mut expected = state_json();
        expected["methods"] = json!([method]);
        // `type` (not `type_`) on the wire, and dropped keys stay off it.
        assert_eq!(serde_json::to_value(&decoded).unwrap(), expected);
    }

    #[test]
    fn auth_state_interaction_distinguishes_null_absent_and_value() {
        let absent = decode_state(state_json()).unwrap();
        assert_eq!(absent.interaction, None);
        assert_eq!(serde_json::to_value(&absent).unwrap(), state_json());

        let mut with_null = state_json();
        with_null["interaction"] = json!(null);
        let null = decode_state(with_null.clone()).unwrap();
        assert_eq!(null.interaction, Some(None));
        assert_eq!(serde_json::to_value(&null).unwrap(), with_null);

        let mut with_value = state_json();
        with_value["interaction"] =
            json!({"type": "deviceCode", "id": "d", "url": "https://x", "userCode": "ABCD"});
        with_value["credentialOwner"] = json!("t3");
        let value = decode_state(with_value.clone()).unwrap();
        assert!(matches!(
            value.interaction,
            Some(Some(ProviderAuthInteraction::DeviceCode { .. }))
        ));
        assert_eq!(value.credential_owner, Some(ProviderCredentialOwner::T3));
        assert_eq!(serde_json::to_value(&value).unwrap(), with_value);

        // A *known* interaction tag with a broken payload also decodes as
        // absent (ForwardCompatibleOptional swallows any decode failure).
        let mut broken = state_json();
        broken["interaction"] = json!({"type": "deviceCode", "id": "d"});
        assert_eq!(decode_state(broken).unwrap().interaction, None);
    }

    #[test]
    fn auth_state_methods_are_capped_after_dropping_unknowns() {
        let method = |i: usize| json!({"id": format!("m{i}"), "name": "M", "description": "d", "type": "terminal"});
        let mut input = state_json();
        input["methods"] = json!((0..32).map(method).collect::<Vec<_>>());
        assert!(decode_state(input.clone()).is_ok());
        let mut over = (0..33).map(method).collect::<Vec<_>>();
        input["methods"] = json!(over.clone());
        assert!(decode_state(input.clone()).is_err());
        // Unknown members do not count against the cap.
        over[0]["type"] = json!("passkey");
        input["methods"] = json!(over);
        assert_eq!(
            decode_state(input.clone()).unwrap().methods.unwrap().len(),
            32
        );
        // null is not a valid `methods` value.
        input["methods"] = json!(null);
        assert!(decode_state(input).is_err());
    }

    #[test]
    fn interactions_enforce_limits() {
        let decode =
            |value: serde_json::Value| serde_json::from_value::<ProviderAuthInteraction>(value);
        assert!(decode(json!({"type": "browser", "id": "b", "url": "https://x", "requiresConsent": true, "acceptsCallback": false})).is_ok());
        assert!(decode(json!({"type": "browser", "id": "b", "url": "x".repeat(16_385), "requiresConsent": true})).is_err());
        assert!(
            decode(
                json!({"type": "deviceCode", "id": "b", "url": "u", "userCode": "x".repeat(257)})
            )
            .is_err()
        );
        assert!(
            decode(json!({"type": "terminal", "id": "t", "output": "x", "outputOffset": -1}))
                .is_err()
        );
        let terminal = json!({"type": "terminal", "id": "t", "output": "x", "outputOffset": 0});
        assert_eq!(
            serde_json::to_value(decode(terminal.clone()).unwrap()).unwrap(),
            terminal
        );
        let field = json!({"name": "n", "label": "L", "secret": true});
        assert!(
            decode(json!({"type": "credentials", "id": "c", "fields": vec![field.clone(); 16]}))
                .is_ok()
        );
        assert!(
            decode(json!({"type": "credentials", "id": "c", "fields": vec![field; 17]})).is_err()
        );
        assert!(decode(json!({"type": "browser", "id": "x".repeat(129), "url": "u", "requiresConsent": false})).is_err());
    }

    #[test]
    fn install_state_tri_state_fields() {
        let base = json!({
            "driver": "codex", "operationId": null, "phase": "idle", "downloadedBytes": 0,
            "totalBytes": null, "version": null, "installedVersion": null,
            "canRemove": false, "message": null,
        });
        let absent: ProviderInstallState = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(absent.executable_path, None);
        assert_eq!(serde_json::to_value(&absent).unwrap(), base);

        let mut with_null = base.clone();
        with_null["executablePath"] = json!(null);
        with_null["source"] = json!(null);
        let null: ProviderInstallState = serde_json::from_value(with_null.clone()).unwrap();
        assert_eq!(null.executable_path, Some(None));
        assert_eq!(null.source, Some(None));
        assert_eq!(serde_json::to_value(&null).unwrap(), with_null);

        let mut with_value = base.clone();
        with_value["executablePath"] = json!("/usr/bin/codex");
        with_value["source"] = json!("managed");
        let value: ProviderInstallState = serde_json::from_value(with_value.clone()).unwrap();
        assert_eq!(value.source, Some(Some(ProviderInstallSource::Managed)));
        assert_eq!(serde_json::to_value(&value).unwrap(), with_value);

        let mut negative = base;
        negative["downloadedBytes"] = json!(-1);
        assert!(serde_json::from_value::<ProviderInstallState>(negative).is_err());
    }

    #[test]
    fn chatgpt_profile_pattern_checks() {
        let decode =
            |value: serde_json::Value| serde_json::from_value::<ChatGptReconnectProfile>(value);
        assert!(decode(json!({"clientId": "oaiapp_abc-DEF_1"})).is_ok());
        for bad in ["oaiapp_", "abc", "oaiapp_a b", "oaiapp_é"] {
            assert!(decode(json!({"clientId": bad})).is_err(), "{bad}");
        }
        for ok in [
            "http://127.0.0.1:1455/auth/callback",
            "http://localhost:65535/auth/callback",
        ] {
            assert!(
                decode(json!({"clientId": "oaiapp_a", "redirectUri": ok})).is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "https://localhost:1455/auth/callback",
            "http://localhost:0/auth/callback",
            "http://localhost:012/auth/callback",
            "http://localhost:123456/auth/callback",
            "http://localhost/auth/callback",
            "http://example.com:1455/auth/callback",
            "http://localhost:1455/auth/callback/",
        ] {
            assert!(
                decode(json!({"clientId": "oaiapp_a", "redirectUri": bad})).is_err(),
                "{bad}"
            );
        }
        // email: absent vs null vs value stay distinct.
        let null = decode(json!({"clientId": "oaiapp_a", "email": null})).unwrap();
        assert_eq!(null.email, Some(None));
        assert_eq!(
            serde_json::to_value(&null).unwrap(),
            json!({"clientId": "oaiapp_a", "email": null})
        );
        assert_eq!(decode(json!({"clientId": "oaiapp_a"})).unwrap().email, None);
        assert!(
            decode(json!({"clientId": "oaiapp_a", "idTokenHint": "x".repeat(16_385)})).is_err()
        );
    }

    #[test]
    fn transferred_profile_credentials_limits() {
        let credentials = |access: &str| {
            json!({
                "registration": {"clientId": "oaiapp_a"},
                "credentials": {
                    "clientId": "c", "accessToken": access, "refreshToken": null,
                    "idToken": "id", "issuer": "i", "expiresAt": 1.5,
                    "earliestRefreshAt": null, "scopes": ["a"], "subject": "s", "email": null,
                },
            })
        };
        let ok: ChatGptTransferredProfile = serde_json::from_value(credentials("tok")).unwrap();
        assert_eq!(ok.credentials.expires_at, 1.5);
        assert_eq!(serde_json::to_value(&ok).unwrap(), credentials("tok"));
        assert!(serde_json::from_value::<ChatGptTransferredProfile>(credentials("")).is_err());
        assert!(
            serde_json::from_value::<ChatGptTransferredProfile>(credentials(&"x".repeat(16_385)))
                .is_err()
        );
    }

    #[test]
    fn handoff_state_and_input() {
        let finished = json!({
            "phase": "finished",
            "profile": {
                "registration": {"clientId": "oaiapp_a"},
                "credentials": {
                    "clientId": "c", "accessToken": "t", "refreshToken": "r", "idToken": "i",
                    "issuer": "i", "expiresAt": 1, "earliestRefreshAt": 2, "scopes": [],
                    "subject": "s", "email": "e@x",
                },
            },
        });
        assert!(matches!(
            serde_json::from_value::<ChatGptHandoffState>(finished).unwrap(),
            ChatGptHandoffState::Finished { .. }
        ));
        let auth = json!({"phase": "auth", "state": state_json()});
        assert!(matches!(
            serde_json::from_value::<ChatGptHandoffState>(auth).unwrap(),
            ChatGptHandoffState::Auth { .. }
        ));
        let input = |attempt: String| {
            serde_json::from_value::<ChatGptHandoffInput>(json!({
                "instanceId": "codex", "environmentId": "env", "attemptId": attempt,
                "returnUrl": "https://x", "profile": null,
            }))
        };
        assert!(input("a".repeat(128)).is_ok());
        assert!(input("a".repeat(129)).is_err());
    }

    #[test]
    fn codex_callback_and_misc_inputs() {
        let input = |flow: &str| {
            serde_json::from_value::<CodexAuthCallbackInput>(json!({
                "authorizationUrl": "https://a", "returnUrl": "https://r",
                "environmentId": "env", "instanceId": "codex", "flowId": flow,
            }))
        };
        assert!(input("  flow  ").unwrap().flow_id.as_str() == "flow");
        assert!(input("   ").is_err());
        assert!(input(&"f".repeat(129)).is_err());
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<CodexAuthCallbackState>(json!({"phase": "ready"}))
                    .unwrap()
            )
            .unwrap(),
            json!({"phase": "ready"})
        );
        assert!(
            serde_json::from_value::<CodexAuthCallbackState>(
                json!({"phase": "finished", "callbackUrl": "x".repeat(16_385)})
            )
            .is_err()
        );
        let start: ProviderAuthStartInput =
            serde_json::from_value(json!({"instanceId": "codex", "callbackMode": "client"}))
                .unwrap();
        assert_eq!(start.callback_mode, Some(ProviderAuthCallbackMode::Client));
        assert_eq!(start.method_id, None);
    }

    #[test]
    fn setup_error_message_is_detail() {
        let error = ProviderSetupError::SetupError {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            operation: "install".into(),
            detail: "disk full".into(),
            cause: None,
        };
        assert_eq!(error.to_string(), "disk full");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"_tag": "ProviderSetupError", "instanceId": "codex", "operation": "install", "detail": "disk full"})
        );
    }

    #[test]
    fn setup_operation_id_counts_utf16_units() {
        assert!(SetupOperationId::new("a".repeat(128)).is_ok());
        assert!(SetupOperationId::new("a".repeat(129)).is_err());
        // 65 astral chars = 130 UTF-16 units (but only 65 chars / 260 bytes).
        assert!(SetupOperationId::new("\u{1F600}".repeat(65)).is_err());
        assert!(SetupOperationId::new("\u{1F600}".repeat(64)).is_ok());
    }
}
