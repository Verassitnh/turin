//! Port of the storage helpers in upstream/apps/server/src/provider/CodexChatGptAuth.ts
//! (`readSessions`, `read`, `writeSessions`, `readRegistrations`,
//! `saveRegistration`, `save`, `clearTokens`, `remove`).
//!
//! Token sets and registrations live in two secret bindings for this instance.
//! Callers hold the session lock and the controller's permit; these methods do
//! not take either.

use std::sync::Arc;

use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::ProviderSetupError;
use turin_provider_core::credential_store::{ProviderCredentials, SecretStore};

use super::records::{
    CodexChatGptCredentials, RegistrationProfiles, Sessions, StoredRegistration,
    decode_registrations, decode_sessions,
};

const SESSIONS_NAMESPACE: &str = "codex-chatgpt";
const REGISTRATION_NAMESPACE: &str = "codex-chatgpt-registration";

pub fn failure(
    instance_id: &ProviderInstanceId,
    operation: &str,
    detail: &str,
) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: operation.to_owned(),
        detail: detail.to_owned(),
        cause: None,
    }
}

/// The two stored bindings of one Codex instance: token sets and registrations.
pub struct ChatGptStore {
    instance_id: ProviderInstanceId,
    sessions: ProviderCredentials,
    registrations: ProviderCredentials,
}

impl ChatGptStore {
    pub fn new(secrets: Arc<dyn SecretStore>, instance_id: ProviderInstanceId) -> Self {
        let binding_id = instance_id.as_str().to_owned();
        Self {
            sessions: ProviderCredentials::open(secrets.clone(), SESSIONS_NAMESPACE, &binding_id),
            registrations: ProviderCredentials::open(secrets, REGISTRATION_NAMESPACE, &binding_id),
            instance_id,
        }
    }

    /// The credential binding the controller advertises (`store.binding`).
    pub fn binding(&self) -> &turin_provider_core::auth::ProviderCredentialBinding {
        &self.sessions.binding
    }

    fn fail(&self, operation: &str, detail: &str) -> ProviderSetupError {
        failure(&self.instance_id, operation, detail)
    }

    /// Upstream `readSessions`: a missing key is an empty store.
    pub async fn read_sessions(&self) -> Result<Sessions, ProviderSetupError> {
        let bytes = self
            .sessions
            .get()
            .await
            .map_err(|_| self.fail("read", "Could not read the saved ChatGPT connection."))?;
        let Some(bytes) = bytes else {
            return Ok(Sessions {
                active_client_id: None,
                sessions: Vec::new(),
            });
        };
        decode_sessions(&String::from_utf8_lossy(&bytes)).map_err(|_| {
            self.fail(
                "read",
                "The saved ChatGPT connection is invalid. Sign in again.",
            )
        })
    }

    /// Upstream `read`: the active token set, if any.
    pub async fn read(&self) -> Result<Option<CodexChatGptCredentials>, ProviderSetupError> {
        let saved = self.read_sessions().await?;
        Ok(saved
            .sessions
            .iter()
            .find(|session| Some(&session.client_id) == saved.active_client_id.as_ref())
            .cloned())
    }

    async fn write_sessions(&self, saved: &Sessions) -> Result<(), ProviderSetupError> {
        let json = serde_json::to_vec(saved)
            .map_err(|_| self.fail("save", "Could not save the ChatGPT connection."))?;
        self.sessions
            .set(&json)
            .await
            .map_err(|_| self.fail("save", "Could not save the ChatGPT connection."))
    }

    /// Upstream `readRegistrations`: the most recent profile first, and every
    /// profile labelled. A missing label is `Connection N` by its stored position.
    pub async fn read_registrations(&self) -> Result<RegistrationProfiles, ProviderSetupError> {
        let bytes = self.registrations.get().await.map_err(|_| {
            self.fail(
                "registration",
                "Could not read the ChatGPT sign-in registration. Try again.",
            )
        })?;
        let Some(bytes) = bytes else {
            return Ok(RegistrationProfiles {
                profiles: Vec::new(),
                last_client_id: None,
            });
        };
        let saved = decode_registrations(&String::from_utf8_lossy(&bytes)).map_err(|_| {
            self.fail(
                "registration",
                "The saved ChatGPT sign-in registration is invalid.",
            )
        })?;
        let last = saved
            .profiles
            .iter()
            .position(|profile| Some(&profile.client_id) == saved.last_client_id.as_ref());
        let mut ordered: Vec<(usize, StoredRegistration)> = Vec::new();
        match last {
            Some(last) => {
                ordered.push((last, saved.profiles[last].clone()));
                for (index, profile) in saved.profiles.iter().enumerate() {
                    if profile.client_id != saved.profiles[last].client_id {
                        ordered.push((index, profile.clone()));
                    }
                }
            }
            None => {
                ordered.extend(saved.profiles.iter().cloned().enumerate());
            }
        }
        let profiles = ordered
            .into_iter()
            .map(|(index, mut profile)| {
                profile.connection_label = Some(
                    profile
                        .connection_label
                        .take()
                        .unwrap_or_else(|| format!("Connection {}", index + 1)),
                );
                profile
            })
            .collect();
        Ok(RegistrationProfiles {
            profiles,
            last_client_id: saved.last_client_id,
        })
    }

