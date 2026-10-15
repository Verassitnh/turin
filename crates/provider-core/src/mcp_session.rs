//! Port of upstream/packages/provider-core/src/server/mcpSession.ts.
//!
//! The orchestrator MCP server that mints these credentials is cut. Adapters
//! still read the config when a host supplies one, and still apply the device
//! environment the same way.

use std::{collections::HashSet, fmt};

use turin_contracts::{EnvironmentId, ThreadId, provider_instance::ProviderInstanceId};

use crate::host_process::{ProcessEnv, RedactedEnvironment};

#[derive(Clone)]
pub struct McpProviderSessionConfig {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub provider_session_id: String,
    pub provider_instance_id: ProviderInstanceId,
    pub endpoint: String,
    pub authorization_header: String,
    /// Whether this credential includes the "preview" capability. Adapters read
    /// it so the prompt does not advertise `preview_*` tools every call would reject.
    pub browser_tools_available: bool,
    /// Capabilities the credential grants ("preview", "device").
    pub capabilities: Option<HashSet<String>>,
    /// Set when the session may drive devices. Adapters spread this into the
    /// provider subprocess environment so `agent-device` is on `PATH`.
    pub agent_device_environment: Option<ProcessEnv>,
}

/// The bearer header and device environment values are credentials, so they
/// print as `[REDACTED]`.
impl fmt::Debug for McpProviderSessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpProviderSessionConfig")
            .field("environment_id", &self.environment_id)
            .field("thread_id", &self.thread_id)
            .field("provider_session_id", &self.provider_session_id)
            .field("provider_instance_id", &self.provider_instance_id)
            .field("endpoint", &self.endpoint)
            .field("authorization_header", &"[REDACTED]")
            .field("browser_tools_available", &self.browser_tools_available)
            .field("capabilities", &self.capabilities)
            .field(
                "agent_device_environment",
                &self
                    .agent_device_environment
                    .as_ref()
                    .map(RedactedEnvironment),
            )
            .finish()
    }
}

/// Provider env with the device variables applied over `base`, or `base` untouched.
pub fn with_agent_device_environment(
    base: &ProcessEnv,
    config: Option<&McpProviderSessionConfig>,
) -> ProcessEnv {
    let Some(extra) = config.and_then(|config| config.agent_device_environment.as_ref()) else {
        return base.clone();
    };
    let separator = extra
        .get("PATH_SEPARATOR")
        .map(String::as_str)
        .unwrap_or(":");
    let base_path = base.get("PATH").or_else(|| base.get("Path"));
    let shim_dir = extra.get("PATH").cloned();
    let mut merged = base.clone();
    for (key, value) in extra {
        if key == "PATH" || key == "PATH_SEPARATOR" {
            continue;
        }
        merged.insert(key.clone(), value.clone());
    }
    if let Some(shim_dir) = shim_dir {
        let path = match base_path {
            Some(base_path) => format!("{shim_dir}{separator}{base_path}"),
            None => shim_dir,
        };
        merged.insert("PATH".into(), path);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ProcessEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn preserves_provider_credentials_while_routing_devices() {
        let base = env(&[
            ("PATH", "/provider/bin:/usr/bin"),
            ("PROVIDER_KEY", "fixture"),
        ]);
        let mut extra = env(&[
            ("PATH", "/t3/device/bin"),
            ("PATH_SEPARATOR", ":"),
            ("AGENT_DEVICE_DAEMON_BASE_URL", "http://127.0.0.1:9000"),
            ("AGENT_DEVICE_DAEMON_AUTH_TOKEN", "fixture-device"),
        ]);
        let _ = &mut extra;
        let config = McpProviderSessionConfig {
            environment_id: EnvironmentId::new("env").unwrap(),
            thread_id: ThreadId::new("thread").unwrap(),
            provider_session_id: "session".into(),
            provider_instance_id: ProviderInstanceId::new("codex").unwrap(),
            endpoint: "http://127.0.0.1".into(),
            authorization_header: "Bearer x".into(),
            browser_tools_available: false,
            capabilities: None,
            agent_device_environment: Some(env(&[
                ("PATH", "/t3/device/bin"),
                ("PATH_SEPARATOR", ":"),
                ("AGENT_DEVICE_DAEMON_BASE_URL", "http://127.0.0.1:9000"),
                ("AGENT_DEVICE_DAEMON_AUTH_TOKEN", "fixture-device"),
            ])),
        };
        assert_eq!(
            with_agent_device_environment(&base, Some(&config)),
            env(&[
                ("PATH", "/t3/device/bin:/provider/bin:/usr/bin"),
                ("PROVIDER_KEY", "fixture"),
                ("AGENT_DEVICE_DAEMON_BASE_URL", "http://127.0.0.1:9000"),
                ("AGENT_DEVICE_DAEMON_AUTH_TOKEN", "fixture-device"),
            ])
        );
    }

    #[test]
    fn debug_output_redacts_the_bearer_and_device_token() {
        let config = McpProviderSessionConfig {
            environment_id: EnvironmentId::new("env").unwrap(),
            thread_id: ThreadId::new("thread").unwrap(),
            provider_session_id: "session".into(),
            provider_instance_id: ProviderInstanceId::new("codex").unwrap(),
            endpoint: "http://127.0.0.1".into(),
            authorization_header: "Bearer fixture-bearer".into(),
            browser_tools_available: false,
            capabilities: None,
            agent_device_environment: Some(env(&[(
                "AGENT_DEVICE_DAEMON_AUTH_TOKEN",
                "fixture-device",
            )])),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("fixture-bearer"), "{debug}");
        assert!(!debug.contains("fixture-device"), "{debug}");
        // Keys and non-secret fields stay visible for diagnostics.
        assert!(debug.contains("AGENT_DEVICE_DAEMON_AUTH_TOKEN"), "{debug}");
        assert!(debug.contains("http://127.0.0.1"), "{debug}");
    }

    #[test]
    fn does_not_grant_cli_access_when_device_access_was_not_supplied() {
        let base = env(&[("PATH", "/usr/bin"), ("PROVIDER_KEY", "fixture")]);
        assert_eq!(with_agent_device_environment(&base, None), base);
    }
}
