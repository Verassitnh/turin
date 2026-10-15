//! Port of upstream/packages/contracts/src/providerSetup.ts

use crate::base_schemas::{
    lenient_nullable_option, lenient_option, ContractError, EnvironmentId, IsoDateTime,
    TrimmedNonEmptyString,
};
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSetupInput {
    pub instance_id: ProviderInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAuthCallbackInput {
    pub authorization_url: String,
    pub return_url: String,
    pub environment_id: EnvironmentId,
    pub instance_id: ProviderInstanceId,
    pub flow_id: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CodexAuthCallbackState {
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "finished")]
    Finished { callback_url: String },
}

/// `SetupOperationId`: trimmed non-empty, ≤ 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetupOperationId(TrimmedNonEmptyString);

impl SetupOperationId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        if value.as_str().len() > 128 {
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
    pub type_: ProviderAuthMethodType,
}

// These describe client interactions, not OAuth grant types. The provider
// adapter remains responsible for credentials, callbacks, and refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderAuthInteraction {
    #[serde(rename = "browser")]
    Browser {
        id: SetupOperationId,
        url: TrimmedNonEmptyString,
        requires_consent: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accepts_callback: Option<bool>,
    },
    #[serde(rename = "deviceCode")]
    DeviceCode {
        id: SetupOperationId,
        url: TrimmedNonEmptyString,
        user_code: TrimmedNonEmptyString,
    },
    #[serde(rename = "terminal")]
    Terminal {
        id: SetupOperationId,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_offset: Option<i64>,
    },
    #[serde(rename = "credentials")]
    Credentials {
        id: SetupOperationId,
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
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderAuthResponse {
    #[serde(rename = "browser")]
    Browser { action: ProviderAuthBrowserAction },
    #[serde(rename = "terminal")]
    Terminal {
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ProviderAuthTerminalSize>,
    },
    #[serde(rename = "credentials")]
    Credentials { values: BTreeMap<SetupOperationId, String> },
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
    pub cols: i64,
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

/// Serde codec for `methods`: forward-compatible array with upstream's
/// 32-entry cap.
mod auth_methods {
    use super::*;

    pub fn serialize<S: Serializer>(
        methods: &Option<Vec<ProviderAuthMethod>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        methods.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<ProviderAuthMethod>>, D::Error> {
        let raw = Option::<Vec<serde_json::Value>>::deserialize(d)?;
        match raw {
            None => Ok(None),
            Some(values) => {
                let methods: Vec<ProviderAuthMethod> = values
                    .into_iter()
                    .filter_map(|value| serde_json::from_value(value).ok())
                    .collect();
                if methods.len() > 32 {
                    return Err(serde::de::Error::custom("auth methods are limited to 32"));
                }
                Ok(Some(methods))
            }
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none", with = "auth_methods")]
    pub methods: Option<Vec<ProviderAuthMethod>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_nullable_option::deserialize"
    )]
    pub interaction: Option<Option<ProviderAuthInteraction>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_option::deserialize"
    )]
    pub credential_owner: Option<ProviderCredentialOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthCompleteInput {
    pub instance_id: ProviderInstanceId,
    pub flow_id: SetupOperationId,
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
    pub downloaded_bytes: i64,
    pub total_bytes: Option<i64>,
    pub version: Option<TrimmedNonEmptyString>,
    pub installed_version: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<Option<TrimmedNonEmptyString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderSetupError {
    #[serde(rename = "ProviderSetupError")]
    #[error("{detail}")]
    SetupError {
        instance_id: ProviderInstanceId,
        operation: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

// A selected registration is reused on the primary without copying refresh ownership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptReconnectProfile {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptTransferredProfileCredentials {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
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
    pub attempt_id: String,
    pub return_url: String,
    pub profile: Option<ChatGptReconnectProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChatGptHandoffState {
    #[serde(rename = "auth")]
    Auth { state: ProviderAuthState },
    #[serde(rename = "finished")]
    Finished { profile: ChatGptTransferredProfile },
}