    /// Upstream `saveRegistration`: the profile moves to the front and becomes
    /// the last used. It keeps the label already stored for its client id.
    pub async fn save_registration(
        &self,
        mut profile: StoredRegistration,
    ) -> Result<(), ProviderSetupError> {
        let saved = self.read_registrations().await?;
        let stored_label = saved
            .profiles
            .iter()
            .find(|entry| entry.client_id == profile.client_id)
            .and_then(|entry| entry.connection_label.clone());
        profile.connection_label = Some(
            stored_label
                .or_else(|| profile.connection_label.take())
                .unwrap_or_else(|| format!("Connection {}", saved.profiles.len() + 1)),
        );
        let client_id = profile.client_id.clone();
        let mut profiles = vec![profile];
        profiles.extend(
            saved
                .profiles
                .into_iter()
                .filter(|entry| entry.client_id != client_id),
        );
        let json = serde_json::to_vec(&RegistrationProfiles {
            profiles,
            last_client_id: Some(client_id),
        })
        .map_err(|_| self.registration_save_failure())?;
        self.registrations
            .set(&json)
            .await
            .map_err(|_| self.registration_save_failure())
    }

    fn registration_save_failure(&self) -> ProviderSetupError {
        self.fail(
            "registration",
            "Could not save the ChatGPT sign-in registration. Try again.",
        )
    }

    /// Upstream `save`: the active pointer and the token sets change in one write.
    pub async fn save(
        &self,
        record: CodexChatGptCredentials,
        activate: bool,
    ) -> Result<(), ProviderSetupError> {
        let saved = self.read_sessions().await?;
        let mut sessions: Vec<CodexChatGptCredentials> = saved
            .sessions
            .into_iter()
            .filter(|session| session.client_id != record.client_id)
            .collect();
        let active_client_id = if activate {
            Some(record.client_id.clone())
        } else {
            saved.active_client_id
        };
        sessions.push(record);
        self.write_sessions(&Sessions {
            active_client_id,
            sessions,
        })
        .await
    }

    /// Upstream `clearTokens`: drop the active token set. Other saved accounts
    /// stay; with none left the whole binding is removed.
    pub async fn clear_tokens(&self) -> Result<(), ProviderSetupError> {
        let saved = self.read_sessions().await?;
        let sessions: Vec<CodexChatGptCredentials> = saved
            .sessions
            .into_iter()
            .filter(|session| Some(&session.client_id) != saved.active_client_id.as_ref())
            .collect();
        if !sessions.is_empty() {
            return self
                .write_sessions(&Sessions {
                    active_client_id: None,
                    sessions,
                })
                .await;
        }
        self.sessions.remove().await.map_err(|_| {
            self.fail(
                "disconnect",
                "Could not clear the ChatGPT connection. Try again.",
            )
        })
    }

    /// Upstream `remove`: promote a legacy identity into its registration
    /// before the token set goes, so the profile keeps its subject and email.
    pub async fn remove(&self) -> Result<(), ProviderSetupError> {
        if let Some(credentials) = self.read().await? {
            let saved = self.read_registrations().await?;
            let profile = saved
                .profiles
                .into_iter()
                .find(|entry| entry.client_id == credentials.client_id);
            if let Some(profile) = profile
                && (profile.subject.is_none() || profile.email.is_none())
            {
                self.save_registration(StoredRegistration {
                    subject: Some(credentials.subject),
                    email: Some(credentials.email),
                    ..profile
                })
                .await?;
            }
        }
        self.clear_tokens().await
    }
}

#[cfg(test)]
mod tests {
    use turin_provider_core::credential_store::MemorySecretStore;

    use super::*;

    fn instance() -> ProviderInstanceId {
        ProviderInstanceId::new("codex-test").unwrap()
    }

    fn store() -> ChatGptStore {
        ChatGptStore::new(Arc::new(MemorySecretStore::new()), instance())
    }

    fn record(client_id: &str, subject: &str, email: Option<&str>) -> CodexChatGptCredentials {
        CodexChatGptCredentials {
            client_id: client_id.to_owned(),
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            expires_at: 1_000.0,
            earliest_refresh_at: None,
            scopes: vec!["chatgpt.tokens.use.direct".to_owned()],
            subject: subject.to_owned(),
            email: email.map(str::to_owned),
            id_token: None,
            issuer: None,
        }
    }

    fn registration(client_id: &str) -> StoredRegistration {
        StoredRegistration {
            client_id: client_id.to_owned(),
            subject: None,
            connection_label: None,
            sharing_enabled: None,
            email: None,
            redirect_uri: None,
        }
    }

    #[tokio::test]
    async fn an_empty_binding_reads_as_no_connection() {
        let store = store();
        assert!(store.read().await.unwrap().is_none());
        assert!(
            store
                .read_registrations()
                .await
                .unwrap()
                .profiles
                .is_empty()
        );
    }

    #[tokio::test]
    async fn saving_activates_and_keeps_other_accounts() {
        let store = store();
        store
            .save(record("oaiapp_a", "s1", Some("a@x")), true)
            .await
            .unwrap();
        store
            .save(record("oaiapp_b", "s2", None), false)
            .await
            .unwrap();
        assert_eq!(store.read().await.unwrap().unwrap().client_id, "oaiapp_a");
        store
            .save(record("oaiapp_b", "s2", None), true)
            .await
            .unwrap();
        assert_eq!(store.read().await.unwrap().unwrap().client_id, "oaiapp_b");
    }

    #[tokio::test]
    async fn clearing_tokens_keeps_other_saved_accounts() {
        let store = store();
        store
            .save(record("oaiapp_a", "s1", None), true)
            .await
            .unwrap();
        store
            .save(record("oaiapp_b", "s2", None), false)
            .await
            .unwrap();
        store.clear_tokens().await.unwrap();
        assert!(store.read().await.unwrap().is_none());
        let saved = store.read_sessions().await.unwrap();
        assert_eq!(saved.sessions.len(), 1);
        assert_eq!(saved.sessions[0].client_id, "oaiapp_b");
    }

    #[tokio::test]
    async fn the_last_registration_is_first_and_labels_are_stable() {
        let store = store();
        store
            .save_registration(registration("oaiapp_a"))
            .await
            .unwrap();
        store
            .save_registration(registration("oaiapp_b"))
            .await
            .unwrap();
        let saved = store.read_registrations().await.unwrap();
        let order: Vec<_> = saved
            .profiles
            .iter()
            .map(|p| p.client_id.as_str())
            .collect();
        assert_eq!(order, ["oaiapp_b", "oaiapp_a"]);
        assert_eq!(
            saved.profiles[0].connection_label.as_deref(),
            Some("Connection 2")
        );
        assert_eq!(
            saved.profiles[1].connection_label.as_deref(),
            Some("Connection 1")
        );
        store
            .save_registration(registration("oaiapp_a"))
            .await
            .unwrap();
        let again = store.read_registrations().await.unwrap();
        assert_eq!(again.profiles[0].client_id, "oaiapp_a");
        assert_eq!(
            again.profiles[0].connection_label.as_deref(),
            Some("Connection 1")
        );
    }

    #[tokio::test]
    async fn remove_promotes_the_legacy_identity_into_its_registration() {
        let store = store();
        store
            .save_registration(registration("oaiapp_a"))
            .await
            .unwrap();
        store
            .save(record("oaiapp_a", "subject-a", Some("a@x.test")), true)
            .await
            .unwrap();
        store.remove().await.unwrap();
        assert!(store.read().await.unwrap().is_none());
        let saved = store.read_registrations().await.unwrap();
        assert_eq!(saved.profiles[0].subject.as_deref(), Some("subject-a"));
        assert_eq!(saved.profiles[0].email, Some(Some("a@x.test".to_owned())));
    }

    #[tokio::test]
    async fn an_invalid_saved_connection_fails_with_the_upstream_message() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = ChatGptStore::new(secrets.clone(), instance());
        store.sessions.set(b"{not json").await.unwrap();
        let error = store.read().await.unwrap_err();
        let ProviderSetupError::SetupError {
            operation, detail, ..
        } = error;
        assert_eq!(operation, "read");
        assert_eq!(
            detail,
            "The saved ChatGPT connection is invalid. Sign in again."
        );
    }
}
